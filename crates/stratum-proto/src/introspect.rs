//! CONTRACTS.md §9.1 and §13 (A5) — session introspection.
//!
//! The trait lives here, over proto types only, so that `stratum-ai` — linked by
//! the desktop, which may not link the engine (C24) — can code against ONE trait
//! whether it is talking to `stratum-exec` in-process or to an event-cache
//! adapter in the desktop backed by `EngineRequest::AiContext`.
//!
//! Every method returns proto types only, and **there is no method on this trait
//! that can return an observation value**. That is what makes `07`'s tier-1
//! privacy guarantee structural rather than a promise: a context packer cannot
//! leak cell data through this seam because the seam has no shape for it.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::complete::CompletionEnv;
use crate::data::{FrameInfo, QuickSummary, VariableInfo};
use crate::diagnostic::Diagnostic;
use crate::ids::{DatasetStateId, ExecutionId, ResultId, SessionId};

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct MacroInfo {
    pub name: String,
    pub scope: MacroScope,
    pub value: String,
    /// True when the value was elided for length; `value` holds the head.
    pub truncated: bool,
    pub defined_at: Option<ExecutionId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum MacroScope {
    Local,
    Global,
}

/// Insertion-ordered, exactly as `return list` / `ereturn list` print (C31).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StoredResultsView {
    pub r_scalars: Vec<(String, f64)>,
    pub r_macros: Vec<(String, String)>,
    pub r_matrices: Vec<(String, MatrixMeta)>,
    pub e_scalars: Vec<(String, f64)>,
    pub e_macros: Vec<(String, String)>,
    pub e_matrices: Vec<(String, MatrixMeta)>,
    pub s_macros: Vec<(String, String)>,
    /// e(b) column names, in coefficient order. The single most useful thing an
    /// AI or a completion popup can know about the last model.
    pub e_b_colnames: Vec<String>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct MatrixMeta {
    pub rows: u32,
    pub cols: u32,
    pub rownames: Vec<String>,
    pub colnames: Vec<String>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct EstimateHandle {
    /// `estimates store` name.
    pub name: String,
    pub cmd: String,
    pub depvar: String,
    pub n: u64,
    pub sample_hash: u64,
    pub result: Option<ResultId>,
    pub stored_at: Option<ExecutionId>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DatasetMeta {
    pub frame: String,
    pub state: DatasetStateId,
    pub n_obs: u64,
    pub n_vars: u32,
    pub sorted_by: Vec<String>,
    pub label: String,
    pub source_path: Option<Utf8PathBuf>,
    /// Metadata only — never cell values. Bounded by
    /// [`crate::complete::COMPLETION_ENV_MAX_VARS`].
    pub vars: Vec<VariableInfo>,
    pub truncated: bool,
}

/// CONTRACTS.md §9.1 writes `#[derive(Default)]` here, which does not compile:
/// the ids in §1 deliberately do not derive `Default`, because `BlockId(0)` is
/// `EPHEMERAL` and a silently-defaulted block id would be a real bug rather than
/// a typo. Hand-written so the public API is exactly what the contract promises —
/// `DatasetMeta::default()` exists and zeroes the id — without putting a
/// dangerous `Default` on every id type.
impl Default for DatasetMeta {
    fn default() -> Self {
        Self {
            frame: String::new(),
            state: DatasetStateId(0),
            n_obs: 0,
            n_vars: 0,
            sorted_by: Vec::new(),
            label: String::new(),
            source_path: None,
            vars: Vec::new(),
            truncated: false,
        }
    }
}

/// The reply to `EngineRequest::AiContext`. Everything here is metadata or
/// aggregate; **no observation-level data ever appears in this type**, which is
/// what makes `07`'s tier-1 privacy guarantee structural rather than a promise.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AiContextSnapshot {
    pub session: SessionId,
    pub generation: u64,
    pub dataset: Option<DatasetMeta>,
    pub macros: Vec<MacroInfo>,
    pub stored: Option<StoredResultsView>,
    pub estimates: Vec<EstimateHandle>,
    pub recent_errors: Vec<Diagnostic>,
    pub recent_commands: Vec<String>,
    pub var_summaries: Vec<QuickSummary>,
}

/// See [`DatasetMeta`]'s `Default` for why this is hand-written.
impl Default for AiContextSnapshot {
    fn default() -> Self {
        Self {
            session: SessionId(0),
            generation: 0,
            dataset: None,
            macros: Vec::new(),
            stored: None,
            estimates: Vec::new(),
            recent_errors: Vec::new(),
            recent_commands: Vec::new(),
            var_summaries: Vec::new(),
        }
    }
}

/// **AMENDED (A5).** Implemented by (a) `stratum-exec`, in-process in the engine,
/// and (b) an event-cache adapter inside `stratum-desktop` backed by
/// `EngineRequest::AiContext`. Consumed by `stratum-intel` (a) and `stratum-ai`
/// (b).
///
/// The pre-audit text said "implemented by stratum-exec, consumed by
/// stratum-intel and stratum-ai" — but C24 forbids `stratum-desktop`, which links
/// `stratum-ai`, from reaching `stratum-exec`, and no wire request existed for
/// `macros()`, `stored_results()`, `recent_errors()` or `dataset_meta()`. W21's
/// context packer was literally unimplementable. Declaring the trait over proto
/// types with two implementations fixes both halves.
pub trait SessionIntrospect: Send + Sync {
    fn frames(&self) -> Vec<FrameInfo>;
    fn variables(&self, frame: &str) -> Vec<VariableInfo>;
    /// Lazy and cached: computing this eagerly for every variable is the thing
    /// spec §20 explicitly refuses to do.
    fn var_stats(&self, frame: &str, v: &str) -> Option<QuickSummary>;
    fn macros(&self) -> Vec<MacroInfo>;
    /// r(), e(), s() including e(b) names.
    fn stored_results(&self) -> StoredResultsView;
    fn estimates_store(&self) -> Vec<EstimateHandle>;
    fn recent_errors(&self, n: usize) -> Vec<Diagnostic>;
    fn dataset_meta(&self) -> DatasetMeta;
    fn completion_env(&self) -> CompletionEnv;
}
