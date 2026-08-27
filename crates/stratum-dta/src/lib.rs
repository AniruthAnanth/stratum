//! Stata `.dta` interoperability: read and write release 117, 118 and 119.
//!
//! This crate is the boundary between a file somebody downloaded and the
//! engine's own data structures, so it is written to two rules that pull in
//! opposite directions:
//!
//! 1. **The bytes are hostile until proven otherwise.** Every length and every
//!    offset taken from the file is checked arithmetic against the *measured*
//!    file length before it indexes, allocates or seeks. `debug_assert!` is not
//!    a check — the shipping profile compiles it away. See
//!    `docs/THREAT_MODEL.md`, which this unit owns and which gated this reader.
//! 2. **Loading is the slowest thing a researcher does.** `.dta` is row-major
//!    and Stratum is column-major, and `04` §12.1 calls that transpose the whole
//!    cost of `use`. So the source is memory-mapped rather than copied, and the
//!    transpose is [`stratum_data::Column::from_row_major`] — W02's chunked,
//!    parallel, exactly-one-copy-per-cell ingest.
//!
//! # Where the numbers come from
//!
//! Nothing here is remembered. The per-release widths, the `strL` key splits,
//! the `<lbl>` length arithmetic and the two interop traps are `04` §§9–11, and
//! `04` recorded the file and the command that produced each one. The
//! missing-value encoding is [`stratum_core::missing`] and is never re-spelled
//! here. Where a committed fixture pins a case, the test cites the fixture.
//!
//! # The two traps that shape the whole reader
//!
//! Both were found in StataCorp's own shipped `auto.dta` (`04` §0.2):
//!
//! * **`map[13]` is not the file size.** The shipped `auto.dta` has one trailing
//!   `0x0A` byte. A reader that asserts `map[13] == len` rejects the single
//!   most-opened dataset in the product. The rule is `<=`, and the excess is
//!   [`ReadReport::trailing_bytes`] plus a [`ReadWarning::TrailingBytes`].
//! * **Stata writes uninitialised memory into fixed-width padding.** Every
//!   fixed-width field — and every `str#` data cell — is read as *bytes up to
//!   the first NUL, remainder discarded* ([`codepage::until_nul`]). It also
//!   means **byte-identical round-trip is impossible in general**, which is why
//!   [`canonical_eq`] exists and why byte identity is reported by
//!   [`byte_diff`] rather than asserted (`04` §11.3).
//!
//! # Reading
//!
//! ```no_run
//! let ds = stratum_dta::read_dta("auto.dta")?;
//! assert_eq!(ds.n_obs(), 74);
//! assert_eq!(ds.var("price").unwrap().ty, stratum_core::StorageType::Int);
//! assert_eq!(ds.f64_at("price", 0), Some(4099.0));
//! # Ok::<(), stratum_dta::DtaError>(())
//! ```
//!
//! # The `Dataset` ↔ [`Frame`](stratum_data::Frame) bridge
//!
//! Both directions ship, and both are lossless:
//!
//! * [`Dataset::from_frame`] projects a live `Frame` into the `.dta` model —
//!   the `save` path. Everything a `.dta` file can hold is readable from a
//!   `Frame`.
//! * [`Dataset::into_frame`] loads a read `Dataset` into a fresh `Frame` —
//!   the `use` path. It exists only since `stratum-data` grew the four
//!   accessors this crate escalated for (`Frame::var_mut`,
//!   `Frame::set_sort_state`, `StrLCol::from_rows`, `ColMut::set_strl`);
//!   an earlier version of this header records why a lossy `to_frame` was
//!   withheld until then.
//!
//! "Lossless" is enforced, not hoped. Every fact the file carries either
//! lands in the `Frame` — storage (fixed-width columns *move*, zero-copy),
//! display formats, variable labels, value-label attachments and tables,
//! characteristics, the sort list, `strL` content with its binary flags, and
//! the dataset label — or comes back to the caller in [`FrameBridge`] (the
//! `<timestamp>`, which a `Frame` has no slot for and the writer needs back
//! verbatim). Anything the engine cannot hold — a name `Frame` refuses, a
//! display format that does not survive the parsed representation, a sort
//! list naming no variable — is a typed [`DtaError`], never a silent drop or
//! a guessed rename: turning a loud refusal into quiet data loss is the one
//! bug class this bridge was held back to avoid.

#![warn(missing_docs)]

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use stratum_core::StorageType;
use stratum_data::{CharTable, Column, Frame, StrLCol, ValueLabel, ValueLabelSet};
use stratum_proto::VarIdx;

pub mod codepage;
pub mod gso;
pub mod reader;
pub mod spec;
pub mod writer;

pub use codepage::Encoding;
pub use reader::{probe_dta, read_dta, read_dta_bytes, read_dta_with, DtaProbe, DtaReadOptions};
pub use spec::{ByteOrder, Release};
pub use writer::{write_dta, write_dta_to_vec, DtaWriteOptions};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong reading or writing a `.dta` file.
///
/// Every variant that can be produced by *reading* names a byte offset or a
/// block, because "this file is corrupt" is not a diagnostic a user can act on
/// and "the varnames block claims a 4-byte stride, we expect 129, at offset
/// 358" is.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DtaError {
    /// The file could not be opened, mapped, read or written.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// The file does not begin `<stata_dta>`.
    #[error("not a Stata .dta file: it does not begin with <stata_dta>")]
    NotDta,

    /// A read ran past the end of the buffer it was given.
    #[error("truncated at byte {at}: needed {need} more bytes, {have} remain")]
    Truncated {
        /// Byte offset the read started at.
        at: u64,
        /// Bytes the read wanted.
        need: u64,
        /// Bytes actually left.
        have: u64,
    },

    /// A tag was not where the map said it would be.
    #[error("expected tag <{expected}> at byte {at}")]
    Tag {
        /// Byte offset the tag should have started at.
        at: u64,
        /// The tag name, without angle brackets.
        expected: &'static str,
    },

    /// `<release>` held something that is not a release number at all.
    #[error("unrecognised .dta release tag {0:?}")]
    BadRelease(String),

    /// A real Stata release Stratum does not read. 113–116 are the pre-117
    /// untagged layout (use Stata's `saveold`); 120 needs alias variables.
    #[error("`.dta` release {0} is not supported; v1 reads 117, 118 and 119")]
    UnsupportedRelease(u16),

    /// `<byteorder>` was neither `LSF` nor `MSF`.
    #[error("unrecognised byte order {0:?}; expected LSF or MSF")]
    BadByteOrder(String),

    /// The 14-entry map is not usable as an index.
    #[error("map entry {index} is {value}, which {why}")]
    Map {
        /// Which of the 14 entries.
        index: usize,
        /// What it said.
        value: u64,
        /// Why that cannot be true.
        why: &'static str,
    },

    /// A fixed-width block's stride, derived from the map, is not the stride
    /// this release's [`spec::ReleaseSpec`] declares.
    ///
    /// This is the check that makes a future Stata release fail loudly instead
    /// of silently returning garbage varnames (`04` §9.4 step 4).
    #[error("block <{block}>: file implies {derived} bytes per entry, release expects {expected}")]
    BlockWidth {
        /// Block name, without angle brackets.
        block: &'static str,
        /// Stride the file implies.
        derived: u64,
        /// Stride the release declares.
        expected: u64,
    },

    /// A fixed-width block's payload is not a whole number of entries.
    #[error("block <{block}>: {payload}-byte payload is not a whole number of entries")]
    BlockLen {
        /// Block name, without angle brackets.
        block: &'static str,
        /// The payload length.
        payload: u64,
    },

    /// `N × row_width` disagrees with the `<data>` payload. This is the check
    /// that makes a 2^40-observation header cost a comparison.
    #[error("<data>: header implies {declared} bytes, the block holds {actual}")]
    DataLen {
        /// `N × row_width`.
        declared: u64,
        /// Bytes actually present.
        actual: u64,
    },

    /// A `<variable_types>` entry is not a type.
    #[error("variable {var}: {code} is not a storage type code")]
    TypeCode {
        /// 0-based variable index.
        var: u32,
        /// The code read.
        code: u16,
    },

    /// The row width overflowed. Reachable only from a hostile type vector.
    #[error("the sum of the storage widths overflows a 64-bit row width")]
    RowWidthOverflow,

    /// `nobs`, `nvar` or a product of them does not fit the machine.
    #[error("{what} of {value} is beyond what this build can address")]
    TooLarge {
        /// Which quantity.
        what: &'static str,
        /// The value.
        value: u64,
    },

    /// A `<strls>` record ran off the end of the block.
    #[error("<strls> at byte {at}: needed {need} bytes, {have} remain")]
    GsoTruncated {
        /// Byte offset in the file.
        at: u64,
        /// Bytes wanted.
        need: u64,
        /// Bytes left in the block.
        have: u64,
    },

    /// A `<strls>` record does not begin `GSO`.
    #[error("<strls> at byte {at}: expected a GSO record header")]
    GsoTag {
        /// Byte offset in the file.
        at: u64,
    },

    /// A GSO record claimed the reserved `(0, o)` or `(v, 0)` key, which means
    /// "the empty string" and must emit no record.
    #[error("<strls> at byte {at}: a GSO record claimed the reserved (0,0) key")]
    GsoReservedKey {
        /// Byte offset in the file.
        at: u64,
    },

    /// A GSO record's `v` names no variable.
    #[error("<strls> at byte {at}: GSO v={v} but the file has {n_vars} variables")]
    GsoVar {
        /// Byte offset in the file.
        at: u64,
        /// The variable index claimed.
        v: u32,
        /// Variables the file declares.
        n_vars: u32,
    },

    /// A GSO record's type byte is neither 129 (binary) nor 130 (string).
    #[error("<strls> at byte {at}: GSO type {ty} is neither 129 nor 130")]
    GsoType {
        /// Byte offset in the file.
        at: u64,
        /// The type byte.
        ty: u8,
    },

    /// A `strL` cell names a GSO record that does not exist.
    #[error("variable {var} observation {obs}: strL key (v={v}, o={o}) names no GSO record")]
    GsoMissing {
        /// 0-based variable index.
        var: u32,
        /// 0-based observation.
        obs: u64,
        /// Unpacked `v`.
        v: u32,
        /// Unpacked `o`.
        o: u64,
    },

    /// A `(v,o)` pair does not fit this release's key packing.
    #[error("release {release} cannot address the strL key (v={v}, o={o})")]
    GsoKeyRange {
        /// The release number.
        release: u16,
        /// The variable index.
        v: u32,
        /// The observation.
        o: u64,
    },

    /// A `<lbl>` record's declared length disagrees with its own contents.
    #[error("<value_labels> at byte {at}: declared length {declared}, contents imply {computed}")]
    LabelLen {
        /// Byte offset in the file.
        at: u64,
        /// The declared `len`.
        declared: u64,
        /// What `8 + 8n + txtlen` comes to.
        computed: u64,
    },

    /// A value-label text offset points outside the record's text arena.
    #[error("value label {table:?} entry {index}: text offset {offset} is outside {txtlen} bytes")]
    LabelOffset {
        /// Table name.
        table: String,
        /// 0-based entry index.
        index: u32,
        /// The offset read.
        offset: u32,
        /// The arena length.
        txtlen: u32,
    },

    /// A `<ch>` record's declared length cannot hold its two fixed name fields.
    #[error("<characteristics> at byte {at}: declared length {declared} is below the {min} needed for its name fields")]
    CharLen {
        /// Byte offset in the file.
        at: u64,
        /// The declared `len`.
        declared: u64,
        /// `2 × varname_len`.
        min: u64,
    },

    /// The writer refuses to write a name a reader could not use.
    #[error("cannot write variable {index} named {name:?}: {why}")]
    BadName {
        /// 0-based variable index.
        index: u32,
        /// The offending name.
        name: String,
        /// What is wrong with it.
        why: &'static str,
    },

    /// Text does not fit the target release's field, or its codepage.
    #[error(
        "cannot write {what}: {value:?} is {len} bytes, the limit for release {release} is {limit}"
    )]
    TooLong {
        /// What was being written.
        what: String,
        /// The value.
        value: String,
        /// Its byte length.
        len: usize,
        /// The field's capacity, terminator included.
        limit: usize,
        /// The target release.
        release: u16,
    },

    /// A character has no encoding in the target codepage.
    #[error("cannot write {what}: {ch:?} has no representation in {encoding}")]
    NotRepresentable {
        /// What was being written.
        what: String,
        /// The offending character.
        ch: char,
        /// The codepage's name.
        encoding: &'static str,
    },

    /// The chosen release cannot hold this dataset's shape.
    #[error("release {release} cannot hold this dataset: {why}")]
    ReleaseTooNarrow {
        /// The requested release.
        release: u16,
        /// What does not fit.
        why: String,
    },

    /// [`Dataset::into_frame`] refused a variable the engine's [`Frame`]
    /// cannot hold: the name is one `Frame` rejects (invalid — kept verbatim
    /// on read, with a [`ReadWarning::InvalidName`] — or a duplicate). Loud on
    /// purpose: renaming or skipping the variable would be the quiet data loss
    /// the bridge was held back to avoid (crate docs).
    #[error("variable {index} ({name:?}) cannot be loaded into a frame: {source}")]
    FrameVar {
        /// 0-based variable index.
        index: u32,
        /// The name as the file carries it.
        name: String,
        /// What the frame refused.
        #[source]
        source: stratum_data::FrameError,
    },

    /// [`Dataset::into_frame`] met a display format that does not survive the
    /// engine's parsed representation — `StataFormat::parse` rejects it, or
    /// parse-then-render is not the identity for this spelling. A `Frame`
    /// stores formats parsed (`04` §8), so accepting it would silently rewrite
    /// the format on the next `save`.
    #[error(
        "variable {index} ({name:?}): display format {format:?} cannot cross into a frame: {why}"
    )]
    FrameFormat {
        /// 0-based variable index.
        index: u32,
        /// The variable's name.
        name: String,
        /// The format, exactly as the file spells it.
        format: String,
        /// The parse error, or the render disagreement.
        why: String,
    },

    /// [`Dataset::into_frame`] met a sort list naming a variable position the
    /// dataset does not have. The reader filters these (with a
    /// [`ReadWarning::InvalidSortList`]), so this is reachable only from a
    /// hand-built [`Dataset`].
    #[error("sortlist names variable position {entry}, which does not exist")]
    FrameSort {
        /// The offending 0-based entry.
        entry: u32,
    },

    /// Two variables share a name, or a column and its metadata disagree.
    #[error("{0}")]
    Inconsistent(String),
}

impl DtaError {
    /// The Stata return code `use`/`save` would report.
    ///
    /// 601 is measured (`tests/golden/stata18/errors.log` line 97–98:
    /// "file /no/such/file.dta not found", `rc = 601`). **610 is not
    /// measured** — the golden capture has no malformed-`.dta` case, so it
    /// comes from Stata's documented return codes and is marked as such rather
    /// than presented as evidence.
    #[must_use]
    pub fn rc(&self) -> u16 {
        match self {
            DtaError::Io(e) if e.kind() == std::io::ErrorKind::NotFound => 601,
            DtaError::Io(_) => 603,
            _ => 610,
        }
    }
}

// ---------------------------------------------------------------------------
// The read report
// ---------------------------------------------------------------------------

/// Something about the file that the user should be told but that is not a
/// reason to refuse to open it.
///
/// The distinction is the product decision in `04` §9.4: a file with one odd
/// variable name opens, with a note. A file whose map cannot be used as an index
/// does not open at all.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReadWarning {
    /// `map[13]` is below the file length. StataCorp's shipped `auto.dta` does
    /// this (`04` §0.2 trap 1).
    TrailingBytes {
        /// What `map[13]` said.
        declared_eof: u64,
        /// What the file actually is.
        file_len: u64,
    },
    /// Bytes that did not decode, replaced with U+FFFD.
    UndecodableText {
        /// The encoding that was used.
        encoding: String,
        /// How many bytes were replaced.
        replaced: u64,
    },
    /// A variable name Stata itself would not accept. Kept verbatim: refusing
    /// to open a dataset because one name is odd is worse than opening it.
    InvalidName {
        /// 0-based variable index.
        var: u32,
        /// The name as read.
        name: String,
    },
    /// Two variables share a name. The name index resolves to the first.
    DuplicateName {
        /// 0-based index of the later variable.
        var: u32,
        /// The shared name.
        name: String,
    },
    /// A variable names a value-label table the file does not define.
    MissingValueLabel {
        /// 0-based variable index.
        var: u32,
        /// The table it named.
        table: String,
    },
    /// Two GSO records shared a `(v,o)` key. The first was kept.
    DuplicateGso {
        /// How many later records were discarded.
        count: u32,
    },
    /// Values in a float or double column violated Invariant M (`04` §2.5) and
    /// were canonicalised to `.` on load.
    NonCanonicalNumeric {
        /// 0-based variable index.
        var: u32,
        /// How many observations were rewritten.
        rows: u64,
    },
    /// The `sortlist` named something that is not a variable of this file.
    InvalidSortList {
        /// The 1-based index it held.
        entry: u32,
    },
}

impl fmt::Display for ReadWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadWarning::TrailingBytes {
                declared_eof,
                file_len,
            } => write!(
                f,
                "{} byte(s) after the declared end of file ({declared_eof} of {file_len}); ignored",
                file_len - declared_eof
            ),
            ReadWarning::UndecodableText { encoding, replaced } => {
                write!(f, "{replaced} byte(s) could not be decoded as {encoding}")
            }
            ReadWarning::InvalidName { var, name } => {
                write!(
                    f,
                    "variable {var} is named {name:?}, which Stata would reject"
                )
            }
            ReadWarning::DuplicateName { var, name } => {
                write!(f, "variable {var} repeats the name {name:?}")
            }
            ReadWarning::MissingValueLabel { var, table } => {
                write!(
                    f,
                    "variable {var} names value label {table:?}, which this file does not define"
                )
            }
            ReadWarning::DuplicateGso { count } => {
                write!(
                    f,
                    "{count} strL record(s) repeated a (v,o) key; the first was kept"
                )
            }
            ReadWarning::NonCanonicalNumeric { var, rows } => write!(
                f,
                "variable {var}: {rows} value(s) were not valid Stata numbers and were read as `.`"
            ),
            ReadWarning::InvalidSortList { entry } => {
                write!(f, "sortlist names variable {entry}, which does not exist")
            }
        }
    }
}

/// What one read did, and what it noticed.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReadReport {
    /// The release the file declared.
    pub release: u16,
    /// The byte order the file declared.
    pub byte_order: Option<ByteOrder>,
    /// The encoding text was decoded with. For 117 this is an assumption the UI
    /// should surface, not a fact from the file (`04` §9.4).
    pub encoding: Encoding,
    /// The file's real length.
    pub file_len: u64,
    /// `map[13]`.
    pub declared_eof: u64,
    /// `file_len - map[13]`. One, for StataCorp's shipped `auto.dta`.
    pub trailing_bytes: u64,
    /// Bytes replaced with U+FFFD across every text field.
    pub replacement_chars: u64,
    /// GSO records indexed.
    pub gso_records: u64,
    /// True when the source was memory-mapped rather than copied to the heap.
    pub mapped: bool,
    /// Everything the user should be told.
    pub warnings: Vec<ReadWarning>,
}

impl ReadReport {
    /// Does the report hold a warning of this shape?
    #[must_use]
    pub fn has<F: Fn(&ReadWarning) -> bool>(&self, pred: F) -> bool {
        self.warnings.iter().any(pred)
    }
}

// ---------------------------------------------------------------------------
// The dataset
// ---------------------------------------------------------------------------

/// One variable's metadata, exactly as `.dta` carries it.
///
/// `format` is the **Stata spelling**, not a parsed
/// [`StataFormat`](stratum_core::fmt::StataFormat). Round-trip fidelity is this
/// unit's acceptance and `parse` ∘ `render` is not guaranteed to be the identity
/// for every format string Stata will accept, so the file's own bytes are what
/// is stored and what is written back. [`DtaVar::parsed_format`] parses on
/// demand for a caller that wants the structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DtaVar {
    /// The variable name, decoded and NUL-trimmed.
    pub name: String,
    /// Storage type, from `<variable_types>`.
    pub ty: StorageType,
    /// Display format, e.g. `%8.0gc`.
    pub format: String,
    /// Variable label. Empty means none.
    pub label: String,
    /// Name of a table in [`Dataset::value_labels`].
    pub value_label: Option<String>,
}

impl DtaVar {
    /// A variable with this release's default format for its type and no label.
    #[must_use]
    pub fn new(name: &str, ty: StorageType) -> Self {
        Self {
            name: name.to_owned(),
            ty,
            format: stratum_core::types::default_format(ty).to_owned(),
            label: String::new(),
            value_label: None,
        }
    }

    /// The display format, parsed.
    ///
    /// # Errors
    ///
    /// The `stratum_core::fmt` parse error, for a format string Stata wrote
    /// that this engine does not yet understand.
    pub fn parsed_format(&self) -> Result<stratum_core::fmt::StataFormat, impl std::error::Error> {
        stratum_core::fmt::StataFormat::parse(&self.format)
    }
}

/// A `strL` column's storage: one arena, one offset vector, one binary bitmap.
///
/// The *file-side* staging of a `strL` column. [`stratum_data::StrLCol`] is
/// the engine's chunked column and has a bulk constructor
/// ([`StrLCol::from_rows`]) that [`Dataset::into_frame`] feeds from this type;
/// this type stays because [`Dataset`] models the file, not the engine — one
/// arena in file order, `Eq`, no chunking.
///
/// Appending is amortised O(bytes), and so is the bridge into `StrLCol`. The
/// per-cell alternative — `ColMut::set_bytes` per row — splices the chunk
/// arena and rewrites every following offset on **every** cell, which is
/// O(bytes²) for a column load and would put a 200 MB `strL` column past the
/// point of usability.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StrLData {
    /// `len + 1` entries, monotonic, into `bytes`.
    offsets: Vec<u64>,
    /// The arena. No terminators are stored.
    bytes: Vec<u8>,
    /// One bit per observation: GSO type 129 rather than 130.
    binary: Vec<u64>,
    len: u64,
}

impl StrLData {
    /// Room for `rows` observations and `bytes` of content.
    #[must_use]
    pub fn with_capacity(rows: usize, bytes: usize) -> Self {
        let mut offsets = Vec::with_capacity(rows + 1);
        offsets.push(0);
        Self {
            offsets,
            bytes: Vec::with_capacity(bytes),
            binary: Vec::with_capacity(rows.div_ceil(64)),
            len: 0,
        }
    }

    /// `rows` empty, text-typed observations.
    #[must_use]
    pub fn empty(rows: u64) -> Self {
        Self {
            offsets: vec![0; rows as usize + 1],
            bytes: Vec::new(),
            binary: vec![0; (rows as usize).div_ceil(64)],
            len: rows,
        }
    }

    /// Append one observation. Cells must be appended in observation order.
    pub fn push(&mut self, content: &[u8], binary: bool) {
        self.bytes.extend_from_slice(content);
        self.offsets.push(self.bytes.len() as u64);
        let i = self.len as usize;
        if i / 64 >= self.binary.len() {
            self.binary.push(0);
        }
        if binary {
            self.binary[i / 64] |= 1u64 << (i % 64);
        }
        self.len += 1;
    }

    /// Observations.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True when there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bytes at `row`, or `&[]` past the end.
    #[must_use]
    pub fn get(&self, row: u64) -> &[u8] {
        let i = row as usize;
        if i + 1 >= self.offsets.len() {
            return &[];
        }
        &self.bytes[self.offsets[i] as usize..self.offsets[i + 1] as usize]
    }

    /// Is `row` a binary blob (GSO type 129) rather than text (130)?
    #[must_use]
    pub fn is_binary(&self, row: u64) -> bool {
        let i = row as usize;
        self.binary
            .get(i / 64)
            .is_some_and(|w| w & (1u64 << (i % 64)) != 0)
    }

    /// Arena bytes held, for resident-memory accounting.
    #[must_use]
    pub fn heap_bytes(&self) -> u64 {
        (self.bytes.len() + self.offsets.len() * 8 + self.binary.len() * 8) as u64
    }
}

/// One variable's storage as this crate holds it.
#[derive(Clone, Debug, PartialEq)]
pub enum DtaColumn {
    /// Every storage type except `strL`, held as W02's chunked
    /// [`Column`] so that the bulk row-major ingest and the copy-on-write
    /// sharing are the engine's, not a second implementation.
    Fixed(Column),
    /// `strL`, held here — see [`StrLData`].
    StrL(StrLData),
}

impl DtaColumn {
    /// The declared storage type.
    #[must_use]
    pub fn storage_type(&self) -> StorageType {
        match self {
            DtaColumn::Fixed(c) => c.storage_type(),
            DtaColumn::StrL(_) => StorageType::StrL,
        }
    }

    /// Observations.
    #[must_use]
    pub fn len(&self) -> u64 {
        match self {
            DtaColumn::Fixed(c) => c.len(),
            DtaColumn::StrL(s) => s.len(),
        }
    }

    /// True when there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// One numeric observation, widened. `None` on a string column.
    #[must_use]
    pub fn get_f64(&self, row: u64) -> Option<f64> {
        match self {
            DtaColumn::Fixed(c) => c.get_f64(row),
            DtaColumn::StrL(_) => None,
        }
    }

    /// One string observation. `None` on a numeric column.
    #[must_use]
    pub fn get_bytes(&self, row: u64) -> Option<&[u8]> {
        match self {
            DtaColumn::Fixed(c) => c.get_bytes(row),
            DtaColumn::StrL(s) => Some(s.get(row)),
        }
    }

    /// GSO type 129 rather than 130. False for everything but a `strL` blob.
    #[must_use]
    pub fn is_binary(&self, row: u64) -> bool {
        match self {
            DtaColumn::Fixed(_) => false,
            DtaColumn::StrL(s) => s.is_binary(row),
        }
    }
}

/// A `.dta` file's entire contents, in memory.
///
/// The read path produces one of these; the write path consumes one. It is a
/// faithful model of the *file*, not of the engine's `Frame`: the sort list is
/// what the file recorded, the display formats are the file's own spellings,
/// and the characteristics are in the file's own order.
#[derive(Clone, Debug)]
pub struct Dataset {
    /// The release read, or the release that will be written.
    pub release: Release,
    /// The byte order read. Writing is always `Lsf`.
    pub byte_order: ByteOrder,
    /// The dataset label.
    pub label: String,
    /// The `<timestamp>` field, verbatim, e.g. `13 Apr 2022 17:45`.
    pub timestamp: String,
    /// Variables, in file order.
    pub vars: Vec<DtaVar>,
    /// Columns, parallel to [`vars`](Self::vars).
    pub cols: Vec<DtaColumn>,
    /// Value-label tables, keyed by name.
    pub value_labels: ValueLabelSet,
    /// Characteristics, in file order. Notes are a projection over these
    /// (`04` §4.3) and are reached with [`Dataset::notes`].
    pub chars: CharTable,
    /// `sortlist`, as **0-based** variable indices in priority order. The file
    /// stores them 1-based and 0-terminated.
    pub sortlist: Vec<u32>,
    n_obs: u64,
    report: ReadReport,
}

impl Dataset {
    /// An empty dataset for `release`.
    #[must_use]
    pub fn new(release: Release) -> Self {
        Self {
            release,
            byte_order: ByteOrder::Lsf,
            label: String::new(),
            timestamp: String::new(),
            vars: Vec::new(),
            cols: Vec::new(),
            value_labels: ValueLabelSet::new(),
            chars: CharTable::new(),
            sortlist: Vec::new(),
            n_obs: 0,
            report: ReadReport::default(),
        }
    }

    /// Observations.
    #[must_use]
    pub fn n_obs(&self) -> u64 {
        self.n_obs
    }

    /// Variables.
    #[must_use]
    pub fn n_vars(&self) -> u32 {
        self.vars.len() as u32
    }

    /// Set the observation count. Only meaningful before columns are added; a
    /// dataset with columns takes its count from them.
    pub fn set_n_obs(&mut self, n: u64) {
        self.n_obs = n;
    }

    /// Append a variable and its column.
    ///
    /// # Errors
    ///
    /// [`DtaError::Inconsistent`] when the column's length or type disagrees
    /// with the dataset or the variable.
    pub fn push_var(&mut self, var: DtaVar, col: DtaColumn) -> Result<(), DtaError> {
        if col.storage_type() != var.ty {
            return Err(DtaError::Inconsistent(format!(
                "variable {:?} is {:?} but its column is {:?}",
                var.name,
                var.ty,
                col.storage_type()
            )));
        }
        if self.vars.is_empty() {
            self.n_obs = col.len();
        } else if col.len() != self.n_obs {
            return Err(DtaError::Inconsistent(format!(
                "variable {:?} has {} observations, the dataset has {}",
                var.name,
                col.len(),
                self.n_obs
            )));
        }
        self.vars.push(var);
        self.cols.push(col);
        Ok(())
    }

    /// 0-based position of `name`, or `None`.
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.vars.iter().position(|v| v.name == name)
    }

    /// Metadata for `name`.
    #[must_use]
    pub fn var(&self, name: &str) -> Option<&DtaVar> {
        self.index_of(name).map(|i| &self.vars[i])
    }

    /// Storage for `name`.
    #[must_use]
    pub fn col(&self, name: &str) -> Option<&DtaColumn> {
        self.index_of(name).map(|i| &self.cols[i])
    }

    /// One numeric cell, widened.
    #[must_use]
    pub fn f64_at(&self, name: &str, row: u64) -> Option<f64> {
        self.col(name).and_then(|c| c.get_f64(row))
    }

    /// One string cell, as bytes.
    #[must_use]
    pub fn bytes_at(&self, name: &str, row: u64) -> Option<&[u8]> {
        self.col(name).and_then(|c| c.get_bytes(row))
    }

    /// One string cell, lossily as text. For display and for tests.
    #[must_use]
    pub fn str_at(&self, name: &str, row: u64) -> Option<std::borrow::Cow<'_, str>> {
        self.bytes_at(name, row).map(String::from_utf8_lossy)
    }

    /// The notes of `owner` (`_dta` for the dataset), read out of `note0..noteN`.
    #[must_use]
    pub fn notes(&self, owner: &str) -> Vec<&str> {
        self.chars.notes(owner)
    }

    /// Variable names of the sort list, in priority order.
    #[must_use]
    pub fn sorted_by(&self) -> Vec<&str> {
        self.sortlist
            .iter()
            .filter_map(|&i| self.vars.get(i as usize))
            .map(|v| v.name.as_str())
            .collect()
    }

    /// What the read noticed. Default-valued on a dataset that was built rather
    /// than read.
    #[must_use]
    pub fn read_report(&self) -> &ReadReport {
        &self.report
    }

    /// Take the columns, for a caller assembling a [`Frame`].
    #[must_use]
    pub fn into_columns(self) -> Vec<DtaColumn> {
        self.cols
    }

    pub(crate) fn set_report(&mut self, report: ReadReport) {
        self.report = report;
    }

    /// Project a live [`Frame`] into the `.dta` model, ready to write.
    ///
    /// Complete in this direction: everything a `.dta` file carries is readable
    /// from a `Frame`. `release` is what the caller intends to write, because
    /// the `strL` key packing depends on it.
    ///
    /// Provenance is deliberately dropped — `04`/W02 are explicit that it lives
    /// in the `.workspace` sidecar and never in a portable file.
    ///
    /// # Errors
    ///
    /// [`DtaError::Inconsistent`] if a column and its `Variable` disagree.
    pub fn from_frame(frame: &Frame, release: Release) -> Result<Self, DtaError> {
        let mut ds = Dataset::new(release);
        ds.label = frame.label().to_owned();
        ds.n_obs = frame.n_obs();
        ds.value_labels = frame.labels().clone();
        ds.chars = frame.chars().clone();
        let sort = frame.sort_state();
        if sort.valid {
            ds.sortlist = sort.keys.iter().map(|k| k.0).collect();
        }
        for (i, v) in frame.vars().iter().enumerate() {
            let idx = stratum_proto::VarIdx(i as u32);
            let col = frame
                .col(idx)
                .ok_or_else(|| DtaError::Inconsistent(format!("variable {i} has no column")))?;
            let var = DtaVar {
                name: v.name.to_string(),
                ty: v.ty,
                format: stratum_data::variable::format_string(&v.format),
                label: v.label.to_string(),
                value_label: v.value_label.as_ref().map(|s| s.to_string()),
            };
            let dc = match col {
                Column::StrL(s) => {
                    let mut data = StrLData::with_capacity(s.len() as usize, 0);
                    for row in 0..s.len() {
                        let chunk = s.chunk(stratum_data::chunk::chunk_of(row));
                        let binary = chunk.is_binary(stratum_data::chunk::offset_in_chunk(row));
                        data.push(s.get(row), binary);
                    }
                    DtaColumn::StrL(data)
                }
                other => DtaColumn::Fixed(other.clone()),
            };
            ds.vars.push(var);
            ds.cols.push(dc);
        }
        Ok(ds)
    }

    /// Load this dataset into a fresh [`Frame`] named `name` — the `use` path,
    /// and the inverse of [`Dataset::from_frame`].
    ///
    /// Lossless by refusal, not by hope: every fact the file carries lands in
    /// the frame or in the returned [`FrameBridge`], and a fact the engine
    /// cannot hold is a typed error (see [`DtaError::FrameVar`],
    /// [`DtaError::FrameFormat`], [`DtaError::FrameSort`]) rather than a
    /// silent drop or a guessed rename. On `Err` the dataset is consumed;
    /// the file it came from is untouched, so the caller re-reads if it wants
    /// another go.
    ///
    /// Costs, because loading is the slowest thing a researcher does:
    ///
    /// * fixed-width columns **move** — the chunked [`Column`]s built by the
    ///   read-side transpose become the frame's columns with zero copies;
    /// * `strL` columns go through [`StrLCol::from_rows`], the bulk
    ///   constructor — one arena append per cell, never the O(bytes²)
    ///   per-cell route;
    /// * the sort list is *recorded* via `Frame::set_sort_state`, exactly as
    ///   Stata trusts a `sortlist`, never re-proven with an O(n log n) sort.
    ///
    /// The returned frame reports [`Frame::changed`] false: a freshly loaded
    /// frame is in sync with its file.
    ///
    /// ```no_run
    /// let ds = stratum_dta::read_dta("auto.dta")?;
    /// let bridge = ds.into_frame("default")?;
    /// assert_eq!(bridge.frame.n_obs(), 74);
    /// # Ok::<(), stratum_dta::DtaError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`DtaError::FrameVar`] for a name the frame refuses,
    /// [`DtaError::FrameFormat`] for a display format that does not survive
    /// the parsed representation, [`DtaError::FrameSort`] for a sort list
    /// naming no variable, and [`DtaError::Inconsistent`] for a hand-built
    /// dataset whose vars and columns disagree.
    pub fn into_frame(self, name: &str) -> Result<FrameBridge, DtaError> {
        let Dataset {
            release: _,
            byte_order: _,
            label,
            timestamp,
            vars,
            cols,
            value_labels,
            chars,
            sortlist,
            n_obs,
            report: _,
        } = self;

        if vars.len() != cols.len() {
            return Err(DtaError::Inconsistent(format!(
                "{} variables but {} columns",
                vars.len(),
                cols.len()
            )));
        }
        let n_vars = vars.len() as u32;
        if let Some(&entry) = sortlist.iter().find(|&&e| e >= n_vars) {
            return Err(DtaError::FrameSort { entry });
        }

        let mut frame = Frame::new(name);
        for (i, (var, col)) in vars.into_iter().zip(cols).enumerate() {
            let index = i as u32;
            if col.storage_type() != var.ty {
                return Err(DtaError::Inconsistent(format!(
                    "variable {:?} is {:?} but its column is {:?}",
                    var.name,
                    var.ty,
                    col.storage_type()
                )));
            }
            if col.len() != n_obs {
                return Err(DtaError::Inconsistent(format!(
                    "variable {:?} has {} observations, the dataset has {n_obs}",
                    var.name,
                    col.len()
                )));
            }
            // Parse before installing anything, and demand the render inverts
            // it: a format that parses but renders differently would come back
            // rewritten on the next `save`, which is silent loss.
            let parsed = stratum_core::fmt::StataFormat::parse(&var.format).map_err(|e| {
                DtaError::FrameFormat {
                    index,
                    name: var.name.clone(),
                    format: var.format.clone(),
                    why: e.to_string(),
                }
            })?;
            let rendered = stratum_data::variable::format_string(&parsed);
            if rendered != var.format {
                return Err(DtaError::FrameFormat {
                    index,
                    name: var.name,
                    format: var.format,
                    why: format!("it would be written back as {rendered:?}"),
                });
            }

            let column = match col {
                // The whole point: the chunks built by the transpose become
                // the frame's chunks. A copy here would double the cost of
                // `use` for every fixed-width variable.
                DtaColumn::Fixed(c) => c,
                DtaColumn::StrL(s) => Column::StrL(StrLCol::from_rows(
                    (0..s.len()).map(|r| (s.get(r), s.is_binary(r))),
                )),
            };
            let idx = frame
                .add_column(&var.name, column)
                .map_err(|source| DtaError::FrameVar {
                    index,
                    name: var.name.clone(),
                    source,
                })?;
            let v = frame.var_mut(idx).expect("the variable was just added");
            v.format = parsed;
            v.label = Arc::from(var.label);
            // Kept even when the file defines no such table — the read warned
            // (`ReadWarning::MissingValueLabel`), and dropping the attachment
            // here would erase what `save` must write back.
            v.value_label = var.value_label.map(Arc::from);
        }

        // `add_column` takes `_N` from the first column; a variable-less file
        // still carries an observation count in its header.
        if frame.n_vars() == 0 && n_obs != 0 {
            frame.set_n_obs(n_obs);
        }
        *frame.labels_mut() = value_labels;
        *frame.chars_mut() = chars;
        frame.set_label(&label);
        let keys: Vec<VarIdx> = sortlist.into_iter().map(VarIdx).collect();
        frame
            .set_sort_state(&keys)
            .expect("every entry was validated against n_vars above");
        // A freshly loaded frame is in sync with its file; leaving `changed`
        // set would make the engine warn "data in memory has changed" over a
        // dataset nobody has touched.
        frame.mark_saved();
        Ok(FrameBridge { frame, timestamp })
    }
}

/// What [`Dataset::into_frame`] produced: the [`Frame`], plus the facts a
/// `Frame` has no slot for.
///
/// A struct rather than a tuple so that a new slot-less fact, should one
/// appear, widens this type instead of every call site's pattern.
#[derive(Debug)]
pub struct FrameBridge {
    /// The loaded frame.
    pub frame: Frame,
    /// The file's `<timestamp>`, verbatim. A `Frame` has no timestamp — it is
    /// a property of the write, not of the data ([`canonical_eq`] excludes it
    /// for the same reason) — but the writer keeps [`Dataset::timestamp`]
    /// verbatim and nothing in `stratum-dta` reads the clock, so the caller
    /// that wants `save` to round-trip it must hold it and hand it back
    /// (via [`Dataset::timestamp`] or [`DtaWriteOptions::timestamp`]).
    pub timestamp: String,
}

// ---------------------------------------------------------------------------
// canonical_eq
// ---------------------------------------------------------------------------

/// Why two datasets are not the same.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Diff(pub String);

macro_rules! diff {
    ($($arg:tt)*) => { return Err(Diff(format!($($arg)*))) };
}

/// Canonical semantic equality: every observable fact about the dataset.
///
/// **Byte identity is not the criterion, and asserting it would be wrong**
/// (`04` §11.3). Two measured reasons: Stata leaves uninitialised memory in
/// fixed-width padding, and its `strL` coalescing decisions are not observable
/// from the file. So this compares what a user could observe:
///
/// * `nobs`, `nvar`, and per variable name / type / label / format /
///   value-label name;
/// * the dataset label and the sort list;
/// * characteristics **order-insensitively**, and the notes projected from them;
/// * value-label tables;
/// * every cell — numeric **by raw bits**, so `.a != .b` and float precision is
///   exact; string cells by bytes; `strL` cells by `(bytes, binary flag)`.
///
/// The dataset's `<timestamp>` and its declared release are *not* compared:
/// they are properties of the write, not of the data, and comparing them would
/// make `read(write(A, 119))` unequal to `A` by construction.
///
/// # Errors
///
/// [`Diff`], naming the first disagreement, with the variable and observation.
pub fn canonical_eq(a: &Dataset, b: &Dataset) -> Result<(), Diff> {
    if a.n_obs() != b.n_obs() {
        diff!("nobs {} vs {}", a.n_obs(), b.n_obs());
    }
    if a.n_vars() != b.n_vars() {
        diff!("nvar {} vs {}", a.n_vars(), b.n_vars());
    }
    if a.label != b.label {
        diff!("dataset label {:?} vs {:?}", a.label, b.label);
    }
    if a.sorted_by() != b.sorted_by() {
        diff!("sortlist {:?} vs {:?}", a.sorted_by(), b.sorted_by());
    }

    for (i, (va, vb)) in a.vars.iter().zip(&b.vars).enumerate() {
        if va != vb {
            diff!("variable {i}: {va:?} vs {vb:?}");
        }
    }

    // Characteristics: order-insensitive, per `04` §11.3.
    let mut ca: Vec<_> = a.chars.iter().collect();
    let mut cb: Vec<_> = b.chars.iter().collect();
    ca.sort_unstable();
    cb.sort_unstable();
    if ca != cb {
        diff!("characteristics differ: {ca:?} vs {cb:?}");
    }

    let mut la = a.value_labels.names();
    let mut lb = b.value_labels.names();
    la.sort_unstable();
    lb.sort_unstable();
    if la != lb {
        diff!("value label tables {la:?} vs {lb:?}");
    }
    for name in &la {
        let ta = a.value_labels.get(name).expect("name came from this set");
        let tb = b.value_labels.get(name).expect("names compared equal");
        let mut ea: Vec<_> = ta.iter().collect();
        let mut eb: Vec<_> = tb.iter().collect();
        ea.sort_unstable();
        eb.sort_unstable();
        if ea != eb {
            diff!("value label {name:?}: {ea:?} vs {eb:?}");
        }
    }

    for (i, (ca, cb)) in a.cols.iter().zip(&b.cols).enumerate() {
        column_eq(i, ca, cb)?;
    }
    Ok(())
}

fn column_eq(i: usize, a: &DtaColumn, b: &DtaColumn) -> Result<(), Diff> {
    if a.len() != b.len() {
        diff!("variable {i}: {} vs {} observations", a.len(), b.len());
    }
    match (a, b) {
        (DtaColumn::StrL(x), DtaColumn::StrL(y)) => {
            for row in 0..x.len() {
                if x.get(row) != y.get(row) {
                    diff!(
                        "variable {i} obs {row}: strL {:?} vs {:?}",
                        String::from_utf8_lossy(x.get(row)),
                        String::from_utf8_lossy(y.get(row))
                    );
                }
                if x.is_binary(row) != y.is_binary(row) {
                    diff!("variable {i} obs {row}: strL binary flag differs");
                }
            }
            Ok(())
        }
        (DtaColumn::Fixed(x), DtaColumn::Fixed(y)) => fixed_eq(i, x, y),
        _ => diff!(
            "variable {i}: {:?} vs {:?}",
            a.storage_type(),
            b.storage_type()
        ),
    }
}

/// Cell comparison for everything but `strL`.
///
/// Numeric columns are compared **by raw bits**, not with `PartialEq`: `f64`
/// equality would make `.a == .b` false (correct, by luck) but also `-0.0 ==
/// 0.0` true and `NaN == NaN` false, and this is the function the round-trip
/// acceptance rests on. `Column`'s derived `PartialEq` is `f64`'s, so it is not
/// usable here.
fn fixed_eq(i: usize, a: &Column, b: &Column) -> Result<(), Diff> {
    if a.storage_type() != b.storage_type() {
        diff!(
            "variable {i}: {:?} vs {:?}",
            a.storage_type(),
            b.storage_type()
        );
    }
    match (a, b) {
        (Column::Byte(x), Column::Byte(y)) => ints(i, x.len(), |r| (x.get(r), y.get(r))),
        (Column::Int(x), Column::Int(y)) => ints(i, x.len(), |r| (x.get(r), y.get(r))),
        (Column::Long(x), Column::Long(y)) => ints(i, x.len(), |r| (x.get(r), y.get(r))),
        (Column::Float(x), Column::Float(y)) => {
            for r in 0..x.len() {
                let (p, q) = (x.get(r).to_bits(), y.get(r).to_bits());
                if p != q {
                    diff!("variable {i} obs {r}: float bits {p:#010x} vs {q:#010x}");
                }
            }
            Ok(())
        }
        (Column::Double(x), Column::Double(y)) => {
            for r in 0..x.len() {
                let (p, q) = (x.get(r).to_bits(), y.get(r).to_bits());
                if p != q {
                    diff!("variable {i} obs {r}: double bits {p:#018x} vs {q:#018x}");
                }
            }
            Ok(())
        }
        (Column::Str(x), Column::Str(y)) => {
            for r in 0..x.len() {
                if x.get(r) != y.get(r) {
                    diff!(
                        "variable {i} obs {r}: str {:?} vs {:?}",
                        String::from_utf8_lossy(x.get(r)),
                        String::from_utf8_lossy(y.get(r))
                    );
                }
            }
            Ok(())
        }
        _ => diff!("variable {i}: column shapes differ"),
    }
}

fn ints<T: PartialEq + fmt::Debug>(
    i: usize,
    len: u64,
    at: impl Fn(u64) -> (T, T),
) -> Result<(), Diff> {
    for r in 0..len {
        let (p, q) = at(r);
        if p != q {
            diff!("variable {i} obs {r}: {p:?} vs {q:?}");
        }
    }
    Ok(())
}

/// How two byte strings differ. `04` §11.3 rule 2: byte identity for a
/// Stata-written file is a **reported metric**, never an assertion, because
/// matching Stata's uninitialised padding is not achievable and not desirable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ByteDiff {
    /// Differing byte positions, plus the length difference.
    pub count: u64,
    /// The first few offsets that differ.
    pub first: Vec<usize>,
    /// Length of the left side.
    pub len_a: usize,
    /// Length of the right side.
    pub len_b: usize,
}

impl fmt::Display for ByteDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} differing byte(s) ({} vs {} bytes); first at {:?}",
            self.count, self.len_a, self.len_b, self.first
        )
    }
}

/// Count the byte differences between two buffers, keeping the first ten
/// offsets. See [`ByteDiff`].
#[must_use]
pub fn byte_diff(a: &[u8], b: &[u8]) -> ByteDiff {
    let mut d = ByteDiff {
        len_a: a.len(),
        len_b: b.len(),
        ..ByteDiff::default()
    };
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if x != y {
            d.count += 1;
            if d.first.len() < 10 {
                d.first.push(i);
            }
        }
    }
    d.count += a.len().abs_diff(b.len()) as u64;
    d
}

// ---------------------------------------------------------------------------
// Counters (ADR-017)
// ---------------------------------------------------------------------------

/// Instrumentation, always compiled.
///
/// ADR-017 is binding: a performance acceptance asserts a **counter** — work
/// done, copies, records — and never a wall-clock duration, because the same
/// unchanged tree measured 33 % apart an hour apart on this machine. A counter
/// that only existed under `cfg(test)` could not be asserted about the code that
/// ships, so these are in the shipping build.
///
/// They are affordable because **nothing here is incremented per cell.** The
/// per-cell work is W02's, counted by `stratum_data::counters().ingest_cells`.
/// Everything below moves once per file, once per column, or once per GSO
/// record.
#[derive(Debug, Default)]
pub struct Counters {
    /// Files read to completion.
    pub files_read: AtomicU64,
    /// Files written to completion.
    pub files_written: AtomicU64,
    /// Source bytes handed to the parser.
    pub source_bytes: AtomicU64,
    /// Reads that memory-mapped the source instead of copying it to the heap.
    pub mapped_reads: AtomicU64,
    /// Reads that copied the source to the heap (small file, or mapping failed).
    pub copied_reads: AtomicU64,
    /// **Whole copies of the `<data>` block made before the transpose.** Zero on
    /// every `LSF` file — the transpose reads straight out of the mapping. A
    /// non-zero value here means an `MSF` file took the byte-swapping path.
    pub data_block_copies: AtomicU64,
    /// Columns built by the transpose.
    pub columns_built: AtomicU64,
    /// Rows scanned for Invariant M on load (float and double columns only).
    pub canon_rows_scanned: AtomicU64,
    /// Rows a load canonicalisation actually rewrote. Zero for every file Stata
    /// wrote.
    pub canon_rows_fixed: AtomicU64,
    /// Columns rebuilt because of the above. Zero for every file Stata wrote.
    pub canon_columns_rebuilt: AtomicU64,
    /// GSO records indexed on read.
    pub gso_records_read: AtomicU64,
    /// `strL` cells fed to the write-side dedup.
    pub strl_cells_written: AtomicU64,
    /// GSO records actually emitted. `strl_cells_written - gso_records_written`
    /// is what dedup saved.
    pub gso_records_written: AtomicU64,
}

/// A plain-value reading of [`Counters`], safe to subtract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs)] // one-for-one with `Counters`, documented there.
pub struct Snapshot {
    pub files_read: u64,
    pub files_written: u64,
    pub source_bytes: u64,
    pub mapped_reads: u64,
    pub copied_reads: u64,
    pub data_block_copies: u64,
    pub columns_built: u64,
    pub canon_rows_scanned: u64,
    pub canon_rows_fixed: u64,
    pub canon_columns_rebuilt: u64,
    pub gso_records_read: u64,
    pub strl_cells_written: u64,
    pub gso_records_written: u64,
}

impl Counters {
    /// Read every counter. Not atomic as a group; counters are read from a test
    /// or a bench with no other work in flight.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        Snapshot {
            files_read: g(&self.files_read),
            files_written: g(&self.files_written),
            source_bytes: g(&self.source_bytes),
            mapped_reads: g(&self.mapped_reads),
            copied_reads: g(&self.copied_reads),
            data_block_copies: g(&self.data_block_copies),
            columns_built: g(&self.columns_built),
            canon_rows_scanned: g(&self.canon_rows_scanned),
            canon_rows_fixed: g(&self.canon_rows_fixed),
            canon_columns_rebuilt: g(&self.canon_columns_rebuilt),
            gso_records_read: g(&self.gso_records_read),
            strl_cells_written: g(&self.strl_cells_written),
            gso_records_written: g(&self.gso_records_written),
        }
    }
}

impl Snapshot {
    /// `self - earlier`, field by field, saturating.
    #[must_use]
    pub fn since(&self, e: Snapshot) -> Snapshot {
        Snapshot {
            files_read: self.files_read.saturating_sub(e.files_read),
            files_written: self.files_written.saturating_sub(e.files_written),
            source_bytes: self.source_bytes.saturating_sub(e.source_bytes),
            mapped_reads: self.mapped_reads.saturating_sub(e.mapped_reads),
            copied_reads: self.copied_reads.saturating_sub(e.copied_reads),
            data_block_copies: self.data_block_copies.saturating_sub(e.data_block_copies),
            columns_built: self.columns_built.saturating_sub(e.columns_built),
            canon_rows_scanned: self.canon_rows_scanned.saturating_sub(e.canon_rows_scanned),
            canon_rows_fixed: self.canon_rows_fixed.saturating_sub(e.canon_rows_fixed),
            canon_columns_rebuilt: self
                .canon_columns_rebuilt
                .saturating_sub(e.canon_columns_rebuilt),
            gso_records_read: self.gso_records_read.saturating_sub(e.gso_records_read),
            strl_cells_written: self.strl_cells_written.saturating_sub(e.strl_cells_written),
            gso_records_written: self
                .gso_records_written
                .saturating_sub(e.gso_records_written),
        }
    }
}

/// The process-wide counters.
pub fn counters() -> &'static Counters {
    static C: OnceLock<Counters> = OnceLock::new();
    C.get_or_init(Counters::default)
}

#[inline]
pub(crate) fn bump(c: &AtomicU64, by: u64) {
    c.fetch_add(by, Ordering::Relaxed);
}

/// Build a [`ValueLabel`] from `(value, text)` pairs. Used by the reader and by
/// the round-trip generators.
pub(crate) fn value_label_from(pairs: impl IntoIterator<Item = (i32, String)>) -> ValueLabel {
    let mut t = ValueLabel::new();
    for (k, v) in pairs {
        t.insert(k, v);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strl_data_appends_in_amortised_linear_time() {
        let mut s = StrLData::with_capacity(4, 16);
        s.push(b"alpha", false);
        s.push(b"", false);
        s.push(b"\x00\xffbin", true);
        s.push(b"omega", false);
        assert_eq!(s.len(), 4);
        assert_eq!(s.get(0), b"alpha");
        assert_eq!(s.get(1), b"");
        assert_eq!(s.get(2), b"\x00\xffbin");
        assert_eq!(s.get(3), b"omega");
        assert!(!s.is_binary(0) && s.is_binary(2) && !s.is_binary(3));
        // Past the end is empty, never a panic — this is reachable from a
        // hostile file whose cell count and column length disagree.
        assert_eq!(s.get(99), b"");
        assert!(!s.is_binary(99));
    }

    #[test]
    fn strl_binary_bitmap_survives_a_chunk_boundary() {
        let mut s = StrLData::with_capacity(200, 200);
        for i in 0..200u64 {
            s.push(b"x", i % 3 == 0);
        }
        for i in 0..200u64 {
            assert_eq!(s.is_binary(i), i % 3 == 0, "row {i}");
        }
    }

    #[test]
    fn byte_diff_reports_rather_than_asserts() {
        let d = byte_diff(b"abcd", b"abXd");
        assert_eq!((d.count, d.first.as_slice()), (1, &[2usize][..]));
        let d = byte_diff(b"abc", b"abcdef");
        assert_eq!(d.count, 3);
    }

    #[test]
    fn diff_names_the_first_disagreement() {
        let mut a = Dataset::new(Release::R118);
        a.push_var(
            DtaVar::new("x", StorageType::Double),
            DtaColumn::Fixed(Column::new_missing(StorageType::Double, 2)),
        )
        .unwrap();
        let mut b = a.clone();
        b.vars[0].label = "changed".into();
        let e = canonical_eq(&a, &b).unwrap_err();
        assert!(e.to_string().contains("variable 0"), "{e}");
        assert!(canonical_eq(&a, &a).is_ok());
    }

    /// The reason `fixed_eq` does not use `Column`'s derived `PartialEq`.
    #[test]
    fn numeric_cells_compare_by_raw_bits() {
        use stratum_core::missing::{missing_f64, SYSMISS};
        let a = Column::Double(stratum_data::NumCol::from_slice(&[missing_f64(1)]));
        let b = Column::Double(stratum_data::NumCol::from_slice(&[missing_f64(2)]));
        assert!(
            fixed_eq(0, &a, &b).is_err(),
            ".a and .b must not compare equal"
        );
        let z = Column::Double(stratum_data::NumCol::from_slice(&[0.0f64]));
        let nz = Column::Double(stratum_data::NumCol::from_slice(&[-0.0f64]));
        assert!(
            fixed_eq(0, &z, &nz).is_err(),
            "-0.0 and 0.0 are different bit patterns and must not compare equal"
        );
        let s = Column::Double(stratum_data::NumCol::from_slice(&[SYSMISS]));
        assert!(fixed_eq(0, &s, &s).is_ok());
    }
}
