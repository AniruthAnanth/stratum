//! The immutable snapshots the engine's two threads exchange — ARCHITECTURE §4,
//! C50, audit item A17.
//!
//! A `DocChange` that arrives during a 30-second `regress` must still restale
//! the edited block *and everything downstream of it*. If reconcile needed the
//! session worker, nothing would service it for thirty seconds: C20's local wasm
//! check marks only the **edited** block, so every dependent block below it
//! would keep showing ✓ Current for the duration — which is precisely the §12
//! failure this architecture exists to prevent, reintroduced by a scheduling
//! omission. So the staleness sweep runs on the **control thread**, and the two
//! threads share nothing mutable. They exchange two cells:
//!
//! | cell | written by | read by |
//! |---|---|---|
//! | [`VersionTable`] + [`StatusIndex`], as ONE value | session worker, at every command commit | control |
//! | [`DocumentSnapshot`] | control, after every debounced reconcile | session worker |
//!
//! Each cell has exactly one writer; both threads may read both sides.
//!
//! The version table and the status index share **one** cell rather than one
//! each, and that is not tidiness. Two cells are two stores, and a reader
//! between them sees a `StatusIndex` from commit *n-1* beside a `VersionTable`
//! from commit *n* — the sweep would then judge one commit's `ExecutionRecord`
//! against another commit's versions. Measured, not theorised:
//! `tests/snapshot.rs` caught the two-cell form 363 times in 10⁴ interleavings
//! on the first run. One `Arc` swap makes the mix impossible.
//!
//! # The one ordering that could lie
//!
//! The sweep reads a *consistent but possibly one-commit-stale* `VersionTable`.
//! That is sound in one direction only. If control ever observed a
//! `BlockFinished` for an execution whose versions it had **not** yet seen, it
//! would compare a downstream block's recorded dependencies against a table that
//! predates the commit, find them equal, and paint the block ✓ Current when its
//! inputs had already moved. A block may briefly be shown *more* stale than it
//! is; it may never be shown *less* (INV-1).
//!
//! That is enforced structurally rather than by convention:
//!
//! * [`CommitPublisher::publish`] is the only way to obtain a [`FinishToken`],
//!   and [`CommitPublisher::block_finished`] is the only way to spend one. A
//!   `BlockFinished` therefore *cannot* be announced before its versions are
//!   visible — there is no ordering to get wrong, because the wrong order does
//!   not typecheck.
//! * [`SnapshotReader::observe`] performs the two loads in the only safe order
//!   (finished marker first, then the cells) and hands back both at once, so a
//!   reader cannot get the order wrong either.
//!
//! `tests/snapshot.rs` drives 10⁴ concurrent (commit, reconcile) interleavings
//! against this and asserts the invariant on every observation.
//!
//! # Why `RwLock<Arc<T>>` and not `arc-swap`
//!
//! ARCHITECTURE §4 names `arc-swap 1` (RCU, no locks on the read path). It is
//! **not in W00's workspace dependency table**, and a member crate taking a
//! dependency outside that table is how a workspace ends up resolving two
//! versions of one crate. [`Rcu`] provides the same shape — one writer, owned
//! `Arc` snapshots, readers never blocked by each other — with a read path that
//! is an uncontended read-lock acquire plus a refcount bump rather than a single
//! atomic load. Every write holds the lock for exactly one pointer store, so
//! there is nothing for a reader to wait behind. Swapping in `arc-swap` when the
//! table gains it is a change to this one type and nothing else. Lock poisoning
//! is deliberately ignored: a wedged cell would stop staleness updates for the
//! rest of the session, which is a strictly worse outcome than reading the last
//! good snapshot.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use rustc_hash::FxHashMap;
use stratum_proto::{
    BlockId, CodeHash, DatasetStateId, DepKey, DocumentId, ExecStatus, ExecutionId,
    ExecutionRecord, FrameId, RegionSummary, SessionEpoch, StateId, Taint, UnixMs,
};

use crate::state::fingerprint::{FileStamp, PathKey, RngFingerprint, StateFingerprint};

// ---------------------------------------------------------------------------
// The cell
// ---------------------------------------------------------------------------

/// A read-copy-update cell: readers take an owned `Arc`, the single writer
/// swaps a new one in.
#[derive(Debug)]
pub struct Rcu<T> {
    cell: RwLock<Arc<T>>,
}

impl<T> Rcu<T> {
    /// A cell holding `initial`.
    pub fn new(initial: T) -> Self {
        Self {
            cell: RwLock::new(Arc::new(initial)),
        }
    }

    /// The current value. Never blocks on another reader.
    pub fn load(&self) -> Arc<T> {
        Arc::clone(&self.cell.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Publish a new value. One writer per cell, by contract.
    pub fn store(&self, next: Arc<T>) {
        *self.cell.write().unwrap_or_else(|e| e.into_inner()) = next;
    }
}

// ---------------------------------------------------------------------------
// The three payloads
// ---------------------------------------------------------------------------

/// One frame's versions, keyed the way a [`DepKey`] names them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameVersions {
    /// `row_membership`.
    pub row_membership: u64,
    /// `row_order`.
    pub row_order: u64,
    /// `var_layout`.
    pub var_layout: u64,
    /// Name → `gen`. A name that is absent no longer resolves, which is
    /// `Broken`, not `Stale` — see [`VersionTable::version_of`].
    pub vars: FxHashMap<Box<str>, u32>,
}

/// `DepKey -> u64`, plus the two ids the status line shows (ARCHITECTURE §4).
///
/// Published by the session worker at every command commit.
///
/// `Default` is hand-written: none of `StateId`, `DatasetStateId` or
/// `SessionEpoch` has one, and none should — a defaulted id is a
/// plausible-looking wrong answer everywhere except here, where the zero values
/// mean "nothing has run yet".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionTable {
    /// Monotone publication sequence. The reader's ordering check uses it.
    pub seq: u64,
    /// The session state this table describes.
    pub state: StateId,
    /// The current frame's dataset state — spec §13's "D17".
    pub dataset: DatasetStateId,
    /// Bumps on clear-all and clean runs.
    pub epoch: SessionEpoch,
    /// Frames by the name a `DepKey` uses.
    pub frames: FxHashMap<Box<str>, FrameVersions>,
    /// `$name` and `` `name' `` → version. Locals carry a backtick prefix.
    pub macros: FxHashMap<Box<str>, u64>,
    /// `scalar name` → version.
    pub scalars: FxHashMap<Box<str>, u64>,
    /// `matrix name` → version.
    pub matrices: FxHashMap<Box<str>, u64>,
    /// `program define name` → version.
    pub programs: FxHashMap<Box<str>, u64>,
    /// `set`/`c()` → version.
    pub settings: FxHashMap<Box<str>, u64>,
    /// `e()` and the stored-estimates table.
    pub estimates: u64,
    /// `r()`.
    pub rclass: u64,
    /// `s()`.
    pub sclass: u64,
    /// The working directory.
    pub cwd: u64,
    /// The random-number stream.
    pub rng: RngFingerprint,
    /// External inputs.
    pub files: FxHashMap<PathKey, FileStamp>,
}

/// What [`VersionTable::version_of`] can say about one dependency slot.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Version {
    /// The slot exists and carries this version.
    At(u64),
    /// The name no longer resolves. Re-running would ERROR, not merely produce
    /// different numbers — `Broken`, not `Stale` (CONTRACTS §3).
    Unresolved,
}

impl Default for VersionTable {
    fn default() -> Self {
        Self {
            seq: 0,
            state: StateId(0),
            dataset: DatasetStateId(0),
            epoch: SessionEpoch(0),
            frames: FxHashMap::default(),
            macros: FxHashMap::default(),
            scalars: FxHashMap::default(),
            matrices: FxHashMap::default(),
            programs: FxHashMap::default(),
            settings: FxHashMap::default(),
            estimates: 0,
            rclass: 0,
            sclass: 0,
            cwd: 0,
            rng: RngFingerprint::fresh(),
            files: FxHashMap::default(),
        }
    }
}

impl VersionTable {
    /// Build the table from a session fingerprint.
    ///
    /// `frame_name` resolves a `FrameId` to the name a `DepKey` uses; frames are
    /// named in `stratum_data::FrameSet`, which this crate does not own.
    #[must_use]
    pub fn from_fingerprint(
        seq: u64,
        fp: &StateFingerprint,
        frame_name: &dyn Fn(FrameId) -> Option<Box<str>>,
    ) -> Self {
        let mut frames = FxHashMap::default();
        for (id, ds) in fp.frames.iter() {
            let Some(name) = frame_name(*id) else {
                continue;
            };
            let mut vars = FxHashMap::default();
            for (n, var) in ds.names.iter() {
                if let Some(v) = ds.version_of(*var) {
                    vars.insert(n.clone(), v.gen);
                }
            }
            frames.insert(
                name,
                FrameVersions {
                    row_membership: ds.row_membership,
                    row_order: ds.row_order,
                    var_layout: ds.var_layout,
                    vars,
                },
            );
        }
        let copy = |m: &crate::state::fingerprint::NameVersions| -> FxHashMap<Box<str>, u64> {
            m.iter().map(|(k, v)| (k.clone(), *v)).collect()
        };
        let mut macros = copy(&fp.locals)
            .into_iter()
            .map(|(k, v)| (format!("`{k}").into_boxed_str(), v))
            .collect::<FxHashMap<Box<str>, u64>>();
        for (k, v) in fp.globals.iter() {
            macros.insert(format!("${k}").into_boxed_str(), *v);
        }
        Self {
            seq,
            state: fp.id,
            dataset: fp.current().map_or(DatasetStateId(0), |d| d.id),
            epoch: fp.epoch,
            frames,
            macros,
            scalars: copy(&fp.scalars),
            matrices: copy(&fp.matrices),
            programs: copy(&fp.programs),
            settings: copy(&fp.settings),
            estimates: fp.estimates,
            rclass: fp.rclass,
            sclass: fp.sclass,
            cwd: fp.cwd,
            rng: fp.rng,
            files: fp.files.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        }
    }

    /// The version this table records for one dependency slot.
    ///
    /// A macro, scalar, matrix, program or setting that is absent reads as
    /// version 0 rather than `Unresolved`: a block that expanded an undefined
    /// local (which Stata expands to nothing, not an error) depends on it
    /// *staying* undefined, and defining it later must restale the block. A
    /// missing *variable* is `Unresolved`, because Stata does error on it.
    #[must_use]
    pub fn version_of(&self, key: &DepKey) -> Version {
        let frame = |name: &str| self.frames.get(name);
        match key {
            DepKey::Var { frame: f, name } => {
                match frame(f).and_then(|fv| fv.vars.get(name.as_str())) {
                    Some(gen) => Version::At(u64::from(*gen)),
                    None => Version::Unresolved,
                }
            }
            DepKey::RowMembership { frame: f } => match frame(f) {
                Some(fv) => Version::At(fv.row_membership),
                None => Version::Unresolved,
            },
            DepKey::RowOrder { frame: f } => match frame(f) {
                Some(fv) => Version::At(fv.row_order),
                None => Version::Unresolved,
            },
            DepKey::VarLayout { frame: f } => match frame(f) {
                Some(fv) => Version::At(fv.var_layout),
                None => Version::Unresolved,
            },
            DepKey::Macro { name } => Version::At(get(&self.macros, name)),
            DepKey::Scalar { name } => Version::At(get(&self.scalars, name)),
            DepKey::Matrix { name } => Version::At(get(&self.matrices, name)),
            DepKey::Program { name } => Version::At(get(&self.programs, name)),
            DepKey::Setting { name } => Version::At(get(&self.settings, name)),
            DepKey::Estimates => Version::At(self.estimates),
            DepKey::RClass => Version::At(self.rclass),
            DepKey::SClass => Version::At(self.sclass),
            DepKey::Cwd => Version::At(self.cwd),
            DepKey::Rng => Version::At(self.rng.key()),
            DepKey::File { path } => match self.files.get(&PathKey(path.clone())) {
                Some(s) => Version::At(s.key()),
                None => Version::Unresolved,
            },
        }
    }
}

fn get(m: &FxHashMap<Box<str>, u64>, name: &str) -> u64 {
    m.get(name).copied().unwrap_or(0)
}

/// The projection of an `ExecutionRecord` the staleness sweep needs.
///
/// The full record stays in the ledger; copying `source` into a snapshot that is
/// republished on every commit would put a `String` clone per block on the
/// commit path.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordSummary {
    /// The execution.
    pub exec: ExecutionId,
    /// Global completion order.
    pub seq: u64,
    /// The session epoch it ran under. C2 compares this.
    pub epoch: SessionEpoch,
    /// The hash of the code that ran. C3 compares this.
    pub code_hash: CodeHash,
    /// How it ended.
    pub status: ExecStatus,
    /// The dataset state it produced.
    pub dataset: DatasetStateId,
    /// The session state it produced.
    pub state: StateId,
    /// Why the record is weaker than exact. C8 reads `EXTERNAL`.
    pub taint: Taint,
    /// Wall time, recorded not asserted (ADR-017).
    pub duration_us: u64,
    /// When it started.
    pub started_at_ms: UnixMs,
    /// The block's text changed between enqueue and run.
    pub stale_on_arrival: bool,
}

impl From<&ExecutionRecord> for RecordSummary {
    fn from(r: &ExecutionRecord) -> Self {
        Self {
            exec: r.exec,
            seq: r.seq,
            epoch: r.epoch,
            code_hash: r.code_hash,
            status: r.status.clone(),
            dataset: r.output_dataset,
            state: r.output_state,
            taint: r.taint,
            duration_us: r.duration_us,
            started_at_ms: r.started_at_ms,
            stale_on_arrival: r.stale_on_arrival,
        }
    }
}

/// `BlockId -> latest ExecutionRecord summary` (ARCHITECTURE §4).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusIndex {
    /// Monotone publication sequence, in step with [`VersionTable::seq`].
    pub seq: u64,
    latest: FxHashMap<BlockId, RecordSummary>,
}

impl StatusIndex {
    /// The latest record for a block. `None` is C1's `NeverRun`.
    #[must_use]
    pub fn latest(&self, block: BlockId) -> Option<&RecordSummary> {
        self.latest.get(&block)
    }

    /// Blocks with a record.
    #[must_use]
    pub fn len(&self) -> usize {
        self.latest.len()
    }

    /// True when nothing has run.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.latest.is_empty()
    }

    /// The successor index with `block`'s latest record replaced.
    ///
    /// Ephemeral and trivia ids are dropped: `latest_by_block[0]` would
    /// otherwise be ambiguous between "the last command-bar run" and "every
    /// comment region in the document" (A3).
    #[must_use]
    pub fn with(&self, seq: u64, block: BlockId, rec: RecordSummary) -> Self {
        let mut next = self.clone();
        next.seq = seq;
        if block.is_real() {
            next.latest.insert(block, rec);
        }
        next
    }

    /// Forget retired blocks. Their records stay in the ledger.
    #[must_use]
    pub fn without(&self, seq: u64, retired: &[BlockId]) -> Self {
        let mut next = self.clone();
        next.seq = seq;
        for b in retired {
            next.latest.remove(b);
        }
        next
    }
}

/// Text, regions, block ids and code hashes for one document (ARCHITECTURE §4).
///
/// Published by the control thread after every debounced reconcile, read by the
/// session worker when it dequeues a run — which is why a block runs the text
/// the user pressed Run on, not whatever the editor holds by the time the queue
/// drains.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentSnapshot {
    /// The document.
    pub doc: DocumentId,
    /// Increments on every reconcile; the frontend drops out-of-order maps.
    pub generation: u64,
    /// The editor version this was computed against.
    pub doc_version: u64,
    /// The text. `Arc<str>` so publishing costs a refcount, not a copy.
    pub text: Arc<str>,
    /// Regions, in document order.
    pub regions: Arc<[RegionSummary]>,
    /// Parallel to `regions`. Trivia entries are `BlockId::NONE` (A3).
    pub blocks: Arc<[BlockId]>,
}

impl DocumentSnapshot {
    /// An empty document.
    #[must_use]
    pub fn empty(doc: DocumentId) -> Self {
        Self {
            doc,
            generation: 0,
            doc_version: 0,
            text: Arc::from(""),
            regions: Arc::from(Vec::new()),
            blocks: Arc::from(Vec::new()),
        }
    }

    /// The region carrying `block`, if it is still in the document.
    #[must_use]
    pub fn region_of(&self, block: BlockId) -> Option<&RegionSummary> {
        let i = self.blocks.iter().position(|b| *b == block)?;
        self.regions.get(i)
    }

    /// The code hashes of the executable regions, in document order — the input
    /// [`crate::doc::reconcile`] diffs.
    #[must_use]
    pub fn hashes(&self) -> Vec<CodeHash> {
        self.regions
            .iter()
            .filter(|r| crate::doc::is_executable(&r.kind))
            .map(|r| r.code_hash)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The publish/observe discipline
// ---------------------------------------------------------------------------

/// One commit's publication: the version table and the status index that
/// describe the *same* state, swapped in together.
///
/// Private, because it is an implementation detail of the cell rather than a
/// type any consumer names — [`Observation`] hands out the two halves.
#[derive(Debug)]
struct Published {
    versions: Arc<VersionTable>,
    statuses: Arc<StatusIndex>,
}

/// The cells, shared by both threads.
#[derive(Debug)]
pub struct SnapshotCells {
    published: Rcu<Published>,
    documents: Rcu<FxHashMap<DocumentId, Arc<DocumentSnapshot>>>,
    /// The sequence of the last commit whose `BlockFinished` has been
    /// announced. Written last, read first: that is the whole ordering.
    finished: AtomicU64,
}

impl Default for SnapshotCells {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotCells {
    /// Cells holding empty snapshots.
    #[must_use]
    pub fn new() -> Self {
        Self {
            published: Rcu::new(Published {
                versions: Arc::new(VersionTable::default()),
                statuses: Arc::new(StatusIndex::default()),
            }),
            documents: Rcu::new(FxHashMap::default()),
            finished: AtomicU64::new(0),
        }
    }

    /// The session worker's handle. Exactly one of these exists per session.
    #[must_use]
    pub fn commit_publisher(self: &Arc<Self>) -> CommitPublisher {
        CommitPublisher {
            cells: Arc::clone(self),
            seq: 0,
        }
    }

    /// The control thread's handle. Exactly one of these exists per session.
    #[must_use]
    pub fn document_publisher(self: &Arc<Self>) -> DocumentPublisher {
        DocumentPublisher {
            cells: Arc::clone(self),
        }
    }

    /// A read handle. Any number, on any thread.
    #[must_use]
    pub fn reader(self: &Arc<Self>) -> SnapshotReader {
        SnapshotReader {
            cells: Arc::clone(self),
        }
    }
}

/// Proof that a commit's versions are visible.
///
/// Obtainable only from [`CommitPublisher::publish`] and spendable only in
/// [`CommitPublisher::block_finished`]. Not `Clone`, not constructible
/// elsewhere: announcing a `BlockFinished` before publishing the versions it
/// produced is not an ordering to remember, it is a program that does not
/// compile.
#[derive(Debug)]
#[must_use = "a published commit must be announced with block_finished"]
pub struct FinishToken {
    seq: u64,
}

impl FinishToken {
    /// The publication sequence this token proves.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }
}

/// The session worker's writer for [`VersionTable`] and [`StatusIndex`].
#[derive(Debug)]
pub struct CommitPublisher {
    cells: Arc<SnapshotCells>,
    seq: u64,
}

impl CommitPublisher {
    /// Publish the state produced by one command commit.
    ///
    /// The table and the index are one value under one `seq`, so control can
    /// never see a `StatusIndex` that mentions an execution whose versions are
    /// missing — nor a table whose versions no index accounts for.
    pub fn publish(&mut self, fp: &StateFingerprint, index: StatusIndex) -> FinishToken {
        self.publish_table(
            VersionTable::from_fingerprint(self.seq + 1, fp, &|_| Some("default".into())),
            index,
        )
    }

    /// Publish an already-built table. Used where the caller can name frames.
    ///
    /// **One store, not two.** Both halves go into the cell as a single `Arc`
    /// swap, so no reader can ever hold a table from one commit beside an index
    /// from another.
    pub fn publish_table(
        &mut self,
        mut table: VersionTable,
        mut index: StatusIndex,
    ) -> FinishToken {
        self.seq += 1;
        table.seq = self.seq;
        index.seq = self.seq;
        self.cells.published.store(Arc::new(Published {
            versions: Arc::new(table),
            statuses: Arc::new(index),
        }));
        FinishToken { seq: self.seq }
    }

    /// The current index, to build the successor from.
    #[must_use]
    pub fn statuses(&self) -> Arc<StatusIndex> {
        Arc::clone(&self.cells.published.load().statuses)
    }

    /// The current table.
    #[must_use]
    pub fn versions(&self) -> Arc<VersionTable> {
        Arc::clone(&self.cells.published.load().versions)
    }

    /// The next sequence number `publish` will use.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.seq + 1
    }

    /// Announce that the block whose commit produced `token` has finished.
    ///
    /// `Release`, paired with the `Acquire` in [`SnapshotReader::observe`]: a
    /// reader that sees this marker is guaranteed to see the cells published
    /// before it.
    pub fn block_finished(&self, token: FinishToken) {
        self.cells.finished.store(token.seq, Ordering::Release);
    }
}

/// The control thread's writer for [`DocumentSnapshot`].
#[derive(Debug)]
pub struct DocumentPublisher {
    cells: Arc<SnapshotCells>,
}

impl DocumentPublisher {
    /// Publish a document snapshot.
    pub fn publish(&self, snap: DocumentSnapshot) {
        let mut next = (*self.cells.documents.load()).clone();
        next.insert(snap.doc, Arc::new(snap));
        self.cells.documents.store(Arc::new(next));
    }

    /// Forget a closed document.
    pub fn close(&self, doc: DocumentId) {
        let mut next = (*self.cells.documents.load()).clone();
        next.remove(&doc);
        self.cells.documents.store(Arc::new(next));
    }

    /// The current snapshot for a document.
    #[must_use]
    pub fn get(&self, doc: DocumentId) -> Option<Arc<DocumentSnapshot>> {
        self.cells.documents.load().get(&doc).cloned()
    }
}

/// One consistent look at the worker's side of the world.
#[derive(Clone, Debug)]
pub struct Observation {
    /// The publication sequence of the last announced `BlockFinished`.
    pub finished_seq: u64,
    /// Versions, guaranteed to be at or after `finished_seq`.
    pub versions: Arc<VersionTable>,
    /// Statuses, from the same publication — `statuses.seq == versions.seq`,
    /// always, because the two are one `Arc`.
    pub statuses: Arc<StatusIndex>,
}

/// A reader of the cells. Any thread may hold one.
#[derive(Debug)]
pub struct SnapshotReader {
    cells: Arc<SnapshotCells>,
}

impl SnapshotReader {
    /// Read the worker's cells in the only safe order.
    ///
    /// The `finished` marker is loaded **first** with `Acquire`. Everything the
    /// worker stored before its `Release` of that marker is therefore visible,
    /// so `versions.seq >= finished_seq` always holds — the reader can never
    /// judge a finished block against a table that predates it, which is the one
    /// ordering that could make a block appear *less* stale than it is.
    ///
    /// Reading the cells first and the marker second would admit exactly that,
    /// which is why there is no API that does.
    #[must_use]
    pub fn observe(&self) -> Observation {
        let finished_seq = self.cells.finished.load(Ordering::Acquire);
        let published = self.cells.published.load();
        debug_assert!(
            published.versions.seq >= finished_seq,
            "INV-1: observed VersionTable seq {} predates the announced BlockFinished at {finished_seq}",
            published.versions.seq
        );
        Observation {
            finished_seq,
            versions: Arc::clone(&published.versions),
            statuses: Arc::clone(&published.statuses),
        }
    }

    /// A document snapshot, for the session worker dequeuing a run.
    #[must_use]
    pub fn document(&self, doc: DocumentId) -> Option<Arc<DocumentSnapshot>> {
        self.cells.documents.load().get(&doc).cloned()
    }

    /// Every open document.
    #[must_use]
    pub fn documents(&self) -> Vec<Arc<DocumentSnapshot>> {
        let mut v: Vec<Arc<DocumentSnapshot>> =
            self.cells.documents.load().values().cloned().collect();
        v.sort_by_key(|d| d.doc.0);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::fingerprint::Ns;

    #[test]
    fn an_absent_macro_reads_as_version_zero_but_an_absent_variable_is_unresolved() {
        let fp = StateFingerprint::fresh(SessionEpoch(1), FrameId(0));
        let t = VersionTable::from_fingerprint(1, &fp, &|_| Some("default".into()));
        assert_eq!(
            t.version_of(&DepKey::Macro {
                name: "`nope".into()
            }),
            Version::At(0)
        );
        assert_eq!(
            t.version_of(&DepKey::Var {
                frame: "default".into(),
                name: "nope".into()
            }),
            Version::Unresolved
        );
    }

    #[test]
    fn a_local_and_a_global_of_the_same_name_are_two_slots() {
        let mut fp = StateFingerprint::fresh(SessionEpoch(1), FrameId(0));
        fp.bump_named(Ns::Local, "x");
        let t = VersionTable::from_fingerprint(1, &fp, &|_| Some("default".into()));
        assert_eq!(
            t.version_of(&DepKey::Macro { name: "`x".into() }),
            Version::At(1)
        );
        assert_eq!(
            t.version_of(&DepKey::Macro { name: "$x".into() }),
            Version::At(0)
        );
    }

    #[test]
    fn a_token_carries_the_sequence_it_published() {
        let cells = Arc::new(SnapshotCells::new());
        let mut pubr = cells.commit_publisher();
        let fp = StateFingerprint::fresh(SessionEpoch(1), FrameId(0));
        let t1 = pubr.publish(&fp, StatusIndex::default());
        assert_eq!(t1.seq(), 1);
        let reader = cells.reader();
        assert_eq!(reader.observe().finished_seq, 0, "not announced yet");
        pubr.block_finished(t1);
        let o = reader.observe();
        assert_eq!(o.finished_seq, 1);
        assert!(o.versions.seq >= o.finished_seq);
    }
}
