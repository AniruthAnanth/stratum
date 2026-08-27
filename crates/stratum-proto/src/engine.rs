//! CONTRACTS.md §7 — the engine protocol.
//!
//! One request enum and one event enum, used by **both** transports: framed
//! MessagePack for the desktop (§10) and NDJSON for `--protocol json` and
//! `stratum run --json` (§7.1).
//!
//! > **AMENDED (A9). This is not JSON-RPC 2.0.** There is no `jsonrpc`, no
//! > `method`, no `params` and no separate name registry: **the Rust variant name
//! > IS the method name.** The NDJSON envelope is
//! > `{ "v": 1, "t": "req"|"resp"|"event", "corr": u32?, "body": … }`, where
//! > `body` is exactly what `serde_json` produces for these enums. A reader that
//! > does not recognise `t` or `body`'s tag MUST skip the line and continue —
//! > that is what makes §15's additive-only rule usable by third parties.
//!
//! **Framing guarantees consumers may rely on**, identical in both encodings:
//!
//! 1. Exactly one `RunStarted` first and one `RunFinished` last per run — always,
//!    including on error, interrupt, and timeout.
//! 2. `BlockStarted`…`BlockFinished` pairs never interleave within one run.
//!    `Output` events between them belong to that block.
//! 3. `Output` chunks preserve byte order and may split anywhere.
//! 4. In `--json` mode stdout carries only the NDJSON stream; all logging goes to
//!    stderr. This is what makes `stratum run x.do --json | jq` work in CI.
//! 5. `seq` is strictly increasing per session and is stamped before fan-out.
//! 6. Additive-only within a schema major (§15).

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::block::BlockMap;
use crate::complete::CompletionEnv;
use crate::data::DataEvent;
use crate::data::{FrameInfo, PageRequest, QuickSummary, VariableInfo};
use crate::defuse::DefUseIndex;
use crate::diagnostic::Diagnostic;
use crate::exec::{CancelLevel, ExecStatus, ExecutionRecord, RunIntent, RunPlan};
use crate::ids::{
    BlockId, CodeHash, DatasetStateId, DocumentId, Edit, ExecutionId, OrderId, ResultId, RunId,
    SessionEpoch, SessionId, Span, StateId, VarIdx,
};
use crate::introspect::AiContextSnapshot;
use crate::repro::ReproReport;
use crate::result::{ResultEnvelope, StyledRun};
use crate::session::{LogHit, LogSearchOpts, SessionConfigWire, SessionStatus};
use crate::status::BlockStatus;
use crate::UnixMs;

/// The one version number covering `EngineRequest`, `EngineResponse` and
/// `EngineEvent`. Consumers MUST check it in `Hello`/`RunStarted` and refuse an
/// unknown major (§15).
pub const STREAM_SCHEMA: u32 = 1;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "req", rename_all = "snake_case")]
pub enum EngineRequest {
    Hello {
        client: String,
        schema: u32,
    },
    SessionOpen {
        project_root: Utf8PathBuf,
        mode: SessionMode,
        config: SessionConfigWire,
    },
    SessionClose {
        session: SessionId,
    },
    Status {
        session: SessionId,
    },

    DocOpen {
        session: SessionId,
        doc: DocumentId,
        path: Option<Utf8PathBuf>,
        text: String,
    },
    /// Fire-and-forget from the UI's perspective; the engine answers with a
    /// `BlockMapChanged` event, not a response.
    DocChange {
        session: SessionId,
        doc: DocumentId,
        version: u64,
        edits: Vec<Edit>,
    },
    DocClose {
        session: SessionId,
        doc: DocumentId,
    },

    /// The engine re-segments the submitted text natively and compares hashes.
    /// A mismatch is `EngineError::BlockMismatch`, which is also our
    /// wasm/native divergence alarm.
    ExecSubmit {
        session: SessionId,
        intent: RunIntent,
        inline_mode: InlineResultsMode,
    },
    ExecCancel {
        session: SessionId,
        run: RunId,
        level: CancelLevel,
    },

    Blocks {
        session: SessionId,
        doc: DocumentId,
    },
    Statuses {
        session: SessionId,
        doc: DocumentId,
    },
    Ledger {
        session: SessionId,
        from_seq: u64,
        limit: u32,
    },

    Variables {
        session: SessionId,
        frame: String,
    },
    VarStats {
        session: SessionId,
        frame: String,
        var: String,
    },
    Frames {
        session: SessionId,
    },
    DataPage {
        session: SessionId,
        request: PageRequest,
    },
    /// **AMENDED (A13).** Establish an engine-side Data-Editor view order.
    /// Returns an `OrderId`; the permutation NEVER crosses the wire.
    DataOrderSet {
        session: SessionId,
        frame: String,
        spec: OrderSpec,
    },
    DataOrderDrop {
        session: SessionId,
        order: OrderId,
    },

    /// **AMENDED (A9/R-2).** Re-render an existing graph at a size/format the
    /// Graph Deck or an export needs. Answers with `Bulk`.
    GraphRender {
        session: SessionId,
        result: ResultId,
        format: GraphFormat,
        width_pt: f32,
    },

    LogRange {
        session: SessionId,
        from_line: u64,
        to_line: u64,
    },
    LogSearch {
        session: SessionId,
        query: String,
        opts: LogSearchOpts,
    },

    ReproReport {
        session: SessionId,
        doc: DocumentId,
        verify: bool,
    },
    DefUse {
        session: SessionId,
    },
    CompletionEnv {
        session: SessionId,
    },
    /// Tail of a truncated `CompletionEnv` (A11). Off the keystroke path: only
    /// an explicit "more…" interaction issues it.
    CompletionEnvPage {
        session: SessionId,
        from: u32,
        count: u32,
    },
    /// **AMENDED (A5).** The context `stratum-ai` needs. `stratum-ai` is linked
    /// into `stratum-desktop`, which may not link `stratum-exec` (C24), so it
    /// cannot call `SessionIntrospect` directly — it needs a wire request, and
    /// there was none. The desktop caches the reply and implements
    /// `SessionIntrospect` (declared in proto, §13) against that cache, so
    /// `stratum-ai`'s context packer codes against one trait either way.
    AiContext {
        session: SessionId,
        want: AiContextWant,
    },

    Shutdown,
}

bitflags::bitflags! {
    /// What the context packer is allowed to ask for THIS request. The privacy
    /// tier gate (`07` §4) filters again on the way out; this narrows the fetch
    /// so tier-3 data is never even read into desktop memory.
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct AiContextWant: u16 {
        /// frame, N, k, varnames+types+labels
        const DATASET_META    = 1 << 0;
        const MACROS          = 1 << 1;
        /// r()/e()/s() names and scalar values
        const STORED_RESULTS  = 1 << 2;
        const ESTIMATES       = 1 << 3;
        const RECENT_ERRORS   = 1 << 4;
        const RECENT_COMMANDS = 1 << 5;
        /// `QuickSummary` for named vars only
        const VAR_SUMMARIES   = 1 << 6;
    }
}

/// Hand-written for the same reason as [`crate::status::Taint`]: `bitflags`'
/// derived serde impls encode differently depending on `is_human_readable()`,
/// and `rmp-serde` disagrees with itself about that between its serializer and
/// its deserializer. The bits are the wire form in both encodings.
impl Serialize for AiContextWant {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.bits().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AiContextWant {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_bits_retain(u16::deserialize(deserializer)?))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum GraphFormat {
    Svg,
    Png,
    Pdf,
}

/// **AMENDED (A13).** Sorting and filtering are computed in Rust
/// (`06` §15.3), from a DECLARATION, not from a client-built permutation.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct OrderSpec {
    /// Sort keys in priority order. Empty ⇒ dataset order.
    pub keys: Vec<(VarIdx, SortDir)>,
    /// Data-Editor filter, an ordinary Stata `if` expression evaluated by the
    /// engine. `None` ⇒ all observations. NEVER mutates the frame.
    pub filter: Option<String>,
    /// The snapshot this order was computed against; invalidated when it moves.
    pub state: DatasetStateId,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    Interactive,
    Clean,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "kebab-case")]
pub enum InlineResultsMode {
    Always,
    EditorRun,
    Compact,
    Off,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "resp", rename_all = "snake_case")]
pub enum EngineResponse {
    Hello {
        engine: String,
        schema: u32,
        target: String,
    },
    SessionOpened {
        session: SessionId,
        epoch: SessionEpoch,
    },
    Ok,
    Status {
        status: SessionStatus,
    },
    BlockMap(BlockMap),
    Statuses {
        doc: DocumentId,
        statuses: Vec<(BlockId, BlockStatus)>,
    },
    Submitted {
        plan: RunPlan,
    },
    Ledger {
        records: Vec<ExecutionRecord>,
        next_seq: u64,
    },
    Variables {
        frame: String,
        vars: Vec<VariableInfo>,
    },
    VarStats(QuickSummary),
    Frames {
        frames: Vec<FrameInfo>,
        current: String,
    },
    /// Bulk. The payload is a [`BulkRef`] into an mmap segment; the desktop's
    /// asset handler resolves it. Never inline bytes.
    Bulk {
        bulk: BulkRef,
    },
    LogRange {
        from_line: u64,
        runs: Vec<StyledRun>,
        line_starts: Vec<u32>,
    },
    LogSearch {
        hits: Vec<LogHit>,
        total: u64,
    },
    ReproReport(ReproReport),
    DefUse(DefUseIndex),
    CompletionEnv(CompletionEnv),
    /// A13. `n_rows` is the post-filter row count the Data Editor scrolls over.
    DataOrder {
        order: OrderId,
        n_rows: u64,
        state: DatasetStateId,
    },
    /// A5.
    AiContext(AiContextSnapshot),
    Error(EngineError),
}

/// Where the engine put a bulk payload (`DataPage`, raw classic text, graph
/// SVG/PNG): a window into the per-session mmap segment ring, which the desktop
/// maps read-only and serves from the `stratum-asset://` handler. Two copies
/// total on the path — engine builder → mmap, mmap → webview response body —
/// and zero parsing on either side.
///
/// CONTRACTS.md declares this in §10 (framing) rather than §7, but
/// `EngineResponse::Bulk` cannot compile without it and `docs/ownership.toml`
/// gives `src/frame.rs` to W07. It is declared here; `frame.rs` should
/// `pub use crate::engine::BulkRef;`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BulkRef {
    pub segment: u32,
    pub offset: u64,
    pub len: u64,
    pub epoch: u64,
}

/// **CONTRACT DEVIATION, reported by W00.** CONTRACTS.md §7 gives
/// `UnknownSession(SessionId)`, `UnknownDocument(DocumentId)` and
/// `Internal(String)` as tuple variants. An internally tagged enum has to put
/// `"error"` somewhere, so a newtype variant wrapping an integer or a string
/// cannot serialize — serde fails at runtime with "cannot serialize tagged
/// newtype variant EngineError::Internal containing a string", in both JSON and
/// MessagePack. Naming the field is the only shape that keeps `tag = "error"`
/// and round-trips. The `Display` text is unchanged, so `EngineError::Internal`
/// still renders exactly its message.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum EngineError {
    #[error("unknown session {session:?}")]
    UnknownSession { session: SessionId },
    #[error("unknown document {doc:?}")]
    UnknownDocument { doc: DocumentId },
    #[error("block text differs from the engine's view (doc version {engine_version}, client {client_version})")]
    BlockMismatch {
        doc: DocumentId,
        engine_version: u64,
        client_version: u64,
    },
    #[error("selection does not parse as whole statements")]
    PartialStatement { span: Span },
    #[error("engine is busy")]
    Busy,
    #[error("schema mismatch: engine {engine}, client {client}")]
    SchemaMismatch { engine: u32, client: u32 },
    #[error("{message}")]
    Internal { message: String },
}

/// Engine → everyone. Seq-stamped BEFORE fan-out so every window observes one
/// order; a window that sees a gap re-snapshots rather than diverging.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EngineEvent {
    /// Always the first event of a run, including on error/interrupt/timeout.
    RunStarted {
        seq: u64,
        schema: u32,
        run: RunId,
        session: SessionId,
        stratum_version: String,
        source: Option<Utf8PathBuf>,
        clean_state: bool,
        cwd: Utf8PathBuf,
        /// A2: milliseconds since the Unix epoch, UTC. Not `OffsetDateTime`.
        started_at_ms: UnixMs,
        seed: Option<u64>,
        plan_len: u32,
    },
    BlockStarted {
        seq: u64,
        run: RunId,
        exec: ExecutionId,
        block: BlockId,
        doc: Option<DocumentId>,
        span: Span,
        code_hash: CodeHash,
        dataset_state_in: DatasetStateId,
        text: String,
    },
    /// Coalesced at 16 ms / 64 KB. Chunks may split mid-grapheme; buffer.
    Output {
        seq: u64,
        exec: ExecutionId,
        stream: OutputStream,
        runs: Vec<StyledRun>,
    },
    /// A window >256 frames behind gets this instead; full text is always at
    /// `stratum-asset://localhost/result/{s}/{r}/raw`.
    OutputTruncated {
        seq: u64,
        exec: ExecutionId,
        dropped_bytes: u64,
    },
    Result {
        seq: u64,
        exec: ExecutionId,
        envelope: ResultEnvelope,
    },
    Diagnostic {
        seq: u64,
        exec: Option<ExecutionId>,
        diagnostic: Diagnostic,
    },
    Progress {
        seq: u64,
        exec: ExecutionId,
        done: u64,
        total: Option<u64>,
        label: String,
    },
    StateChanged {
        seq: u64,
        exec: ExecutionId,
        dataset_state: DatasetStateId,
        state: StateId,
        frame: String,
        n_obs: u64,
        n_vars: u32,
        events: Vec<DataEvent>,
    },
    BlockFinished {
        seq: u64,
        run: RunId,
        exec: ExecutionId,
        block: BlockId,
        result: Option<ResultId>,
        status: ExecStatus,
        /// The TRUE Stata return code. 0 == success. `--rc-file`, exit code 1
        /// vs 10, and the differential harness all depend on this.
        rc: u32,
        duration_us: u64,
        dataset_state_out: DatasetStateId,
    },
    /// AUTHORITATIVE staleness. Emitted after every commit and after every
    /// debounced reconcile.
    StatusChanged {
        seq: u64,
        doc: DocumentId,
        changed: Vec<(BlockId, BlockStatus)>,
    },
    BlockMapChanged {
        seq: u64,
        map: BlockMap,
    },
    RunFinished {
        seq: u64,
        run: RunId,
        rc: u32,
        blocks_run: u32,
        blocks_failed: u32,
        duration_us: u64,
        finished_at_ms: UnixMs,
    },
    CompletionEnvChanged {
        seq: u64,
        env: CompletionEnv,
    },
    EngineHealth {
        seq: u64,
        health: EngineHealth,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Results,
    Error,
    Trace,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "health", rename_all = "snake_case")]
pub enum EngineHealth {
    Starting,
    Ready,
    Busy {
        exec: ExecutionId,
    },
    Crashed {
        signal: Option<i32>,
        last_statement: Option<String>,
        log_tail: String,
    },
    Stopped,
}
