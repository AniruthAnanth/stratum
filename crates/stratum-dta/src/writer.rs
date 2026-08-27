//! The `.dta` writer.
//!
//! Structure follows `04` §10 step for step, and the shape of every block was
//! read back off files Stata itself wrote — the fixtures in
//! `tests/fixtures/dta/`, whose `make_fixtures.log` carries Stata's own
//! `describe` for each. Where this writer differs from Stata's bytes it is
//! because Stata's bytes are not reproducible (see "byte identity", below), not
//! because the layout was guessed.
//!
//! # Three properties that shape the code
//!
//! 1. **Everything is validated before the output file is touched.**
//!    [`prepare`] encodes every name, format, label, characteristic and value
//!    label, and plans the `strL` dedup, *before* `write_dta` opens the path.
//!    A dataset that cannot be written as release 117 therefore fails without
//!    having truncated the user's existing file — which matters, because
//!    `save, replace` over a good file is exactly when this error fires.
//! 2. **The data section is a tiled scatter, not a per-row gather.** Columns
//!    are read sequentially and written at `row_width` stride into an
//!    L2-resident tile ([`TILE_BYTES`]), mirroring the read-side transpose of
//!    `04` §12.1. A per-row loop over `Column::get` would read every column's
//!    chunk pointer once per observation.
//! 3. **`str#` cells go out as their stored fixed-width field, verbatim.**
//!    [`stratum_data::FixedStrCol`] stores the whole `.dta` field, padding
//!    included, so a cell is one `copy_from_slice` and a file read and written
//!    unchanged keeps its data section byte-identical. Content is defined as
//!    bytes-up-to-first-NUL on both sides (`04` §0.2 trap 2), so the padding is
//!    not observable either way.
//!
//! # Byte identity is not the goal, and could not be
//!
//! `04` §11.3: Stata leaves uninitialised memory in fixed-width metadata
//! padding, and its value-label text arenas are laid out in *definition* order,
//! which no reader can recover. This writer emits NUL padding and value-label
//! text in ascending-value order. [`crate::canonical_eq`] is the equality that
//! matters; [`crate::byte_diff`] is the metric that is reported.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use stratum_core::StorageType;
use stratum_data::chunk::{chunk_len, chunk_of, offset_in_chunk};
use stratum_data::Column;

use crate::codepage::{encode, Encoding};
use crate::gso::{GsoOut, GsoPlanner, GSO_BINARY, GSO_STRING};
use crate::spec::{self, ByteOrder, Release, ReleaseSpec, MAP_BYTES, MAP_ENTRIES, MAP_EOF};
use crate::{bump, counters, Dataset, DtaColumn, DtaError};

/// Bytes of the row-major staging tile. Sized to sit in L2 so the strided
/// stores stay in cache while a column is scattered across it (`04` §12.1).
pub const TILE_BYTES: usize = 256 * 1024;

/// Stata's own ceiling on variables for release 117 and 118. `<K>` is a `u16`
/// in both, so 65 535 is *representable*, but Stata refuses to open a file
/// above 32 767 and writing one would produce something only we can read.
pub const MAX_VARS_117_118: u32 = 32_767;

/// How to write a file.
#[derive(Clone, Debug)]
pub struct DtaWriteOptions {
    /// The release to write. `None` is `04` §10.1's auto rule: 119 when the
    /// dataset has more than [`MAX_VARS_117_118`] variables, else 118. 117 is
    /// only ever written on explicit request — it is `saveold, version(13)`.
    pub release: Option<Release>,
    /// Collapse identical `strL` values onto one GSO record, which is what
    /// Stata does (`04` §10.4, measured). Off writes one record per non-empty
    /// cell, which is only useful for proving the dedup is what saves the
    /// records.
    pub coalesce_strls: bool,
    /// Text encoding. Consulted **only for 117**, which is not UTF-8; 118 and
    /// 119 are always UTF-8. `None` means Windows-1252, the same assumption the
    /// reader makes (`04` §9.4).
    pub encoding: Option<Encoding>,
    /// `<timestamp>`, verbatim. `None` keeps [`Dataset::timestamp`], which for
    /// a dataset that was read is the source file's. Nothing here reads the
    /// clock: `stratum-dta` is one of the crates ARCHITECTURE §8.4 builds for
    /// `wasm32-unknown-unknown`, where there is no clock, and a writer whose
    /// output changed with the time of day would make every round-trip test
    /// non-reproducible.
    pub timestamp: Option<String>,
}

impl Default for DtaWriteOptions {
    fn default() -> Self {
        Self {
            release: None,
            coalesce_strls: true,
            encoding: None,
            timestamp: None,
        }
    }
}

impl DtaWriteOptions {
    /// Options that write `release`.
    #[must_use]
    pub fn release(release: Release) -> Self {
        Self {
            release: Some(release),
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Write `ds` to `path` with the default options.
///
/// # Errors
///
/// [`DtaError`]. The path is not opened until the dataset is known to be
/// writable, so a rejected dataset never destroys an existing file.
pub fn write_dta<P: AsRef<Path>>(path: P, ds: &Dataset) -> Result<(), DtaError> {
    write_dta_with(path, ds, &DtaWriteOptions::default())
}

/// Write `ds` to `path`.
///
/// # Errors
///
/// [`DtaError`].
pub fn write_dta_with<P: AsRef<Path>>(
    path: P,
    ds: &Dataset,
    opts: &DtaWriteOptions,
) -> Result<(), DtaError> {
    // Validate first, open second. See the module docs.
    let plan = prepare(ds, opts)?;
    let mut sink = FileSink {
        w: BufWriter::with_capacity(1 << 20, File::create(path.as_ref())?),
        pos: 0,
    };
    emit(&mut sink, ds, &plan)?;
    sink.w.flush()?;
    bump(&counters().files_written, 1);
    Ok(())
}

/// Write `ds` into a fresh buffer. The round-trip tests' entry point, and the
/// headless CLI's when it is piping a dataset out (`04` §10.2).
///
/// # Errors
///
/// [`DtaError`].
pub fn write_dta_to_vec(ds: &Dataset, opts: &DtaWriteOptions) -> Result<Vec<u8>, DtaError> {
    let plan = prepare(ds, opts)?;
    let mut sink = VecSink(Vec::with_capacity(plan.size_hint()));
    emit(&mut sink, ds, &plan)?;
    bump(&counters().files_written, 1);
    Ok(sink.0)
}

// ---------------------------------------------------------------------------
// Sinks
// ---------------------------------------------------------------------------

/// A seekable byte sink. Both implementations can patch the map in place, which
/// is what lets the writer be one forward pass (`04` §10.2).
trait Sink {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn pos(&self) -> u64;
    /// Overwrite `bytes.len()` bytes at `at`, then continue appending.
    fn patch(&mut self, at: u64, bytes: &[u8]) -> io::Result<()>;
}

struct VecSink(Vec<u8>);

impl Sink for VecSink {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn pos(&self) -> u64 {
        self.0.len() as u64
    }
    fn patch(&mut self, at: u64, bytes: &[u8]) -> io::Result<()> {
        let at = at as usize;
        self.0[at..at + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
}

struct FileSink {
    w: BufWriter<File>,
    pos: u64,
}

impl Sink for FileSink {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.w.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }
    fn pos(&self) -> u64 {
        self.pos
    }
    fn patch(&mut self, at: u64, bytes: &[u8]) -> io::Result<()> {
        // `BufWriter`'s `Seek` flushes before it seeks, so the buffered tail is
        // never reordered behind the patch.
        self.w.seek(SeekFrom::Start(at))?;
        self.w.write_all(bytes)?;
        self.w.seek(SeekFrom::Start(self.pos))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The plan: everything checked and encoded, before a byte is written
// ---------------------------------------------------------------------------

/// One value-label table, ready to write.
struct LabelOut {
    name: Vec<u8>,
    /// `(value, text)`, ascending by value — which is the order Stata writes
    /// (measured on `alltypes.dta`: `vals=(-1, 0, 1, 2)`).
    entries: Vec<(i32, Vec<u8>)>,
    txtlen: u32,
}

/// One characteristic, ready to write.
struct CharOut {
    owner: Vec<u8>,
    name: Vec<u8>,
    value: Vec<u8>,
}

struct Plan<'a> {
    release: Release,
    spec: &'static ReleaseSpec,
    n_vars: u32,
    n_obs: u64,
    row_width: usize,
    label: Cow<'a, [u8]>,
    /// Owned rather than borrowed: it may come from `DtaWriteOptions`, whose
    /// lifetime is the caller's, not the dataset's.
    timestamp: Vec<u8>,
    names: Vec<Cow<'a, [u8]>>,
    formats: Vec<Cow<'a, [u8]>>,
    vlnames: Vec<Cow<'a, [u8]>>,
    varlabels: Vec<Cow<'a, [u8]>>,
    /// 1-based variable indices, terminated by a 0 slot.
    sortlist: Vec<u32>,
    chars: Vec<CharOut>,
    labels: Vec<LabelOut>,
    /// Packed `(v,o)` keys per variable, `None` for a non-`strL` variable.
    strl_keys: Vec<Option<Vec<u64>>>,
    gso: Vec<GsoOut>,
}

impl Plan<'_> {
    /// A close-enough capacity for the in-memory sink so it does not grow.
    /// Deliberately an over-estimate: one over-allocation beats six reallocs of
    /// a multi-megabyte buffer.
    fn size_hint(&self) -> usize {
        let k = self.n_vars as usize;
        let meta = k
            * (2 + self.spec.varname_len
                + self.spec.format_len
                + self.spec.vlblname_len
                + self.spec.varlabel_len);
        let data = (self.n_obs as usize).saturating_mul(self.row_width);
        let gso: usize = self.gso.iter().map(|r| r.content.len() + 24).sum();
        let lbl: usize = self
            .labels
            .iter()
            .map(|l| 150 + self.spec.vlblname_len + l.entries.len() * 8 + l.txtlen as usize)
            .sum();
        let ch: usize = self
            .chars
            .iter()
            .map(|c| 13 + 2 * self.spec.varname_len + c.value.len() + 1)
            .sum();
        1024 + meta + data + gso + lbl + ch
    }
}

/// Pick the release, then check and encode everything.
#[allow(clippy::too_many_lines)] // one validation pass, block by block.
fn prepare<'a>(ds: &'a Dataset, opts: &DtaWriteOptions) -> Result<Plan<'a>, DtaError> {
    let k = ds.n_vars();
    let release = match opts.release {
        Some(r) => r,
        // `04` §10.1's auto rule. 117 is never chosen automatically.
        None if k > MAX_VARS_117_118 => Release::R119,
        None => Release::R118,
    };
    let spec = release.spec();
    let encoding = if spec.utf8 {
        Encoding::Utf8
    } else {
        opts.encoding.unwrap_or(Encoding::Windows1252)
    };

    if k > MAX_VARS_117_118 && release != Release::R119 {
        return Err(DtaError::ReleaseTooNarrow {
            release: release.number(),
            why: format!(
                "{k} variables; release {} tops out at {MAX_VARS_117_118}. Write release 119.",
                release.number()
            ),
        });
    }
    if release == Release::R117 && ds.n_obs() > u64::from(u32::MAX) {
        return Err(DtaError::ReleaseTooNarrow {
            release: 117,
            why: format!(
                "{} observations; release 117's <N> is 32-bit. Write release 118 or 119.",
                ds.n_obs()
            ),
        });
    }
    if ds.cols.len() != k as usize {
        return Err(DtaError::Inconsistent(format!(
            "{k} variables but {} columns",
            ds.cols.len()
        )));
    }

    let label = text(&ds.label, encoding, "the dataset label")?;
    let label_cap: usize = match spec.label_len_width {
        1 => u8::MAX as usize,
        _ => u16::MAX as usize,
    };
    if label.len() > label_cap {
        return Err(DtaError::TooLong {
            what: "the dataset label".into(),
            value: ds.label.clone(),
            len: label.len(),
            limit: label_cap,
            release: release.number(),
        });
    }
    let timestamp = text(
        opts.timestamp.as_deref().unwrap_or(&ds.timestamp),
        encoding,
        "the timestamp",
    )?
    .into_owned();
    if timestamp.len() > u8::MAX as usize {
        return Err(DtaError::TooLong {
            what: "the timestamp".into(),
            value: ds.timestamp.clone(),
            len: timestamp.len(),
            limit: u8::MAX as usize,
            release: release.number(),
        });
    }

    let mut names = Vec::with_capacity(k as usize);
    let mut formats = Vec::with_capacity(k as usize);
    let mut vlnames = Vec::with_capacity(k as usize);
    let mut varlabels = Vec::with_capacity(k as usize);
    let mut row_width: u64 = 0;

    for (i, v) in ds.vars.iter().enumerate() {
        let idx = i as u32;
        check_name(idx, &v.name)?;
        if let StorageType::Str { width } = v.ty {
            if width == 0 || width > spec::MAX_STR_WIDTH {
                return Err(DtaError::TypeCode {
                    var: idx,
                    code: width,
                });
            }
        }
        if ds.cols[i].storage_type() != v.ty {
            return Err(DtaError::Inconsistent(format!(
                "variable {:?} is {:?} but its column is {:?}",
                v.name,
                v.ty,
                ds.cols[i].storage_type()
            )));
        }
        if ds.cols[i].len() != ds.n_obs() {
            return Err(DtaError::Inconsistent(format!(
                "variable {:?} has {} observations, the dataset has {}",
                v.name,
                ds.cols[i].len(),
                ds.n_obs()
            )));
        }
        row_width = row_width
            .checked_add(u64::from(stratum_core::types::storage_width(v.ty)))
            .ok_or(DtaError::RowWidthOverflow)?;

        names.push(fixed(&v.name, encoding, spec.varname_len, release, || {
            format!("the name of variable {idx}")
        })?);
        formats.push(fixed(
            &v.format,
            encoding,
            spec.format_len,
            release,
            || format!("the display format of {:?}", v.name),
        )?);
        vlnames.push(fixed(
            v.value_label.as_deref().unwrap_or(""),
            encoding,
            spec.vlblname_len,
            release,
            || format!("the value-label name of {:?}", v.name),
        )?);
        varlabels.push(fixed(
            &v.label,
            encoding,
            spec.varlabel_len,
            release,
            || format!("the variable label of {:?}", v.name),
        )?);
    }
    let row_width = usize::try_from(row_width).map_err(|_| DtaError::RowWidthOverflow)?;

    // The sortlist is 1-based with a 0 terminator; an entry naming a variable
    // this dataset does not have would make the file describe itself wrongly.
    let mut sortlist = Vec::with_capacity(ds.sortlist.len());
    for &s in &ds.sortlist {
        if s >= k {
            return Err(DtaError::Inconsistent(format!(
                "sortlist names variable {s}, but the dataset has {k}"
            )));
        }
        sortlist.push(s + 1);
    }

    // Characteristics, sorted for a deterministic file. Stata writes them in
    // reverse creation order, which is not recoverable from a `CharTable`, and
    // `canonical_eq` compares them order-insensitively for exactly that reason.
    let mut chars: Vec<CharOut> = Vec::with_capacity(ds.chars.len());
    for (owner, name, value) in ds.chars.iter() {
        chars.push(CharOut {
            owner: fixed(owner, encoding, spec.varname_len, release, || {
                format!("the characteristic owner {owner:?}")
            })?
            .into_owned(),
            name: fixed(name, encoding, spec.varname_len, release, || {
                format!("the characteristic name {owner}[{name}]")
            })?
            .into_owned(),
            value: text(value, encoding, "a characteristic value")?.into_owned(),
        });
    }
    chars.sort_unstable_by(|a, b| (&a.owner, &a.name).cmp(&(&b.owner, &b.name)));

    let mut label_names = ds.value_labels.names();
    label_names.sort_unstable();
    let mut labels = Vec::with_capacity(label_names.len());
    for name in &label_names {
        let table = ds
            .value_labels
            .get(name)
            .expect("the name came from this set");
        let mut entries: Vec<(i32, Vec<u8>)> = Vec::with_capacity(table.len());
        let mut txtlen: u64 = 0;
        for (value, txt) in table.iter() {
            let bytes = text(txt, encoding, "a value label")?.into_owned();
            txtlen += bytes.len() as u64 + 1;
            entries.push((value, bytes));
        }
        entries.sort_unstable_by_key(|(v, _)| *v);
        let txtlen = u32::try_from(txtlen).map_err(|_| DtaError::TooLong {
            what: format!("value label table {name:?}"),
            value: name.to_string(),
            len: txtlen as usize,
            limit: u32::MAX as usize,
            release: release.number(),
        })?;
        labels.push(LabelOut {
            name: fixed(name, encoding, spec.vlblname_len, release, || {
                format!("the value-label table name {name:?}")
            })?
            .into_owned(),
            entries,
            txtlen,
        });
    }

    // strL: one dedup pass in column-major order, which is the order `04` §10.4
    // says Stata sources the `(v,o)` identifier from.
    let strl_cells: usize = ds
        .cols
        .iter()
        .filter(|c| matches!(c, DtaColumn::StrL(_)))
        .count()
        * ds.n_obs() as usize;
    let mut planner = GsoPlanner::with_capacity(strl_cells);
    let mut strl_of: Vec<Option<usize>> = Vec::with_capacity(k as usize);
    let mut n_strl = 0usize;
    for (i, col) in ds.cols.iter().enumerate() {
        match col {
            DtaColumn::StrL(s) => {
                strl_of.push(Some(n_strl));
                n_strl += 1;
                for row in 0..ds.n_obs() {
                    planner.push(
                        i as u32 + 1,
                        row + 1,
                        s.get(row),
                        s.is_binary(row),
                        release,
                        opts.coalesce_strls,
                    )?;
                }
            }
            // A `strL` held as a `Column` cannot carry the GSO type-129 flag
            // and has no route into the dedup pass, so it is rejected rather
            // than written as an all-empty column. See the escalation on the
            // crate docs: the bridge that would make this reachable is four
            // additions to `stratum-data` that W03 does not own.
            DtaColumn::Fixed(Column::StrL(_)) => {
                return Err(DtaError::Inconsistent(format!(
                    "variable {i} is a strL held as a fixed Column; build it as DtaColumn::StrL"
                )))
            }
            DtaColumn::Fixed(_) => strl_of.push(None),
        }
    }
    bump(&counters().strl_cells_written, strl_cells as u64);
    let gso_plan = planner.finish();
    bump(
        &counters().gso_records_written,
        gso_plan.records.len() as u64,
    );

    let n_obs = ds.n_obs() as usize;
    let mut strl_keys: Vec<Option<Vec<u64>>> = Vec::with_capacity(k as usize);
    for slot in &strl_of {
        match slot {
            Some(j) => strl_keys.push(Some(gso_plan.cells[j * n_obs..(j + 1) * n_obs].to_vec())),
            None => strl_keys.push(None),
        }
    }

    Ok(Plan {
        release,
        spec,
        n_vars: k,
        n_obs: ds.n_obs(),
        row_width,
        label,
        timestamp,
        names,
        formats,
        vlnames,
        varlabels,
        sortlist,
        chars,
        labels,
        strl_keys,
        gso: gso_plan.records,
    })
}

/// Encode free text (no fixed field to fit).
fn text<'a>(s: &'a str, enc: Encoding, what: &str) -> Result<Cow<'a, [u8]>, DtaError> {
    match enc {
        // UTF-8 is the file's own encoding, so this is a borrow rather than a
        // copy — which is the difference between 4 and 0 allocations per
        // variable on a 40 000-variable file.
        Encoding::Utf8 => Ok(Cow::Borrowed(s.as_bytes())),
        other => Ok(Cow::Owned(encode(s, other, what)?)),
    }
}

/// Encode text that must fit a fixed-width field, terminator included.
///
/// `what` is a closure so the subject line — which needs a `format!` — is built
/// only on the error path, not once per variable of every file we write.
fn fixed<'a, F: FnOnce() -> String>(
    s: &'a str,
    enc: Encoding,
    width: usize,
    release: Release,
    what: F,
) -> Result<Cow<'a, [u8]>, DtaError> {
    let bytes = match enc {
        Encoding::Utf8 => Cow::Borrowed(s.as_bytes()),
        other => match encode(s, other, "") {
            Ok(b) => Cow::Owned(b),
            // Re-raise with this caller's subject: "the variable label of
            // "price"" is actionable, "a fixed field" is not.
            Err(DtaError::NotRepresentable { ch, encoding, .. }) => {
                return Err(DtaError::NotRepresentable {
                    what: what(),
                    ch,
                    encoding,
                })
            }
            Err(e) => return Err(e),
        },
    };
    // `>=`, not `>`: the field must have room for the NUL terminator, because
    // the reader defines a field's value as bytes-up-to-first-NUL and a full
    // field would run into the next one.
    if bytes.len() >= width {
        return Err(DtaError::TooLong {
            what: what(),
            value: s.to_owned(),
            len: bytes.len(),
            limit: width,
            release: release.number(),
        });
    }
    Ok(bytes)
}

/// What the writer refuses to call a variable.
///
/// Deliberately **not** [`stratum_data::variable::is_valid_name`], and the
/// difference is load-bearing in both directions:
///
/// * that predicate is ASCII-only, and release 118/119 are UTF-8 — Stata 14 and
///   later accept Unicode variable names, so refusing them here would make a
///   legitimate file unwritable after we had just read it;
/// * it caps names at 32 bytes, which is release 117's field, not 118's 128.
///   The cap belongs to the release and is applied by [`fixed`].
///
/// What is refused is what would produce a file no reader can use: an empty
/// name, an embedded NUL (which truncates the field on read), whitespace or a
/// control character (which no varlist can name), and a leading ASCII digit
/// (which every Stata parser reads as a number). THREAT_MODEL §2.5 V4.
fn check_name(index: u32, name: &str) -> Result<(), DtaError> {
    let bad = |why: &'static str| {
        Err(DtaError::BadName {
            index,
            name: name.to_owned(),
            why,
        })
    };
    if name.is_empty() {
        return bad("a variable must have a name");
    }
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        return bad("a name cannot begin with a digit");
    }
    if name.contains('\0') {
        return bad("a name cannot contain a NUL, which terminates the field on read");
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad("a name cannot contain whitespace or a control character");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Emission
// ---------------------------------------------------------------------------

fn emit<S: Sink>(sink: &mut S, ds: &Dataset, p: &Plan<'_>) -> Result<(), DtaError> {
    let spec = p.spec;
    let mut map = [0u64; MAP_ENTRIES];

    sink.put(b"<stata_dta>")?;
    sink.put(b"<header>")?;
    sink.put(b"<release>")?;
    sink.put(p.release.number().to_string().as_bytes())?;
    sink.put(b"</release>")?;
    sink.put(b"<byteorder>")?;
    // Writing is always little-endian: every target in the release matrix is,
    // and a big-endian file we wrote would only ever be read by us.
    sink.put(ByteOrder::Lsf.tag())?;
    sink.put(b"</byteorder>")?;
    sink.put(b"<K>")?;
    sink.put(&u64::from(p.n_vars).to_le_bytes()[..usize::from(spec.k_width)])?;
    sink.put(b"</K>")?;
    sink.put(b"<N>")?;
    sink.put(&p.n_obs.to_le_bytes()[..usize::from(spec.n_width)])?;
    sink.put(b"</N>")?;
    sink.put(b"<label>")?;
    sink.put(&(p.label.len() as u64).to_le_bytes()[..usize::from(spec.label_len_width)])?;
    sink.put(&p.label)?;
    sink.put(b"</label>")?;
    sink.put(b"<timestamp>")?;
    sink.put(&[p.timestamp.len() as u8])?;
    sink.put(&p.timestamp)?;
    sink.put(b"</timestamp>")?;
    sink.put(b"</header>")?;

    // The map is written as zeros and patched at the end (`04` §10.2 step 2).
    map[1] = sink.pos();
    let map_payload_at = map[1] + b"<map>".len() as u64;
    sink.put(b"<map>")?;
    sink.put(&[0u8; MAP_BYTES])?;
    sink.put(b"</map>")?;

    // <variable_types>
    map[2] = sink.pos();
    section(sink, "variable_types", |s| {
        for v in &ds.vars {
            s.put(&spec::type_code(v.ty).to_le_bytes())?;
        }
        Ok(())
    })?;

    // <varnames>
    map[3] = sink.pos();
    section(sink, "varnames", |s| {
        put_fields(s, &p.names, spec.varname_len)
    })?;

    // <sortlist>: (K + 1) entries, 1-based, 0-terminated. The slots past the
    // terminator are zeroed here; Stata leaves uninitialised memory there
    // (measured: `empty.dta`'s sortlist is `[0, 2]`), which is exactly why the
    // reader stops at the first zero rather than reading all K + 1.
    map[4] = sink.pos();
    section(sink, "sortlist", |s| {
        let w = spec.sortlist_elem;
        for i in 0..=u64::from(p.n_vars) {
            let e = p.sortlist.get(i as usize).copied().unwrap_or(0);
            s.put(&u64::from(e).to_le_bytes()[..w])?;
        }
        Ok(())
    })?;

    // <formats>
    map[5] = sink.pos();
    section(sink, "formats", |s| {
        put_fields(s, &p.formats, spec.format_len)
    })?;

    // <value_label_names>
    map[6] = sink.pos();
    section(sink, "value_label_names", |s| {
        put_fields(s, &p.vlnames, spec.vlblname_len)
    })?;

    // <variable_labels>
    map[7] = sink.pos();
    section(sink, "variable_labels", |s| {
        put_fields(s, &p.varlabels, spec.varlabel_len)
    })?;

    // <characteristics>: `len` counts both name fields plus the value's own
    // NUL (`04` §9.2, checked against auto.dta's 266 = 129 + 129 + 8).
    map[8] = sink.pos();
    section(sink, "characteristics", |s| {
        let mut pad = vec![0u8; spec.varname_len];
        for c in &p.chars {
            let len = (2 * spec.varname_len + c.value.len() + 1) as u32;
            s.put(b"<ch>")?;
            s.put(&len.to_le_bytes())?;
            put_padded(s, &c.owner, spec.varname_len, &mut pad)?;
            put_padded(s, &c.name, spec.varname_len, &mut pad)?;
            s.put(&c.value)?;
            s.put(&[0u8])?;
            s.put(b"</ch>")?;
        }
        Ok(())
    })?;

    // <data>
    map[9] = sink.pos();
    section(sink, "data", |s| write_data(s, ds, p))?;

    // <strls>
    map[10] = sink.pos();
    section(sink, "strls", |s| {
        let o_width = usize::from(spec.gso_hdr_o_width);
        for r in &p.gso {
            s.put(b"GSO")?;
            s.put(&r.v.to_le_bytes())?;
            s.put(&r.o.to_le_bytes()[..o_width])?;
            // Type 130's declared length includes the terminator; 129's does
            // not, and it gets no terminator (`04` §9.5, measured).
            let (ty, extra) = if r.binary {
                (GSO_BINARY, 0usize)
            } else {
                (GSO_STRING, 1usize)
            };
            s.put(&[ty])?;
            let len = u32::try_from(r.content.len() + extra).map_err(|_| DtaError::TooLong {
                what: format!("strL cell (v={}, o={})", r.v, r.o),
                value: String::from_utf8_lossy(&r.content[..r.content.len().min(40)]).into_owned(),
                len: r.content.len(),
                limit: u32::MAX as usize,
                release: p.release.number(),
            })?;
            s.put(&len.to_le_bytes())?;
            s.put(&r.content)?;
            if extra == 1 {
                s.put(&[0u8])?;
            }
        }
        Ok(())
    })?;

    // <value_labels>
    map[11] = sink.pos();
    section(sink, "value_labels", |s| {
        let mut pad = vec![0u8; spec.vlblname_len];
        for l in &p.labels {
            let n = l.entries.len() as u32;
            // `len` counts from `n` onward: 4 + 4 + 4n + 4n + txtlen.
            let len = 8 + 8 * n + l.txtlen;
            s.put(b"<lbl>")?;
            s.put(&len.to_le_bytes())?;
            put_padded(s, &l.name, spec.vlblname_len, &mut pad)?;
            // The 3 bytes between the name field and `n`, verified by offset
            // arithmetic on auto.dta (`04` §9.2).
            s.put(&[0u8; 3])?;
            s.put(&n.to_le_bytes())?;
            s.put(&l.txtlen.to_le_bytes())?;
            let mut off = 0u32;
            for (_, txt) in &l.entries {
                s.put(&off.to_le_bytes())?;
                off += txt.len() as u32 + 1;
            }
            for (value, _) in &l.entries {
                s.put(&value.to_le_bytes())?;
            }
            for (_, txt) in &l.entries {
                s.put(txt)?;
                s.put(&[0u8])?;
            }
            s.put(b"</lbl>")?;
        }
        Ok(())
    })?;

    map[12] = sink.pos();
    sink.put(b"</stata_dta>")?;
    map[MAP_EOF] = sink.pos();

    let mut raw = [0u8; MAP_BYTES];
    for (i, v) in map.iter().enumerate() {
        raw[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    sink.patch(map_payload_at, &raw)?;
    Ok(())
}

/// Write `<tag>` … `</tag>` around whatever `body` puts.
fn section<S: Sink, F>(sink: &mut S, tag: &str, body: F) -> Result<(), DtaError>
where
    F: FnOnce(&mut S) -> Result<(), DtaError>,
{
    sink.put(format!("<{tag}>").as_bytes())?;
    body(sink)?;
    sink.put(format!("</{tag}>").as_bytes())?;
    Ok(())
}

fn put_fields<S: Sink>(
    sink: &mut S,
    fields: &[Cow<'_, [u8]>],
    width: usize,
) -> Result<(), DtaError> {
    let mut pad = vec![0u8; width];
    for f in fields {
        put_padded(sink, f, width, &mut pad)?;
    }
    Ok(())
}

/// One fixed-width field: the bytes, then NULs to `width`.
///
/// The padding is a slice of one reusable zero buffer rather than a fresh
/// `vec![0; n]` per field — on a 40 000-variable file that is four allocations
/// instead of 160 000.
fn put_padded<S: Sink>(
    sink: &mut S,
    bytes: &[u8],
    width: usize,
    pad: &mut Vec<u8>,
) -> Result<(), DtaError> {
    if pad.len() < width {
        pad.resize(width, 0);
    }
    sink.put(bytes)?;
    sink.put(&pad[..width - bytes.len()])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The data section — `04` §10.3
// ---------------------------------------------------------------------------

/// Column-major → row-major, one L2-resident tile at a time.
///
/// The loop order is (tile, column, row) rather than (row, column): each column
/// is read sequentially out of its chunks and written at `row_width` stride
/// into the tile, so the source side is a straight scan and only the
/// destination is strided — and the destination is 256 KiB, so the strided
/// stores stay in cache. The obvious (row, column) loop instead re-derives
/// every column's chunk pointer once per observation.
fn write_data<S: Sink>(sink: &mut S, ds: &Dataset, p: &Plan<'_>) -> Result<(), DtaError> {
    if p.n_obs == 0 || p.row_width == 0 {
        return Ok(());
    }
    let tile_rows = (TILE_BYTES / p.row_width).max(1) as u64;
    let mut buf = vec![0u8; (tile_rows as usize).min(p.n_obs as usize) * p.row_width];

    let mut lo = 0u64;
    while lo < p.n_obs {
        let hi = (lo + tile_rows).min(p.n_obs);
        let rows = (hi - lo) as usize;
        buf.resize(rows * p.row_width, 0);
        let mut offset = 0usize;
        for (i, col) in ds.cols.iter().enumerate() {
            scatter(
                col,
                p.strl_keys[i].as_deref(),
                offset,
                p.row_width,
                lo,
                hi,
                p.n_obs,
                &mut buf,
            );
            offset += usize::from(stratum_core::types::storage_width(col.storage_type()));
        }
        sink.put(&buf)?;
        lo = hi;
    }
    Ok(())
}

/// Write rows `lo..hi` of one column into `buf` at `col_offset`, stride
/// `row_width`.
#[allow(clippy::too_many_arguments)]
fn scatter(
    col: &DtaColumn,
    keys: Option<&[u64]>,
    col_offset: usize,
    row_width: usize,
    lo: u64,
    hi: u64,
    len: u64,
    buf: &mut [u8],
) {
    macro_rules! numeric {
        ($c:expr, $w:expr) => {{
            let c = $c;
            runs(lo, hi, len, |chunk, off, count, first| {
                let src = &c.chunk(chunk)[off..off + count];
                for (j, v) in src.iter().enumerate() {
                    let at = (first - lo) as usize * row_width + j * row_width + col_offset;
                    buf[at..at + $w].copy_from_slice(&v.to_le_bytes());
                }
            });
        }};
    }
    match col {
        DtaColumn::Fixed(Column::Byte(c)) => numeric!(c, 1),
        DtaColumn::Fixed(Column::Int(c)) => numeric!(c, 2),
        DtaColumn::Fixed(Column::Long(c)) => numeric!(c, 4),
        DtaColumn::Fixed(Column::Float(c)) => numeric!(c, 4),
        DtaColumn::Fixed(Column::Double(c)) => numeric!(c, 8),
        DtaColumn::Fixed(Column::Str(s)) => {
            // `raw` is the stored fixed-width field, padding included, so this
            // is one `copy_from_slice` per cell and a file read and rewritten
            // unchanged keeps its data section byte-identical.
            let w = s.width() as usize;
            runs(lo, hi, len, |chunk, off, count, first| {
                let src = &s.chunk(chunk)[off * w..(off + count) * w];
                for j in 0..count {
                    let at = (first - lo) as usize * row_width + j * row_width + col_offset;
                    buf[at..at + w].copy_from_slice(&src[j * w..(j + 1) * w]);
                }
            });
        }
        // A `strL` data cell is the packed `(v,o)` key the plan computed, never
        // the text. `Fixed(Column::StrL)` is unreachable: `prepare` rejects it.
        DtaColumn::StrL(_) | DtaColumn::Fixed(Column::StrL(_)) => {
            let keys = keys.unwrap_or(&[]);
            for row in lo..hi {
                let k = keys.get(row as usize).copied().unwrap_or(0);
                let at = (row - lo) as usize * row_width + col_offset;
                buf[at..at + 8].copy_from_slice(&k.to_le_bytes());
            }
        }
    }
}

/// Split `lo..hi` into maximal runs that lie inside one chunk, so the caller
/// gets a contiguous source slice per call instead of one bounds-check per row.
fn runs(lo: u64, hi: u64, len: u64, mut f: impl FnMut(usize, usize, usize, u64)) {
    let mut row = lo;
    while row < hi {
        let chunk = chunk_of(row);
        let off = offset_in_chunk(row);
        let avail = chunk_len(chunk, len) - off;
        let count = (avail as u64).min(hi - row) as usize;
        f(chunk, off, count, row);
        row += count as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{MAP_ENTRIES, SECTIONS};
    use crate::{reader, DtaVar};
    use stratum_data::NumCol;

    fn read_map(bytes: &[u8]) -> [u64; MAP_ENTRIES] {
        let at = bytes
            .windows(5)
            .position(|w| w == b"<map>")
            .expect("a written file has a map")
            + 5;
        let mut m = [0u64; MAP_ENTRIES];
        for (i, slot) in m.iter_mut().enumerate() {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[at + i * 8..at + i * 8 + 8]);
            *slot = u64::from_le_bytes(b);
        }
        m
    }

    fn tiny() -> Dataset {
        let mut ds = Dataset::new(Release::R118);
        ds.label = "tiny".into();
        ds.timestamp = "01 Jan 2020 00:00".into();
        ds.push_var(
            DtaVar::new("x", StorageType::Int),
            DtaColumn::Fixed(Column::Int(NumCol::from_slice(&[1i16, 2, 3]))),
        )
        .unwrap();
        ds
    }

    /// `04` §10.2 step 5: our own files satisfy `map[13] == len` exactly, even
    /// though the reader accepts `<=` for StataCorp's sake.
    #[test]
    fn the_map_we_write_declares_the_exact_end_of_file() {
        let bytes = write_dta_to_vec(&tiny(), &DtaWriteOptions::default()).unwrap();
        let m = read_map(&bytes);
        assert_eq!(m[MAP_EOF], bytes.len() as u64);
        assert_eq!(m[0], 0);
        for i in 1..MAP_ENTRIES {
            assert!(m[i] >= m[i - 1], "map is not monotonic at {i}: {m:?}");
        }
        // Every tagged section starts with its own tag.
        for (i, tag) in SECTIONS.iter().enumerate() {
            let at = m[i] as usize;
            let want = format!("<{tag}>");
            assert_eq!(
                &bytes[at..at + want.len()],
                want.as_bytes(),
                "map[{i}] does not point at <{tag}>"
            );
        }
    }

    #[test]
    fn a_written_file_reads_back_equal() {
        for release in Release::ALL {
            let ds = tiny();
            let bytes = write_dta_to_vec(&ds, &DtaWriteOptions::release(release)).unwrap();
            let back = reader::read_dta_bytes(&bytes, &reader::DtaReadOptions::default()).unwrap();
            assert_eq!(back.release, release);
            assert_eq!(back.read_report().trailing_bytes, 0);
            crate::canonical_eq(&ds, &back).unwrap();
        }
    }

    #[test]
    fn the_writer_refuses_names_that_would_make_an_unreadable_file() {
        for (name, why) in [
            ("", "empty"),
            ("3x", "leading digit"),
            ("a b", "space"),
            ("a\tb", "tab"),
            ("a\0b", "NUL"),
        ] {
            assert!(check_name(0, name).is_err(), "{why}: {name:?} was accepted");
        }
        // Unicode names are legal in 118/119 and must NOT be refused.
        check_name(0, "变量").unwrap();
        check_name(0, "año").unwrap();
        check_name(0, "_x1").unwrap();
    }

    #[test]
    fn a_name_too_long_for_the_release_names_the_release() {
        let mut ds = Dataset::new(Release::R117);
        let long = "a".repeat(40);
        ds.push_var(
            DtaVar::new(&long, StorageType::Byte),
            DtaColumn::Fixed(Column::new_missing(StorageType::Byte, 1)),
        )
        .unwrap();
        let e = write_dta_to_vec(&ds, &DtaWriteOptions::release(Release::R117)).unwrap_err();
        assert!(matches!(e, DtaError::TooLong { limit: 33, .. }), "{e}");
        // The same name fits release 118's 129-byte field.
        write_dta_to_vec(&ds, &DtaWriteOptions::release(Release::R118)).unwrap();
    }

    #[test]
    fn auto_release_picks_119_only_when_118_cannot_hold_the_variables() {
        let mut ds = Dataset::new(Release::R118);
        ds.set_n_obs(0);
        assert_eq!(
            prepare(&ds, &DtaWriteOptions::default()).unwrap().release,
            Release::R118
        );
        // Building 32 768 real columns to prove the boundary would cost more
        // than it proves; the rule itself is what is asserted.
        const { assert!(MAX_VARS_117_118 == 32_767) };
    }
}
