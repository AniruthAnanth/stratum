//! CONTRACTS.md §9.1 — the session-shaped types §13 referenced but never declared
//! (A29).
//!
//! Three of the twelve live here because they are what the exec/session layer
//! hands to a window: [`SessionStatus`] (the status line), [`SessionSnapshot`]
//! (what a late-joining window gets instead of replaying history through IPC),
//! and [`SessionConfigWire`] (the subset of session configuration that crosses
//! the wire at all — ado path resolution and allocator knobs stay engine-side).

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::block::{BlockMap, RegionSummary};
use crate::complete::CompletionEnv;
use crate::engine::{EngineHealth, SessionMode};
use crate::ids::{
    BlockId, DatasetStateId, DocumentId, ExecutionId, ResultId, SessionEpoch, SessionId, StateId,
};
use crate::status::BlockStatus;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SessionStatus {
    pub session: SessionId,
    pub epoch: SessionEpoch,
    pub health: EngineHealth,
    pub current: Option<ExecutionId>,
    pub queued: u32,
    pub state: StateId,
    pub dataset_state: DatasetStateId,
    pub frame: String,
    pub n_obs: u64,
    pub n_vars: u32,
    pub mode: SessionMode,
}

/// Handed to a window on `session_subscribe` so a late joiner never replays
/// history through IPC.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SessionSnapshot {
    pub status: SessionStatus,
    pub docs: Vec<BlockMap>,
    pub statuses: Vec<(DocumentId, Vec<(BlockId, BlockStatus)>)>,
    /// Result IDs only; the envelopes are fetched with `result_get` on demand.
    pub recent_results: Vec<(BlockId, ResultId)>,
    pub completion_env: CompletionEnv,
    pub log_lines: u64,
    pub from_seq: u64,
}

/// The subset of session configuration that crosses the wire. The full
/// `SessionConfig` (ado path resolution, allocator knobs) stays engine-side.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SessionConfigWire {
    pub cwd: Option<Utf8PathBuf>,
    pub seed: Option<u64>,
    /// Always 80 in v1; a different value is rejected with rc 10 (C44/A16).
    pub linesize: u16,
    /// Default 95.0.
    pub level: f64,
    pub varabbrev: bool,
    pub more: bool,
    pub max_memory_bytes: Option<u64>,
    /// False for clean runs.
    pub ado_personal: bool,
    pub write_sandbox: Option<Utf8PathBuf>,
}

/// One block as the exec/session layer sees it. `DocumentModel::blocks` returns
/// `&[Block]`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Block {
    pub id: BlockId,
    pub region: RegionSummary,
    pub doc: DocumentId,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct LogHit {
    pub line: u64,
    pub col: u32,
    pub len: u32,
    pub preview: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct LogSearchOpts {
    pub regex: bool,
    pub case_sensitive: bool,
    pub max_hits: u32,
}
