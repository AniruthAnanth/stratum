//! CONTRACTS.md §2 — blocks, regions, and the block map.
//!
//! `stratum-parse` produces `Region` (borrowed, engine-internal). [`RegionSummary`]
//! is the owned wire projection. **`BlockId` is allocated only by `stratum-exec`.**
//!
//! The reconcile contract that keeps a `BlockId` attached to a block the user
//! edited — Myers diff, positional mapping inside each replace hunk, fresh ids
//! for surplus blocks, retirement for the rest — is normative in §2 and
//! implemented by `stratum_runtime::doc::reconcile`, not here.

use serde::{Deserialize, Serialize};

use crate::diagnostic::Diagnostic;
use crate::ids::{BlockId, CodeHash, DocumentId, LineRange, SectionId, Span};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Delimiter {
    Cr,
    Semi,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RegionKind {
    /// Exactly one logical line (`///` continuations already folded in).
    Simple,
    Brace {
        opener: BraceOpener,
    },
    EndBlock {
        opener: EndBlockOpener,
        name: Option<String>,
    },
    /// `#delimit cr|;` — executable (it mutates scanner state), no output.
    Directive {
        directive: DirectiveKind,
    },
    /// Comments and/or blank lines. NOT executable; no run affordance.
    Trivia {
        has_marker: bool,
    },
    /// EOF with the block still open. Executable only via explicit override.
    Unterminated {
        expected: Unterminated,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum BraceOpener {
    Foreach,
    Forvalues,
    While,
    IfElseChain,
    Capture,
    Quietly,
    Noisily,
    Anonymous,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum EndBlockOpener {
    Program,
    Input,
    Mata,
    Python,
    Java,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum DirectiveKind {
    DelimitCr,
    DelimitSemi,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Unterminated {
    CloseBrace,
    End,
    BlockComment,
    CompoundQuote,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RegionSummary {
    /// Position in the document's region vector. NOT stable across edits.
    pub index: u32,
    /// Executable extent: first code byte .. last code byte.
    pub span: Span,
    /// `span` plus attached leading comments and the trailing comment on the
    /// last physical line. Consecutive `outer_span`s TILE THE FILE EXACTLY.
    pub outer_span: Span,
    /// Lines of `outer_span`.
    pub lines: LineRange,
    /// Lines of `span` only — what the gutter marker aligns to.
    pub code_lines: LineRange,
    pub kind: RegionKind,
    /// Delimiter mode in force at `span.start`. REQUIRED to run this region in
    /// isolation inside a `#delimit ;` stretch.
    pub entry_delimiter: Delimiter,
    pub exit_delimiter: Delimiter,
    pub code_hash: CodeHash,
    /// 0-based occurrence index of this `code_hash` within the document.
    /// (code_hash, hash_ordinal) is the frontend's pre-BlockMap key.
    pub hash_ordinal: u32,
    /// Canonical command name if it resolves without macro expansion.
    pub canonical: Option<String>,
    /// Drives spec §19 "Compare models".
    pub is_estimation: bool,
    /// True if a macro reference appears in the command position. The gutter
    /// still offers Run; completion downgrades to text.
    pub has_macro_in_head: bool,
    pub section: Option<SectionId>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CellMarker {
    pub span: Span,
    pub line: u32,
    pub title: String,
    pub section: SectionId,
}

/// The engine's authoritative identity assignment. Sent on doc open and after
/// every debounced `doc_change`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct BlockMap {
    pub doc: DocumentId,
    /// Increments on every reconcile. The frontend drops out-of-order maps.
    pub generation: u64,
    /// Document version this map was computed against (see `doc_change`).
    pub doc_version: u64,
    /// Parallel to `regions`: `blocks[i]` is the BlockId of `regions[i]`.
    /// **AMENDED (A3): `Trivia` regions get `BlockId::NONE`, not `EPHEMERAL`.**
    /// Consumers MUST skip `!id.is_real()` entries when applying a
    /// `StatusChanged` batch or building `latest_by_block`.
    pub blocks: Vec<BlockId>,
    pub regions: Vec<RegionSummary>,
    pub markers: Vec<CellMarker>,
    pub sections: Vec<SectionSpan>,
    /// Blocks whose ExecutionRecords remain in the ledger but which are no
    /// longer in the document. The UI removes their widgets.
    pub retired: Vec<BlockId>,
    pub diagnostics: Vec<Diagnostic>,
    pub end_delimiter: Delimiter,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SectionSpan {
    pub id: SectionId,
    pub span: Span,
    pub title: String,
    pub lines: LineRange,
}
