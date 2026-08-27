//! CONTRACTS.md §6 — execution records, run intents, plans.
//!
//! [`RunIntent`] is what the user pressed; [`RunPlan`] is what the engine decided
//! to do about it, including what it deliberately skipped — the UI reports
//! "12 blocks skipped — unaffected", because silence there would feel like a bug.
//! [`ExecutionRecord`] is the append-only history row, immutable once `status`
//! leaves `Running`.
//!
//! **Cancellation ladder (normative).** `Interrupt` → expect ack ≤ 50 ms → if no
//! `BlockFinished{Interrupted}` within **2000 ms** the button becomes *Force
//! stop* → `Abort` → if still alive at **4000 ms** the supervisor kills the
//! engine process, respawns it, and offers **"Replay to Execution N"** from the
//! ledger.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::ids::{
    BlockId, CodeHash, DatasetStateId, DocumentId, ExecutionId, ResultId, RunId, SessionEpoch,
    SessionId, Span, StateId,
};
use crate::status::Taint;
use crate::UnixMs;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum RunIntent {
    CurrentBlock {
        doc: DocumentId,
        cursor: u32,
    },
    RunAndAdvance {
        doc: DocumentId,
        cursor: u32,
    },
    Selection {
        doc: DocumentId,
        span: Span,
    },
    FromHere {
        doc: DocumentId,
        block: BlockId,
        scope: ForwardScope,
    },
    EverythingAbove {
        doc: DocumentId,
        block: BlockId,
    },
    ToCursor {
        doc: DocumentId,
        cursor: u32,
    },
    CurrentSection {
        doc: DocumentId,
        cursor: u32,
    },
    AllStale {
        doc: DocumentId,
    },
    WholeFile {
        doc: DocumentId,
    },
    CleanRun {
        entry: DocumentId,
        isolation: Isolation,
    },
    /// **AMENDED (A23).** Spec §2 lists "project entry point" as an execution
    /// target; the nine original verbs had no project-scoped one. Resolves to a
    /// clean run of the entry `.do` recorded in the workspace
    /// (`WorkspaceState.entry_point`). What remains deferred to v1.1 is ORDERING
    /// several entry points, not running the configured one.
    ProjectEntryPoint {
        project_root: Utf8PathBuf,
        isolation: Isolation,
    },
    CommandBar {
        text: String,
    },
}

/// Default `Dependents`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ForwardScope {
    Dependents,
    AllBelow,
}

/// The §16 "runs from clean state" tick may ONLY be set by `Subprocess`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    InProcess,
    Subprocess,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RunPlan {
    pub run: RunId,
    /// In execution order.
    pub items: Vec<PlanItem>,
    pub epoch_reset: bool,
    pub clean_state: bool,
    /// Blocks deliberately NOT run, with the reason. The UI reports
    /// "12 blocks skipped — unaffected"; silence would feel like a bug.
    pub skipped: Vec<(BlockId, SkipReason)>,
    /// Non-blocking banner input: "3 upstream blocks are stale — [Run them first]"
    pub stale_upstream: Vec<BlockId>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PlanItem {
    pub block: BlockId,
    pub span: Span,
    pub code_hash: CodeHash,
    pub reason: PlanReason,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum PlanReason {
    Requested,
    DependencyOf,
    Stale,
    Prefix,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Unaffected,
    AlreadyCurrent,
    NotExecutable,
}

/// The append-only history record. Immutable once `status` leaves Running.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ExecutionRecord {
    pub exec: ExecutionId,
    /// Global monotone completion order.
    pub seq: u64,
    pub session: SessionId,
    pub epoch: SessionEpoch,
    pub run: RunId,
    /// `BlockId::EPHEMERAL` for command bar / selection.
    pub block: BlockId,
    pub doc: Option<DocumentId>,
    pub origin: ExecOrigin,
    pub code_hash: CodeHash,
    /// The exact text executed, snapshotted AT ENQUEUE TIME.
    pub source: String,
    pub input_state: StateId,
    /// `== input_state` if nothing changed.
    pub output_state: StateId,
    /// The "D17" surfaced in the UI.
    pub input_dataset: DatasetStateId,
    pub output_dataset: DatasetStateId,
    pub result: Option<ResultId>,
    pub status: ExecStatus,
    pub started_at_ms: UnixMs,
    pub duration_us: u64,
    /// The block's text changed between enqueue and run. We still ran the
    /// snapshot — running what the user pressed is deterministic — and the block
    /// shows Stale(CodeChanged) the instant it finishes.
    pub stale_on_arrival: bool,
    /// See [`Taint`] for why the TypeScript type is the raw `u16`.
    #[cfg_attr(feature = "specta", specta(type = u16))]
    pub taint: Taint,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExecStatus {
    Queued,
    Running,
    Succeeded,
    Failed {
        rc: u32,
        message: String,
        span: Option<Span>,
    },
    Interrupted {
        rolled_back: bool,
        at: Option<Span>,
    },
    Skipped {
        reason: SkipReason,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ExecOrigin {
    Editor,
    CommandBar,
    Selection,
    DoFile,
    CleanRun,
    Cli,
    Api,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[repr(u8)]
pub enum CancelLevel {
    Interrupt = 1,
    Abort = 2,
}
