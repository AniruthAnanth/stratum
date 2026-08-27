//! The SDP1 reference fixtures — CONTRACTS §8.1, ADR-007.
//!
//! SDP1 is the one bulk transport (A13): engine → mmap segment ring →
//! `stratum-asset://localhost/frame/{session}/{frame}/page` → a `DataView` over
//! one `ArrayBuffer`. Both ends decode it independently, so both ends need a
//! third artifact to agree with, and they need it before either exists — W12's
//! `decodeDataPage` is asserted in week 1 while `Frame::page()` (W02b) starts in
//! week 2. Those bytes are `tests/fixtures/sdp1/*.bin` (audit finding A29).
//!
//! **`tests/fixtures/sdp1/README.md` is the specification.** §8.1 leaves four
//! things underdetermined that a byte-exact fixture cannot be agnostic about,
//! and that README rules on all four; they are as normative as §8.1 itself for
//! anyone matching these bytes, and [`encode`] implements them:
//!
//!   1. the header is compact JSON right-padded with spaces until the payload
//!      starts 8-aligned, because `new Float64Array(buf, byteOffset, n)` throws
//!      unless `byteOffset % 8 == 0`;
//!   2. every region is aligned to its element (f64 → 8, u32 → 4, u8 → 1) with
//!      zero fill that belongs to no region;
//!   3. columns are laid out in `idx` order, and within a column the regions go
//!      down in the order §8.1's own table lists them — `aux` then `data` for
//!      `text` and `blob`, `data` then `aux` for `num`;
//!   4. the `blob` bitmap is the last `ceil(nrows/8)` bytes of `data`.
//!
//! **`auto_40x12.bin` is not generated here.** It is `RenderMode::Display`, so
//! every cell is what Stata's own `string(x, "%fmt")` returned; the fixture's
//! whole value is that one side of the comparison did not come from our code.
//! `generate` therefore refuses to write it unless it is handed a capture
//! (`--display-cells`, the `col<TAB>row<TAB>text` file
//! `scripts/capture-golden.sh` produces) rather than falling back to a formatter
//! of ours. `check` still validates it structurally, which needs no Stata.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Subcommand};

use crate::Ctx;

pub const MAGIC: &[u8; 4] = b"SDP1";

/// `255 = not missing`, `0 = "."`, `1..=26 = ".a".."\.z"` (CONTRACTS §8.1).
pub const TAG_PRESENT: u8 = 255;

/// `f64::from_bits(0x7FE0_0000_0000_0000 + (k << 40))` — 04 §2.3. The wire
/// carries the sentinel bit-exactly, so the fixture must too.
pub const F64_MISS_BITS: u64 = 0x7FE0_0000_0000_0000;

pub fn missing_bits(tag: u8) -> u64 {
    debug_assert!(u64::from(tag) <= 26);
    F64_MISS_BITS + (u64::from(tag) << 40)
}

/// Spec §13's own worked example, "Dataset state: D17". Deliberately not `0`, so
/// a decoder that forgets to read `state` fails visibly rather than agreeing by
/// accident.
const AUTO_STATE: u64 = 17;
const STRL_STATE: u64 = 1;

const AUTO_DISPLAY: &str = "auto_40x12.bin";
const AUTO_EDIT: &str = "auto_40x12_edit.bin";
const STRL_EDIT: &str = "strl_3x2_edit.bin";

#[derive(Args)]
pub struct Cmd {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Write the fixtures this tool can produce without Stata.
    Generate {
        /// Directory to write into. Defaults to `tests/fixtures/sdp1`.
        #[arg(long, value_name = "DIR")]
        out_dir: Option<Utf8PathBuf>,
        /// Stata's captured `Display` cells, as `col<TAB>row<TAB>text` lines.
        /// Without it `auto_40x12.bin` is left alone rather than regenerated
        /// from our own formatter.
        #[arg(long, value_name = "FILE")]
        display_cells: Option<Utf8PathBuf>,
    },
    /// Verify the committed fixtures: byte-compare what this tool can build, and
    /// structurally validate everything else against the layout rules.
    Check {
        #[arg(long, value_name = "DIR")]
        out_dir: Option<Utf8PathBuf>,
        #[arg(long, value_name = "FILE")]
        display_cells: Option<Utf8PathBuf>,
        /// Fail if the fixtures have not been committed yet. CI turns this on
        /// once `tests/fixtures/sdp1/` exists.
        #[arg(long)]
        require: bool,
    },
    /// Decode a `.bin` and print its header and first cells. For when the two
    /// decoders disagree and someone needs to see which one is wrong.
    Dump { file: Utf8PathBuf },
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    match &cmd.action {
        Action::Generate {
            out_dir,
            display_cells,
        } => {
            let dir = out_dir
                .clone()
                .unwrap_or_else(|| ctx.path("tests/fixtures/sdp1"));
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
            for (name, bytes) in generable(display_cells.as_deref())? {
                let path = dir.join(name);
                std::fs::write(&path, &bytes).with_context(|| format!("writing {path}"))?;
                println!("sdp1: wrote {path} ({} bytes)", bytes.len());
            }
            if display_cells.is_none() {
                println!(
                    "sdp1: {AUTO_DISPLAY} NOT regenerated — its cells are Stata's output, not \
                     ours. Capture them with scripts/capture-golden.sh and pass --display-cells."
                );
            }
            Ok(())
        }

        Action::Check {
            out_dir,
            display_cells,
            require,
        } => {
            let dir = out_dir
                .clone()
                .unwrap_or_else(|| ctx.path("tests/fixtures/sdp1"));
            if !dir.is_dir() {
                anyhow::ensure!(!require, "--require: {dir} does not exist");
                println!("sdp1: skipped, {dir} does not exist yet");
                return Ok(());
            }

            let mut problems = Vec::new();
            let mut exact = 0usize;
            for (name, want) in generable(display_cells.as_deref())? {
                match std::fs::read(dir.join(name)) {
                    Ok(got) if got == want => exact += 1,
                    Ok(got) => problems.push(format!(
                        "{name}: committed {} bytes, generator produces {} — {}",
                        got.len(),
                        want.len(),
                        first_difference(&got, &want)
                    )),
                    Err(e) => problems.push(format!("{name}: {e}")),
                }
            }

            // Everything else — `auto_40x12.bin` above all — is validated
            // against the layout rules rather than regenerated. That is not a
            // weaker check for the thing that matters: it is the check that
            // catches an emitter which mis-aligns a region or writes an offset
            // that walks off the end, which is every bug a decoder will hit.
            let mut structural = 0usize;
            for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {dir}"))? {
                let path = Utf8PathBuf::from_path_buf(entry?.path())
                    .map_err(|p| anyhow::anyhow!("non-UTF-8 path {}", p.display()))?;
                if path.extension() != Some("bin") {
                    continue;
                }
                let name = path.file_name().unwrap_or_default().to_owned();
                let bytes = std::fs::read(&path).with_context(|| format!("reading {path}"))?;
                match validate(&bytes) {
                    Ok(()) => structural += 1,
                    Err(e) => problems.push(format!("{name}: {e:#}")),
                }
            }

            if problems.is_empty() {
                println!(
                    "sdp1: OK — {exact} fixture(s) byte-identical to the generator, \
                     {structural} valid against the layout rules"
                );
                return Ok(());
            }
            for p in &problems {
                eprintln!("sdp1: {p}");
            }
            anyhow::bail!("the committed SDP1 fixtures and the encoder disagree");
        }

        Action::Dump { file } => {
            let bytes = std::fs::read(file).with_context(|| format!("reading {file}"))?;
            print!("{}", decode(&bytes)?.describe());
            Ok(())
        }
    }
}

fn first_difference(a: &[u8], b: &[u8]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(i) => format!("first difference at byte {i}"),
        None => "one is a prefix of the other".to_owned(),
    }
}

/// The fixtures this tool can build from the repository alone, as
/// `(file name, bytes)`. `auto_40x12.bin` joins the list only when Stata's
/// captured cells are supplied.
pub fn generable(display_cells: Option<&Utf8Path>) -> Result<Vec<(&'static str, Vec<u8>)>> {
    let mut out = vec![
        (AUTO_EDIT, auto_edit_page()?),
        (STRL_EDIT, strl_edit_page()?),
    ];
    if let Some(path) = display_cells {
        out.insert(0, (AUTO_DISPLAY, auto_display_page(path)?));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// One column as it goes on the wire. The variants are exactly §8.1's three
/// `kind` values; there is no fourth, and adding one is an `SDP2`.
pub enum Column {
    /// `kind: "text"` — `(nrows+1)` u32 offsets, then a UTF-8 arena.
    Text { idx: u32, values: Vec<String> },
    /// `kind: "num"` — `nrows` f64, then `nrows` u8 tags.
    Num {
        idx: u32,
        bits: Vec<u64>,
        tags: Vec<u8>,
    },
    /// `kind: "blob"` — `(nrows+1)` u32 offsets, then a byte arena with a
    /// `ceil(nrows/8)` bitmap on the end where a set bit means GSO type 129.
    Blob {
        idx: u32,
        values: Vec<Vec<u8>>,
        binary: Vec<bool>,
    },
}

impl Column {
    fn idx(&self) -> u32 {
        match self {
            Column::Text { idx, .. } | Column::Num { idx, .. } | Column::Blob { idx, .. } => *idx,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Column::Text { .. } => "text",
            Column::Num { .. } => "num",
            Column::Blob { .. } => "blob",
        }
    }

    fn rows(&self) -> usize {
        match self {
            Column::Text { values, .. } => values.len(),
            Column::Num { bits, .. } => bits.len(),
            Column::Blob { values, .. } => values.len(),
        }
    }

    /// `(data, aux)` for this column, already in wire form.
    fn regions(&self) -> (Vec<u8>, Vec<u8>) {
        match self {
            Column::Text { values, .. } => {
                let (data, aux) = arena(values.iter().map(String::as_bytes));
                (data, aux)
            }
            Column::Num { bits, tags, .. } => {
                let mut data = Vec::with_capacity(bits.len() * 8);
                for b in bits {
                    data.extend_from_slice(&b.to_le_bytes());
                }
                (data, tags.clone())
            }
            Column::Blob { values, binary, .. } => {
                let (mut data, aux) = arena(values.iter().map(Vec::as_slice));
                let mut bitmap = vec![0u8; values.len().div_ceil(8)];
                for (row, is_binary) in binary.iter().enumerate() {
                    if *is_binary {
                        bitmap[row / 8] |= 1 << (row % 8);
                    }
                }
                data.extend_from_slice(&bitmap);
                (data, aux)
            }
        }
    }

    /// `(alignment, is_data)` pairs in the order §8.1 lists the regions for this
    /// kind. `aux` before `data` for `text`/`blob`, `data` before `aux` for
    /// `num`.
    fn layout(&self) -> [(usize, bool); 2] {
        match self {
            Column::Num { .. } => [(8, true), (1, false)],
            _ => [(4, false), (1, true)],
        }
    }
}

fn arena<'a>(values: impl Iterator<Item = &'a [u8]>) -> (Vec<u8>, Vec<u8>) {
    let mut data = Vec::new();
    let mut aux = 0u32.to_le_bytes().to_vec();
    for v in values {
        data.extend_from_slice(v);
        aux.extend_from_slice(&(data.len() as u32).to_le_bytes());
    }
    (data, aux)
}

/// Build a page. Little-endian throughout; see the module docs for the four
/// layout rulings this implements.
pub fn encode(state: u64, row0: u64, seq: u32, cols: &[Column]) -> Result<Vec<u8>> {
    anyhow::ensure!(!cols.is_empty(), "a DataPage needs at least one column");
    let nrows = cols[0].rows();
    anyhow::ensure!(
        cols.iter().all(|c| c.rows() == nrows),
        "every column of a page must have the same row count"
    );

    let mut payload: Vec<u8> = Vec::new();
    let mut entries = Vec::with_capacity(cols.len());
    for col in cols {
        let (data, aux) = col.regions();
        let mut placed = [(0u64, 0u64); 2];
        for (slot, (align, is_data)) in col.layout().into_iter().enumerate() {
            let region = if is_data { &data } else { &aux };
            // Zero fill to the region's element alignment. `8 + header_len` is
            // itself 8-aligned, so aligning the relative offset aligns the
            // absolute one the client will hand to a typed-array view.
            payload.resize(payload.len().next_multiple_of(align), 0);
            placed[slot] = (payload.len() as u64, region.len() as u64);
            payload.extend_from_slice(region);
        }
        let ((off, len), (aux_off, aux_len)) = match col.layout()[0].1 {
            true => (placed[0], placed[1]),
            false => (placed[1], placed[0]),
        };
        entries.push(format!(
            r#"{{"idx":{},"kind":"{}","off":{off},"len":{len},"aux_off":{aux_off},"aux_len":{aux_len}}}"#,
            col.idx(),
            col.kind()
        ));
    }

    // Hand-written rather than `serde_json`: the fixture's bytes are the
    // contract, and a serializer that one day reorders keys or changes how it
    // spells a number would silently invalidate every checked-in fixture.
    let mut header = format!(
        r#"{{"state":{state},"row0":{row0},"nrows":{nrows},"seq":{seq},"cols":[{}]}}"#,
        entries.join(",")
    );
    while (8 + header.len()) % 8 != 0 {
        header.push(' ');
    }

    let mut out = Vec::with_capacity(8 + header.len() + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Decoder and validator — the Rust twin of `decodeDataPage` (CONTRACTS §12)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub struct Page {
    pub state: u64,
    pub row0: u64,
    pub nrows: u32,
    pub seq: u32,
    pub cols: Vec<DecodedColumn>,
}

#[derive(Debug, PartialEq)]
pub enum DecodedColumn {
    Text {
        idx: u32,
        values: Vec<String>,
    },
    Num {
        idx: u32,
        bits: Vec<u64>,
        tags: Vec<u8>,
    },
    Blob {
        idx: u32,
        values: Vec<Vec<u8>>,
        binary: Vec<bool>,
    },
}

impl Page {
    pub fn describe(&self) -> String {
        use std::fmt::Write as _;
        let mut s = format!(
            "SDP1 state={} row0={} nrows={} seq={} cols={}\n",
            self.state,
            self.row0,
            self.nrows,
            self.seq,
            self.cols.len()
        );
        for col in &self.cols {
            match col {
                DecodedColumn::Text { idx, values } => {
                    let head: Vec<&str> = values.iter().take(3).map(String::as_str).collect();
                    let _ = writeln!(s, "  [{idx}] text  {head:?}…");
                }
                DecodedColumn::Num { idx, bits, tags } => {
                    let head: Vec<String> = bits
                        .iter()
                        .zip(tags)
                        .take(3)
                        .map(|(b, t)| match *t {
                            TAG_PRESENT => format!("{}", f64::from_bits(*b)),
                            0 => ".".to_owned(),
                            t => format!(".{}", (b'a' + t - 1) as char),
                        })
                        .collect();
                    let _ = writeln!(s, "  [{idx}] num   {}…", head.join(", "));
                }
                DecodedColumn::Blob {
                    idx,
                    values,
                    binary,
                } => {
                    let _ = writeln!(
                        s,
                        "  [{idx}] blob  {} values, {} binary",
                        values.len(),
                        binary.iter().filter(|b| **b).count()
                    );
                }
            }
        }
        s
    }
}

struct Header {
    header_len: usize,
    base: usize,
    state: u64,
    row0: u64,
    nrows: u32,
    seq: u32,
    cols: Vec<ColHeader>,
}

struct ColHeader {
    idx: u32,
    kind: String,
    off: usize,
    len: usize,
    aux_off: usize,
    aux_len: usize,
}

fn read_header(buf: &[u8]) -> Result<Header> {
    anyhow::ensure!(buf.len() >= 8, "buffer is shorter than an SDP1 header");
    anyhow::ensure!(&buf[..4] == MAGIC, "bad magic: not an SDP1 page");
    let header_len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
    let base = 8 + header_len;
    anyhow::ensure!(
        buf.len() >= base,
        "header_len {header_len} runs past the buffer"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&buf[8..base]).context("SDP1 header is not valid JSON")?;

    let int = |k: &str| -> Result<u64> {
        json[k]
            .as_u64()
            .with_context(|| format!("header field `{k}` missing or not an integer"))
    };
    let mut cols = Vec::new();
    for c in json["cols"]
        .as_array()
        .context("header has no `cols` array")?
    {
        let field = |k: &str| -> Result<u64> {
            c[k].as_u64()
                .with_context(|| format!("column field `{k}` missing or not an integer"))
        };
        cols.push(ColHeader {
            idx: field("idx")? as u32,
            kind: c["kind"]
                .as_str()
                .context("column has no `kind`")?
                .to_owned(),
            off: field("off")? as usize,
            len: field("len")? as usize,
            aux_off: field("aux_off")? as usize,
            aux_len: field("aux_len")? as usize,
        });
    }
    Ok(Header {
        header_len,
        base,
        state: int("state")?,
        row0: int("row0")?,
        nrows: int("nrows")? as u32,
        seq: int("seq")? as u32,
        cols,
    })
}

pub fn decode(buf: &[u8]) -> Result<Page> {
    let h = read_header(buf)?;
    let n = h.nrows as usize;
    let mut cols = Vec::with_capacity(h.cols.len());

    for c in &h.cols {
        let slice = |off: usize, len: usize, what: &str| -> Result<&[u8]> {
            buf.get(h.base + off..h.base + off + len)
                .with_context(|| format!("column {} {what} runs past the buffer", c.idx))
        };
        let data = slice(c.off, c.len, "data")?;
        let aux = slice(c.aux_off, c.aux_len, "aux")?;

        cols.push(match c.kind.as_str() {
            "text" => {
                let offsets = read_u32s(aux, n + 1)?;
                let mut values = Vec::with_capacity(n);
                for w in offsets.windows(2) {
                    let raw = data
                        .get(w[0] as usize..w[1] as usize)
                        .context("text arena slice out of range")?;
                    values
                        .push(String::from_utf8(raw.to_vec()).context("text arena is not UTF-8")?);
                }
                DecodedColumn::Text { idx: c.idx, values }
            }
            "num" => {
                anyhow::ensure!(data.len() == n * 8, "num column data is not nrows × f64");
                anyhow::ensure!(aux.len() == n, "num column aux is not nrows × u8");
                let bits = data
                    .chunks_exact(8)
                    .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                    .collect();
                DecodedColumn::Num {
                    idx: c.idx,
                    bits,
                    tags: aux.to_vec(),
                }
            }
            "blob" => {
                let offsets = read_u32s(aux, n + 1)?;
                let arena_len = *offsets.last().unwrap_or(&0) as usize;
                let mut values = Vec::with_capacity(n);
                for w in offsets.windows(2) {
                    values.push(
                        data.get(w[0] as usize..w[1] as usize)
                            .context("blob arena slice out of range")?
                            .to_vec(),
                    );
                }
                let bitmap = data
                    .get(arena_len..arena_len + n.div_ceil(8))
                    .context("blob column is missing its binary bitmap")?;
                let binary = (0..n)
                    .map(|r| bitmap[r / 8] & (1 << (r % 8)) != 0)
                    .collect();
                DecodedColumn::Blob {
                    idx: c.idx,
                    values,
                    binary,
                }
            }
            other => anyhow::bail!("unknown column kind {other:?}"),
        });
    }
    Ok(Page {
        state: h.state,
        row0: h.row0,
        nrows: h.nrows,
        seq: h.seq,
        cols,
    })
}

/// Every rule `tests/fixtures/sdp1/README.md` states, checked against one file.
/// This is what makes a fixture nobody here can regenerate — the Stata-captured
/// `Display` page — still worth having in CI.
pub fn validate(buf: &[u8]) -> Result<()> {
    let h = read_header(buf)?;
    let n = h.nrows as usize;

    anyhow::ensure!(
        h.base % 8 == 0,
        "payload starts at {} which is not 8-aligned; §2.1 requires the header \
         to be space-padded so a Float64Array view is constructible",
        h.base
    );
    anyhow::ensure!(
        buf[8..h.base]
            .iter()
            .rev()
            .take_while(|b| **b == b' ')
            .count()
            < 8,
        "header padding exceeds 8 bytes, so it is not minimal"
    );
    anyhow::ensure!(h.seq > 0, "seq must be set");

    let mut end = 0usize;
    for (position, c) in h.cols.iter().enumerate() {
        anyhow::ensure!(
            c.idx as usize == position,
            "columns must be laid out in idx order; slot {position} carries idx {}",
            c.idx
        );
        let (data_align, aux_align, first_is_data) = match c.kind.as_str() {
            "num" => (8usize, 1usize, true),
            "text" | "blob" => (1usize, 4usize, false),
            other => anyhow::bail!("unknown column kind {other:?}"),
        };
        anyhow::ensure!(
            c.off % data_align == 0 && c.aux_off % aux_align == 0,
            "column {} regions are not aligned to their element ({}/{})",
            c.idx,
            c.off,
            c.aux_off
        );
        let (first, second) = if first_is_data {
            ((c.off, c.len), (c.aux_off, c.aux_len))
        } else {
            ((c.aux_off, c.aux_len), (c.off, c.len))
        };
        anyhow::ensure!(
            first.0 >= end && second.0 >= first.0 + first.1,
            "column {} regions overlap or are out of order",
            c.idx
        );
        anyhow::ensure!(
            h.base + second.0 + second.1 <= buf.len(),
            "column {} runs past the end of the file",
            c.idx
        );
        end = second.0 + second.1;

        match c.kind.as_str() {
            "num" => {
                anyhow::ensure!(c.len == n * 8, "column {}: len is not nrows × f64", c.idx);
                anyhow::ensure!(c.aux_len == n, "column {}: aux_len is not nrows", c.idx);
            }
            _ => anyhow::ensure!(
                c.aux_len == (n + 1) * 4,
                "column {}: aux_len is not (nrows+1) × u32",
                c.idx
            ),
        }
    }
    anyhow::ensure!(
        h.base + end == buf.len(),
        "the payload ends at {} but the file is {} bytes; §2.2 forbids trailing slack",
        h.base + end,
        buf.len()
    );

    // Decoding is itself part of the validation: it is what checks that every
    // arena offset is ascending and in range, that text is UTF-8, and that a
    // blob column carries its bitmap.
    let page = decode(buf)?;
    for col in &page.cols {
        if let DecodedColumn::Num { idx, bits, tags } = col {
            for (row, (b, t)) in bits.iter().zip(tags).enumerate() {
                anyhow::ensure!(
                    *t == TAG_PRESENT || u64::from(*t) <= 26,
                    "column {idx} row {row}: tag {t} is not a missing tag"
                );
                // The tag is redundant with the payload by construction. Which
                // is exactly why it is worth checking: it is the one place a
                // producer can contradict itself.
                if *t != TAG_PRESENT {
                    anyhow::ensure!(
                        *b == missing_bits(*t),
                        "column {idx} row {row}: tag {t} but the f64 is {b:#018x}"
                    );
                } else {
                    anyhow::ensure!(
                        *b < F64_MISS_BITS,
                        "column {idx} row {row}: tagged present but the f64 is a sentinel"
                    );
                }
            }
        }
    }
    let _ = h.header_len;
    Ok(())
}

fn read_u32s(bytes: &[u8], count: usize) -> Result<Vec<u32>> {
    anyhow::ensure!(
        bytes.len() == count * 4,
        "expected {count} u32 offsets, got {} bytes",
        bytes.len()
    );
    let offsets: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    anyhow::ensure!(offsets[0] == 0, "an arena's first offset is always 0");
    anyhow::ensure!(
        offsets.windows(2).all(|w| w[0] <= w[1]),
        "arena offsets must be ascending"
    );
    Ok(offsets)
}

// ---------------------------------------------------------------------------
// Fixture data
// ---------------------------------------------------------------------------

/// `auto.dta` observations 1..=40, all 12 variables, unformatted. Column 0 is
/// `make`'s raw bytes; 1..=11 are the numerics as f64 plus a tag byte.
fn auto_edit_page() -> Result<Vec<u8>> {
    let mut cols: Vec<Column> = Vec::with_capacity(12);
    cols.push(Column::Text {
        idx: 0,
        values: AUTO_MAKE.iter().map(|s| (*s).to_owned()).collect(),
    });
    for c in 0..AUTO_NUMERIC_NAMES.len() {
        let mut bits = Vec::with_capacity(AUTO_NUM.len());
        let mut tags = Vec::with_capacity(AUTO_NUM.len());
        for (r, row) in AUTO_NUM.iter().enumerate() {
            match AUTO_MISSING.iter().find(|(mr, mc, _)| *mr == r && *mc == c) {
                Some((_, _, tag)) => {
                    bits.push(missing_bits(*tag));
                    tags.push(*tag);
                }
                None => {
                    bits.push(row[c].to_bits());
                    tags.push(TAG_PRESENT);
                }
            }
        }
        cols.push(Column::Num {
            idx: c as u32 + 1,
            bits,
            tags,
        });
    }
    encode(AUTO_STATE, 0, 1, &cols)
}

/// The same page as Stata renders it. `cells` is `col<TAB>row<TAB>text`, 0-based
/// on both indices — the shape `scripts/capture-golden.sh` writes.
fn auto_display_page(cells: &Utf8Path) -> Result<Vec<u8>> {
    let text = std::fs::read_to_string(cells).with_context(|| format!("reading {cells}"))?;
    let mut table: BTreeMap<(usize, usize), String> = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let mut num = |what: &str| -> Result<usize> {
            parts
                .next()
                .and_then(|s| s.trim().parse().ok())
                .with_context(|| format!("{cells}:{}: no {what} index", n + 1))
        };
        let (col, row) = (num("column")?, num("row")?);
        let value = parts
            .next()
            .with_context(|| format!("{cells}:{}: no cell text", n + 1))?;
        anyhow::ensure!(
            table.insert((col, row), value.to_owned()).is_none(),
            "{cells}:{}: cell ({col},{row}) appears twice",
            n + 1
        );
    }

    let rows = AUTO_MAKE.len();
    let ncols = AUTO_NUMERIC_NAMES.len() + 1;
    let mut cols = Vec::with_capacity(ncols);
    for c in 0..ncols {
        let mut values = Vec::with_capacity(rows);
        for r in 0..rows {
            values.push(
                table
                    .get(&(c, r))
                    .with_context(|| format!("{cells}: no cell for column {c} row {r}"))?
                    .clone(),
            );
        }
        cols.push(Column::Text {
            idx: c as u32,
            values,
        });
    }
    anyhow::ensure!(
        table.len() == rows * ncols,
        "{cells}: expected {} cells, got {}",
        rows * ncols,
        table.len()
    );
    encode(AUTO_STATE, 0, 1, &cols)
}

/// `strl.dta` in full — the only fixture that exercises `kind: "blob"` and its
/// trailing bitmap, which is the part of §8.1 a decoder is most likely to get
/// wrong and least likely to notice.
fn strl_edit_page() -> Result<Vec<u8>> {
    let cols = vec![
        Column::Blob {
            idx: 0,
            values: vec![
                STRL_SHORT.as_bytes().to_vec(),
                STRL_LONG.as_bytes().to_vec(),
                Vec::new(),
            ],
            // Both GSOs in strl.dta are type 130 (ASCII) and the third cell is
            // the null strL, so the bitmap's shape is exercised and a set bit is
            // not. Whoever adds a binary-strL `.dta` should add the page here.
            binary: vec![false, false, false],
        },
        Column::Text {
            idx: 1,
            values: vec!["abc".to_owned(), "abc".to_owned(), "abc".to_owned()],
        },
    ];
    encode(STRL_STATE, 0, 1, &cols)
}

const AUTO_NUMERIC_NAMES: [&str; 11] = [
    "price",
    "mpg",
    "rep78",
    "headroom",
    "trunk",
    "weight",
    "length",
    "turn",
    "displacement",
    "gear_ratio",
    "foreign",
];

/// `(row, numeric-column, tag)`. Only `rep78` is missing in the first 40
/// observations, twice, both plain `.` — kept sparse so the table below stays
/// readable and a new missing value is a visible one-line change.
const AUTO_MISSING: &[(usize, usize, u8)] = &[(2, 2, 0), (6, 2, 0)];

const AUTO_MAKE: [&str; 40] = [
    "AMC Concord",
    "AMC Pacer",
    "AMC Spirit",
    "Buick Century",
    "Buick Electra",
    "Buick LeSabre",
    "Buick Opel",
    "Buick Regal",
    "Buick Riviera",
    "Buick Skylark",
    "Cad. Deville",
    "Cad. Eldorado",
    "Cad. Seville",
    "Chev. Chevette",
    "Chev. Impala",
    "Chev. Malibu",
    "Chev. Monte Carlo",
    "Chev. Monza",
    "Chev. Nova",
    "Dodge Colt",
    "Dodge Diplomat",
    "Dodge Magnum",
    "Dodge St. Regis",
    "Ford Fiesta",
    "Ford Mustang",
    "Linc. Continental",
    "Linc. Mark V",
    "Linc. Versailles",
    "Merc. Bobcat",
    "Merc. Cougar",
    "Merc. Marquis",
    "Merc. Monarch",
    "Merc. XR-7",
    "Merc. Zephyr",
    "Olds 98",
    "Olds Cutl Supr",
    "Olds Cutlass",
    "Olds Delta 88",
    "Olds Omega",
    "Olds Starfire",
];

/// `headroom` and `gear_ratio` are Stata `float`s, so their f64 values are the
/// widened f32 — `3.5799999237060547`, not `3.58`. That is not noise: 04 §2.6
/// says a `float` widens through its exact `f32` value, and a fixture that
/// rounded it would make the Data Editor and `list` disagree.
#[rustfmt::skip]
const AUTO_NUM: [[f64; 11]; 40] = [
    [4099.0, 22.0, 3.0, 2.5, 11.0, 2930.0, 186.0, 40.0, 121.0, 3.5799999237060547, 0.0],
    [4749.0, 17.0, 3.0, 3.0, 11.0, 3350.0, 173.0, 40.0, 258.0, 2.5299999713897705, 0.0],
    [3799.0, 22.0, 0.0, 3.0, 12.0, 2640.0, 168.0, 35.0, 121.0, 3.0799999237060547, 0.0],
    [4816.0, 20.0, 3.0, 4.5, 16.0, 3250.0, 196.0, 40.0, 196.0, 2.930000066757202, 0.0],
    [7827.0, 15.0, 4.0, 4.0, 20.0, 4080.0, 222.0, 43.0, 350.0, 2.4100000858306885, 0.0],
    [5788.0, 18.0, 3.0, 4.0, 21.0, 3670.0, 218.0, 43.0, 231.0, 2.7300000190734863, 0.0],
    [4453.0, 26.0, 0.0, 3.0, 10.0, 2230.0, 170.0, 34.0, 304.0, 2.869999885559082, 0.0],
    [5189.0, 20.0, 3.0, 2.0, 16.0, 3280.0, 200.0, 42.0, 196.0, 2.930000066757202, 0.0],
    [10372.0, 16.0, 3.0, 3.5, 17.0, 3880.0, 207.0, 43.0, 231.0, 2.930000066757202, 0.0],
    [4082.0, 19.0, 3.0, 3.5, 13.0, 3400.0, 200.0, 42.0, 231.0, 3.0799999237060547, 0.0],
    [11385.0, 14.0, 3.0, 4.0, 20.0, 4330.0, 221.0, 44.0, 425.0, 2.2799999713897705, 0.0],
    [14500.0, 14.0, 2.0, 3.5, 16.0, 3900.0, 204.0, 43.0, 350.0, 2.190000057220459, 0.0],
    [15906.0, 21.0, 3.0, 3.0, 13.0, 4290.0, 204.0, 45.0, 350.0, 2.240000009536743, 0.0],
    [3299.0, 29.0, 3.0, 2.5, 9.0, 2110.0, 163.0, 34.0, 231.0, 2.930000066757202, 0.0],
    [5705.0, 16.0, 4.0, 4.0, 20.0, 3690.0, 212.0, 43.0, 250.0, 2.559999942779541, 0.0],
    [4504.0, 22.0, 3.0, 3.5, 17.0, 3180.0, 193.0, 31.0, 200.0, 2.7300000190734863, 0.0],
    [5104.0, 22.0, 2.0, 2.0, 16.0, 3220.0, 200.0, 41.0, 200.0, 2.7300000190734863, 0.0],
    [3667.0, 24.0, 2.0, 2.0, 7.0, 2750.0, 179.0, 40.0, 151.0, 2.7300000190734863, 0.0],
    [3955.0, 19.0, 3.0, 3.5, 13.0, 3430.0, 197.0, 43.0, 250.0, 2.559999942779541, 0.0],
    [3984.0, 30.0, 5.0, 2.0, 8.0, 2120.0, 163.0, 35.0, 98.0, 3.5399999618530273, 0.0],
    [4010.0, 18.0, 2.0, 4.0, 17.0, 3600.0, 206.0, 46.0, 318.0, 2.4700000286102295, 0.0],
    [5886.0, 16.0, 2.0, 4.0, 17.0, 3600.0, 206.0, 46.0, 318.0, 2.4700000286102295, 0.0],
    [6342.0, 17.0, 2.0, 4.5, 21.0, 3740.0, 220.0, 46.0, 225.0, 2.940000057220459, 0.0],
    [4389.0, 28.0, 4.0, 1.5, 9.0, 1800.0, 147.0, 33.0, 98.0, 3.1500000953674316, 0.0],
    [4187.0, 21.0, 3.0, 2.0, 10.0, 2650.0, 179.0, 43.0, 140.0, 3.0799999237060547, 0.0],
    [11497.0, 12.0, 3.0, 3.5, 22.0, 4840.0, 233.0, 51.0, 400.0, 2.4700000286102295, 0.0],
    [13594.0, 12.0, 3.0, 2.5, 18.0, 4720.0, 230.0, 48.0, 400.0, 2.4700000286102295, 0.0],
    [13466.0, 14.0, 3.0, 3.5, 15.0, 3830.0, 201.0, 41.0, 302.0, 2.4700000286102295, 0.0],
    [3829.0, 22.0, 4.0, 3.0, 9.0, 2580.0, 169.0, 39.0, 140.0, 2.7300000190734863, 0.0],
    [5379.0, 14.0, 4.0, 3.5, 16.0, 4060.0, 221.0, 48.0, 302.0, 2.75, 0.0],
    [6165.0, 15.0, 3.0, 3.5, 23.0, 3720.0, 212.0, 44.0, 302.0, 2.259999990463257, 0.0],
    [4516.0, 18.0, 3.0, 3.0, 15.0, 3370.0, 198.0, 41.0, 250.0, 2.430000066757202, 0.0],
    [6303.0, 14.0, 4.0, 3.0, 16.0, 4130.0, 217.0, 45.0, 302.0, 2.75, 0.0],
    [3291.0, 20.0, 3.0, 3.5, 17.0, 2830.0, 195.0, 43.0, 140.0, 3.0799999237060547, 0.0],
    [8814.0, 21.0, 4.0, 4.0, 20.0, 4060.0, 220.0, 43.0, 350.0, 2.4100000858306885, 0.0],
    [5172.0, 19.0, 3.0, 2.0, 16.0, 3310.0, 198.0, 42.0, 231.0, 2.930000066757202, 0.0],
    [4733.0, 19.0, 3.0, 4.5, 16.0, 3300.0, 198.0, 42.0, 231.0, 2.930000066757202, 0.0],
    [4890.0, 18.0, 4.0, 4.0, 20.0, 3690.0, 218.0, 42.0, 231.0, 2.7300000190734863, 0.0],
    [4181.0, 19.0, 3.0, 4.5, 14.0, 3370.0, 200.0, 43.0, 231.0, 3.0799999237060547, 0.0],
    [4195.0, 24.0, 1.0, 2.0, 10.0, 2730.0, 180.0, 40.0, 151.0, 2.7300000190734863, 0.0],
];

const STRL_SHORT: &str = "short";
const STRL_LONG: &str = "a much longer string that exceeds the usual inline threshold used by Stata for strL storage, repeated to be certain: a much longer string that exceeds the usual inline threshold used by Stata for strL storage";

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures_dir() -> Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests/fixtures/sdp1")
    }

    #[test]
    fn every_generated_fixture_round_trips_and_validates() {
        for (name, bytes) in generable(None).unwrap() {
            validate(&bytes).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let page = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(page.row0, 0, "{name}");
            assert_eq!(page.seq, 1, "{name}");
        }
    }

    /// `tests/fixtures/sdp1/README.md` and this encoder are two statements of
    /// the same layout, and W02b will make a third. They agree or the fixture is
    /// worthless.
    #[test]
    fn the_generator_reproduces_the_committed_fixtures() {
        let dir = fixtures_dir();
        if !dir.is_dir() {
            return; // not committed yet
        }
        for (name, want) in generable(None).unwrap() {
            let Ok(got) = std::fs::read(dir.join(name)) else {
                continue;
            };
            assert_eq!(got, want, "{name} differs from the encoder's output");
        }
    }

    /// Including the one this tool cannot regenerate: `auto_40x12.bin` holds
    /// Stata's own formatted cells, and validating it is how CI keeps a hold on
    /// bytes nothing in the repository can rebuild.
    #[test]
    fn every_committed_fixture_satisfies_the_layout_rules() {
        let dir = fixtures_dir();
        if !dir.is_dir() {
            return;
        }
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("bin") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            validate(&bytes).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
            seen += 1;
        }
        assert!(
            seen >= 3,
            "expected the three committed fixtures, saw {seen}"
        );
    }

    #[test]
    fn the_auto_edit_page_decodes_to_the_auto_data() {
        let page = decode(&auto_edit_page().unwrap()).unwrap();
        assert_eq!(page.nrows, 40);
        assert_eq!(page.cols.len(), 12);
        assert_eq!(page.state, 17, "spec §13's D17, not 0");

        let DecodedColumn::Text { idx, values } = &page.cols[0] else {
            panic!("column 0 must be the make string");
        };
        assert_eq!(*idx, 0);
        assert_eq!(values[0], "AMC Concord");
        assert_eq!(values[39], "Olds Starfire");
        assert_eq!(values.len(), 40);

        let DecodedColumn::Num { bits, tags, .. } = &page.cols[1] else {
            panic!("column 1 must be price");
        };
        assert_eq!(f64::from_bits(bits[0]), 4099.0);
        assert_eq!(tags[0], TAG_PRESENT);

        // rep78 is missing at observations 3 and 7 (0-based 2 and 6), and the
        // value on the wire is the sentinel itself, not a zero.
        let DecodedColumn::Num { bits, tags, .. } = &page.cols[3] else {
            panic!("column 3 must be rep78");
        };
        assert_eq!(tags[2], 0, "observation 3 of rep78 is `.`");
        assert_eq!(bits[2], F64_MISS_BITS);
        assert_eq!(tags[6], 0);
        assert_eq!(f64::from_bits(bits[0]), 3.0);

        let DecodedColumn::Num { bits, .. } = &page.cols[10] else {
            panic!("column 10 must be gear_ratio");
        };
        assert_eq!(f64::from_bits(bits[0]), 3.579_999_923_706_054_7_f64);
        assert!((f64::from_bits(bits[0]) - 3.58_f64).abs() > 0.0);
    }

    #[test]
    fn the_strl_page_carries_a_binary_bitmap() {
        let page = decode(&strl_edit_page().unwrap()).unwrap();
        assert_eq!(page.nrows, 3);
        let DecodedColumn::Blob { values, binary, .. } = &page.cols[0] else {
            panic!("column 0 must be the strL");
        };
        assert_eq!(values[0], b"short");
        assert_eq!(values[1].len(), 208);
        assert!(values[2].is_empty(), "the null strL is the empty string");
        assert_eq!(binary, &[false, false, false]);
    }

    /// The bitmap is LSB-first within each byte, so row 0 is bit 0 of byte 0.
    #[test]
    fn the_binary_bitmap_indexes_by_row() {
        let flags = [false, true, false, false, false, false, false, false, true];
        let cols = vec![Column::Blob {
            idx: 0,
            values: (0..9u8).map(|i| vec![i]).collect(),
            binary: flags.to_vec(),
        }];
        let bytes = encode(1, 0, 1, &cols).unwrap();
        validate(&bytes).unwrap();
        let DecodedColumn::Blob { binary, .. } = &decode(&bytes).unwrap().cols[0] else {
            panic!()
        };
        assert_eq!(binary, &flags);
    }

    /// §2.1: the payload must start 8-aligned whatever the header happens to
    /// spell, or `new Float64Array(buf, byteOffset, n)` throws.
    #[test]
    fn the_header_is_padded_until_the_payload_is_eight_aligned() {
        // Row counts chosen so the offsets in the header change digit count.
        for nrows in [1usize, 9, 10, 99, 100] {
            let cols = vec![
                Column::Text {
                    idx: 0,
                    values: (0..nrows).map(|i| format!("r{i}")).collect(),
                },
                Column::Num {
                    idx: 1,
                    bits: vec![0; nrows],
                    tags: vec![TAG_PRESENT; nrows],
                },
            ];
            let bytes = encode(17, 0, 1, &cols).unwrap();
            validate(&bytes).unwrap_or_else(|e| panic!("nrows={nrows}: {e:#}"));
            let header_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
            assert_eq!((8 + header_len) % 8, 0, "nrows={nrows}");
            // The padding is spaces, and JSON tolerates trailing whitespace.
            let raw = &bytes[8..8 + header_len];
            assert!(serde_json::from_slice::<serde_json::Value>(raw).is_ok());
            assert!(raw.iter().rev().take_while(|b| **b == b' ').count() < 8);
        }
    }

    /// §2.2: a `num` column's f64 region is 8-aligned in the file, not just
    /// relative to the payload.
    #[test]
    fn num_regions_are_eight_aligned_in_the_file() {
        let bytes = auto_edit_page().unwrap();
        let header_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let header: serde_json::Value = serde_json::from_slice(&bytes[8..8 + header_len]).unwrap();
        for c in header["cols"].as_array().unwrap() {
            if c["kind"] == "num" {
                let abs = 8 + header_len + c["off"].as_u64().unwrap() as usize;
                assert_eq!(abs % 8, 0, "column {} is not 8-aligned", c["idx"]);
            }
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let (a, b) = (generable(None).unwrap(), generable(None).unwrap());
        assert_eq!(a, b);
    }

    #[test]
    fn a_truncated_or_foreign_buffer_is_rejected_rather_than_misread() {
        let good = strl_edit_page().unwrap();
        assert!(decode(&good[..4]).is_err());
        assert!(decode(&good[..good.len() - 1]).is_err());
        let mut wrong_magic = good.clone();
        wrong_magic[3] = b'2';
        assert!(
            decode(&wrong_magic).is_err(),
            "SDP2 must not decode as SDP1"
        );
    }

    /// The validator has to reject, or it is decoration on top of the encoder.
    #[test]
    fn the_validator_rejects_a_broken_page() {
        let good = auto_edit_page().unwrap();
        validate(&good).unwrap();

        // Trailing slack: §2.2 forbids it, and it is how a length bug hides.
        let mut padded = good.clone();
        padded.push(0);
        assert!(validate(&padded).is_err());

        // A tag that contradicts its own payload.
        let mut lying = good.clone();
        let header_len = u32::from_le_bytes(lying[4..8].try_into().unwrap()) as usize;
        let header: serde_json::Value = serde_json::from_slice(&lying[8..8 + header_len]).unwrap();
        let aux_off = header["cols"][1]["aux_off"].as_u64().unwrap() as usize;
        lying[8 + header_len + aux_off] = 3; // claims `.c`, payload says 4099.0
        let err = validate(&lying).unwrap_err();
        assert!(format!("{err:#}").contains("tag 3"), "{err:#}");
    }

    #[test]
    fn missing_bits_follow_the_sentinel_ladder() {
        assert_eq!(missing_bits(0), 0x7FE0_0000_0000_0000);
        assert_eq!(missing_bits(1), 0x7FE0_0100_0000_0000);
        assert_eq!(missing_bits(26), 0x7FE0_1A00_0000_0000);
        // The sentinels are finite normal doubles, not NaNs (04 §2.3).
        for tag in 0..=26u8 {
            let v = f64::from_bits(missing_bits(tag));
            assert!(v.is_finite() && v > 0.0, "tag {tag} is not a finite double");
        }
    }
}
