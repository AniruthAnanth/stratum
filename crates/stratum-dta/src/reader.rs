//! The `.dta` reader.
//!
//! Structure follows `04` §9.4 step for step. What is *not* in §9.4, and is the
//! reason this file is as long as it is, is that every one of those steps is
//! written against a hostile input: `docs/THREAT_MODEL.md` §2 enumerates the
//! cases and `tests/hostile.rs` asserts one clean typed error for each.
//!
//! The three properties that shape the code:
//!
//! 1. **No allocation is ever sized by a number read out of the file** until
//!    that number has been compared against the measured file length. `<N>` of
//!    2^40 costs one `checked_mul` and one comparison.
//! 2. **The map is validated as a whole before any of it is used** as an index.
//!    Piecemeal validation is how a reader ends up trusting `map[9]` because
//!    `map[3]` looked fine.
//! 3. **Fixed-width fields are bytes up to the first NUL.** Stata writes
//!    uninitialised memory into the padding, in files it ships (`04` §0.2).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use memmap2::Mmap;
use stratum_core::missing::{
    F32_MISS_BITS, F32_MISS_STEP, F64_MISS_BITS, F64_MISS_STEP, MAX_TAG, SYSMISS, SYSMISS_F32,
};
use stratum_core::StorageType;
use stratum_data::{CharTable, Column, NumCol, ValueLabelSet};

use crate::codepage::{decode, until_nul, DecodeStat, Encoding};
use crate::gso::{GsoTable, EMPTY_KEY};
use crate::spec::{
    self, ByteOrder, Release, ReleaseSpec, MAP_BYTES, MAP_CLOSE, MAP_DATA, MAP_ENTRIES, MAP_EOF,
    MAP_STRLS, MAP_VALUE_LABELS, SECTIONS,
};
use crate::{
    bump, counters, value_label_from, Dataset, DtaColumn, DtaError, DtaVar, ReadReport,
    ReadWarning, StrLData,
};

/// Files at least this large are memory-mapped; below it a plain read is
/// cheaper than the mapping's page faults (`04` §1.1).
pub const MMAP_MIN_BYTES: u64 = 1 << 20;

/// How to read a file.
#[derive(Clone, Copy, Debug, Default)]
pub struct DtaReadOptions {
    /// Override the text encoding. `None` means the release's own rule: UTF-8
    /// for 118/119, Windows-1252 for 117 — an **assumption**, because a 117
    /// file does not record its codepage (`04` §9.4). The assumption used is
    /// reported in [`ReadReport::encoding`].
    pub encoding: Option<Encoding>,
    /// Set false to force the `read`-to-heap path. Only tests and the fuzzer
    /// want this.
    pub no_mmap: bool,
}

/// The header, without reading the data. Cheap enough for a file listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DtaProbe {
    /// The declared release.
    pub release: Release,
    /// The declared byte order.
    pub byte_order: ByteOrder,
    /// Variables.
    pub n_vars: u32,
    /// Observations.
    pub n_obs: u64,
    /// The dataset label.
    pub label: String,
    /// `<timestamp>`, verbatim.
    pub timestamp: String,
    /// The file's real length in bytes.
    pub file_len: u64,
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Read a `.dta` file with the default options.
///
/// # Errors
///
/// [`DtaError`]. Never panics, on any input.
pub fn read_dta<P: AsRef<Path>>(path: P) -> Result<Dataset, DtaError> {
    read_dta_with(path, &DtaReadOptions::default())
}

/// Read a `.dta` file.
///
/// # Errors
///
/// [`DtaError`]. Never panics, on any input.
pub fn read_dta_with<P: AsRef<Path>>(path: P, opts: &DtaReadOptions) -> Result<Dataset, DtaError> {
    let file = File::open(path.as_ref())?;
    let len = file.metadata()?.len();
    let source = if opts.no_mmap || len < MMAP_MIN_BYTES {
        let mut v = Vec::with_capacity(len as usize);
        (&file).read_to_end(&mut v)?;
        bump(&counters().copied_reads, 1);
        Source::Owned(v)
    } else {
        match map_file(&file) {
            Ok(m) => {
                bump(&counters().mapped_reads, 1);
                Source::Mapped(m)
            }
            // A mapping failure is not a read failure: network mounts, /proc,
            // and sandboxes all refuse mappings for files that read fine.
            Err(_) => {
                let mut v = Vec::with_capacity(len as usize);
                (&file).read_to_end(&mut v)?;
                bump(&counters().copied_reads, 1);
                Source::Owned(v)
            }
        }
    };
    let mapped = matches!(source, Source::Mapped(_));
    let mut ds = read_dta_bytes(source.as_slice(), opts)?;
    let mut report = ds.read_report().clone();
    report.mapped = mapped;
    ds.set_report(report);
    // The mapping is dropped here, on purpose: every `Column` owns its bytes by
    // the time this returns, which is what bounds THREAT_MODEL §6.1's residual
    // mmap risk to the duration of this call.
    drop(source);
    Ok(ds)
}

/// Read a `.dta` file already in memory. The fuzzer's entry point.
///
/// # Errors
///
/// [`DtaError`]. Never panics, on any input.
pub fn read_dta_bytes(buf: &[u8], opts: &DtaReadOptions) -> Result<Dataset, DtaError> {
    let ds = parse(buf, opts)?;
    bump(&counters().files_read, 1);
    bump(&counters().source_bytes, buf.len() as u64);
    Ok(ds)
}

/// Read only the header.
///
/// # Errors
///
/// [`DtaError`].
pub fn probe_dta<P: AsRef<Path>>(path: P) -> Result<DtaProbe, DtaError> {
    let mut file = File::open(path.as_ref())?;
    let file_len = file.metadata()?.len();
    // The header is bounded: tags, two 3-byte codes, K, N, and two
    // length-prefixed strings of at most 255 + 65535 bytes. 70 KiB covers every
    // legal header with room to spare, and a short read is simply a short
    // buffer that the cursor rejects with `Truncated`.
    let mut head = vec![0u8; 70 * 1024];
    let got = read_up_to(&mut file, &mut head)?;
    head.truncate(got);
    let h = parse_header(&head, None)?;
    Ok(DtaProbe {
        release: h.release,
        byte_order: h.byte_order,
        n_vars: h.n_vars,
        n_obs: h.n_obs,
        label: h.label,
        timestamp: h.timestamp,
        file_len,
    })
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match file.read(&mut buf[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

enum Source {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Source {
    fn as_slice(&self) -> &[u8] {
        match self {
            Source::Mapped(m) => m,
            Source::Owned(v) => v,
        }
    }
}

/// The one `unsafe` in this crate.
///
/// SAFETY: `memmap2::Mmap::map` is `unsafe` because the mapping's contents are
/// undefined if another process truncates or writes the file while it is
/// mapped. We open the file read-only, hold the mapping for the duration of one
/// `read_dta_with` call, and every `Column` owns its bytes before the mapping is
/// dropped, so nothing derived from it outlives the call. `04` §1.1 accepted
/// this tradeoff to avoid a full heap copy of a multi-gigabyte dataset, and
/// THREAT_MODEL §6.1 records the residual risk (a user truncating the file
/// mid-`use` from another process can still fault) as not defended in v1.
#[allow(unsafe_code)]
fn map_file(file: &File) -> std::io::Result<Mmap> {
    unsafe { Mmap::map(file) }
}

// ---------------------------------------------------------------------------
// Cursor — every read is checked
// ---------------------------------------------------------------------------

struct Cur<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(buf: &'a [u8], pos: usize) -> Self {
        Self { buf, pos }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DtaError> {
        let have = self.buf.len().saturating_sub(self.pos);
        if have < n {
            return Err(DtaError::Truncated {
                at: self.pos as u64,
                need: n as u64,
                have: have as u64,
            });
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn byte(&mut self) -> Result<u8, DtaError> {
        Ok(self.take(1)?[0])
    }

    /// A `width`-byte unsigned integer in the file's byte order.
    fn uint(&mut self, width: usize, bo: ByteOrder) -> Result<u64, DtaError> {
        let s = self.take(width)?;
        let mut b = [0u8; 8];
        match bo {
            ByteOrder::Lsf => b[..width].copy_from_slice(s),
            ByteOrder::Msf => {
                for (i, &v) in s.iter().enumerate() {
                    b[width - 1 - i] = v;
                }
            }
        }
        Ok(u64::from_le_bytes(b))
    }

    fn open(&mut self, tag: &'static str) -> Result<(), DtaError> {
        self.literal(tag, false)
    }

    fn close(&mut self, tag: &'static str) -> Result<(), DtaError> {
        self.literal(tag, true)
    }

    fn literal(&mut self, tag: &'static str, closing: bool) -> Result<(), DtaError> {
        let at = self.pos as u64;
        let want = tag_bytes(tag, closing);
        let got = self.take(want.len())?;
        if got != want.as_slice() {
            self.pos -= want.len();
            return Err(DtaError::Tag { at, expected: tag });
        }
        Ok(())
    }
}

fn tag_bytes(tag: &str, closing: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(tag.len() + 3);
    v.push(b'<');
    if closing {
        v.push(b'/');
    }
    v.extend_from_slice(tag.as_bytes());
    v.push(b'>');
    v
}

const fn open_len(tag: &str) -> usize {
    tag.len() + 2
}
const fn close_len(tag: &str) -> usize {
    tag.len() + 3
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

struct Header {
    release: Release,
    byte_order: ByteOrder,
    n_vars: u32,
    n_obs: u64,
    label: String,
    timestamp: String,
    end: usize,
    decoded: DecodeStat,
    encoding: Encoding,
}

fn parse_header(buf: &[u8], want: Option<Encoding>) -> Result<Header, DtaError> {
    if buf.len() < 11 || &buf[..11] != b"<stata_dta>" {
        return Err(DtaError::NotDta);
    }
    let mut c = Cur::new(buf, 11);
    c.open("header")?;

    c.open("release")?;
    let release = Release::parse(c.take(3)?)?;
    c.close("release")?;
    let spec = release.spec();

    c.open("byteorder")?;
    let byte_order = ByteOrder::parse(c.take(3)?)?;
    c.close("byteorder")?;

    c.open("K")?;
    let n_vars = u32::try_from(c.uint(usize::from(spec.k_width), byte_order)?).map_err(|_| {
        DtaError::TooLarge {
            what: "nvar",
            value: u64::MAX,
        }
    })?;
    c.close("K")?;

    c.open("N")?;
    let n_obs = c.uint(usize::from(spec.n_width), byte_order)?;
    c.close("N")?;

    // The encoding is not known from the file for 117; it is chosen by the
    // caller or assumed. Header text is decoded with the same rule as the rest.
    let encoding = want.unwrap_or(if spec.utf8 {
        Encoding::Utf8
    } else {
        Encoding::Windows1252
    });
    let mut decoded = DecodeStat::default();

    c.open("label")?;
    let label_len = c.uint(usize::from(spec.label_len_width), byte_order)? as usize;
    let (label, st) = decode(c.take(label_len)?, encoding);
    decoded.merge(st);
    c.close("label")?;

    c.open("timestamp")?;
    let ts_len = usize::from(c.byte()?);
    let (timestamp, st) = decode(c.take(ts_len)?, encoding);
    decoded.merge(st);
    c.close("timestamp")?;

    c.close("header")?;

    Ok(Header {
        release,
        byte_order,
        n_vars,
        n_obs,
        label,
        timestamp,
        end: c.pos,
        decoded,
        encoding,
    })
}

// ---------------------------------------------------------------------------
// The map
// ---------------------------------------------------------------------------

fn parse_map(buf: &[u8], at: usize, bo: ByteOrder) -> Result<[u64; MAP_ENTRIES], DtaError> {
    let mut c = Cur::new(buf, at);
    c.open("map")?;
    let raw = c.take(MAP_BYTES)?;
    c.close("map")?;
    let mut map = [0u64; MAP_ENTRIES];
    for (i, slot) in map.iter_mut().enumerate() {
        let mut b = [0u8; 8];
        b.copy_from_slice(&raw[i * 8..i * 8 + 8]);
        *slot = match bo {
            ByteOrder::Lsf => u64::from_le_bytes(b),
            ByteOrder::Msf => u64::from_be_bytes(b),
        };
    }
    Ok(map)
}

/// Validate the map **as a whole**, before any entry is used as an index.
///
/// THREAT_MODEL §2.2. `map[13] <= file_len` is the deliberate inequality: the
/// `auto.dta` StataCorp ships has one trailing `0x0A`, and a reader asserting
/// equality rejects it (`04` §0.2 trap 1).
fn validate_map(map: &[u64; MAP_ENTRIES], file_len: u64) -> Result<(), DtaError> {
    if map[0] != 0 {
        return Err(DtaError::Map {
            index: 0,
            value: map[0],
            why: "must be 0: it is the offset of <stata_dta>, which is the file's first byte",
        });
    }
    for (i, &v) in map.iter().enumerate() {
        if v > file_len {
            return Err(DtaError::Map {
                index: i,
                value: v,
                why: "is past the end of the file",
            });
        }
        if i > 0 && v < map[i - 1] {
            return Err(DtaError::Map {
                index: i,
                value: v,
                why: "is before the entry that precedes it; the map must not decrease",
            });
        }
    }
    Ok(())
}

/// The payload of block `i`, tags excluded.
fn block(map: &[u64; MAP_ENTRIES], i: usize) -> Result<(usize, usize), DtaError> {
    let tag = SECTIONS[i];
    let start = map[i] as usize + open_len(tag);
    let end_of_block = map[i + 1] as usize;
    let end = end_of_block
        .checked_sub(close_len(tag))
        .ok_or(DtaError::BlockLen {
            block: tag,
            payload: 0,
        })?;
    if end < start {
        return Err(DtaError::BlockLen {
            block: tag,
            payload: 0,
        });
    }
    Ok((start, end))
}

// ---------------------------------------------------------------------------
// The parse
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)] // one file format, read top to bottom.
fn parse(buf: &[u8], opts: &DtaReadOptions) -> Result<Dataset, DtaError> {
    let file_len = buf.len() as u64;
    let h = parse_header(buf, opts.encoding)?;
    let release = h.release;
    let spec: &ReleaseSpec = release.spec();
    let bo = h.byte_order;
    let k = h.n_vars;
    let ke = u64::from(k);
    let encoding = h.encoding;

    let mut decoded = h.decoded;
    let mut warnings: Vec<ReadWarning> = Vec::new();

    // --- step 3: the map, validated as a whole ---------------------------
    let map = parse_map(buf, h.end, bo)?;
    validate_map(&map, file_len)?;
    if map[1] as usize != h.end {
        return Err(DtaError::Map {
            index: 1,
            value: map[1],
            why: "does not point at the <map> that follows the header",
        });
    }
    for (i, tag) in SECTIONS.iter().enumerate() {
        let mut c = Cur::new(buf, map[i] as usize);
        c.open(tag)?;
    }
    {
        let mut c = Cur::new(buf, map[MAP_CLOSE] as usize);
        c.close("stata_dta")?;
    }
    let trailing = file_len - map[MAP_EOF];
    if trailing > 0 {
        warnings.push(ReadWarning::TrailingBytes {
            declared_eof: map[MAP_EOF],
            file_len,
        });
    }

    // --- step 4: derive every fixed-width stride and assert the constants -
    let (vt_start, vt_end) = block(&map, 2)?;
    ReleaseSpec::check_block("variable_types", (vt_end - vt_start) as u64, ke, 2)?;
    let (vn_start, vn_end) = block(&map, 3)?;
    ReleaseSpec::check_block("varnames", (vn_end - vn_start) as u64, ke, spec.varname_len)?;
    let (sl_start, sl_end) = block(&map, 4)?;
    ReleaseSpec::check_block(
        "sortlist",
        (sl_end - sl_start) as u64,
        ke + 1,
        spec.sortlist_elem,
    )?;
    let (fm_start, fm_end) = block(&map, 5)?;
    ReleaseSpec::check_block("formats", (fm_end - fm_start) as u64, ke, spec.format_len)?;
    let (vl_start, vl_end) = block(&map, 6)?;
    ReleaseSpec::check_block(
        "value_label_names",
        (vl_end - vl_start) as u64,
        ke,
        spec.vlblname_len,
    )?;
    let (lb_start, lb_end) = block(&map, 7)?;
    ReleaseSpec::check_block(
        "variable_labels",
        (lb_end - lb_start) as u64,
        ke,
        spec.varlabel_len,
    )?;

    // --- step 5: types and the row width ---------------------------------
    let mut types = Vec::with_capacity(k as usize);
    let mut row_width: u64 = 0;
    {
        let mut c = Cur::new(&buf[..vt_end], vt_start);
        for v in 0..k {
            let code = u16::try_from(c.uint(2, bo)?).expect("two bytes fit a u16");
            let ty = spec::storage_type(code, v)?;
            row_width = row_width
                .checked_add(u64::from(stratum_core::types::storage_width(ty)))
                .ok_or(DtaError::RowWidthOverflow)?;
            types.push(ty);
        }
    }

    let (data_start, data_end) = block(&map, MAP_DATA)?;
    let data_payload = (data_end - data_start) as u64;
    // THE guard that makes `<N> = 2^40` cost a comparison and not 8 TB of
    // address space (THREAT_MODEL §2.1 H1). Nothing has been sized by `n_obs`
    // up to this point and nothing will be until it survives this.
    let declared = h.n_obs.checked_mul(row_width).ok_or(DtaError::DataLen {
        declared: u64::MAX,
        actual: data_payload,
    })?;
    if declared != data_payload {
        return Err(DtaError::DataLen {
            declared,
            actual: data_payload,
        });
    }
    let n_obs = h.n_obs;

    // --- step 6: names, formats, value-label names, labels ---------------
    let names = fixed_fields(buf, vn_start, k, spec.varname_len, encoding, &mut decoded);
    let formats = fixed_fields(buf, fm_start, k, spec.format_len, encoding, &mut decoded);
    let vlnames = fixed_fields(buf, vl_start, k, spec.vlblname_len, encoding, &mut decoded);
    let vlabels = fixed_fields(buf, lb_start, k, spec.varlabel_len, encoding, &mut decoded);

    let mut vars: Vec<DtaVar> = Vec::with_capacity(k as usize);
    let mut seen: rustc_hash::FxHashMap<String, u32> = rustc_hash::FxHashMap::default();
    for i in 0..k as usize {
        let name = names[i].clone();
        if !stratum_data::variable::is_valid_name(&name) {
            warnings.push(ReadWarning::InvalidName {
                var: i as u32,
                name: name.clone(),
            });
        }
        if seen.insert(name.clone(), i as u32).is_some() {
            warnings.push(ReadWarning::DuplicateName {
                var: i as u32,
                name: name.clone(),
            });
        }
        vars.push(DtaVar {
            name,
            ty: types[i],
            format: formats[i].clone(),
            label: vlabels[i].clone(),
            value_label: if vlnames[i].is_empty() {
                None
            } else {
                Some(vlnames[i].clone())
            },
        });
    }

    // --- step 7: sortlist -------------------------------------------------
    let mut sortlist = Vec::new();
    {
        let mut c = Cur::new(&buf[..sl_end], sl_start);
        for _ in 0..=ke {
            let e = u32::try_from(c.uint(spec.sortlist_elem, bo)?).unwrap_or(u32::MAX);
            if e == 0 {
                break;
            }
            if e > k {
                warnings.push(ReadWarning::InvalidSortList { entry: e });
                break;
            }
            sortlist.push(e - 1);
        }
    }

    // --- step 8: characteristics -----------------------------------------
    let (ch_start, ch_end) = block(&map, 8)?;
    let chars = parse_chars(buf, ch_start, ch_end, spec, encoding, &mut decoded)?;

    // --- step 9: value labels --------------------------------------------
    let (vlb_start, vlb_end) = block(&map, MAP_VALUE_LABELS)?;
    let value_labels = parse_value_labels(buf, vlb_start, vlb_end, spec, encoding, &mut decoded)?;
    for (i, v) in vars.iter().enumerate() {
        if let Some(t) = &v.value_label {
            if value_labels.get(t).is_none() {
                warnings.push(ReadWarning::MissingValueLabel {
                    var: i as u32,
                    table: t.clone(),
                });
            }
        }
    }

    // --- step 10: the GSO index (ranges into `buf`; nothing copied) -------
    let (gs_start, gs_end) = block(&map, MAP_STRLS)?;
    let gso = GsoTable::scan(&buf[gs_start..gs_end], gs_start, release, k)?;
    bump(&counters().gso_records_read, gso.len() as u64);
    if gso.duplicates > 0 {
        warnings.push(ReadWarning::DuplicateGso {
            count: gso.duplicates,
        });
    }

    // --- step 11: the transpose ------------------------------------------
    let data = &buf[data_start..data_end];
    let rw = usize::try_from(row_width).map_err(|_| DtaError::RowWidthOverflow)?;
    let mut cols: Vec<DtaColumn> = Vec::with_capacity(k as usize);
    let mut offset = 0usize;
    for (i, &ty) in types.iter().enumerate() {
        let col = match ty {
            StorageType::StrL => DtaColumn::StrL(build_strl(
                buf, data, rw, offset, n_obs, release, bo, &gso, i as u32,
            )?),
            _ => {
                let raw = if bo == ByteOrder::Lsf {
                    Column::from_row_major(ty, data, rw, offset, n_obs)
                } else {
                    gather_msf(ty, data, rw, offset, n_obs)
                };
                DtaColumn::Fixed(canonicalise(raw, i as u32, &mut warnings))
            }
        };
        bump(&counters().columns_built, 1);
        offset += usize::from(stratum_core::types::storage_width(ty));
        cols.push(col);
    }

    if decoded.replacements > 0 {
        warnings.push(ReadWarning::UndecodableText {
            encoding: encoding.name().to_owned(),
            replaced: decoded.replacements,
        });
    }

    let mut ds = Dataset::new(release);
    ds.byte_order = bo;
    ds.label = h.label;
    ds.timestamp = h.timestamp;
    ds.vars = vars;
    ds.cols = cols;
    ds.sortlist = sortlist;
    ds.value_labels = value_labels;
    ds.chars = chars;
    ds.set_n_obs(n_obs);
    ds.set_report(ReadReport {
        release: release.number(),
        byte_order: Some(bo),
        encoding,
        file_len,
        declared_eof: map[MAP_EOF],
        trailing_bytes: trailing,
        replacement_chars: decoded.replacements,
        gso_records: gso.len() as u64,
        mapped: false,
        warnings,
    });
    Ok(ds)
}

/// `04` §0.2 trap 2: bytes up to the first NUL, remainder DISCARDED.
fn fixed_fields(
    buf: &[u8],
    start: usize,
    count: u32,
    width: usize,
    encoding: Encoding,
    decoded: &mut DecodeStat,
) -> Vec<String> {
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let lo = start + i * width;
        // The block-width check already proved `start + count*width` is inside
        // the file; `get` rather than `[..]` so that a future change to that
        // proof degrades to an empty name instead of to a panic.
        let field = buf.get(lo..lo + width).unwrap_or(&[]);
        let (s, st) = decode(until_nul(field), encoding);
        decoded.merge(st);
        out.push(s);
    }
    out
}

fn parse_chars(
    buf: &[u8],
    start: usize,
    end: usize,
    spec: &ReleaseSpec,
    encoding: Encoding,
    decoded: &mut DecodeStat,
) -> Result<CharTable, DtaError> {
    let mut table = CharTable::new();
    let mut c = Cur::new(&buf[..end], start);
    let min = (spec.varname_len * 2) as u64;
    while c.pos < end {
        c.open("ch")?;
        let at = c.pos as u64;
        let len = c.uint(4, ByteOrder::Lsf)? as usize;
        if (len as u64) < min {
            return Err(DtaError::CharLen {
                at,
                declared: len as u64,
                min,
            });
        }
        let body = c.take(len)?;
        let (owner, st) = decode(until_nul(&body[..spec.varname_len]), encoding);
        decoded.merge(st);
        let (name, st) = decode(
            until_nul(&body[spec.varname_len..spec.varname_len * 2]),
            encoding,
        );
        decoded.merge(st);
        let (value, st) = decode(until_nul(&body[spec.varname_len * 2..]), encoding);
        decoded.merge(st);
        c.close("ch")?;
        // `CharTable::set` treats an empty value as a delete, which is Stata's
        // own rule (`char x[n] ""` removes it), so an empty record is dropped
        // rather than stored as an empty string.
        table.set(&owner, &name, &value);
    }
    Ok(table)
}

fn parse_value_labels(
    buf: &[u8],
    start: usize,
    end: usize,
    spec: &ReleaseSpec,
    encoding: Encoding,
    decoded: &mut DecodeStat,
) -> Result<ValueLabelSet, DtaError> {
    let mut set = ValueLabelSet::new();
    let mut c = Cur::new(&buf[..end], start);
    while c.pos < end {
        c.open("lbl")?;
        let at = c.pos as u64;
        let declared = c.uint(4, ByteOrder::Lsf)?;
        let (name, st) = decode(until_nul(c.take(spec.vlblname_len)?), encoding);
        decoded.merge(st);
        // 129 (or 33) bytes of name, then 3 bytes of padding, then the table.
        // Verified by offset arithmetic on `auto.dta`: labname at 0x310F, `n`
        // at 0x3193 = 0x310F + 132 (`04` §9.2).
        c.take(3)?;
        let n = c.uint(4, ByteOrder::Lsf)?;
        let txtlen = c.uint(4, ByteOrder::Lsf)?;
        // `len` counts from `n` onward: 4 + 4 + 4n + 4n + txtlen (`04` §9.2,
        // checked against auto.dta's `origin`: 4+4+8+8+17 = 41, and the file
        // says 41). Recomputing it is what bounds `n` — otherwise `n` = 2^31
        // would size two allocations before anything noticed.
        let computed = 8u64
            .checked_add(n.checked_mul(8).ok_or(DtaError::LabelLen {
                at,
                declared,
                computed: u64::MAX,
            })?)
            .and_then(|v| v.checked_add(txtlen))
            .ok_or(DtaError::LabelLen {
                at,
                declared,
                computed: u64::MAX,
            })?;
        if computed != declared {
            return Err(DtaError::LabelLen {
                at,
                declared,
                computed,
            });
        }
        let n_usize = usize::try_from(n).map_err(|_| DtaError::LabelLen {
            at,
            declared,
            computed,
        })?;
        let offs = c.take(n_usize * 4)?;
        let vals = c.take(n_usize * 4)?;
        let txt = c.take(usize::try_from(txtlen).map_err(|_| DtaError::LabelLen {
            at,
            declared,
            computed,
        })?)?;
        c.close("lbl")?;

        let mut pairs = Vec::with_capacity(n_usize);
        for i in 0..n_usize {
            let off = u32::from_le_bytes(offs[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
            let val = i32::from_le_bytes(vals[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
            // The check that would otherwise read adjacent heap
            // (THREAT_MODEL §2.6 L4).
            if u64::from(off) > txtlen {
                return Err(DtaError::LabelOffset {
                    table: name,
                    index: i as u32,
                    offset: off,
                    txtlen: u32::try_from(txtlen).unwrap_or(u32::MAX),
                });
            }
            let rest = &txt[off as usize..];
            let (text, st) = decode(until_nul(rest), encoding);
            decoded.merge(st);
            pairs.push((val, text));
        }
        set.insert(&name, value_label_from(pairs));
    }
    Ok(set)
}

/// Resolve one `strL` column's `(v,o)` cells through the GSO index.
#[allow(clippy::too_many_arguments)]
fn build_strl(
    buf: &[u8],
    data: &[u8],
    row_width: usize,
    col_offset: usize,
    n_obs: u64,
    release: Release,
    bo: ByteOrder,
    gso: &GsoTable,
    var: u32,
) -> Result<StrLData, DtaError> {
    let mut out = StrLData::with_capacity(usize::try_from(n_obs).unwrap_or(0), 0);
    for row in 0..n_obs {
        let o = row as usize * row_width + col_offset;
        let mut b = [0u8; 8];
        b.copy_from_slice(&data[o..o + 8]);
        let raw = match bo {
            ByteOrder::Lsf => u64::from_le_bytes(b),
            ByteOrder::Msf => u64::from_be_bytes(b),
        };
        if raw == EMPTY_KEY {
            out.push(&[], false);
            continue;
        }
        match gso.get(raw) {
            Some(e) => out.push(&buf[e.start..e.end], e.binary),
            None => {
                let (v, ofs) = crate::gso::unpack_vo(raw, release);
                return Err(DtaError::GsoMissing {
                    var,
                    obs: row,
                    v,
                    o: ofs,
                });
            }
        }
    }
    Ok(out)
}

/// The big-endian ingest. Deliberately its own path: `Column::from_row_major`
/// is documented as assuming little-endian, and every target in the release
/// matrix is little-endian, so `MSF` pays one column-sized temporary and the
/// `LSF` path pays nothing at all.
fn gather_msf(
    ty: StorageType,
    data: &[u8],
    row_width: usize,
    col_offset: usize,
    n_obs: u64,
) -> Column {
    bump(&counters().data_block_copies, 1);
    let n = usize::try_from(n_obs).unwrap_or(0);
    macro_rules! swap_gather {
        ($t:ty, $w:expr, $from:path, $variant:path) => {{
            let mut v: Vec<$t> = Vec::with_capacity(n);
            for row in 0..n {
                let o = row * row_width + col_offset;
                let mut b = [0u8; $w];
                b.copy_from_slice(&data[o..o + $w]);
                v.push($from(b));
            }
            $variant(NumCol::from_slice(&v))
        }};
    }
    match ty {
        StorageType::Byte => swap_gather!(i8, 1, i8::from_be_bytes, Column::Byte),
        StorageType::Int => swap_gather!(i16, 2, i16::from_be_bytes, Column::Int),
        StorageType::Long => swap_gather!(i32, 4, i32::from_be_bytes, Column::Long),
        StorageType::Float => swap_gather!(f32, 4, f32::from_be_bytes, Column::Float),
        StorageType::Double => swap_gather!(f64, 8, f64::from_be_bytes, Column::Double),
        // Strings are bytes; there is nothing to swap.
        _ => Column::from_row_major(ty, data, row_width, col_offset, n_obs),
    }
}

// ---------------------------------------------------------------------------
// Invariant M on load
// ---------------------------------------------------------------------------

/// Is this `f64` a value Stata could have produced? (`04` §2.5, Invariant M.)
///
/// Ordinary numbers are `-SYSMISS < v < SYSMISS`. The only other legal values
/// are the **27 exact sentinels** `.`, `.a`..`.z`. Everything else — a real NaN,
/// an infinity, a bit pattern between two sentinels — is a value no Stata
/// version can store, and reading it verbatim would break the branchless
/// `is_missing` the whole engine depends on.
///
/// Note this is emphatically **not** `stratum_core::canon`, which collapses
/// `.a` to `.`. Applying `canon` on load would destroy every extended missing
/// value in the file.
#[inline]
fn conforms_f64(v: f64) -> bool {
    if v > -SYSMISS && v < SYSMISS {
        return true;
    }
    let b = v.to_bits();
    b >= F64_MISS_BITS && {
        let d = b - F64_MISS_BITS;
        d.is_multiple_of(F64_MISS_STEP) && d / F64_MISS_STEP <= u64::from(MAX_TAG)
    }
}

#[inline]
fn conforms_f32(v: f32) -> bool {
    if v > -SYSMISS_F32 && v < SYSMISS_F32 {
        return true;
    }
    let b = v.to_bits();
    b >= F32_MISS_BITS && {
        let d = b - F32_MISS_BITS;
        d.is_multiple_of(F32_MISS_STEP) && d / F32_MISS_STEP <= u32::from(MAX_TAG)
    }
}

/// Enforce Invariant M on a freshly built float or double column.
///
/// **Cost, and why it is this shape.** The check is one sequential pass over the
/// *compact* column, which for a 40-variable dataset touches 8 bytes per row
/// against the 320 the strided gather already touched — a few percent, and it
/// vectorises. The rebuild allocates a second copy of the column and happens
/// **only** when the file actually contains a non-Stata value, which no file
/// Stata wrote ever does. `counters().canon_rows_fixed` is zero for every
/// fixture in the tree, and `tests/hostile.rs` asserts it is not zero for a file
/// carrying a NaN.
fn canonicalise(col: Column, var: u32, warnings: &mut Vec<ReadWarning>) -> Column {
    macro_rules! pass {
        ($c:expr, $conforms:path, $miss:expr, $variant:path) => {{
            let c = $c;
            bump(&counters().canon_rows_scanned, c.len());
            let mut bad = 0u64;
            for i in 0..c.n_chunks() {
                bad += c.chunk(i).iter().filter(|v| !$conforms(**v)).count() as u64;
            }
            if bad == 0 {
                return $variant(c);
            }
            let mut flat = Vec::with_capacity(c.len() as usize);
            for i in 0..c.n_chunks() {
                flat.extend(
                    c.chunk(i)
                        .iter()
                        .map(|&v| if $conforms(v) { v } else { $miss }),
                );
            }
            bump(&counters().canon_rows_fixed, bad);
            bump(&counters().canon_columns_rebuilt, 1);
            warnings.push(ReadWarning::NonCanonicalNumeric { var, rows: bad });
            $variant(NumCol::from_slice(&flat))
        }};
    }
    match col {
        Column::Float(c) => pass!(c, conforms_f32, SYSMISS_F32, Column::Float),
        Column::Double(c) => pass!(c, conforms_f64, SYSMISS, Column::Double),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_core::missing::{missing_f32, missing_f64};

    #[test]
    fn invariant_m_accepts_every_sentinel_and_rejects_everything_else() {
        for tag in 0..=MAX_TAG {
            assert!(conforms_f64(missing_f64(tag)), "double .{tag}");
            assert!(conforms_f32(missing_f32(tag)), "float .{tag}");
        }
        for v in [0.0f64, -1.5, 1e300, -1e300, -0.0, f64::MIN_POSITIVE] {
            assert!(conforms_f64(v), "{v}");
        }
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!conforms_f64(v), "{v}");
        }
        // Invariant M (`04` §2.5) is "no values below `-SYSMISS`", and `SYSMISS`
        // is 2^1023: Stata's `maxdouble` is the largest double strictly below
        // it. `f64::MAX` is therefore NOT a value Stata can store, and a file
        // carrying one is a file that has been corrupted or written by
        // something that is not Stata.
        assert!(!conforms_f64(f64::MAX));
        assert!(!conforms_f64(f64::MIN));
        assert!(
            conforms_f64(f64::from_bits(SYSMISS.to_bits() - 1)),
            "maxdouble"
        );
        assert!(
            conforms_f64(-f64::from_bits(SYSMISS.to_bits() - 1)),
            "mindouble"
        );
        // A bit pattern strictly between `.` and `.a` is not a value Stata can
        // store, and this is exactly what a corrupt file produces.
        assert!(!conforms_f64(f64::from_bits(F64_MISS_BITS + 1)));
        assert!(!conforms_f64(f64::from_bits(
            F64_MISS_BITS + F64_MISS_STEP * 27
        )));
        assert!(!conforms_f32(f32::NAN));
        assert!(!conforms_f32(f32::from_bits(F32_MISS_BITS + 1)));
    }

    #[test]
    fn canonicalise_is_a_no_op_on_conforming_data() {
        let mut w = Vec::new();
        let c = Column::Double(NumCol::from_slice(&[1.0, missing_f64(3), -7.5]));
        let out = canonicalise(c.clone(), 0, &mut w);
        assert!(w.is_empty());
        assert_eq!(
            out.get_f64(1).map(f64::to_bits),
            Some(missing_f64(3).to_bits())
        );
    }

    #[test]
    fn canonicalise_rewrites_a_nan_to_plain_dot_and_says_so() {
        let mut w = Vec::new();
        let c = Column::Double(NumCol::from_slice(&[1.0, f64::NAN, missing_f64(1)]));
        let out = canonicalise(c, 4, &mut w);
        assert_eq!(out.get_f64(1).map(f64::to_bits), Some(SYSMISS.to_bits()));
        // The tagged missing survives: `canon` would have collapsed it.
        assert_eq!(
            out.get_f64(2).map(f64::to_bits),
            Some(missing_f64(1).to_bits())
        );
        assert!(matches!(
            w.as_slice(),
            [ReadWarning::NonCanonicalNumeric { var: 4, rows: 1 }]
        ));
    }

    #[test]
    fn the_cursor_never_panics_on_a_short_buffer() {
        let mut c = Cur::new(b"<sta", 0);
        assert!(matches!(c.take(99), Err(DtaError::Truncated { .. })));
        assert!(matches!(
            c.open("stata_dta"),
            Err(DtaError::Truncated { .. })
        ));
        let mut c = Cur::new(b"<xxxxxxxxx>", 0);
        assert!(matches!(c.open("stata_dta"), Err(DtaError::Tag { .. })));
    }

    #[test]
    fn map_validation_rejects_the_named_hostile_shapes() {
        let mut m = [0u64; MAP_ENTRIES];
        for (i, slot) in m.iter_mut().enumerate() {
            *slot = i as u64 * 10;
        }
        validate_map(&m, 1000).unwrap();
        // Past EOF.
        assert!(matches!(
            validate_map(&m, 100),
            Err(DtaError::Map { index: 11, .. })
        ));
        // Decreasing.
        let mut bad = m;
        bad[5] = 1;
        assert!(matches!(
            validate_map(&bad, 1000),
            Err(DtaError::Map { index: 5, .. })
        ));
        // map[0] must be 0.
        let mut bad = m;
        bad[0] = 4;
        assert!(matches!(
            validate_map(&bad, 1000),
            Err(DtaError::Map { index: 0, .. })
        ));
        // `map[13] < file_len` is ACCEPTED: StataCorp's own auto.dta.
        validate_map(&m, m[MAP_EOF] + 1).unwrap();
    }
}
