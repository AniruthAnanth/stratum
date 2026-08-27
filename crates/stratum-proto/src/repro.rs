//! CONTRACTS.md §9 — reproducibility.
//!
//! The one honest thing this report does is refuse to guess: [`ReproReport::runs_clean`]
//! is [`Tri::Unknown`] until an ACTUAL `Isolation::Subprocess` clean run verifies
//! it, and the UI renders "not verified" rather than a tick. A green mark that
//! was inferred from static analysis is the single worst thing this feature could
//! ship.

use serde::{Deserialize, Serialize};

use crate::diagnostic::{Confidence, Related, Severity, Suggestion};
use crate::ids::{BlockId, DocumentId, ExecutionId, Span, TextHash};
use crate::UnixMs;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Tri {
    Yes,
    No,
    Unknown,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ReproReport {
    pub doc: DocumentId,
    pub file_hash: TextHash,
    /// A2.
    pub generated_at_ms: UnixMs,
    /// `Tri::Unknown` until an ACTUAL `Isolation::Subprocess` clean run verifies
    /// it. Never inferred from static analysis. The UI renders "not verified",
    /// not ✓.
    pub runs_clean: Tri,
    pub verified_by: Option<ExecutionId>,
    pub verified_duration_us: Option<u64>,
    pub seed_defined: Tri,
    pub inputs_resolved: Tri,
    pub no_hidden_deps: Tri,
    pub findings: Vec<Finding>,
    /// Suppressions (`*! nolint(R001)`) are listed so they cannot hide problems.
    pub suppressed: Vec<(String, Span)>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Finding {
    /// "R001"
    pub lint: String,
    pub severity: Severity,
    pub title: String,
    pub message: String,
    pub detail: Option<String>,
    pub evidence: Vec<Related>,
    pub block: Option<BlockId>,
    pub span: Option<Span>,
    /// Deterministic text edit. NEVER AI-generated.
    pub fix: Option<Suggestion>,
    pub confidence: Confidence,
}
