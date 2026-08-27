//! The engine — design 03 §9, ARCHITECTURE §5.
//!
//! One session, two threads: a **session worker** that owns the `!Sync` session
//! and runs user code, and a **control thread** ([`crate::control`]) that owns
//! documents, statuses and the sweep. Everything the control thread reads from
//! the worker arrives as an immutable snapshot published by a pointer swap, so
//! a 30-second `regress` never stops the engine from telling the UI that the
//! block the user just edited made three blocks below it stale.
//!
//! # The port
//!
//! [`SessionHost`] is what `stratum-session` (W08b) implements. It is
//! deliberately small: run this text, tell me what you read and wrote. Version
//! bookkeeping is NOT its job — the engine owns the version table and applies
//! the write footprint to it, which is what makes a commit O(slots touched)
//! rather than O(columns), and what lets the convergence rule live where the
//! digests are: **a column whose content digest did not change is simply absent
//! from the write footprint**, so `03` §4.4's false-cascade cancellation is
//! expressed by omission rather than by a flag nobody would set.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use camino::Utf8PathBuf;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use parking_lot::{Mutex, RwLock};
use rustc_hash::FxHashMap;
use stratum_proto::{
    BlockId, BlockStatus, CancelLevel, CodeHash, DatasetStateId, DocumentId, EngineEvent,
    EngineHealth, ExecOrigin, ExecStatus, ExecutionId, ExecutionRecord, InlineResultsMode,
    OutputStream, ResultEnvelope, ResultId, ResultPayload, RunIntent, RunPlan, SessionEpoch,
    SessionId, StateId, StyledRun, Taint, UnixMs,
};

use crate::cancel::CancelToken;
use crate::control::{ControlLoop, ControlMsg, Snapshot};
use crate::introspect::IntrospectSnapshot;
use crate::ledger::{Committed, ExecutionLedger, IdAllocator};
use crate::plan::PlanError;
use crate::queue::{QueuedRun, RunQueue};
use crate::resultstore::ResultStore;
use crate::staleness::{AnalysedDoc, RecordedReads, RecordedWrites, RunState, Versions};
use crate::stream::{EventBus, EventSink, OutputCoalescer};

/// Milliseconds since the Unix epoch. The only time representation the engine
/// puts on the wire (A2).
#[must_use]
pub fn now_ms() -> UnixMs {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// What a clean run needs to build a fresh session (`03` §8).
///
/// The 16-item clean-state checklist itself is `Session::fresh`, owned by
/// `stratum-session` (A7/C49). Clean execution does not RESET a session, it
/// CONSTRUCTS one — there is no cleanup path that can forget an item — so this
/// is a constructor argument, not a reset request.
#[derive(Clone, Debug)]
pub struct CleanRunConfig {
    /// The directory containing the entry `.do` file. Not the app's cwd and not
    /// the last-used directory: this is what makes relative paths in a shared
    /// repository work on a colleague's machine.
    pub cwd: Utf8PathBuf,
    /// `123456789` unless the caller overrides it.
    pub seed: u64,
    /// Excludes `PERSONAL`/`PLUS` — a do-file that only works because of an ado
    /// in your home directory is not reproducible.
    pub ado_personal: bool,
}

impl Default for CleanRunConfig {
    fn default() -> Self {
        Self {
            cwd: Utf8PathBuf::from("."),
            seed: 123_456_789,
            ado_personal: false,
        }
    }
}

/// One block's execution, from the host's point of view.
pub struct ExecContext<'a> {
    /// The execution id this run was allocated.
    pub exec: ExecutionId,
    /// The block, `EPHEMERAL` for command-bar and selection runs.
    pub block: BlockId,
    /// The text to run — the ENQUEUE-TIME snapshot, not the current buffer.
    pub source: &'a str,
    /// Poll this at every safepoint.
    pub cancel: &'a CancelToken,
    /// Where output goes.
    pub result: ResultId,
    /// True when this is a clean run.
    pub clean: bool,
    /// Inline-result policy for this run.
    pub inline: InlineResultsMode,
    store: &'a ResultStore,
    events: &'a dyn EventSink,
    coalescer: &'a mut OutputCoalescer,
}

impl ExecContext<'_> {
    /// Append output. The bytes land in the store immediately; the notification
    /// is coalesced at a line boundary on a 16 ms timer, so a
    /// `forvalues i=1/100000 { display \`i' }` produces ~60 notifications per
    /// second rather than 100 000.
    pub fn output(&mut self, runs: &[StyledRun]) {
        self.store.text(self.result).append(runs);
        if let Some(batch) = self.coalescer.append(runs, now_ms()) {
            self.events.emit(EngineEvent::Output {
                seq: 0,
                exec: self.exec,
                stream: OutputStream::Results,
                runs: batch,
            });
        }
    }

    /// Report progress from a safepoint. Forces an output flush first, so the
    /// progress line never overtakes the text that explains it.
    pub fn progress(&mut self, done: u64, total: Option<u64>, label: &str) {
        self.flush();
        self.events.emit(EngineEvent::Progress {
            seq: 0,
            exec: self.exec,
            done,
            total,
            label: label.to_owned(),
        });
    }

    /// Flush buffered output now.
    pub fn flush(&mut self) {
        if let Some(batch) = self.coalescer.force(now_ms()) {
            self.events.emit(EngineEvent::Output {
                seq: 0,
                exec: self.exec,
                stream: OutputStream::Results,
                runs: batch,
            });
        }
    }
}

/// What the host reports back about one block.
#[derive(Clone, Debug)]
pub struct ExecOutcome {
    /// How it ended.
    pub status: ExecStatus,
    /// The TRUE Stata return code. 0 == success.
    pub rc: u32,
    /// What it actually read.
    pub reads: RecordedReads,
    /// What it actually wrote. A converged column is deliberately absent.
    pub writes: RecordedWrites,
    /// Why the INV-1 proof is weaker than exact, if it is.
    pub taint: Taint,
    /// Payloads to commit to the result store.
    pub payloads: Vec<ResultPayload>,
    /// The dataset state id after the command, when the host interns its own.
    /// `None` lets the engine mint one, or keep the current one when nothing
    /// was written.
    pub dataset_state: Option<DatasetStateId>,
    /// The current frame after the command, if it changed.
    pub frame: Option<String>,
    /// Observation count after the command, for `StateChanged`.
    pub n_obs: u64,
    /// Variable count after the command.
    pub n_vars: u32,
}

impl Default for ExecOutcome {
    fn default() -> Self {
        Self {
            status: ExecStatus::Succeeded,
            rc: 0,
            reads: RecordedReads::default(),
            writes: RecordedWrites::default(),
            taint: Taint::empty(),
            payloads: Vec::new(),
            dataset_state: None,
            frame: None,
            n_obs: 0,
            n_vars: 0,
        }
    }
}

/// What `stratum-exec` needs from the session layer.
///
/// Implemented by `stratum-session` (W08b) over a real `Session`, and by
/// [`crate::testkit`] over a scripted one. The engine never sees a `Frame`, a
/// macro table or an `ExecCtx`: everything it needs to keep INV-1 is in the
/// footprints this trait returns.
pub trait SessionHost: Send + 'static {
    /// Run one block against the live session.
    fn execute(&mut self, ctx: &mut ExecContext<'_>) -> ExecOutcome;

    /// Build a fresh session for a clean run and return its new epoch. This is
    /// `Session::fresh`, which CONSTRUCTS rather than resets.
    fn reset_clean(&mut self, config: &CleanRunConfig) -> SessionEpoch;

    /// A metadata-only view for `SessionIntrospect` and `AiContext`. Called on
    /// the worker between commands, never concurrently with `execute`.
    fn introspect(&self) -> IntrospectSnapshot {
        IntrospectSnapshot::default()
    }
}

struct Shared {
    session: SessionId,
    queue: Mutex<RunQueue>,
    queue_state: Arc<Snapshot<RunState>>,
    ledger: Arc<RwLock<ExecutionLedger>>,
    versions: Arc<Snapshot<Versions>>,
    hashes: Arc<Snapshot<FxHashMap<BlockId, CodeHash>>>,
    introspect: Arc<Snapshot<IntrospectSnapshot>>,
    ids: Arc<IdAllocator>,
    store: Arc<ResultStore>,
    events: Arc<dyn EventSink>,
    epoch: AtomicU32,
    control: Sender<ControlMsg>,
    wake: Sender<()>,
}

/// Spawns and owns the two threads.
pub struct ExecutionEngine;

impl ExecutionEngine {
    /// Start an engine over `host`.
    ///
    /// Returns the handle and the event receiver. The handle owns the threads;
    /// dropping it shuts them down.
    #[must_use]
    pub fn spawn<H: SessionHost>(
        host: H,
        session: SessionId,
        frame: &str,
    ) -> (EngineHandle, Receiver<EngineEvent>) {
        let (bus, events_rx) = EventBus::new();
        let bus: Arc<dyn EventSink> = Arc::new(bus);
        let (control_tx, control_rx) = unbounded::<ControlMsg>();
        let (wake_tx, wake_rx) = unbounded::<()>();

        let ids = Arc::new(IdAllocator::new());
        let ledger = Arc::new(RwLock::new(ExecutionLedger::new()));
        let versions = Arc::new(Snapshot::new(Versions::new(frame)));
        let queue_state = Arc::new(Snapshot::new(RunState::default()));
        let hashes = Arc::new(Snapshot::new(FxHashMap::default()));
        let introspect = Arc::new(Snapshot::new(IntrospectSnapshot::default()));
        let epoch = SessionEpoch(0);

        let shared = Arc::new(Shared {
            session,
            queue: Mutex::new(RunQueue::new()),
            queue_state: Arc::clone(&queue_state),
            ledger: Arc::clone(&ledger),
            versions: Arc::clone(&versions),
            hashes: Arc::clone(&hashes),
            introspect: Arc::clone(&introspect),
            ids: Arc::clone(&ids),
            store: Arc::new(ResultStore::new()),
            events: Arc::clone(&bus),
            epoch: AtomicU32::new(epoch.0),
            control: control_tx.clone(),
            wake: wake_tx.clone(),
        });

        let control = {
            let mut loop_ = ControlLoop::new(
                Arc::clone(&versions),
                Arc::clone(&ledger),
                Arc::clone(&queue_state),
                Arc::clone(&bus),
                Arc::clone(&ids),
                epoch,
            );
            loop_.set_hashes(Arc::clone(&hashes));
            std::thread::Builder::new()
                .name("stratum-control".into())
                .spawn(move || loop_.run(&control_rx))
                .expect("control thread")
        };

        let worker = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("stratum-session".into())
                .spawn(move || worker_loop(host, &shared, &wake_rx))
                .expect("session worker")
        };

        shared.events.emit(EngineEvent::EngineHealth {
            seq: 0,
            health: EngineHealth::Ready,
        });

        (
            EngineHandle {
                shared,
                control: control_tx,
                threads: Some((control, worker)),
            },
            events_rx,
        )
    }
}

/// The public handle.
pub struct EngineHandle {
    shared: Arc<Shared>,
    control: Sender<ControlMsg>,
    threads: Option<(JoinHandle<()>, JoinHandle<()>)>,
}

impl EngineHandle {
    /// Open or re-analyse a document. The control thread sweeps it and emits
    /// `BlockMapChanged` + `StatusChanged`.
    pub fn set_doc(&self, doc: AnalysedDoc, text: Arc<str>, announce: bool) {
        let _ = self.control.send(ControlMsg::Doc {
            doc: Box::new(doc),
            text,
            announce,
        });
    }

    /// Close a document.
    pub fn close_doc(&self, doc: DocumentId) {
        let _ = self.control.send(ControlMsg::DocClosed(doc));
    }

    /// Resolve and enqueue an intent. Returns the plan, which is
    /// `EngineResponse::Submitted`'s payload.
    ///
    /// # Errors
    /// Every [`PlanError`] the planner can raise; each maps to an
    /// `EngineError` the UI can explain.
    pub fn submit(
        &self,
        intent: RunIntent,
        inline: InlineResultsMode,
    ) -> Result<RunPlan, PlanError> {
        let (tx, rx) = bounded(1);
        let origin = origin_of(&intent);
        if self
            .control
            .send(ControlMsg::Resolve {
                intent: Box::new(intent),
                reply: tx,
            })
            .is_err()
        {
            return Err(PlanError::NothingToRun);
        }
        let (plan, sources, doc) = rx.recv().map_err(|_| PlanError::NothingToRun)??;
        if !plan.items.is_empty() {
            let run = QueuedRun::new(&plan, doc, origin, inline, sources);
            self.shared.queue.lock().push(run);
            self.publish_queue();
            let _ = self.shared.wake.send(());
        }
        Ok(plan)
    }

    /// Request cancellation of one run.
    pub fn cancel(&self, run: stratum_proto::RunId, level: CancelLevel) -> bool {
        let ok = self.shared.queue.lock().cancel(run, level, now_ms());
        self.publish_queue();
        ok
    }

    /// Request cancellation of everything queued and running.
    pub fn cancel_all(&self, level: CancelLevel) {
        self.shared.queue.lock().cancel_all(level, now_ms());
        self.publish_queue();
    }

    /// A document's statuses (`EngineRequest::Statuses`).
    #[must_use]
    pub fn statuses(&self, doc: DocumentId) -> Vec<(BlockId, BlockStatus)> {
        let (tx, rx) = bounded(1);
        if self
            .control
            .send(ControlMsg::Statuses { doc, reply: tx })
            .is_err()
        {
            return Vec::new();
        }
        rx.recv().unwrap_or_default()
    }

    /// The ledger, for `EngineRequest::Ledger` and spec §11's History pane.
    #[must_use]
    pub fn ledger(&self) -> Arc<RwLock<ExecutionLedger>> {
        Arc::clone(&self.shared.ledger)
    }

    /// The result store, for the asset handler.
    #[must_use]
    pub fn results(&self) -> Arc<ResultStore> {
        Arc::clone(&self.shared.store)
    }

    /// The current version table — a lock-free-ish snapshot (`03` §11.1's
    /// `state()`).
    #[must_use]
    pub fn versions(&self) -> Arc<Versions> {
        self.shared.versions.load()
    }

    /// The current introspection snapshot, which implements
    /// [`stratum_proto::SessionIntrospect`].
    #[must_use]
    pub fn introspect(&self) -> Arc<IntrospectSnapshot> {
        self.shared.introspect.load()
    }

    /// Serve `EngineRequest::AiContext` (A5).
    ///
    /// Answered from the published snapshot and the ledger, never by asking the
    /// worker: the context packer must not be able to stall behind a running
    /// command, and a metadata answer as of the last completed command is the
    /// one the UI wants anyway.
    #[must_use]
    pub fn ai_context(
        &self,
        want: stratum_proto::AiContextWant,
    ) -> stratum_proto::AiContextSnapshot {
        let snapshot = self.shared.introspect.load();
        let ledger = self.shared.ledger.read();
        snapshot.ai_context(want, &ledger.view())
    }

    /// The session epoch.
    #[must_use]
    pub fn epoch(&self) -> SessionEpoch {
        SessionEpoch(self.shared.epoch.load(Ordering::Acquire))
    }

    /// Queue counters `(enqueued, dequeued, cancelled)`.
    #[must_use]
    pub fn queue_counters(&self) -> (u64, u64, u64) {
        self.shared.queue.lock().counters()
    }

    /// Block until the queue is empty and nothing is running. Test-facing;
    /// production code watches `RunFinished`.
    pub fn wait_idle(&self) {
        while !self.shared.queue.lock().is_idle() {
            std::thread::yield_now();
        }
    }

    fn publish_queue(&self) {
        let state = self.shared.queue.lock().run_state();
        self.shared.queue_state.store(state);
        let _ = self.control.send(ControlMsg::QueueChanged);
    }

    /// Stop both threads and join them.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.control.send(ControlMsg::Shutdown);
        drop(std::mem::replace(&mut self.shared.wake.clone(), {
            let (tx, _) = unbounded();
            tx
        }));
        if let Some((control, worker)) = self.threads.take() {
            // Waking the worker with a closed channel is how it learns to stop.
            let _ = control.join();
            let _ = worker.join();
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.control.send(ControlMsg::Shutdown);
        if let Some((control, worker)) = self.threads.take() {
            let _ = control.join();
            let _ = worker.join();
        }
    }
}

fn origin_of(intent: &RunIntent) -> ExecOrigin {
    match intent {
        RunIntent::CommandBar { .. } => ExecOrigin::CommandBar,
        RunIntent::Selection { .. } => ExecOrigin::Selection,
        RunIntent::CleanRun { .. } | RunIntent::ProjectEntryPoint { .. } => ExecOrigin::CleanRun,
        _ => ExecOrigin::Editor,
    }
}

// ---------------------------------------------------------------------------
// The session worker
// ---------------------------------------------------------------------------

fn worker_loop<H: SessionHost>(mut host: H, shared: &Arc<Shared>, wake: &Receiver<()>) {
    loop {
        // Drain everything available before sleeping, so a queue of ten blocks
        // is ten commands and one wakeup.
        loop {
            let next = shared.queue.lock().pop();
            let Some(next) = next else { break };
            run_one(&mut host, shared, next);
        }
        if wake.recv().is_err() {
            break;
        }
    }
}

fn run_one<H: SessionHost>(host: &mut H, shared: &Arc<Shared>, next: crate::queue::Dequeued) {
    let started_at_ms = now_ms();
    let started = std::time::Instant::now();

    if next.first {
        let (epoch_reset, clean_state, cwd) = {
            let mut q = shared.queue.lock();
            let run = q.front_mut().expect("the run we just popped from");
            run.started_at_ms = started_at_ms;
            (run.epoch_reset, run.clean_state, Utf8PathBuf::from("."))
        };
        if epoch_reset {
            let cfg = CleanRunConfig {
                cwd,
                ..CleanRunConfig::default()
            };
            let epoch = host.reset_clean(&cfg);
            shared.epoch.store(epoch.0, Ordering::Release);
            // A fresh session has an empty version table by construction.
            shared
                .versions
                .store(Versions::new(shared.versions.load().frame().to_owned()));
            let _ = shared.control.send(ControlMsg::EpochChanged(epoch));
        }
        shared.events.emit(EngineEvent::RunStarted {
            seq: 0,
            schema: stratum_proto::STREAM_SCHEMA,
            run: next.run,
            session: shared.session,
            stratum_version: env!("CARGO_PKG_VERSION").to_owned(),
            source: None,
            clean_state,
            cwd: Utf8PathBuf::from("."),
            started_at_ms,
            seed: None,
            plan_len: u32::try_from(shared.queue.lock().pending() + 1).unwrap_or(u32::MAX),
        });
    }

    let exec = shared.ids.exec();
    let result = shared.ids.result();
    {
        let mut q = shared.queue.lock();
        q.mark_running(next.run, next.item.block, exec, started_at_ms);
    }
    publish_queue(shared);

    let dataset_in = current_dataset(shared);
    shared.events.emit(EngineEvent::BlockStarted {
        seq: 0,
        run: next.run,
        exec,
        block: next.item.block,
        doc: None,
        span: next.item.span,
        code_hash: next.item.code_hash,
        dataset_state_in: dataset_in,
        text: next.item.source.to_string(),
    });

    let mut coalescer = OutputCoalescer::new();
    let outcome = {
        let mut ctx = ExecContext {
            exec,
            block: next.item.block,
            source: &next.item.source,
            cancel: &next.token,
            result,
            clean: false,
            inline: InlineResultsMode::Always,
            store: &shared.store,
            events: shared.events.as_ref(),
            coalescer: &mut coalescer,
        };
        let outcome = host.execute(&mut ctx);
        ctx.flush();
        outcome
    };

    // ---- commit -----------------------------------------------------------
    // Order is load-bearing (A17): the version table is published BEFORE
    // `BlockFinished` is emitted, so a client that reacts to the event can
    // never observe a version table older than the execution it just saw.
    let mut writes = outcome.writes.clone();
    writes.normalize();
    let mut reads = outcome.reads.clone();
    reads.normalize();

    let dataset_out = apply_writes(
        shared,
        exec,
        &writes,
        outcome.frame.as_deref(),
        outcome.dataset_state,
        dataset_in,
    );
    let state_out = if writes.is_empty() {
        current_state(shared)
    } else {
        shared.ids.state()
    };

    let stale_on_arrival = shared
        .hashes
        .load()
        .get(&next.item.block)
        .is_some_and(|h| *h != next.item.code_hash);

    for payload in &outcome.payloads {
        shared.store.push_item(result, payload.clone());
    }

    let duration_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let record = ExecutionRecord {
        exec,
        seq: 0,
        session: shared.session,
        epoch: SessionEpoch(shared.epoch.load(Ordering::Acquire)),
        run: next.run,
        block: next.item.block,
        doc: None,
        origin: ExecOrigin::Editor,
        code_hash: next.item.code_hash,
        source: next.item.source.to_string(),
        input_state: current_state(shared),
        output_state: state_out,
        input_dataset: dataset_in,
        output_dataset: dataset_out,
        result: Some(result),
        status: outcome.status.clone(),
        started_at_ms,
        duration_us,
        stale_on_arrival,
        taint: outcome.taint,
    };
    set_state(shared, state_out);

    let writes = Arc::new(writes);
    shared.ledger.write().append(Committed {
        record: record.clone(),
        reads: Arc::new(reads),
        writes: Arc::clone(&writes),
    });
    shared.introspect.store(host.introspect());

    if !outcome.payloads.is_empty() {
        shared.events.emit(EngineEvent::Result {
            seq: 0,
            exec,
            envelope: envelope(shared, &record, result, &outcome),
        });
    }
    if !writes.is_empty() {
        shared.events.emit(EngineEvent::StateChanged {
            seq: 0,
            exec,
            dataset_state: dataset_out,
            state: state_out,
            frame: outcome
                .frame
                .clone()
                .unwrap_or_else(|| shared.versions.load().frame().to_owned()),
            n_obs: outcome.n_obs,
            n_vars: outcome.n_vars,
            events: Vec::new(),
        });
    }
    shared.events.emit(EngineEvent::BlockFinished {
        seq: 0,
        run: next.run,
        exec,
        block: next.item.block,
        result: Some(result),
        status: outcome.status.clone(),
        rc: outcome.rc,
        duration_us,
        dataset_state_out: dataset_out,
    });

    let _ = shared.control.send(ControlMsg::Committed {
        doc: None,
        block: next.item.block,
        writes,
    });

    // ---- run bookkeeping --------------------------------------------------
    let finished = {
        let mut q = shared.queue.lock();
        q.mark_idle();
        if let Some(run) = q.front_mut() {
            run.blocks_run += 1;
            if outcome.rc != 0 {
                run.blocks_failed += 1;
                run.rc = run.rc.max(outcome.rc);
            }
        }
        // A cancelled run stops here even if items remain; the queue already
        // cleared them, so this is just the drained check.
        q.front_is_drained().then(|| q.pop_run()).flatten()
    };
    publish_queue(shared);

    if let Some(run) = finished {
        shared.events.emit(EngineEvent::RunFinished {
            seq: 0,
            run: run.run,
            rc: run.rc,
            blocks_run: run.blocks_run,
            blocks_failed: run.blocks_failed,
            duration_us: now_ms()
                .saturating_sub(run.started_at_ms)
                .saturating_mul(1_000),
            finished_at_ms: now_ms(),
        });
    }
}

fn publish_queue(shared: &Arc<Shared>) {
    let state = shared.queue.lock().run_state();
    shared.queue_state.store(state);
    let _ = shared.control.send(ControlMsg::QueueChanged);
}

/// Apply a write footprint, and any frame switch, to the version table —
/// O(slots touched), never O(columns) and certainly never O(rows).
fn apply_writes(
    shared: &Arc<Shared>,
    exec: ExecutionId,
    writes: &RecordedWrites,
    frame: Option<&str>,
    reported: Option<DatasetStateId>,
    current: DatasetStateId,
) -> DatasetStateId {
    // The current frame decides where a STATIC variable read resolves (C5) and
    // which `DepKey::Var` slot a bare name denotes, so a `frame change` that
    // wrote nothing still has to move it — and it has to move BEFORE the
    // emptiness shortcut below. Leaving the table on the old frame makes the
    // dep index key later reads under a frame nothing writes, which loses
    // candidates and leaves blocks showing Current after their inputs moved:
    // the one direction INV-1 forbids.
    let moved = frame.is_some_and(|f| f != shared.versions.load().frame());
    if writes.is_empty() && !moved {
        // Nothing changed, so the dataset state is literally the same state.
        // This is what makes "Dataset state: D17" a recurring identity rather
        // than a counter that only goes up (`03` §4.4).
        return reported.unwrap_or(current);
    }
    let mut versions = (*shared.versions.load()).clone();
    if let Some(f) = frame.filter(|_| moved) {
        versions.set_frame(f);
    }
    for key in writes.creates.iter().chain(writes.writes.iter()) {
        versions.bump(key.clone(), exec);
    }
    for key in &writes.drops {
        versions.remove(key);
    }
    shared.versions.store(versions);
    if writes.is_empty() {
        // A frame switch alone moves no data, so it mints no dataset state.
        return reported.unwrap_or(current);
    }
    reported.unwrap_or_else(|| shared.ids.dataset())
}

fn current_dataset(shared: &Arc<Shared>) -> DatasetStateId {
    shared
        .ledger
        .read()
        .records()
        .last()
        .map_or(DatasetStateId(0), |c| c.record.output_dataset)
}

fn current_state(shared: &Arc<Shared>) -> StateId {
    shared
        .ledger
        .read()
        .records()
        .last()
        .map_or(StateId(0), |c| c.record.output_state)
}

fn set_state(_shared: &Arc<Shared>, _state: StateId) {}

fn envelope(
    shared: &Arc<Shared>,
    record: &ExecutionRecord,
    result: ResultId,
    outcome: &ExecOutcome,
) -> ResultEnvelope {
    ResultEnvelope {
        result,
        revision: 0,
        exec: record.exec,
        block: record.block.is_real().then_some(record.block),
        dataset_state: record.output_dataset,
        code_hash: record.code_hash,
        cmdline: record.source.clone(),
        started_at_ms: record.started_at_ms,
        duration_us: record.duration_us,
        rc: outcome.rc,
        payloads: outcome.payloads.clone(),
        raw: shared.store.raw_ref(shared.session, result),
        layout_hint: stratum_proto::LayoutHint::default(),
        // A22: computed in Rust from what this build implements, never
        // synthesised by the frontend. `RawOutput` is always present and always
        // last (spec §17).
        actions: vec![stratum_proto::CardAction::RawOutput],
    }
}
