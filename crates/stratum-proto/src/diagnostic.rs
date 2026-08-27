//! CONTRACTS.md §4 — diagnostics.
//!
//! One [`Diagnostic`] type for parse errors, runtime errors, lints, and repro
//! findings. One registry of codes (ARCHITECTURE C14), so a code seen in the
//! problems pane, in `--json` output and in a `*! nolint(...)` suppression is
//! the same string.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::ids::{BlockId, Edit, Span};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Diagnostic {
    pub severity: Severity,
    /// Stable machine code from the ONE registry (ARCHITECTURE C14):
    ///   `"R001".."R026"` reproducibility checks,
    ///   `"L001".."L012"` editor / dataflow lints,
    ///   `"PARSE0001"..` parse errors,
    ///   `"STATA0111"..` runtime errors mirroring a Stata return code.
    pub code: String,
    /// The Stata return code, when there is one. 111, 198, 199, 109, 601, …
    pub stata_rc: Option<u32>,
    pub message: String,
    pub file: Option<Utf8PathBuf>,
    /// Byte range in the ORIGINAL source (composed back through `SpanMap`).
    pub span: Option<Span>,
    /// THE critical field for spec §21. Without it "Did you mean 'income'?"
    /// degrades to regex-scraping English prose. The runtime MUST populate it
    /// for every r(111)/r(199)/r(198)-class error it raises.
    pub offending_token: Option<String>,
    pub block: Option<BlockId>,
    pub related: Vec<Related>,
    /// Deterministic fixes only. AI-proposed edits travel as `ProposedPatch`.
    pub suggestions: Vec<Suggestion>,
    pub notes: Vec<String>,
    /// Set when the finding came from a conservative/approximate analysis.
    pub confidence: Confidence,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Related {
    pub span: Span,
    pub file: Option<Utf8PathBuf>,
    pub message: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Suggestion {
    /// "Did you mean `income`?"
    pub label: String,
    pub kind: SuggestionKind,
    /// Applying ALL edits atomically is the fix. Empty ⇒ informational only.
    pub edits: Vec<Edit>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    Rename,
    InsertOption,
    RemoveOption,
    Rewrite,
    InsertLine,
    ChangePath,
    Explain,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    #[default]
    Exact,
    Probable,
    Speculative,
}
