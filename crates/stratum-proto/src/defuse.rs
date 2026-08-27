//! CONTRACTS.md §9 — def/use, behind spec §20's "Created by" / "Used by".
//!
//! Names the analysis could not resolve are reported as [`UnresolvedRef`] rather
//! than dropped or guessed at: the editor renders them with a dotted underline
//! and a tooltip, which is the honest rendering of "a macro built this name and
//! we do not know what it was".

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::diagnostic::Confidence;
use crate::ids::{BlockId, Span};

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DefUseIndex {
    pub generation: u64,
    pub files: Vec<Utf8PathBuf>,
    pub defs: Vec<(String, Vec<SiteRef>)>,
    pub uses: Vec<(String, Vec<SiteRef>)>,
    /// Macro-constructed names we could not resolve. We do NOT fabricate
    /// certainty: these render with a dotted underline and a tooltip.
    pub unresolved: Vec<UnresolvedRef>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SiteRef {
    /// Index into `DefUseIndex::files`.
    pub file: u32,
    /// 1-based, for display.
    pub line: u32,
    /// 1-based, for display.
    pub col: u32,
    /// Byte offsets, for navigation.
    pub span: Span,
    pub block: Option<BlockId>,
    /// Trimmed source, for display.
    pub statement: String,
    pub kind: SiteKind,
    pub confidence: Confidence,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SiteKind {
    Generate,
    Replace,
    Egen,
    Rename,
    Merge,
    Import,
    Recode,
    Encode,
    Loop,
    Drop,
    Read,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct UnresolvedRef {
    pub pattern: String,
    pub site: SiteRef,
}
