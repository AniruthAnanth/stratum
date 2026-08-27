//! Logical executable regions — design 02 §5, spec §2.
//!
//! This is the shared definition the editor gutter, "run block / run from here /
//! run all stale", and the headless CLI all code against. It is a PURE function
//! over source text: no I/O, no globals, no runtime state, deterministic, O(n).
//!
//! The seven properties of 02 §5.4 are what the tests assert; the two that shape
//! the code are:
//!
//! * **Tiling** — consecutive `outer_span`s reproduce the source byte for byte.
//!   That falls out of building every region from a contiguous run of logical
//!   lines and never from a rescan of the text between them.
//! * **Self-containment** — re-segmenting `src[r.span]` with
//!   `initial_delimiter = r.entry_delimiter` yields one region of the same kind.
//!   That is what makes `Cmd+Enter` correct, and it is why `entry_delimiter` is
//!   on the region rather than being re-derived by the runtime.

use std::ops::Range;

use rustc_hash::FxHashMap;
use stratum_proto::{
    BraceOpener, CellMarker, CodeHash, Confidence, Delimiter, Diagnostic, DirectiveKind,
    EndBlockOpener, LineRange, RegionKind, RegionSummary, SectionId, SectionSpan, Severity, Span,
    Unterminated,
};

use crate::ast::PrefixKind;
use crate::canon::code_hash_into;
use crate::cmdsig::{CmdFlags, CmdId, CommandSig, CommandTable};
use crate::lineindex::LineIndex;
use crate::scan::logical::{read_logical_line, Derived, DerivedText, LogicalLine};
use crate::scan::marker;
use crate::scan::state::ScanState;

/// Knobs on [`segment_with`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SegmentOptions {
    /// `Cr` for a whole do-file. Set to a region's `entry_delimiter` when
    /// re-segmenting a fragment in isolation.
    pub initial_delimiter: Delimiter,
    /// Join `} ⏎ else { … }` into one region. Executing `else { … }` on its own
    /// is an error, so this defaults to true and turning it off is a debugging
    /// aid, not a user setting.
    pub join_else_chains: bool,
    /// Attach a contiguous run of comment lines directly above a command to that
    /// command's `outer_span`.
    pub attach_leading_comments: bool,
    /// Cap on `Segmentation::diags`.
    pub max_diagnostics: usize,
}

impl Default for SegmentOptions {
    fn default() -> Self {
        Self {
            initial_delimiter: Delimiter::Cr,
            join_else_chains: true,
            attach_leading_comments: true,
            max_diagnostics: 200,
        }
    }
}

/// The prefix chain of a region head, packed into one word.
///
/// A `SmallVec<[PrefixKind; 2]>` is 24 bytes whatever the inline capacity — and
/// every one of them is moved on every keystroke, because the region vector is
/// (see [`resegment`]). Seven kinds fit in three bits and a chain longer than
/// eight prefixes does not occur in Stata, so the whole chain is one `u32`:
/// three bits per kind from the bottom, length in the top nibble.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct PrefixChain(u32);

impl PrefixChain {
    const CAP: u32 = 8;
    const BITS: u32 = 3;

    /// Append `k`. Beyond [`PrefixChain::CAP`] the chain saturates: `len` keeps
    /// counting so `by: by: …` is still visibly a chain, but the ninth kind is
    /// not recorded. Nothing in the product reads past the fourth.
    fn push(&mut self, k: PrefixKind) {
        let n = self.len();
        if n < Self::CAP {
            self.0 |= (k as u32) << (n * Self::BITS);
        }
        self.0 = (self.0 & 0x0fff_ffff) | ((n + 1).min(0xf) << 28);
    }

    /// Number of prefixes, saturating at 15.
    pub fn len(&self) -> u32 {
        self.0 >> 28
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The `i`th prefix, outermost first.
    pub fn get(&self, i: u32) -> Option<PrefixKind> {
        if i >= self.len().min(Self::CAP) {
            return None;
        }
        Some(match (self.0 >> (i * Self::BITS)) & 0b111 {
            0 => PrefixKind::By,
            1 => PrefixKind::Quietly,
            2 => PrefixKind::Noisily,
            3 => PrefixKind::Capture,
            4 => PrefixKind::Version,
            5 => PrefixKind::Frame,
            _ => PrefixKind::Generic,
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = PrefixKind> + '_ {
        (0..self.len().min(Self::CAP)).filter_map(|i| self.get(i))
    }
}

/// What the region's first code line says about itself, without expanding a
/// single macro.
///
/// Every field is `Copy` and the whole thing is 24 bytes rather than 56. Both
/// facts are load-bearing on the keystroke path and neither is free: `canonical`
/// and `is_estimation` are now read through the `CommandSig` they both came
/// from, and `command_span` uses an empty span for "no command word" instead of
/// an `Option` that costs four bytes of discriminant. See [`resegment`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct HeadInfo {
    /// The prefix chain, outermost first.
    pub prefixes: PrefixChain,
    /// Row the command word resolved to in the table segmentation was run with,
    /// or [`HeadInfo::NO_CMD`]. A `CmdId` and not a `&'static CommandSig`: the
    /// reference is eight bytes and forces `Region` to eight-byte alignment,
    /// which together is 8 of its 88 bytes — and every one of those bytes is
    /// moved on every keystroke (see [`resegment`]).
    cmd: u16,
    /// [`HeadInfo::MACRO_IN_HEAD`] | [`HeadInfo::ESTIMATION`].
    flags: u8,
}

/// Not derived: `cmd` defaults to a row id, and row 0 is a real command.
impl Default for HeadInfo {
    fn default() -> Self {
        HeadInfo {
            prefixes: PrefixChain::default(),
            cmd: Self::NO_CMD,
            flags: 0,
        }
    }
}

impl HeadInfo {
    /// `cmd` for a head whose command word did not resolve.
    const NO_CMD: u16 = u16::MAX;
    /// A macro reference appears in the command position.
    const MACRO_IN_HEAD: u8 = 1 << 0;
    /// The resolved command is e-class. Kept as a bit rather than read back off
    /// the row, so the one thing the wire projection needs from the signature
    /// does not need the table.
    const ESTIMATION: u8 = 1 << 1;

    fn new(sig: Option<&'static CommandSig>, id: Option<CmdId>, has_macro: bool) -> Self {
        let mut flags = 0u8;
        if has_macro {
            flags |= Self::MACRO_IN_HEAD;
        }
        if sig.is_some_and(|s| s.flags.contains(CmdFlags::ESTIMATION)) {
            flags |= Self::ESTIMATION;
        }
        HeadInfo {
            cmd: id.map_or(Self::NO_CMD, |i| i.0),
            flags,
            ..HeadInfo::default()
        }
    }

    /// The resolved signature, for callers that want more than the name.
    ///
    /// **The id indexes the table this region was segmented with**, and every
    /// entry point in this file segments with [`CommandTable::core`] — the
    /// grouper, [`Region::command_span`] and [`Region::end_block_name`] all name
    /// it directly. When W04b's generated table replaces it, the table becomes a
    /// [`SegmentOptions`] field and this accessor takes it as an argument; there
    /// is exactly one place to change, which is why the id is stored and the
    /// pointer is not.
    pub fn sig(&self) -> Option<&'static CommandSig> {
        (self.cmd != Self::NO_CMD).then(|| CommandTable::core().get(CmdId(self.cmd)))
    }

    /// Canonical command name, when the word resolves unambiguously without
    /// macro expansion.
    pub fn canonical(&self) -> Option<&'static str> {
        self.sig().map(|s| s.canonical)
    }

    /// The canonical command is e-class — powers spec §19 "Compare models".
    pub fn is_estimation(&self) -> bool {
        self.flags & Self::ESTIMATION != 0
    }

    /// A macro reference appears in the command position.
    pub fn has_macro_in_head(&self) -> bool {
        self.flags & Self::MACRO_IN_HEAD != 0
    }

    /// True when the head resolved to a command word at all.
    pub fn is_resolved(&self) -> bool {
        self.cmd != Self::NO_CMD
    }
}

/// [`stratum_proto::RegionKind`] with the `EndBlock` name left out.
///
/// The wire kind carries `Option<String>`, which makes it 32 bytes and not
/// `Copy` for the sake of a name that only `program` blocks have. The name is a
/// pure function of the region's head line, so it is recomputed in
/// [`Region::summary`] — which runs once per debounced change, over the regions
/// the UI asks for — instead of being carried through every keystroke on every
/// region in the document.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RegionShape {
    Simple,
    Brace { opener: BraceOpener },
    EndBlock { opener: EndBlockOpener },
    Directive { directive: DirectiveKind },
    Trivia { has_marker: bool },
    Unterminated { expected: Unterminated },
}

/// A half-open `u32` index range.
///
/// `std::ops::Range` is deliberately not `Copy`, and [`Region`] must be: what
/// makes a keystroke affordable is moving the region vector's tail and rebasing
/// its coordinates in ONE pass, and a pass that cannot copy an element out of
/// its slot has to leave something behind in it instead.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct IdxRange {
    pub start: u32,
    pub end: u32,
}

impl IdxRange {
    /// The `usize` range this indexes with.
    pub fn usize(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

impl From<Range<u32>> for IdxRange {
    fn from(r: Range<u32>) -> Self {
        IdxRange {
            start: r.start,
            end: r.end,
        }
    }
}

/// One logical executable region.
///
/// **`Copy`, and 112 bytes rather than 176.** Not an aesthetic choice: an edit
/// near the top of a document moves the whole tail of the region vector, so
/// every byte of this struct is memory traffic on every keystroke, and a `Copy`
/// element is what lets [`resegment`] move the tail and rebase its coordinates
/// in one pass instead of two. What it cost is that `kind` is [`RegionShape`]
/// rather than the wire's `RegionKind` and `head` reads through accessors.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Region {
    /// Position in [`Segmentation::regions`].
    pub index: u32,
    /// Executable extent: first code byte .. last code byte, comments trimmed at
    /// both ends. For `Trivia` this equals `outer_span`: a comment run has no
    /// executable extent, and an empty span there would make "which region is
    /// the cursor in" ambiguous at every blank line.
    pub span: Span,
    /// `span` plus attached leading comment lines and the trailing comment on
    /// the last physical line. Consecutive `outer_span`s tile the file exactly.
    pub outer_span: Span,
    /// 0-based physical lines of `outer_span`, half-open.
    pub lines: LineRange,
    /// 0-based physical lines of `span` only — what the gutter aligns to.
    pub code_lines: LineRange,
    /// What kind of region this is. The wire's `RegionKind`, minus the
    /// `EndBlock` name — [`Region::summary`] puts that back.
    pub kind: RegionShape,
    /// Delimiter mode in force at `span.start`.
    pub entry_delimiter: Delimiter,
    /// Delimiter mode in force after the region.
    pub exit_delimiter: Delimiter,
    /// What the head line says about itself.
    pub head: HeadInfo,
    /// Identity for staleness (spec §12/§13) — see [`crate::canon`].
    pub code_hash: CodeHash,
    /// 0-based occurrence index of this `code_hash` within the document.
    pub hash_ordinal: u32,
    /// Index range into [`Segmentation::lines`].
    pub logical_lines: IdxRange,
    /// Index range into [`Segmentation::diags`].
    pub diags: IdxRange,
    /// Section this region falls in, when the document uses `%%` markers, as a
    /// raw id with [`Region::NO_SECTION`] for "none". `Option<SectionId>` costs
    /// eight bytes for one bit — read it through [`Region::section`].
    section: u32,
}

impl Region {
    /// The owned wire projection (CONTRACTS §2).
    ///
    /// `src` and `lines` are the buffer and the line vector this region was
    /// segmented from — [`Segmentation::summaries`] passes its own. They are
    /// needed for exactly one field: an `EndBlock`'s program name, which is not
    /// carried on the region (see [`RegionShape`]) and is re-derived here.
    pub fn summary(
        &self,
        src: &str,
        lines: &[LogicalLine],
        derived: &[DerivedText],
    ) -> RegionSummary {
        RegionSummary {
            index: self.index,
            span: self.span,
            outer_span: self.outer_span,
            lines: self.lines,
            code_lines: self.code_lines,
            kind: self.wire_kind(src, lines, derived),
            entry_delimiter: self.entry_delimiter,
            exit_delimiter: self.exit_delimiter,
            code_hash: self.code_hash,
            hash_ordinal: self.hash_ordinal,
            canonical: self.head.canonical().map(str::to_owned),
            is_estimation: self.head.is_estimation(),
            has_macro_in_head: self.head.has_macro_in_head(),
            section: self.section(),
        }
    }

    /// The wire `RegionKind`, with an `EndBlock`'s name recovered from the head
    /// line. The name is a pure function of that line — the same function the
    /// grouper used to decide this was an `EndBlock` at all — so recomputing it
    /// cannot disagree with what segmentation saw.
    pub fn wire_kind(
        &self,
        src: &str,
        lines: &[LogicalLine],
        derived: &[DerivedText],
    ) -> RegionKind {
        match self.kind {
            RegionShape::Simple => RegionKind::Simple,
            RegionShape::Brace { opener } => RegionKind::Brace { opener },
            RegionShape::EndBlock { opener } => RegionKind::EndBlock {
                opener,
                name: self.end_block_name(src, lines, derived),
            },
            RegionShape::Directive { directive } => RegionKind::Directive { directive },
            RegionShape::Trivia { has_marker } => RegionKind::Trivia { has_marker },
            RegionShape::Unterminated { expected } => RegionKind::Unterminated { expected },
        }
    }

    /// Span of the command word as typed (after prefixes), in ORIGINAL source
    /// bytes, or `None` when the head is `{` or punctuation.
    ///
    /// Recomputed rather than stored, for the same reason as the `EndBlock` name
    /// (see [`RegionShape`]): it is eight bytes and a `SpanMap` clone per region
    /// on the keystroke path for a value only completion asks for, one region at
    /// a time.
    pub fn command_span(
        &self,
        src: &str,
        lines: &[LogicalLine],
        derived: &[DerivedText],
    ) -> Option<Span> {
        let table = CommandTable::core();
        let at = self.logical_lines.usize();
        let (i, line) = lines[at.clone()]
            .iter()
            .enumerate()
            .find(|(_, l)| !l.is_trivia)?;
        let d = derived[at.start + i].as_deref();
        let h = head::parse(src, line, d, &table);
        if h.word.is_empty() {
            return None;
        }
        Some(Span {
            start: line.to_source(d, h.word.start as u32),
            end: line.to_source(d, h.word.end as u32),
        })
    }

    fn end_block_name(
        &self,
        src: &str,
        lines: &[LogicalLine],
        derived: &[DerivedText],
    ) -> Option<String> {
        let table = CommandTable::core();
        let at = self.logical_lines.usize();
        let (i, line) = lines[at.clone()]
            .iter()
            .enumerate()
            .find(|(_, l)| !l.is_trivia)?;
        let d = derived[at.start + i].as_deref();
        let h = head::parse(src, line, d, &table);
        head::end_block_opener(src, line, d, &h, &table).and_then(|(_, name)| name)
    }

    /// `Region::section` when the region is in no section.
    pub const NO_SECTION: u32 = u32::MAX;

    /// Section this region falls in, when the document uses `%%` markers.
    pub fn section(&self) -> Option<SectionId> {
        (self.section != Self::NO_SECTION).then_some(SectionId(self.section))
    }

    /// True when the gutter offers a run affordance.
    pub fn is_executable(&self) -> bool {
        !matches!(
            self.kind,
            RegionShape::Trivia { .. } | RegionShape::Unterminated { .. }
        )
    }

    /// A region that stands in for a slot being moved through. Never observed:
    /// [`splice_rebase`] overwrites or truncates every one it creates.
    pub(crate) fn vacant() -> Self {
        Region {
            index: 0,
            span: Span { start: 0, end: 0 },
            outer_span: Span { start: 0, end: 0 },
            lines: LineRange { start: 0, end: 0 },
            code_lines: LineRange { start: 0, end: 0 },
            kind: RegionShape::Simple,
            entry_delimiter: Delimiter::Cr,
            exit_delimiter: Delimiter::Cr,
            head: HeadInfo::default(),
            code_hash: CodeHash([0; 16]),
            hash_ordinal: 0,
            logical_lines: IdxRange { start: 0, end: 0 },
            diags: IdxRange { start: 0, end: 0 },
            section: Region::NO_SECTION,
        }
    }

    /// Scanner state at `outer_span.start` — see [`ScanState`].
    pub fn entry_state(&self) -> ScanState {
        ScanState::new(self.entry_delimiter)
    }
}

/// The result of segmenting a buffer.
///
/// **Deviation from design 02 §5.1, deliberate.** §5.1 lists `src_hash: u64`
/// (xxh3 of the whole source). It is not here. It is not derived from
/// segmentation, it is a property of the buffer; CONTRACTS §1.1 already names
/// that value `TextHash` and defines it as blake3-128 including comments; and
/// computing it inside `segment` would put a full pass over a 2 MB buffer on the
/// keystroke path to answer a question ("did the file change on disk?") that the
/// keystroke path never asks. Callers that want it call
/// [`crate::canon::text_hash`].
#[derive(Clone, PartialEq, Debug)]
pub struct Segmentation<'a> {
    /// The buffer this segmentation was produced from. Every `Span` in it
    /// indexes THIS string, and [`LogicalLine::code`] must be given it.
    ///
    /// It is held here rather than on each line so that a keystroke can move the
    /// whole line vector into the new segmentation untouched: re-pointing 70 000
    /// borrowed lines at the edited buffer was 40 % of the incremental path, for
    /// lines whose bytes did not change.
    pub src: &'a str,
    /// Regions, in document order, tiling the source.
    pub regions: Vec<Region>,
    /// Every logical line, in document order, tiling the source.
    pub lines: Vec<LogicalLine>,
    /// Spliced text, one slot per entry of `lines` — see [`DerivedText`]. Held
    /// beside the line vector rather than inside it so that `LogicalLine` stays
    /// `Copy` and the keystroke path can `memmove` the line tail.
    pub derived: Vec<DerivedText>,
    /// `// %%` / `* %%` cell markers (spec §3).
    pub markers: Vec<CellMarker>,
    /// One section per marker.
    pub sections: Vec<SectionSpan>,
    /// Diagnostics, indexed by `Region::diags`.
    pub diags: Vec<Diagnostic>,
    /// Byte↔line table for this source.
    pub line_index: LineIndex,
    /// Delimiter mode at end of source.
    pub end_delimiter: Delimiter,
    /// `src.len()`.
    pub src_len: u32,
    /// The options this segmentation was produced with; [`resegment`] reuses
    /// them so an incremental pass cannot silently change the grouping rules.
    pub options: SegmentOptions,
    /// Occurrences of each `code_hash`. Kept so [`resegment`] can prove that a
    /// rescan did not disturb any `hash_ordinal` outside the rescanned run,
    /// which is the common case and costs one lookup instead of a full pass.
    hash_counts: FxHashMap<CodeHash, u32>,
}

impl Segmentation<'_> {
    /// The owned wire projection of every region.
    pub fn summaries(&self) -> Vec<RegionSummary> {
        self.regions
            .iter()
            .map(|r| r.summary(self.src, &self.lines, &self.derived))
            .collect()
    }

    /// The region containing `byte`, by `outer_span`. `None` only for an empty
    /// source or a byte past the end.
    pub fn region_at(&self, byte: u32) -> Option<&Region> {
        let i = self
            .regions
            .partition_point(|r| r.outer_span.start <= byte)
            .checked_sub(1)?;
        let r = &self.regions[i];
        (byte < r.outer_span.end).then_some(r)
    }

    /// Scanner state at end of source.
    pub fn end_state(&self) -> ScanState {
        ScanState::new(self.end_delimiter)
    }
}

/// One text replacement, in the coordinates of the source `prev` was built from.
///
/// **This is NOT `stratum_proto::TextEdit`.** That type is
/// `{ span, text_index }` — a batch entry pointing into a table of replacement
/// strings (CONTRACTS §1.1). Design 02 §5.5 spells the incremental-segmentation
/// argument `TextEdit { range, new_len }`, which is a different type with the
/// same name; reusing proto's would have made `new_len` and `text_index` the
/// same field. See the report accompanying this unit.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SourceEdit {
    /// Replaced range, in OLD coordinates.
    pub range: Span,
    /// Length in bytes of the replacement text.
    pub new_len: u32,
}

/// How much work [`resegment`] actually avoided. Not part of `Segmentation`,
/// because property 4 says `resegment(...) == segment(...)` and an instrumentation
/// counter inside the value would make that comparison meaningless.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct ResegmentStats {
    /// Regions reused unchanged from before the edit.
    pub reused_prefix: u32,
    /// Regions freshly grouped AND freshly hashed. The A25 gate is on this.
    pub rescanned: u32,
    /// Regions reused from after the edit, spans shifted, hashes kept.
    pub reused_suffix: u32,
    /// Source bytes the scanner actually walked.
    pub bytes_scanned: u32,
    /// True when the scanner re-converged with the previous segmentation instead
    /// of running to end of source.
    pub converged: bool,
    /// True when the rescan changed how many times some hash occurs, so the
    /// reused suffix had to be renumbered.
    pub ordinals_repaired: bool,
}

/// Segment a whole do-file. Entry point for everything that is not an
/// incremental editor pass.
pub fn segment(src: &str) -> Segmentation<'_> {
    segment_with(src, &SegmentOptions::default())
}

/// Segment with explicit options.
///
/// A fragment must be segmented with `initial_delimiter` set to the region's
/// `entry_delimiter`; segmenting one without it is the bug design 02 §13.2 warns
/// about, because the delimiter mode at any byte depends on all preceding text.
pub fn segment_with<'a>(src: &'a str, opts: &SegmentOptions) -> Segmentation<'a> {
    let mut lines = Vec::with_capacity(src.len() / 24 + 1);
    let mut derived: Vec<DerivedText> = Vec::with_capacity(src.len() / 24 + 1);
    let mut cursor = 0u32;
    let mut delim = opts.initial_delimiter;
    let mut line = 0u32;
    while (cursor as usize) < src.len() {
        let (l, d) = read_logical_line(src, &mut cursor, &mut delim, &mut line);
        lines.push(l);
        derived.push(d);
    }
    let li = LineIndex::new(src);
    let end_delimiter = delim;

    let (markers, sections) = marker::collect(src, &lines, &li);
    let mut g = Grouper::new(src, &lines, &derived, opts, lines.len() / 2 + 1, 0);
    g.group(0, &mut |_| false);
    let Grouper {
        regions,
        diags,
        counts,
        ..
    } = g;

    let mut seg = Segmentation {
        src,
        regions,
        lines,
        derived,
        markers,
        sections,
        diags,
        line_index: li,
        end_delimiter,
        src_len: src.len() as u32,
        options: opts.clone(),
        hash_counts: counts,
    };
    assign_sections(&mut seg, false);
    seg
}

/// Incremental re-segmentation. `resegment(prev, new_src, edit)` is equal to
/// `segment_with(new_src, &prev.options)` for every input — property 4 — and the
/// proptest in `tests/resegment.rs` is what keeps it that way.
pub fn resegment<'a>(
    prev: Segmentation<'_>,
    new_src: &'a str,
    edit: SourceEdit,
) -> Segmentation<'a> {
    resegment_with_stats(prev, new_src, edit).0
}

/// [`resegment`] plus the instrumentation the A25 acceptance gate asserts on.
///
/// The shape of the work, and why it is not 02 §5.5's "rescan to end of file":
///
/// 1. Reuse `regions[..=R]` for the last region `R` entirely before the edit.
/// 2. Rescan forward from `R.outer_span.end` until the SCANNER re-converges — a
///    line boundary whose old counterpart is a line boundary in the same
///    [`ScanState`], with every byte from there on unchanged. From that point
///    the two scans are the same scan.
/// 3. Regroup from `R`, splicing in the old regions the moment a new region
///    boundary coincides with an old one. Grouping only ever looks forward
///    (02 §5.2), so identical lines group identically.
///
/// The result is that a keystroke re-hashes a handful of regions instead of
/// every region below the cursor. §5.5's rule is O(bytes after the edit), which
/// at line 100 of a 2 MB file is ~2 MB and a blake3 of every following region,
/// per keystroke, in wasm, inside a 6 ms budget.
///
/// **`prev` is CONSUMED.** 02 §5.5 spells the argument `&Segmentation`, which
/// forces the result to be built out of fresh allocations; `floor/copying_resegment_2mb`
/// measures that shape at ~1.0 ms before it does any work at all. Taking
/// ownership lets the untouched prefix and the untouched suffix stay in the
/// allocation they are already in, and only the edited window is rebuilt.
/// CONTRACTS §14's wasm surface is `Engine::resegment(&mut self)` — an engine
/// that owns exactly one segmentation and replaces it — so this is also the
/// shape the one real caller wants. A caller that needs `prev` afterwards clones
/// it explicitly and pays for the copy on purpose.
///
/// # What is left, and where the A25 gate went
///
/// After the rescan, the only remaining work is bookkeeping on the part of the
/// document the edit did not touch, and for an edit 5 % into a 2 MB file that is
/// 95 % of it: 42 690 regions of 88 bytes and 71 150 logical lines of 48. The
/// work the edit CAUSED — three logical lines read, one region grouped, one
/// blake3 — is under 3 % of the call. Everything else is moving and rebasing
/// metadata the edit did not change, and that cost is BYTES, not elements, which
/// is why every lever that worked was a byte:
///
/// | | `five_percent` | `typing` |
/// |---|---|---|
/// | pre-audit shape (`Region` 176 B, `Clone`, two passes) | 403 µs | 197 µs |
/// | `Region` 96 B and `Copy`, move fused with rebase      | 277 µs | 150 µs |
/// | `LogicalLine` 48 B and `Copy`, `Region` 88 B, `u32` rebase | **226 µs** | 123 µs |
///
/// The last row is three things, all of them bytes or instructions on the tail:
///
/// * **`LogicalLine` lost its `Box`** to the parallel [`DerivedText`] table. An
///   element with drop glue cannot be bit-moved in safe code, so the line tail
///   had to be lifted out of its slots one at a time — 159 µs against a 59 µs
///   `memmove` floor. The table itself needs no rebase, because a `Derived`'s
///   piece map is relative to its line, so it moves with one `Vec::splice`.
/// * **`HeadInfo` holds a `CmdId`, not a `&'static CommandSig`**, which takes
///   `Region` to 88 bytes and its alignment to 4.
/// * **The rebase is eight `u32` wrapping adds**, with `ord_delta` renumbering
///   and diagnostic-index shifting — both empty on the overwhelmingly common
///   edit — hoisted out of the loop instead of branched on inside it.
///
/// What is left is within ~45 % of `memmove` on both vectors. The next lever is
/// still the one 02 §5.5 names — chunked storage with a per-chunk base, or a
/// tail delta applied lazily on read — and both move cost onto the read path
/// that `stratum-exec` walks to build a `BlockMap` after every debounced change.
/// The gate no longer needs it, so it is not built here.
pub fn resegment_with_stats<'a>(
    mut prev: Segmentation<'_>,
    new_src: &'a str,
    edit: SourceEdit,
) -> (Segmentation<'a>, ResegmentStats) {
    let old_len = prev.src_len;
    let consistent = edit.range.start <= edit.range.end
        && edit.range.end <= old_len
        && (old_len as i64 - i64::from(edit.range.end - edit.range.start)
            + i64::from(edit.new_len))
            == new_src.len() as i64;
    if !consistent {
        // A malformed edit descriptor is a caller bug, and guessing at what was
        // meant would corrupt a document silently. Redo the whole file instead.
        return (
            segment_with(new_src, &prev.options),
            ResegmentStats::default(),
        );
    }

    let delta = i64::from(edit.new_len) - i64::from(edit.range.end - edit.range.start);
    let mut stats = ResegmentStats::default();

    let mut keep = prev
        .regions
        .partition_point(|r| r.outer_span.end <= edit.range.start);
    // Not every region entirely before the edit is a legal boundary: some kinds
    // GROW when text is appended after them, and reusing one of those would
    // silently split a region the full pass would have kept whole. See
    // `is_reusable_boundary`.
    while keep > 0 && !is_reusable_boundary(&prev.regions[keep - 1], old_len) {
        keep -= 1;
    }
    if keep == 0 {
        return (segment_with(new_src, &prev.options), stats);
    }
    let rk = keep - 1;
    let resume_byte = prev.regions[rk].outer_span.end;
    let resume_line = prev.regions[rk].logical_lines.end as usize;
    stats.reused_prefix = keep as u32;

    // Taken, not borrowed: `patch` reuses the table it is given rather than
    // allocating a second one the size of the document. What is left behind is
    // the one-element default, which the destructure below discards.
    let li = std::mem::take(&mut prev.line_index).patch(new_src, edit.range, edit.new_len);

    // ---- phase 1: rescan forward until the scanner re-converges -------------
    // Only the rescanned run is materialised here. The lines before and after it
    // are already in `prev.lines` and are re-pointed at the new buffer in place
    // below, never rebuilt.
    let mut window: Vec<LogicalLine> = Vec::new();
    let mut window_derived: Vec<DerivedText> = Vec::new();
    let mut cursor = resume_byte;
    let mut delim = prev.regions[rk].exit_delimiter;
    // NOT `prev.lines[resume_line - 1].last_line + 1`: in `;` mode a logical line
    // can end in the middle of a physical one, and the next line then starts on
    // that same physical line.
    // `li`, not the old table: `resume_byte` is at or before the edit, and
    // `patch` leaves every line start there unchanged, so the two agree.
    let mut line = li.line_of(resume_byte);
    let mut converged: Option<usize> = None;
    while (cursor as usize) < new_src.len() {
        let (l, d) = read_logical_line(new_src, &mut cursor, &mut delim, &mut line);
        window.push(l);
        window_derived.push(d);
        let old_off = i64::from(cursor) - delta;
        if old_off < i64::from(edit.range.end) {
            continue;
        }
        let old_off = old_off as u32;
        if let Ok(k) = prev.lines.binary_search_by_key(&old_off, |l| l.span.start) {
            if prev.lines[k].entry_delimiter == delim {
                converged = Some(k);
                break;
            }
        }
    }
    stats.bytes_scanned = cursor - resume_byte;
    stats.converged = converged.is_some();
    let converge_new_line = resume_line + window.len();

    // Physical-line shift for everything after the convergence point.
    let line_delta = converged.map_or(0, |k| {
        i64::from(li.line_of(cursor)) - i64::from(prev.lines[k].first_line)
    });
    let end_delimiter = match converged {
        Some(_) => prev.end_delimiter,
        None => delim,
    };
    // New line index `i` is old line index `i - idx_shift`, for `i >= converge`.
    let idx_shift = converged.map(|k| converge_new_line as i64 - k as i64);

    let Segmentation {
        src: old_src,
        mut regions,
        lines: prev_lines,
        mut derived,
        markers: prev_markers,
        sections: prev_sections,
        mut diags,
        line_index: _,
        end_delimiter: _,
        src_len: _,
        options: opts,
        hash_counts: mut counts,
    } = prev;

    // A closed brace block is a legal boundary only because the else-chain
    // lookahead is the one thing that can still extend it, and that is decidable
    // here: if the first code line after it now begins `else`, the two belong to
    // one region and the boundary was never real.
    if opts.join_else_chains && matches!(regions[rk].kind, RegionShape::Brace { .. }) {
        let else_follows = match window.iter().position(|l| !l.is_trivia) {
            Some(i) => head::starts_with_else(new_src, &window[i], window_derived[i].as_deref()),
            // These lines are still in OLD coordinates — they have not been
            // shifted yet — so they must be read against the old buffer.
            None => converged
                .and_then(|k| {
                    prev_lines[k..]
                        .iter()
                        .position(|l| !l.is_trivia)
                        .map(|i| (k + i, &prev_lines[k + i]))
                })
                .is_some_and(|(i, l)| head::starts_with_else(old_src, l, derived[i].as_deref())),
        };
        if else_follows {
            return (segment_with(new_src, &opts), ResegmentStats::default());
        }
    }

    // ---- phase 2: regroup from the resume point, splicing on convergence ----
    // Markers are recomputed only for the rescanned run; the ones before and
    // after it are carried over with their spans moved. Rescanning all of them
    // would mean streaming the whole line vector — the single largest avoidable
    // cost on the keystroke path, for a feature most documents do not use.
    let mut raw_markers: Vec<marker::RawMarker> = prev_markers
        .iter()
        .take_while(|m| m.span.end <= resume_byte)
        .map(|m| (m.span, m.line, m.title.clone()))
        .collect();
    marker::scan_range(new_src, &window, &mut raw_markers);
    if let Some(k) = converged {
        let from = prev_lines[k].span.start;
        raw_markers.extend(
            prev_markers
                .iter()
                .filter(|m| m.span.start >= from)
                .map(|m| {
                    let mut span = m.span;
                    shift_span(&mut span, delta as u32);
                    (
                        span,
                        (i64::from(m.line) + line_delta) as u32,
                        m.title.clone(),
                    )
                }),
        );
    }
    let (markers, sections) = marker::finish(raw_markers, new_src.len() as u32, li.line_count());

    // ---- phase 3: splice the rescanned window into the line vector ---------
    // Nothing else in the vector is rewritten. The lines before the resume point
    // did not move at all; the lines from the convergence point on moved by
    // `delta` bytes and `line_delta` physical lines, and that is six `u32`s
    // each. `LogicalLine` deliberately does not borrow the source (see its doc
    // comment) — when it did, this phase had to rebuild every line in the
    // document to re-point it at the new buffer, and that alone was 40 % of the
    // whole keystroke path.
    let conv_k = converged.unwrap_or(prev_lines.len());
    let mut lines = prev_lines;
    // The spliced-text table runs parallel to the line vector and holds no
    // coordinates at all (its piece tables are relative — see [`Derived`]), so
    // it takes the same splice and needs no rebase: one `memmove` of pointers.
    derived
        .splice(resume_line..conv_k, window_derived)
        .for_each(drop);
    let (d, ld) = (delta as u32, line_delta as u32);
    if delta != 0 || line_delta != 0 {
        splice_rebase(&mut lines, resume_line..conv_k, window, |l| {
            l.shift(d, ld);
        });
    } else {
        splice_rebase(&mut lines, resume_line..conv_k, window, |_| {});
    }

    let mut g = Grouper::new(new_src, &lines, &derived, &opts, 16, keep as u32);
    let mut splice_at: Option<usize> = None;
    {
        let mut stop = |line_idx: usize| -> bool {
            let Some(sh) = idx_shift else { return false };
            if line_idx < converge_new_line {
                return false;
            }
            let old_line = (line_idx as i64 - sh) as u32;
            match regions.binary_search_by_key(&old_line, |r| r.logical_lines.start) {
                Ok(ri) => {
                    splice_at = Some(ri);
                    true
                }
                Err(_) => false,
            }
        };
        g.group(resume_line, &mut stop);
    }
    let Grouper {
        regions: mut fresh,
        diags: window_diags,
        counts: _,
        ..
    } = g;
    stats.rescanned = fresh.len() as u32;

    let ri = splice_at.unwrap_or(regions.len());
    stats.reused_suffix = (regions.len() - ri) as u32;

    // Did the rescan disturb any `hash_ordinal` outside the rescanned run? Only
    // if a hash whose occurrence count changed ALSO occurs outside it. Both
    // sides are small, so this is a handful of lookups rather than a pass over
    // the document — and `hash_counts`, which the new segmentation inherits
    // wholesale, is what makes the question answerable without one.
    let mut mid_old: FxHashMap<CodeHash, u32> = FxHashMap::default();
    for r in &regions[keep..ri] {
        *mid_old.entry(r.code_hash).or_insert(0) += 1;
    }
    let mut mid_new: FxHashMap<CodeHash, u32> = FxHashMap::default();
    for r in &fresh {
        *mid_new.entry(r.code_hash).or_insert(0) += 1;
    }
    // The freshly grouped regions were numbered from 0 within the window. That
    // is already the answer unless the same hash occurs before the window, which
    // `hash_counts` reports without touching a region.
    let mut base: FxHashMap<CodeHash, u32> = FxHashMap::default();
    for h in mid_new.keys().chain(mid_old.keys()) {
        if counts.get(h).copied().unwrap_or(0) > mid_old.get(h).copied().unwrap_or(0) {
            base.entry(*h).or_insert(0);
        }
    }
    if !base.is_empty() {
        for r in &regions[..keep] {
            if let Some(c) = base.get_mut(&r.code_hash) {
                *c += 1;
            }
        }
        for r in &mut fresh {
            if let Some(b) = base.get(&r.code_hash) {
                r.hash_ordinal += *b;
            }
        }
    }
    // How every `hash_ordinal` AFTER the window moves. A region's ordinal is the
    // number of earlier regions with its hash, and the only thing the rescan
    // changed is how many of those the window holds — so the suffix is renumbered
    // by one signed delta per hash, applied in the pass that shifts it anyway.
    //
    // The pre-audit spelling recomputed every ordinal in the document through a
    // fresh hash map whenever a duplicated hash moved. This corpus has ~7 100
    // byte-identical `foreach` blocks, so an edit inside one of them took that
    // path: 500 µs, twice the whole A25 budget, to renumber 42 690 regions
    // because one of them changed.
    let mut ord_delta: FxHashMap<CodeHash, i64> = FxHashMap::default();
    for (h, n) in &mid_new {
        *ord_delta.entry(*h).or_insert(0) += i64::from(*n);
    }
    for (h, n) in &mid_old {
        *ord_delta.entry(*h).or_insert(0) -= i64::from(*n);
    }
    // A hash the window invented, or one that lived only inside it, renumbers
    // nothing: `base` already holds exactly the hashes that also occur outside
    // the window. Without this the three `di` regions of a keystroke would put a
    // map probe on all 40 000 suffix regions to discover that none of them match.
    ord_delta.retain(|h, d| *d != 0 && base.contains_key(h));
    stats.ordinals_repaired = !ord_delta.is_empty();

    // `hash_counts` for the whole new document is the old one minus the regions
    // the rescan replaced plus the ones it produced. Rebuilding it by walking the
    // reused suffix would be one hash-map insert per region of the document, per
    // keystroke — measurably the largest single cost in this function before it
    // was written this way.
    for (h, n) in &mid_old {
        if let Some(c) = counts.get_mut(h) {
            *c = c.saturating_sub(*n);
        }
    }
    for (h, n) in &mid_new {
        *counts.entry(*h).or_insert(0) += *n;
    }
    // Zero counts must not survive: `hash_counts` is asked "does this hash occur
    // outside the rescanned run", and a stale zero would answer yes. Only a hash
    // the rescan REMOVED can have reached zero, and only if the rescan did not
    // also put it back — so the question is asked of those few keys by name.
    // `retain` would answer it by walking a map with one entry per distinct
    // region in the document: 37 µs on a 2 MB file, measured, an eighth of the
    // A25 budget spent proving that nothing needed removing. Nor is it enough to
    // watch for a zero while subtracting: the overwhelmingly common rescan
    // re-emits the region it replaced unchanged, which drives its count to zero
    // and straight back to one.
    for h in mid_old.keys() {
        if counts.get(h).is_some_and(|c| *c == 0) {
            counts.remove(h);
        }
    }

    // ---- phase 4: move the reused suffix, in place --------------------------
    let diag_prefix = regions[rk].diags.end as usize;
    let diag_suffix_start = if ri < regions.len() {
        regions[ri].diags.start as usize
    } else {
        diags.len()
    };
    for r in &mut fresh {
        r.diags.start += diag_prefix as u32;
        r.diags.end += diag_prefix as u32;
    }
    let diag_base = (diag_prefix + window_diags.len()) as i64 - diag_suffix_start as i64;
    for d in &mut diags[diag_suffix_start..] {
        if let Some(s) = d.span.as_mut() {
            shift_span(s, delta as u32);
        }
        for rel in &mut d.related {
            shift_span(&mut rel.span, delta as u32);
        }
    }

    let region_base = (keep + fresh.len()) as i64 - ri as i64;
    let line_base = idx_shift.unwrap_or(0);
    let renumber = !ord_delta.is_empty();

    diags
        .splice(diag_prefix..diag_suffix_start, window_diags)
        .for_each(drop);
    // Two spellings of one rebase. `ord_delta` is empty for every edit that did
    // not change how many times some hash occurs OUTSIDE the rescanned window —
    // which is nearly all of them — and `diag_base` is zero for every document
    // with no diagnostics, so the common path is written without either. Hoisting
    // the test out of the loop rather than branching inside it is what lets the
    // rebase stay eight adds on a value already in L1.
    let (db, lb, ld, d) = (
        diag_base as u32,
        line_base as u32,
        line_delta as u32,
        delta as u32,
    );
    let rb = region_base as u32;
    if renumber || diag_base != 0 {
        splice_rebase(&mut regions, keep..ri, fresh, |r| {
            r.index = r.index.wrapping_add(rb);
            if renumber {
                if let Some(d) = ord_delta.get(&r.code_hash) {
                    r.hash_ordinal = (i64::from(r.hash_ordinal) + d) as u32;
                }
            }
            shift_span(&mut r.span, d);
            shift_span(&mut r.outer_span, d);
            shift_lines(&mut r.lines, ld);
            shift_lines(&mut r.code_lines, ld);
            shift_idx(&mut r.logical_lines, lb);
            shift_idx(&mut r.diags, db);
        });
    } else {
        splice_rebase(&mut regions, keep..ri, fresh, |r| {
            r.index = r.index.wrapping_add(rb);
            shift_span(&mut r.span, d);
            shift_span(&mut r.outer_span, d);
            shift_lines(&mut r.lines, ld);
            shift_lines(&mut r.code_lines, ld);
            shift_idx(&mut r.logical_lines, lb);
        });
    }

    let mut seg = Segmentation {
        src: new_src,
        regions,
        lines,
        derived,
        markers,
        sections,
        diags,
        line_index: li,
        end_delimiter,
        src_len: new_src.len() as u32,
        options: opts,
        hash_counts: counts,
    };
    assign_sections(&mut seg, !prev_sections.is_empty());
    (seg, stats)
}

/// A vector element `splice_rebase` can grow a hole with. The value is never
/// observed: every slot it creates is overwritten or truncated away.
pub(crate) trait Vacant: Sized {
    fn vacant() -> Self;
}

impl Vacant for Region {
    fn vacant() -> Self {
        Region::vacant()
    }
}

/// Elements moved per `copy_within` in [`splice_rebase_copy`].
///
/// 1024 `Region`s is 88 KB, which is what the moved run and the rebase pass
/// over it have to share of a 128 KB L1. Measured on the 2 MB region tail:
/// element-wise 129 µs, chunked at 16/64/256/1024 elements 127/115/110/108 µs,
/// and move-then-rebase in two full passes 148 µs. Below ~256 the `memmove`
/// call stops being amortised; above L1 the rebase re-reads from L2 and the
/// number walks back towards the two-pass one.
const REBASE_CHUNK: usize = 1024;

/// Splice `new` into `v[range]` and rebase everything after it.
///
/// **The move is `memmove`'s job and the rebase is done a chunk behind it.**
/// The obvious spelling — lift each element out of its slot, add `delta` to its
/// coordinates, put it down `k` slots along — cannot use wide loads and stores,
/// because it has to modify eight `u32` fields on the way past. The `tail/*`
/// bench is the A/B, on the 2 MB region tail: element-wise 116 µs,
/// `copy_within` plus a rebase pass over the whole tail 138 µs (that second pass
/// re-reads 4 MB from L2), and `copy_within` plus a rebase of each chunk while
/// it is still in L1 114 µs. The margin was 20 % when `Region` was 96 bytes and
/// the rebase went through `i64`; it is small now, and the reason to keep the
/// chunked form is that it is the shape that does not degrade as the element
/// grows. "Rebase everything, then splice" is worse than all three: it touches
/// every tail element twice AND still leaves the move to `Vec::splice`.
///
/// This is why [`Region`] and [`LogicalLine`] are both `Copy` — an element with
/// drop glue cannot be bit-moved in safe code, and the line vector's own move
/// went from 159 µs to 92 µs when its `Box` moved out to [`DerivedText`].
fn splice_rebase<T: Copy + Vacant>(
    v: &mut Vec<T>,
    range: Range<usize>,
    new: Vec<T>,
    mut rebase: impl FnMut(&mut T),
) {
    let old_len = v.len();
    let (removed, added) = (range.end - range.start, new.len());
    if added >= removed {
        let k = added - removed;
        if k == 0 {
            for e in &mut v[range.end..] {
                rebase(e);
            }
        } else {
            v.resize(old_len + k, T::vacant());
            // Downwards: a chunk's destination is above every slot still to be
            // read, so nothing is overwritten before it is moved.
            let mut hi = old_len;
            while hi > range.end {
                let lo = hi.saturating_sub(REBASE_CHUNK).max(range.end);
                v.copy_within(lo..hi, lo + k);
                for e in &mut v[lo + k..hi + k] {
                    rebase(e);
                }
                hi = lo;
            }
        }
    } else {
        let k = removed - added;
        let mut lo = range.end;
        while lo < old_len {
            let hi = (lo + REBASE_CHUNK).min(old_len);
            v.copy_within(lo..hi, lo - k);
            for e in &mut v[lo - k..hi - k] {
                rebase(e);
            }
            lo = hi;
        }
        v.truncate(old_len - k);
    }
    for (slot, val) in v[range.start..].iter_mut().zip(new) {
        *slot = val;
    }
}

/// May `regions[..=r]` be reused verbatim when the text after `r` changes?
///
/// Only if `r`'s own extent cannot grow. Three ways it can:
///
/// * **A region that ends at end of source.** Its last logical line was
///   terminated by EOF, not by a newline or a `;`, so appending extends that
///   very line.
/// * **`Trivia`.** Two comment runs separated by nothing merge into one, and a
///   comment run directly above a new command attaches to it (02 §5.2(a)).
/// * **`Unterminated`.** By construction it ran to end of source.
///
/// `Brace` is admitted with a caveat the caller checks: the else-chain lookahead
/// can extend it, and that is decided after the rescan. `Simple`, `Directive`
/// and a closed `EndBlock` end at a terminator inside their own span and are
/// unconditionally safe.
fn is_reusable_boundary(r: &Region, src_len: u32) -> bool {
    r.outer_span.end < src_len
        && matches!(
            r.kind,
            RegionShape::Simple
                | RegionShape::Directive { .. }
                | RegionShape::EndBlock { .. }
                | RegionShape::Brace { .. }
        )
}

// Two's complement: a negative delta added as a wrapped `u32` IS the
// subtraction, and no coordinate a legal edit moves can go below zero. The i64
// round trip these replaced sign-extended and truncated around every one of the
// eleven adds a region needs, on all 42 000 regions a keystroke moves.
#[inline]
fn shift_span(s: &mut Span, delta: u32) {
    s.start = s.start.wrapping_add(delta);
    s.end = s.end.wrapping_add(delta);
}

#[inline]
fn shift_idx(r: &mut IdxRange, delta: u32) {
    r.start = r.start.wrapping_add(delta);
    r.end = r.end.wrapping_add(delta);
}

#[inline]
fn shift_lines(l: &mut LineRange, delta: u32) {
    l.start = l.start.wrapping_add(delta);
    l.end = l.end.wrapping_add(delta);
}

/// Assign `Region::section` from the `%%` markers. Skipped entirely when the
/// document has no markers, which is the overwhelmingly common case — and
/// `Region::section` is already `None` for a freshly grouped region, so there is
/// nothing to clear.
fn assign_sections(seg: &mut Segmentation<'_>, may_be_stale: bool) {
    if seg.sections.is_empty() {
        if may_be_stale {
            // Reused regions carry the section they had before the edit. If the
            // edit deleted the last marker they must be cleared — but only then,
            // which is why this is a flag and not an unconditional pass.
            for r in &mut seg.regions {
                r.section = Region::NO_SECTION;
            }
        }
        return;
    }
    let mut s = 0usize;
    for r in &mut seg.regions {
        while s + 1 < seg.sections.len() && seg.sections[s + 1].span.start <= r.outer_span.start {
            s += 1;
        }
        r.section = if seg.sections[s].span.start <= r.outer_span.start {
            seg.sections[s].id.0
        } else {
            Region::NO_SECTION
        };
    }
}

// ---------------------------------------------------------------------------
// The grouping algorithm — 02 §5.2, transcribed.
// ---------------------------------------------------------------------------

struct Grouper<'a, 's> {
    /// The buffer `lines` index into; `LogicalLine` does not carry it.
    src: &'s str,
    lines: &'a [LogicalLine],
    /// Parallel to `lines` — see [`DerivedText`].
    derived: &'a [DerivedText],
    opts: &'a SegmentOptions,
    table: CommandTable,
    regions: Vec<Region>,
    diags: Vec<Diagnostic>,
    counts: FxHashMap<CodeHash, u32>,
    /// `Region::index` of the first region this grouper emits. Non-zero on the
    /// keystroke path, where the regions before the edit are never handed to the
    /// grouper at all — they are already in the vector the result is built in.
    index_base: u32,
    /// Reused across regions; see [`code_hash_into`].
    hash_buf: Vec<u8>,
}

impl<'a, 's> Grouper<'a, 's> {
    fn new(
        src: &'s str,
        lines: &'a [LogicalLine],
        derived: &'a [DerivedText],
        opts: &'a SegmentOptions,
        capacity: usize,
        index_base: u32,
    ) -> Self {
        Self {
            src,
            lines,
            derived,
            opts,
            table: CommandTable::core(),
            regions: Vec::with_capacity(capacity),
            diags: Vec::new(),
            counts: FxHashMap::default(),
            index_base,
            hash_buf: Vec::with_capacity(512),
        }
    }

    /// Group `lines[from..]`, stopping early when `stop(line_index)` returns true
    /// at a region boundary.
    fn group(&mut self, from: usize, stop: &mut dyn FnMut(usize) -> bool) {
        let n = self.lines.len();
        let mut i = from;
        while i < n {
            if stop(i) {
                return;
            }

            // (0) The scanner ran off the end of the source inside a construct
            // that can never close. Checked first: an unterminated `/*` produces
            // a line with no code at all, which would otherwise look like trivia
            // and hide the fact that the rest of the file was swallowed.
            if let Some(u) = self.lines[i].open_at_end {
                self.emit(i..n, i, RegionShape::Unterminated { expected: u });
                i = n;
                continue;
            }

            // (a) trivia run
            if self.lines[i].is_trivia {
                let mut j = i;
                while j < n && self.lines[j].is_trivia {
                    j += 1;
                }
                let k = if self.opts.attach_leading_comments && j < n {
                    self.attach_start(i, j)
                } else {
                    j
                };
                if k > i {
                    self.emit(
                        i..k,
                        i,
                        RegionShape::Trivia {
                            has_marker: self.has_marker(i..k),
                        },
                    );
                }
                if k == j {
                    i = j;
                } else {
                    // lines[k..j] attach to the region that starts at j.
                    i = self.emit_command(k, j, n);
                }
                continue;
            }

            i = self.emit_command(i, i, n);
        }
    }

    /// Emit the region whose first CODE line is `code_at`, with `first..code_at`
    /// already known to be attachable leading comments. Returns the next line.
    fn emit_command(&mut self, first: usize, code_at: usize, n: usize) -> usize {
        // `self.lines` is a shared slice reference and therefore `Copy`; taking a
        // local copy keeps the `&mut self` calls below out of a borrow conflict.
        let lines = self.lines;
        // The source ran out inside a string-ish construct that can never close.
        // Checked here as well as at the top of `group`, because a trivia run
        // that attaches to this command jumps straight in.
        if let Some(u) = lines[code_at].open_at_end {
            self.emit(first..n, code_at, RegionShape::Unterminated { expected: u });
            return n;
        }
        let line = &lines[code_at];
        let dline = self.derived[code_at].as_deref();
        let head = head::parse(self.src, line, dline, &self.table);
        let info = head::info_of(&head);

        // (b) `end`-terminated block
        if let Some((opener, _name)) =
            head::end_block_opener(self.src, line, dline, &head, &self.table)
        {
            let mut j = code_at + 1;
            let mut depth = 0i32;
            while j < n {
                if depth == 0 && is_bare_end(self.src, &lines[j], self.derived[j].as_deref()) {
                    break;
                }
                depth += lines[j].brace_delta;
                j += 1;
            }
            if j == n {
                self.emit(
                    first..n,
                    code_at,
                    RegionShape::Unterminated {
                        expected: Unterminated::End,
                    },
                );
                return n;
            }
            self.emit_with(
                first..j + 1,
                code_at,
                RegionShape::EndBlock { opener },
                Some(info),
            );
            return j + 1;
        }

        // (c) brace block
        let mut depth = line.brace_delta;
        if depth > 0 {
            let mut j = code_at;
            while depth > 0 && j + 1 < n {
                j += 1;
                depth += lines[j].brace_delta;
            }
            if depth > 0 {
                self.emit(
                    first..n,
                    code_at,
                    RegionShape::Unterminated {
                        expected: Unterminated::CloseBrace,
                    },
                );
                return n;
            }
            if self.opts.join_else_chains {
                loop {
                    let mut k = j + 1;
                    while k < n && lines[k].is_trivia {
                        k += 1;
                    }
                    if k < n
                        && head::starts_with_else(self.src, &lines[k], self.derived[k].as_deref())
                    {
                        let mut d = lines[k].brace_delta;
                        j = k;
                        while d > 0 && j + 1 < n {
                            j += 1;
                            d += lines[j].brace_delta;
                        }
                        if d > 0 {
                            self.emit(
                                first..n,
                                code_at,
                                RegionShape::Unterminated {
                                    expected: Unterminated::CloseBrace,
                                },
                            );
                            return n;
                        }
                        continue;
                    }
                    break;
                }
            }
            let opener = head::brace_opener(self.src, line, dline, &head, &self.table);
            self.emit_with(
                first..j + 1,
                code_at,
                RegionShape::Brace { opener },
                Some(info),
            );
            return j + 1;
        }

        // (d) directive
        if let Some(directive) = line.directive {
            self.emit_with(
                first..code_at + 1,
                code_at,
                RegionShape::Directive { directive },
                Some(info),
            );
            return code_at + 1;
        }

        // (e) simple command
        self.emit_with(first..code_at + 1, code_at, RegionShape::Simple, Some(info));
        code_at + 1
    }

    /// The first line of the maximal run of comment lines directly above the
    /// command at `j` — 02 §5.2(a). A blank line, or a `%%` cell marker, ends the
    /// run: a marker always begins a new group, and a blank line means the
    /// comment is about the file, not about the command below it.
    fn attach_start(&self, i: usize, j: usize) -> usize {
        let mut k = j;
        while k > i {
            let cand = &self.lines[k - 1];
            let below = self.lines[k].first_line;
            if cand.is_blank || cand.is_cell_marker || cand.last_line + 1 != below {
                break;
            }
            k -= 1;
        }
        k
    }

    fn has_marker(&self, r: Range<usize>) -> bool {
        self.lines[r].iter().any(|l| l.is_cell_marker)
    }

    fn emit(&mut self, run: Range<usize>, code_at: usize, kind: RegionShape) {
        self.emit_with(run, code_at, kind, None);
    }

    fn emit_with(
        &mut self,
        run: Range<usize>,
        code_at: usize,
        kind: RegionShape,
        head: Option<HeadInfo>,
    ) {
        let first = run.start;
        let last = run.end - 1;
        let outer_span = Span {
            start: self.lines[first].span.start,
            end: self.lines[last].span.end,
        };
        // A comment run that ends in an unterminated `/*` is not trivia: it
        // swallowed the rest of the file, and showing it as "just comments"
        // hides that from the gutter.
        let kind = match (self.lines[last].open_at_end, &kind) {
            (Some(expected), RegionShape::Trivia { .. }) => RegionShape::Unterminated { expected },
            _ => kind,
        };
        let trivia = matches!(kind, RegionShape::Trivia { .. });
        let span = if trivia {
            outer_span
        } else {
            let end = self.lines[code_at..run.end]
                .iter()
                .rev()
                .find(|l| !l.is_trivia)
                .map_or(self.lines[code_at].code_span, |l| l.code_span)
                .end;
            let start = self.lines[code_at].code_span.start;
            // An `Unterminated` region can have no code at all (a file that is
            // one unclosed `/*`). An empty executable extent would make "which
            // region is the cursor in" unanswerable, so it falls back to the
            // outer span exactly as trivia does.
            if end > start {
                Span { start, end }
            } else {
                outer_span
            }
        };
        let code_lines = if span == outer_span {
            LineRange {
                start: self.lines[first].first_line,
                end: self.lines[last].last_line + 1,
            }
        } else {
            LineRange {
                start: self.lines[code_at].code_first_line,
                end: self.lines[code_at..run.end]
                    .iter()
                    .rev()
                    .find(|l| !l.is_trivia)
                    .map_or(self.lines[code_at].code_last_line, |l| l.code_last_line)
                    + 1,
            }
        };
        let head = match head {
            Some(h) if !trivia => h,
            _ if trivia => HeadInfo::default(),
            // The `Unterminated` paths reach here without a parsed head.
            _ => head::info(
                self.src,
                &self.lines[code_at],
                self.derived[code_at].as_deref(),
                &self.table,
            ),
        };
        let hash = {
            let mut buf = std::mem::take(&mut self.hash_buf);
            let h = code_hash_into(
                self.src,
                &self.lines[run.clone()],
                &self.derived[run.clone()],
                &mut buf,
            );
            self.hash_buf = buf;
            h
        };
        let ordinal = {
            let slot = self.counts.entry(hash).or_insert(0);
            let v = *slot;
            *slot += 1;
            v
        };
        let diags_from = self.diags.len() as u32;
        if let RegionShape::Unterminated { expected } = kind {
            self.push_diag(expected, span);
        }
        self.regions.push(Region {
            index: self.index_base + self.regions.len() as u32,
            span,
            outer_span,
            lines: LineRange {
                start: self.lines[first].first_line,
                end: self.lines[last].last_line + 1,
            },
            code_lines,
            kind,
            entry_delimiter: self.lines[code_at].entry_delimiter,
            exit_delimiter: self.lines[last].exit_delimiter,
            head,
            code_hash: hash,
            hash_ordinal: ordinal,
            logical_lines: IdxRange {
                start: first as u32,
                end: run.end as u32,
            },
            diags: IdxRange {
                start: diags_from,
                end: self.diags.len() as u32,
            },
            section: Region::NO_SECTION,
        });
    }

    fn push_diag(&mut self, expected: Unterminated, span: Span) {
        if self.diags.len() >= self.opts.max_diagnostics {
            return;
        }
        let (code, message) = match expected {
            Unterminated::BlockComment => (
                "PARSE0001",
                "unterminated /* comment: it swallows the rest of the file",
            ),
            Unterminated::CompoundQuote => (
                "PARSE0002",
                "unterminated compound double quote: expected \"'",
            ),
            Unterminated::CloseBrace => ("PARSE0003", "block is never closed: expected }"),
            Unterminated::End => ("PARSE0004", "block is never closed: expected end"),
        };
        self.diags.push(Diagnostic {
            severity: Severity::Error,
            code: code.to_owned(),
            stata_rc: None,
            message: message.to_owned(),
            file: None,
            span: Some(span),
            offending_token: None,
            block: None,
            related: Vec::new(),
            suggestions: Vec::new(),
            notes: Vec::new(),
            confidence: Confidence::Exact,
        });
    }
}

/// `is_bare_end` of 02 §5.2(b): the line is exactly `end`, at brace depth 0.
fn is_bare_end(src: &str, l: &LogicalLine, d: Option<&Derived>) -> bool {
    l.trimmed(src, d) == "end"
}

// ---------------------------------------------------------------------------
// Head parsing — the prefix chain, the command word, and the two table lookups
// segmentation is allowed to make (02 §5.2 note (b), §5.3).
//
// This lives inside `region.rs` rather than in a file of its own because the
// unit's Owns list names exactly five files under `scan/`. It is the ONLY place
// segmentation consults a command table, and it never looks past the first word
// (plus the second for `program`), which is what keeps `segment()` free of the
// parser.
// ---------------------------------------------------------------------------

mod head {
    use std::ops::Range;

    use stratum_proto::{BraceOpener, EndBlockOpener};

    use super::{HeadInfo, PrefixChain};
    use crate::ast::PrefixKind;
    use crate::cmdsig::{CmdFlags, CmdId, CommandSig, CommandTable};
    use crate::scan::logical::{Derived, LogicalLine};

    /// v2 prefix commands that ALWAYS require a colon. Kept explicit: treating
    /// "any word followed by `:`" as a prefix would swallow `mata:`, which opens
    /// an `end` block rather than prefixing a command.
    const GENERIC_PREFIXES: &[&str] = &[
        "bayes",
        "bootstrap",
        "fmm",
        "fp",
        "jackknife",
        "mfp",
        "nestreg",
        "permute",
        "rolling",
        "simulate",
        "statsby",
        "stepwise",
        "svy",
        "xi",
    ];

    /// What the head of a line is made of, in `code` byte offsets.
    pub struct Head {
        pub prefixes: PrefixChain,
        /// The command word. Empty when the line starts with `{` or punctuation.
        pub word: Range<usize>,
        /// The signature `word` resolves to. Resolved once here rather than
        /// again in `info_of` and again in `end_block_opener`: the table lookup
        /// is a binary search with a string compare per level and it runs once
        /// per region.
        pub sig: Option<&'static CommandSig>,
        /// `sig`'s row id — what a `HeadInfo` stores.
        pub id: Option<CmdId>,
        /// A macro reference sits in the command position.
        pub has_macro: bool,
    }

    /// Split the prefix chain off the command word.
    pub fn parse(src: &str, line: &LogicalLine, d: Option<&Derived>, table: &CommandTable) -> Head {
        let code = line.code(src, d);
        let b = code.as_bytes();
        let mut prefixes = PrefixChain::default();
        let mut p = 0usize;

        loop {
            p = skip_ws(b, p);
            if p >= b.len() {
                return Head {
                    prefixes,
                    word: p..p,
                    sig: None,
                    id: None,
                    has_macro: false,
                };
            }
            if b[p] == b'`' || b[p] == b'$' {
                let e = macro_end(b, p);
                return Head {
                    prefixes,
                    word: p..e,
                    sig: None,
                    id: None,
                    has_macro: true,
                };
            }
            let we = word_end(b, p);
            if we == p {
                return Head {
                    prefixes,
                    word: p..p,
                    sig: None,
                    id: None,
                    has_macro: false,
                };
            }
            let w = &code[p..we];
            let id = table.canonical_id(w);
            let sig = id.map(|i| table.get(i));
            let Some(kind) = prefix_kind(w, sig) else {
                return Head {
                    prefixes,
                    word: p..we,
                    sig,
                    id,
                    has_macro: false,
                };
            };
            match kind {
                // A colon is mandatory. Without one the word is the command:
                // `by` alone is not a prefix, it is a syntax error the parser
                // reports, and mis-labelling it here would hide that.
                PrefixKind::By | PrefixKind::Frame | PrefixKind::Generic => {
                    match find_colon(b, we) {
                        Some(c) => {
                            prefixes.push(kind);
                            p = c + 1;
                        }
                        None => {
                            return Head {
                                prefixes,
                                word: p..we,
                                sig,
                                id,
                                has_macro: false,
                            }
                        }
                    }
                }
                PrefixKind::Version => {
                    let q = skip_ws(b, we);
                    let ve = version_end(b, q);
                    let q2 = skip_ws(b, ve);
                    if q2 < b.len() && b[q2] == b':' {
                        prefixes.push(kind);
                        p = q2 + 1;
                    } else {
                        return Head {
                            prefixes,
                            word: p..we,
                            sig,
                            id,
                            has_macro: false,
                        };
                    }
                }
                // `capture`, `quietly`, `noisily` may omit the colon
                // ([U] 11.1.10) — and `capture {` is the command itself.
                _ => {
                    let q = skip_ws(b, we);
                    if q < b.len() && b[q] == b':' {
                        prefixes.push(kind);
                        p = q + 1;
                    } else if q >= b.len() || b[q] == b'{' {
                        return Head {
                            prefixes,
                            word: p..we,
                            sig,
                            id,
                            has_macro: false,
                        };
                    } else {
                        prefixes.push(kind);
                        p = q;
                    }
                }
            }
        }
    }

    /// The `HeadInfo` a region carries.
    pub fn info(
        src: &str,
        line: &LogicalLine,
        d: Option<&Derived>,
        table: &CommandTable,
    ) -> HeadInfo {
        info_of(&parse(src, line, d, table))
    }

    /// [`info`] over an already-parsed head. Segmentation parses the head once
    /// per region and uses it for the opener tests as well.
    pub fn info_of(h: &Head) -> HeadInfo {
        let resolved = !(h.has_macro || h.word.is_empty());
        let mut info = HeadInfo::new(
            resolved.then_some(h.sig).flatten(),
            resolved.then_some(h.id).flatten(),
            h.has_macro,
        );
        info.prefixes = h.prefixes;
        info
    }

    /// 02 §5.3's table. Prefixes are skipped first, so `quietly program define
    /// foo` opens a block.
    pub fn end_block_opener(
        src: &str,
        line: &LogicalLine,
        d: Option<&Derived>,
        h: &Head,
        _table: &CommandTable,
    ) -> Option<(EndBlockOpener, Option<String>)> {
        if h.has_macro || h.word.is_empty() {
            return None;
        }
        let code = line.code(src, d);
        let sig = h.sig?;
        if !sig.flags.contains(CmdFlags::BLOCK_END) {
            return None;
        }
        let tail = code[h.word.end..].trim_matches(|c: char| c.is_ascii_whitespace());
        match sig.canonical {
            "program" => {
                let (w2, rest) = split_word(tail);
                // `program drop`, `program dir` and `program list` are queries,
                // not definitions.
                if matches!(w2, "drop" | "dir" | "list") {
                    return None;
                }
                let name = if matches!(w2, "define" | "defin" | "defi" | "def") {
                    split_word(rest).0
                } else {
                    w2
                };
                Some((
                    EndBlockOpener::Program,
                    (!name.is_empty()).then(|| name.to_owned()),
                ))
            }
            // `input using` reads a file and does not consume following lines.
            "input" => (!tail.split_ascii_whitespace().any(|w| w == "using"))
                .then_some((EndBlockOpener::Input, None)),
            // `mata: x = 1` is a one-liner; a bare `mata` or `mata:` opens.
            "mata" | "python" | "java" => {
                let tail = tail.strip_prefix(':').unwrap_or(tail);
                if !tail
                    .trim_matches(|c: char| c.is_ascii_whitespace())
                    .is_empty()
                {
                    return None;
                }
                let opener = match sig.canonical {
                    "mata" => EndBlockOpener::Mata,
                    "python" => EndBlockOpener::Python,
                    _ => EndBlockOpener::Java,
                };
                Some((opener, None))
            }
            _ => None,
        }
    }

    /// Which brace opener a `{`-block is. `capture {`, `quietly {`, `noisily {`
    /// and a bare `{` all fall out of the same rule (02 §5.2 note (c)).
    pub fn brace_opener(
        src: &str,
        line: &LogicalLine,
        d: Option<&Derived>,
        h: &Head,
        _table: &CommandTable,
    ) -> BraceOpener {
        if h.word.is_empty() {
            return BraceOpener::Anonymous;
        }
        if h.has_macro {
            return BraceOpener::Other;
        }
        let w = &line.code(src, d)[h.word.clone()];
        if w == "if" || w == "else" {
            return BraceOpener::IfElseChain;
        }
        match h.sig.map(|s| s.canonical) {
            Some("foreach") => BraceOpener::Foreach,
            Some("forvalues") => BraceOpener::Forvalues,
            Some("while") => BraceOpener::While,
            Some("capture") => BraceOpener::Capture,
            Some("quietly") => BraceOpener::Quietly,
            Some("noisily") => BraceOpener::Noisily,
            _ => BraceOpener::Other,
        }
    }

    /// The `} ⏎ else { … }` layout Stata code actually uses. Joining is required:
    /// executing `else { … }` on its own is an error.
    pub fn starts_with_else(src: &str, line: &LogicalLine, d: Option<&Derived>) -> bool {
        let t = line.trimmed(src, d);
        let t = t.strip_prefix('}').unwrap_or(t);
        let t = t.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let Some(rest) = t.strip_prefix("else") else {
            return false;
        };
        rest.is_empty()
            || rest.starts_with(|c: char| c.is_ascii_whitespace() || c == '{' || c == '(')
    }

    /// The table lookup is done first because it is one binary search, whereas
    /// the two literal lists below are up to sixteen string compares — and the
    /// overwhelmingly common word here is an ordinary command that is in the
    /// table and is not a prefix.
    fn prefix_kind(w: &str, sig: Option<&'static CommandSig>) -> Option<PrefixKind> {
        let Some(sig) = sig else {
            return match w {
                "version" => Some(PrefixKind::Version),
                "frame" | "frames" => Some(PrefixKind::Frame),
                _ => GENERIC_PREFIXES.contains(&w).then_some(PrefixKind::Generic),
            };
        };
        if !sig.flags.contains(CmdFlags::PREFIX) {
            return None;
        }
        Some(match sig.canonical {
            "by" | "bysort" => PrefixKind::By,
            "capture" => PrefixKind::Capture,
            "quietly" => PrefixKind::Quietly,
            "noisily" => PrefixKind::Noisily,
            _ => PrefixKind::Generic,
        })
    }

    fn skip_ws(b: &[u8], mut p: usize) -> usize {
        while p < b.len() && b[p].is_ascii_whitespace() {
            p += 1;
        }
        p
    }

    /// End of the identifier-ish word at `p`. Stata command words are ASCII.
    fn word_end(b: &[u8], mut p: usize) -> usize {
        while p < b.len() && (b[p].is_ascii_alphanumeric() || b[p] == b'_') {
            p += 1;
        }
        p
    }

    fn version_end(b: &[u8], mut p: usize) -> usize {
        while p < b.len() && (b[p].is_ascii_digit() || b[p] == b'.') {
            p += 1;
        }
        p
    }

    /// End of a `` `…' `` or `$…` reference at `p`, nesting-counted.
    fn macro_end(b: &[u8], p: usize) -> usize {
        let mut i = p;
        if b[i] == b'$' {
            i += 1;
            if i < b.len() && b[i] == b'{' {
                while i < b.len() && b[i] != b'}' {
                    i += 1;
                }
                return (i + 1).min(b.len());
            }
            return word_end(b, i);
        }
        let mut depth = 0u32;
        while i < b.len() {
            match b[i] {
                b'`' => depth += 1,
                b'\'' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        i
    }

    /// The first `:` at nesting depth 0 outside quotes, or `None`.
    fn find_colon(b: &[u8], from: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut in_str = false;
        let mut i = from;
        while i < b.len() {
            let c = b[i];
            if in_str {
                if c == b'"' {
                    in_str = false;
                }
                i += 1;
                continue;
            }
            match c {
                b'"' => in_str = true,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b':' if depth <= 0 => return Some(i),
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// `(first word, rest)` of an already-trimmed string.
    fn split_word(s: &str) -> (&str, &str) {
        let end = s.find(|c: char| c.is_ascii_whitespace()).unwrap_or(s.len());
        let (w, rest) = s.split_at(end);
        (
            w,
            rest.trim_start_matches(|c: char| c.is_ascii_whitespace()),
        )
    }
}
