//! The control thread — ARCHITECTURE C50, audit finding A17.
//!
//! Reconcile and the C0–C9 sweep run **here**, never on the session worker.
//!
//! The pre-audit design gave reconcile no thread owner while user code ran, so
//! during a 30-second `regress` nothing serviced `DocChange`: the frontend's
//! local wasm check would mark the edited block `Stale(CodeChanged)` and every
//! dependent block below it would keep showing ✓ Current until the command
//! finished. That is precisely the §12 failure this subsystem exists to
//! prevent, reintroduced by a scheduling omission.
//!
//! # The discipline that makes it hold
//!
//! The control thread never takes a lock the session worker can hold across a
//! command. State reaches it through [`Snapshot`], whose write path is a
//! pointer swap: the worker builds a new value, takes the lock for the length
//! of one `Arc` assignment, and releases it. A reader clones the `Arc` and
//! works on an immutable value with no lock held at all.
//!
//! `parking_lot::RwLock<Arc<T>>` rather than `arc-swap` because the workspace
//! dependency table (W00's root `Cargo.toml`) does not carry `arc-swap` and a
//! member crate that adds a dependency outside that table is how a workspace
//! ends up resolving two versions of one crate. The property C50 needs is
//! "the critical section contains no user code", and that holds either way.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use stratum_proto::{
    BlockId, BlockStatus, CodeHash, DocumentId, EngineEvent, RunIntent, RunPlan, SessionEpoch,
};

use crate::depindex::{DepIndex, SweepStats};
use crate::ledger::ExecutionLedger;
use crate::plan::{self, PlanCtx, PlanError};
use crate::staleness::{AnalysedDoc, RecordedWrites, RunState, StatusMap, SweepInput, Versions};
use crate::stream::EventSink;

/// A swap-only publication cell.
///
/// The writer replaces the whole value; readers clone an `Arc`. Nothing that
/// runs user code is ever inside the critical section, which is the invariant
/// C50 actually needs.
#[derive(Debug)]
pub struct Snapshot<T>(RwLock<Arc<T>>);

impl<T> Snapshot<T> {
    /// Publish an initial value.
    pub fn new(value: T) -> Self {
        Self(RwLock::new(Arc::new(value)))
    }

    /// Read the current value. Holds the lock for one `Arc` clone.
    pub fn load(&self) -> Arc<T> {
        Arc::clone(&self.0.read())
    }

    /// Publish a new value. Holds the lock for one pointer assignment.
    pub fn store(&self, value: T) {
        *self.0.write() = Arc::new(value);
    }

    /// Publish an already-shared value.
    pub fn store_arc(&self, value: Arc<T>) {
        *self.0.write() = value;
    }
}

/// What [`ControlMsg::Resolve`] answers with: the plan, one enqueue-time source
/// snapshot per plan item, and the document the plan came from — `None` for a
/// command-bar run, which belongs to no document.
pub type Resolved = (RunPlan, Vec<Arc<str>>, Option<DocumentId>);

/// What the control thread is asked to do.
pub enum ControlMsg {
    /// A document was opened or re-analysed. Carries the reconciled map and its
    /// text; the control thread does a full sweep and emits `BlockMapChanged`
    /// followed by `StatusChanged`.
    Doc {
        /// The analysed document.
        doc: Box<AnalysedDoc>,
        /// Its current text, for plan-item snapshots.
        text: Arc<str>,
        /// Emit `BlockMapChanged` for it (false on the initial open, where the
        /// caller already has the map it just sent).
        announce: bool,
    },
    /// A document was closed.
    DocClosed(DocumentId),
    /// An execution committed. The write footprint drives the incremental
    /// candidate set.
    Committed {
        /// The document the block belongs to, if any.
        doc: Option<DocumentId>,
        /// The block that ran, `EPHEMERAL` for a command-bar run.
        block: BlockId,
        /// What it wrote.
        writes: Arc<RecordedWrites>,
    },
    /// The queue moved: something started, finished or was cancelled. Clause C0
    /// is a fact about the queue, so it gets its own trigger.
    QueueChanged,
    /// The session epoch changed — a clean run built a new session.
    EpochChanged(SessionEpoch),
    /// Resolve an intent into a plan. Runs here because this thread owns the
    /// documents and the statuses, so a plan can never disagree with the
    /// gutter that produced it.
    Resolve {
        /// What the user pressed.
        intent: Box<RunIntent>,
        /// Where to send the answer.
        reply: Sender<Result<Resolved, PlanError>>,
    },
    /// Hand back a document's statuses (`EngineRequest::Statuses`).
    Statuses {
        /// Which document.
        doc: DocumentId,
        /// Where to send the answer.
        reply: Sender<Vec<(BlockId, BlockStatus)>>,
    },
    /// Stop.
    Shutdown,
}

struct DocState {
    doc: Arc<AnalysedDoc>,
    text: Arc<str>,
    status: StatusMap,
    index: DepIndex,
}

/// The control thread's state and loop.
pub struct ControlLoop {
    docs: FxHashMap<DocumentId, DocState>,
    versions: Arc<Snapshot<Versions>>,
    ledger: Arc<RwLock<ExecutionLedger>>,
    queue_state: Arc<Snapshot<RunState>>,
    events: Arc<dyn EventSink>,
    epoch: SessionEpoch,
    run_ids: Arc<crate::ledger::IdAllocator>,
    project_entry: Option<DocumentId>,
    verify: bool,
    /// Every open document's CURRENT code hash per block, republished on every
    /// reconcile. See [`ControlLoop::set_hashes`].
    hashes: Arc<Snapshot<FxHashMap<BlockId, CodeHash>>>,
}

impl ControlLoop {
    /// Build the loop over the state it shares with the session worker.
    #[must_use]
    pub fn new(
        versions: Arc<Snapshot<Versions>>,
        ledger: Arc<RwLock<ExecutionLedger>>,
        queue_state: Arc<Snapshot<RunState>>,
        events: Arc<dyn EventSink>,
        run_ids: Arc<crate::ledger::IdAllocator>,
        epoch: SessionEpoch,
    ) -> Self {
        Self {
            docs: FxHashMap::default(),
            versions,
            ledger,
            queue_state,
            events,
            epoch,
            run_ids,
            project_entry: None,
            verify: cfg!(debug_assertions) || cfg!(feature = "verify-staleness"),
            // Replaced by `set_hashes` with the cell the session worker reads.
            // Owning one from the start means every code path below can publish
            // unconditionally, so there is no "hashes were never wired up" state
            // in which the loop silently stops maintaining them.
            hashes: Arc::new(Snapshot::new(FxHashMap::default())),
        }
    }

    /// Share the cell the session worker reads to stamp `stale_on_arrival`.
    ///
    /// The worker holds the ENQUEUE-TIME source and its hash; this thread owns
    /// the documents, so it is the only one that knows what the block says
    /// *now*. Publishing from here is what makes the enqueue-time snapshot
    /// honest during a long run: edit a queued block while a 30-second
    /// `regress` is on the worker and the next reconcile republishes the map,
    /// so when the queued block finally runs it is recorded as having run text
    /// the document no longer contains. C3 independently marks it
    /// `Stale(CodeChanged)`; this flag is what lets the History pane say WHY
    /// without re-deriving it.
    pub fn set_hashes(&mut self, hashes: Arc<Snapshot<FxHashMap<BlockId, CodeHash>>>) {
        self.hashes = hashes;
        self.publish_hashes();
    }

    /// `--verify-staleness`: run the full O(n²) sweep beside the incremental
    /// one and panic on divergence.
    pub fn set_verify(&mut self, on: bool) {
        self.verify = on;
        for d in self.docs.values_mut() {
            d.index.set_verify(on);
        }
    }

    /// The workspace's entry point, for `RunIntent::ProjectEntryPoint`.
    pub fn set_project_entry(&mut self, doc: Option<DocumentId>) {
        self.project_entry = doc;
    }

    /// Sweep counters, summed across documents.
    #[must_use]
    pub fn stats(&self) -> SweepStats {
        self.docs
            .values()
            .fold(SweepStats::default(), |mut acc, d| {
                let s = d.index.stats();
                acc.classified += s.classified;
                acc.upstream_tests += s.upstream_tests;
                acc.full += s.full;
                acc.incremental += s.incremental;
                acc.verified += s.verified;
                acc
            })
    }

    /// Run until `Shutdown` or the channel closes.
    pub fn run(mut self, rx: &Receiver<ControlMsg>) {
        while let Ok(msg) = rx.recv() {
            if !self.handle(msg) {
                break;
            }
        }
    }

    /// Handle one message. Returns false on shutdown.
    pub fn handle(&mut self, msg: ControlMsg) -> bool {
        match msg {
            ControlMsg::Doc {
                doc,
                text,
                announce,
            } => self.on_doc(*doc, text, announce),
            ControlMsg::DocClosed(id) => {
                self.docs.remove(&id);
                self.publish_hashes();
            }
            ControlMsg::Committed { doc, block, writes } => self.on_commit(doc, block, &writes),
            ControlMsg::QueueChanged => self.resweep_all(),
            ControlMsg::EpochChanged(epoch) => {
                self.epoch = epoch;
                // Every block is Stale(EpochReset) by C2; a full sweep is both
                // correct and cheap here because it happens once per clean run.
                let ids: Vec<DocumentId> = self.docs.keys().copied().collect();
                for id in ids {
                    self.full_sweep(id);
                }
            }
            ControlMsg::Resolve { intent, reply } => {
                let _ = reply.send(self.resolve(&intent));
            }
            ControlMsg::Statuses { doc, reply } => {
                let out = self
                    .docs
                    .get(&doc)
                    .map(|d| d.status.wire())
                    .unwrap_or_default();
                let _ = reply.send(out);
            }
            ControlMsg::Shutdown => return false,
        }
        true
    }

    fn on_doc(&mut self, doc: AnalysedDoc, text: Arc<str>, announce: bool) {
        let id = doc.map.doc;
        if announce {
            self.events.emit(EngineEvent::BlockMapChanged {
                seq: 0,
                map: doc.map.clone(),
            });
        }
        let mut index = DepIndex::new();
        index.set_verify(self.verify);
        // Retire the ledger's status entries for blocks that left the document.
        // Their records stay — history is append-only — but they are no longer
        // nodes in the graph (CONTRACTS §2).
        {
            let mut ledger = self.ledger.write();
            for retired in &doc.map.retired {
                ledger.retire(*retired);
            }
        }
        let doc = Arc::new(doc);
        let previous = self.docs.get(&id).map(|d| d.status.clone());
        let status = {
            let versions = self.versions.load();
            let queue = self.queue_state.load();
            let ledger = self.ledger.read();
            let input = SweepInput {
                doc: &doc,
                versions: &versions,
                ledger: &ledger,
                epoch: self.epoch,
                run: &queue,
            };
            index.full(&input)
        };
        let delta = previous
            .as_ref()
            .map_or_else(|| status.wire(), |prev| status.delta(prev));
        self.docs.insert(
            id,
            DocState {
                doc,
                text,
                status,
                index,
            },
        );
        // Before the statuses, not after: the worker may finish a block between
        // the two, and it must not stamp `stale_on_arrival = false` against a
        // hash map that predates the edit that just arrived.
        self.publish_hashes();
        self.emit_status(id, delta);
    }

    /// Republish `block -> current code hash` across every open document.
    ///
    /// Rebuilt rather than patched. Reconcile can retire, add and renumber
    /// blocks in one message, and a patched map that missed a retirement would
    /// leave a stale hash under a live id — which reads as "this block was
    /// edited" forever. The map is a few thousand 24-byte entries and this runs
    /// once per reconcile, not once per keystroke.
    fn publish_hashes(&self) {
        let mut map = FxHashMap::default();
        for state in self.docs.values() {
            for (idx, block) in state.doc.map.blocks.iter().enumerate() {
                if state.doc.is_executable(idx) {
                    map.insert(*block, state.doc.map.regions[idx].code_hash);
                }
            }
        }
        self.hashes.store(map);
    }

    fn on_commit(&mut self, doc: Option<DocumentId>, block: BlockId, writes: &RecordedWrites) {
        // A command-bar run belongs to no document, and that is exactly the case
        // document order cannot see (§6.4 Example 3): its writes still bump
        // versions and still make real blocks stale, in EVERY open document.
        let targets: Vec<DocumentId> = match doc {
            Some(_) if block.is_real() => self.docs.keys().copied().collect(),
            _ => self.docs.keys().copied().collect(),
        };
        for id in targets {
            let candidates = {
                let Some(state) = self.docs.get(&id) else {
                    continue;
                };
                let at = state.doc.index_of(block);
                state.index.candidates_for_commit(writes, at)
            };
            self.incremental_sweep(id, &candidates);
        }
    }

    /// Re-evaluate after a queue transition. Only C0 can have moved, so the
    /// candidate set is the blocks the queue mentions plus whatever was
    /// previously Queued or Running.
    fn resweep_all(&mut self) {
        let queue = self.queue_state.load();
        let ids: Vec<DocumentId> = self.docs.keys().copied().collect();
        for id in ids {
            let candidates: Vec<u32> = {
                let Some(state) = self.docs.get(&id) else {
                    continue;
                };
                let mut out: Vec<u32> = Vec::new();
                for (i, b) in state.doc.map.blocks.iter().enumerate() {
                    let i = u32::try_from(i).unwrap_or(u32::MAX);
                    let was_busy = matches!(
                        state.status.at(i as usize),
                        Some(BlockStatus::Queued { .. } | BlockStatus::Running { .. })
                    );
                    let is_busy =
                        queue.running.map(|(b, ..)| b) == Some(*b) || queue.queued.contains(b);
                    if was_busy || is_busy {
                        out.push(i);
                    }
                }
                out
            };
            if candidates.is_empty() {
                continue;
            }
            self.incremental_sweep(id, &candidates);
        }
    }

    fn full_sweep(&mut self, id: DocumentId) {
        let Some(state) = self.docs.get_mut(&id) else {
            return;
        };
        let doc = Arc::clone(&state.doc);
        let versions = self.versions.load();
        let queue = self.queue_state.load();
        let ledger = self.ledger.read();
        let input = SweepInput {
            doc: &doc,
            versions: &versions,
            ledger: &ledger,
            epoch: self.epoch,
            run: &queue,
        };
        let next = state.index.full(&input);
        let delta = next.delta(&state.status);
        state.status = next;
        drop(ledger);
        self.emit_status(id, delta);
    }

    fn incremental_sweep(&mut self, id: DocumentId, candidates: &[u32]) {
        let Some(state) = self.docs.get_mut(&id) else {
            return;
        };
        let doc = Arc::clone(&state.doc);
        let versions = self.versions.load();
        let queue = self.queue_state.load();
        let ledger = self.ledger.read();
        let input = SweepInput {
            doc: &doc,
            versions: &versions,
            ledger: &ledger,
            epoch: self.epoch,
            run: &queue,
        };
        let next = state.index.incremental(&input, &state.status, candidates);
        let delta = next.delta(&state.status);
        state.status = next;
        drop(ledger);
        self.emit_status(id, delta);
    }

    fn emit_status(&self, doc: DocumentId, changed: Vec<(BlockId, BlockStatus)>) {
        if changed.is_empty() {
            return;
        }
        self.events.emit(EngineEvent::StatusChanged {
            seq: 0,
            doc,
            changed,
        });
    }

    fn resolve(&mut self, intent: &RunIntent) -> Result<Resolved, PlanError> {
        let target = intent_doc(intent).or(self.project_entry);
        let Some(id) = target else {
            // A command-bar run needs no document at all.
            return self.resolve_docless(intent);
        };
        let Some(state) = self.docs.get(&id) else {
            return self.resolve_docless(intent);
        };
        let doc = Arc::clone(&state.doc);
        let text = Arc::clone(&state.text);
        let status = state.status.clone();
        let versions = self.versions.load();
        let queue = self.queue_state.load();
        let ledger = self.ledger.read();
        let sweep = SweepInput {
            doc: &doc,
            versions: &versions,
            ledger: &ledger,
            epoch: self.epoch,
            run: &queue,
        };
        let ctx = PlanCtx {
            doc: &doc,
            status: &status,
            text: &text,
            run: self.run_ids.run(),
            project_entry: self.project_entry,
            sweep: Some(&sweep),
        };
        let plan = plan::resolve(intent, &ctx)?;
        let sources = plan::snapshots(intent, &ctx, &plan);
        Ok((plan, sources, Some(id)))
    }

    fn resolve_docless(&self, intent: &RunIntent) -> Result<Resolved, PlanError> {
        let RunIntent::CommandBar { .. } = intent else {
            return Err(match intent_doc(intent) {
                Some(d) => PlanError::WrongDocument(d),
                None => PlanError::NoEntryPoint,
            });
        };
        let empty = AnalysedDoc::new(empty_map(), Vec::new());
        let status = StatusMap::empty(&empty);
        let ctx = PlanCtx {
            doc: &empty,
            status: &status,
            text: "",
            run: self.run_ids.run(),
            project_entry: None,
            sweep: None,
        };
        let plan = plan::resolve(intent, &ctx)?;
        let sources = plan::snapshots(intent, &ctx, &plan);
        Ok((plan, sources, None))
    }
}

fn intent_doc(intent: &RunIntent) -> Option<DocumentId> {
    match intent {
        RunIntent::CurrentBlock { doc, .. }
        | RunIntent::RunAndAdvance { doc, .. }
        | RunIntent::Selection { doc, .. }
        | RunIntent::FromHere { doc, .. }
        | RunIntent::EverythingAbove { doc, .. }
        | RunIntent::ToCursor { doc, .. }
        | RunIntent::CurrentSection { doc, .. }
        | RunIntent::AllStale { doc }
        | RunIntent::WholeFile { doc } => Some(*doc),
        RunIntent::CleanRun { entry, .. } => Some(*entry),
        RunIntent::ProjectEntryPoint { .. } | RunIntent::CommandBar { .. } => None,
    }
}

fn empty_map() -> stratum_proto::BlockMap {
    stratum_proto::BlockMap {
        doc: DocumentId(0),
        generation: 0,
        doc_version: 0,
        blocks: Vec::new(),
        regions: Vec::new(),
        markers: Vec::new(),
        sections: Vec::new(),
        retired: Vec::new(),
        diagnostics: Vec::new(),
        end_delimiter: stratum_proto::Delimiter::Cr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::IdAllocator;
    use crate::stream::EventBus;
    use crate::testkit::{code_hash, doc};

    struct Harness {
        loop_: ControlLoop,
        hashes: Arc<Snapshot<FxHashMap<BlockId, CodeHash>>>,
        // Held so the bus's blocking `send` for facts always has a receiver.
        _events: crossbeam_channel::Receiver<EngineEvent>,
    }

    fn harness() -> Harness {
        let (bus, rx) = EventBus::new();
        let hashes = Arc::new(Snapshot::new(FxHashMap::default()));
        let mut loop_ = ControlLoop::new(
            Arc::new(Snapshot::new(Versions::new("default"))),
            Arc::new(RwLock::new(ExecutionLedger::new())),
            Arc::new(Snapshot::new(RunState::default())),
            Arc::new(bus),
            Arc::new(IdAllocator::new()),
            SessionEpoch(0),
        );
        loop_.set_hashes(Arc::clone(&hashes));
        Harness {
            loop_,
            hashes,
            _events: rx,
        }
    }

    fn set_doc(h: &mut Harness, id: DocumentId, generation: u64, hashes: &[CodeHash]) {
        assert!(h.loop_.handle(ControlMsg::Doc {
            doc: Box::new(doc(id, generation, hashes)),
            text: Arc::from(""),
            announce: false,
        }));
    }

    #[test]
    fn reconcile_republishes_the_code_hash_the_worker_compares_against() {
        // The worker stamps `stale_on_arrival` by comparing the hash it RAN
        // against the hash the document holds NOW. Nothing else publishes that
        // map, so an edit that never reached it would silently record an
        // outdated execution as having been up to date.
        let mut h = harness();
        let d = DocumentId(1);
        set_doc(&mut h, d, 1, &[code_hash(1), code_hash(2)]);

        let published = h.hashes.load();
        assert_eq!(published.get(&BlockId(1)), Some(&code_hash(1)));
        assert_eq!(published.get(&BlockId(2)), Some(&code_hash(2)));

        // Edit block 1 while block 2 is untouched.
        set_doc(&mut h, d, 2, &[code_hash(9), code_hash(2)]);
        let published = h.hashes.load();
        assert_eq!(
            published.get(&BlockId(1)),
            Some(&code_hash(9)),
            "the edit must be visible to the worker before the queued block finishes"
        );
        assert_eq!(published.get(&BlockId(2)), Some(&code_hash(2)));
    }

    #[test]
    fn a_shrinking_document_drops_the_hashes_of_blocks_that_left() {
        // Rebuilt rather than patched: a hash left behind under a live id reads
        // as "this block was edited" forever.
        let mut h = harness();
        let d = DocumentId(1);
        set_doc(&mut h, d, 1, &[code_hash(1), code_hash(2), code_hash(3)]);
        assert_eq!(h.hashes.load().len(), 3);

        set_doc(&mut h, d, 2, &[code_hash(1)]);
        let published = h.hashes.load();
        assert_eq!(published.len(), 1);
        assert!(published.get(&BlockId(2)).is_none());

        assert!(h.loop_.handle(ControlMsg::DocClosed(d)));
        assert!(
            h.hashes.load().is_empty(),
            "a closed document publishes none"
        );
    }

    #[test]
    fn two_documents_publish_into_one_map() {
        // The worker holds a block id, not a (doc, block) pair, so the map has
        // to span every open document.
        let mut h = harness();
        set_doc(&mut h, DocumentId(1), 1, &[code_hash(1)]);
        set_doc(&mut h, DocumentId(2), 1, &[code_hash(5)]);
        assert_eq!(h.hashes.load().len(), 1, "ids collide across the fixtures");

        assert!(h.loop_.handle(ControlMsg::DocClosed(DocumentId(2))));
        assert_eq!(h.hashes.load().get(&BlockId(1)), Some(&code_hash(1)));
    }
}
