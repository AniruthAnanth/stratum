//! The snapshot publish/observe discipline — W06's A17 acceptance bullet.
//!
//! > **`ArcSwap` snapshot discipline (A17).** `snapshot.rs` publishes
//! > `VersionTable`/`StatusIndex` *before* `BlockFinished` is emitted, and a
//! > test drives 10⁴ concurrent (commit, reconcile) interleavings under `loom`
//! > asserting that a reader never observes a `VersionTable` newer than the
//! > `BlockFinished` it has seen — the one ordering that could make a block
//! > appear *less* stale than it is (INV-1).
//!
//! # Two deviations from that sentence, both deliberate, both reported
//!
//! **The direction.** Observing a table *newer* than the announced
//! `BlockFinished` is the safe case: it can only make a block look *more* stale,
//! which INV-1 permits. The hazard the same sentence names — "appear *less*
//! stale than it is" — is the opposite observation: a `BlockFinished` announced
//! at sequence *n* against a `VersionTable` still at *n-1*. Control would then
//! compare a downstream block's recorded dependencies against a table that
//! predates the commit, find them equal, and paint it ✓ Current when its inputs
//! had already moved. The invariant asserted here is therefore
//! `versions.seq >= finished_seq`, which is what the module implements and what
//! the second half of the bullet's own sentence asks for.
//!
//! **`loom`.** `loom` is not in W00's workspace dependency table, and
//! `crates/stratum-runtime/Cargo.toml` is W06a's file — W06b can add neither. So
//! the property is established two ways instead:
//!
//! 1. **Structurally**, by [`the_ordering_cannot_be_written_the_wrong_way`]: a
//!    `FinishToken` is obtainable only from `publish` and spendable only in
//!    `block_finished`, so announcing a finish before publishing its versions is
//!    not an ordering to remember, it is a program that does not compile.
//!    `loom` explores schedules of a program that *could* be wrong; this rules
//!    the wrong program out at the type level, which is strictly stronger for
//!    this particular hazard.
//! 2. **Empirically**, by [`ten_thousand_commit_reconcile_interleavings`], which
//!    runs the real `Release`/`Acquire` pair on real threads. `loom` would model
//!    the orderings; on the aarch64 hosts this project targets, a relaxed store
//!    in place of the `Release` is *architecturally* observable, so the real
//!    threads have teeth here rather than merely passing.
//!
//! Both are counters, never durations (ADR-017): rounds published, observations
//! taken, violations found.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use stratum_proto::{
    BlockId, CodeHash, DatasetStateId, DepKey, DocumentId, ExecStatus, ExecutionId, FrameId,
    LineRange, RegionKind, RegionSummary, SessionEpoch, StateId, Taint,
};
use stratum_runtime::doc::{reconcile, BlockIdAlloc};
use stratum_runtime::snapshot::{
    DocumentSnapshot, RecordSummary, SnapshotCells, StatusIndex, Version, VersionTable,
};
use stratum_runtime::state::fingerprint::{Ns, StateFingerprint};

/// The bullet's number.
const ROUNDS: u64 = 10_000;

const FRAME: FrameId = FrameId(0);
const EPOCH: SessionEpoch = SessionEpoch(1);
const DOC: DocumentId = DocumentId(1);

/// The macro whose version tracks the commit sequence, so a reader can check
/// that the table it observed really is the one that commit produced — not just
/// that a counter is large enough.
const TRACER: &str = "tracer";

fn tracer_key() -> DepKey {
    DepKey::Macro {
        name: format!("`{TRACER}"),
    }
}

fn record(seq: u64) -> RecordSummary {
    RecordSummary {
        exec: ExecutionId(seq),
        seq,
        epoch: EPOCH,
        code_hash: CodeHash([0; 16]),
        status: ExecStatus::Succeeded,
        dataset: DatasetStateId(seq),
        state: StateId(seq),
        taint: Taint::empty(),
        duration_us: 0,
        started_at_ms: 0,
        stale_on_arrival: false,
    }
}

// ---------------------------------------------------------------------------
// The structural half
// ---------------------------------------------------------------------------

#[test]
fn the_ordering_cannot_be_written_the_wrong_way() {
    // `publish` is the only source of a `FinishToken` and `block_finished` the
    // only sink. There is no API that announces a finish for a commit whose
    // versions are not already in the cells.
    let cells = Arc::new(SnapshotCells::new());
    let mut worker = cells.commit_publisher();
    let reader = cells.reader();
    let mut fp = StateFingerprint::fresh(EPOCH, FRAME);

    assert_eq!(reader.observe().finished_seq, 0);
    assert_eq!(reader.observe().versions.seq, 0);

    fp.bump_named(Ns::Local, TRACER);
    let token = worker.publish(&fp, worker.statuses().with(1, BlockId(1), record(1)));
    assert_eq!(token.seq(), 1);

    // Between publish and announce the cells are ALREADY ahead. This is the
    // whole safety argument: the window exists, and in it the reader is
    // over-informed rather than under-informed.
    let mid = reader.observe();
    assert_eq!(mid.versions.seq, 1);
    assert_eq!(mid.statuses.seq, 1);
    assert_eq!(mid.finished_seq, 0, "not announced yet");
    assert!(mid.versions.seq >= mid.finished_seq);

    worker.block_finished(token);
    let after = reader.observe();
    assert_eq!(after.finished_seq, 1);
    assert!(after.versions.seq >= after.finished_seq);
    assert_eq!(after.versions.version_of(&tracer_key()), Version::At(1));
    assert_eq!(after.statuses.latest(BlockId(1)).map(|r| r.seq), Some(1));
}

#[test]
fn both_cells_move_together_under_one_sequence() {
    // A `StatusIndex` that mentions an execution whose versions are missing
    // would let the sweep judge a finished block against a table that never saw
    // it. The two stores are one publication and carry one `seq`.
    let cells = Arc::new(SnapshotCells::new());
    let mut worker = cells.commit_publisher();
    let reader = cells.reader();
    let mut fp = StateFingerprint::fresh(EPOCH, FRAME);
    for i in 1..=32u64 {
        fp.bump_named(Ns::Local, TRACER);
        let index = worker
            .statuses()
            .with(worker.next_seq(), BlockId(i), record(i));
        let token = worker.publish(&fp, index);
        worker.block_finished(token);
        let o = reader.observe();
        assert_eq!(o.versions.seq, i);
        assert_eq!(o.statuses.seq, i);
        assert_eq!(o.versions.version_of(&tracer_key()), Version::At(i));
    }
}

// ---------------------------------------------------------------------------
// The empirical half: 10⁴ concurrent (commit, reconcile) interleavings
// ---------------------------------------------------------------------------

#[test]
fn ten_thousand_commit_reconcile_interleavings() {
    let cells = Arc::new(SnapshotCells::new());
    let done = Arc::new(AtomicBool::new(false));

    // Counters, per ADR-017. `observations` is the one that says the readers
    // were actually inside the window rather than sampling a quiet cell.
    let observations = Arc::new(AtomicU64::new(0));
    let violations = Arc::new(AtomicU64::new(0));
    let stale_tables = Arc::new(AtomicU64::new(0));
    let reconciles = Arc::new(AtomicU64::new(0));

    /// One look at the worker's side of the world, with the invariant checked.
    ///
    /// The two counters are separated on purpose. `violations` is the sequence
    /// form — a table published before the finish it is being judged against.
    /// `stale_tables` is the *concrete* form: the announced finish is at
    /// sequence *n*, so the tracer must already read at least *n*, or a
    /// downstream block is being compared against inputs that had already moved.
    fn check(
        o: &stratum_runtime::snapshot::Observation,
        violations: &AtomicU64,
        stale_tables: &AtomicU64,
    ) {
        if o.versions.seq < o.finished_seq {
            violations.fetch_add(1, Ordering::Relaxed);
        }
        assert_eq!(
            o.statuses.seq, o.versions.seq,
            "the two cells are one publication: an observation that mixes a \
             VersionTable from one commit with a StatusIndex from another lets \
             the sweep judge a record against versions it never saw"
        );
        if let Version::At(v) = o.versions.version_of(&tracer_key()) {
            if v < o.finished_seq {
                stale_tables.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    // The control thread: debounced reconcile, publishing document snapshots.
    // This is the second half of "(commit, reconcile) interleavings" — it is the
    // thread the sweep runs on, and it must never need the session worker. It
    // runs its own ROUNDS iterations rather than racing the worker to a flag, so
    // the counter this test asserts on is not a function of machine load
    // (ADR-017).
    let control = {
        let cells = Arc::clone(&cells);
        let observations = Arc::clone(&observations);
        let violations = Arc::clone(&violations);
        let stale_tables = Arc::clone(&stale_tables);
        let reconciles = Arc::clone(&reconciles);
        std::thread::spawn(move || {
            let publisher = cells.document_publisher();
            let reader = cells.reader();
            let mut alloc = BlockIdAlloc::new();
            let mut prev_hashes: Vec<CodeHash> = Vec::new();
            let mut prev_ids: Vec<BlockId> = Vec::new();
            for generation in 1..=ROUNDS {
                check(&reader.observe(), &violations, &stale_tables);
                observations.fetch_add(1, Ordering::Relaxed);

                // A debounced edit: three blocks, the middle one retyped.
                let next: Vec<CodeHash> = [1u8, (generation % 251) as u8 + 2, 3]
                    .iter()
                    .map(|n| CodeHash([*n; 16]))
                    .collect();
                let r = reconcile(&prev_hashes, &prev_ids, &next, &mut alloc);
                assert_eq!(r.blocks.len(), next.len());
                // A duplicated id would attach one block's history to another's
                // code — the failure reconcile exists to prevent, checked on
                // every one of the ROUNDS edits rather than once.
                assert!(r.blocks.iter().all(|b| b.is_real()));
                assert!(
                    r.blocks[0] != r.blocks[1] && r.blocks[1] != r.blocks[2],
                    "reconcile issued a duplicate id at generation {generation}"
                );
                publisher.publish(snapshot(generation, &next, &r.blocks));
                prev_hashes = next;
                prev_ids = r.blocks;
                reconciles.fetch_add(1, Ordering::Relaxed);
            }
        })
    };

    // A pure reader, spinning for the whole run. The control thread samples once
    // per reconcile; this one samples as fast as it can, which is what puts an
    // observation inside the publish/announce window rather than beside it.
    let spinner = {
        let cells = Arc::clone(&cells);
        let done = Arc::clone(&done);
        let observations = Arc::clone(&observations);
        let violations = Arc::clone(&violations);
        let stale_tables = Arc::clone(&stale_tables);
        std::thread::spawn(move || {
            let reader = cells.reader();
            let mut n = 0u64;
            while !done.load(Ordering::Relaxed) {
                check(&reader.observe(), &violations, &stale_tables);
                n += 1;
            }
            observations.fetch_add(n, Ordering::Relaxed);
            n
        })
    };

    // The session worker: commit, publish, announce.
    let worker = {
        let cells = Arc::clone(&cells);
        std::thread::spawn(move || {
            let mut publisher = cells.commit_publisher();
            let reader = cells.reader();
            let mut fp = StateFingerprint::fresh(EPOCH, FRAME);
            let mut docs_seen = 0u64;
            for i in 1..=ROUNDS {
                fp.bump_named(Ns::Local, TRACER);
                let index =
                    publisher
                        .statuses()
                        .with(publisher.next_seq(), BlockId(i % 7 + 1), record(i));
                let token = publisher.publish(&fp, index);
                // The worker reads the control thread's side while control reads
                // its own — the interleaving the bullet asks for.
                if reader.document(DOC).is_some() {
                    docs_seen += 1;
                }
                publisher.block_finished(token);
            }
            docs_seen
        })
    };

    let docs_seen = worker.join().expect("the session worker panicked");
    control.join().expect("the control thread panicked");
    done.store(true, Ordering::Relaxed);
    let spun = spinner.join().expect("the spinning reader panicked");

    let observations = observations.load(Ordering::Relaxed);
    let reconciles = reconciles.load(Ordering::Relaxed);
    assert_eq!(
        violations.load(Ordering::Relaxed),
        0,
        "a reader observed a VersionTable predating the BlockFinished it had seen"
    );
    assert_eq!(
        stale_tables.load(Ordering::Relaxed),
        0,
        "a reader observed an announced finish against a table that did not \
         carry the version that commit produced — INV-1's exact failure"
    );
    assert_eq!(reconciles, ROUNDS);
    assert!(
        observations >= ROUNDS,
        "only {observations} observations against {ROUNDS} commits"
    );
    println!(
        "{ROUNDS} commits, {ROUNDS} reconciles, {observations} observations \
         ({spun} from the spinning reader), {docs_seen} cross-thread document \
         reads, 0 violations"
    );
}

#[test]
fn many_readers_never_disagree_with_each_other() {
    // Every reader takes an owned `Arc` and is never blocked by another reader;
    // what must also hold is that no reader can be *behind* an announcement it
    // has already seen. Four of them, sharing the same cells.
    let cells = Arc::new(SnapshotCells::new());
    let done = Arc::new(AtomicBool::new(false));
    let violations = Arc::new(AtomicU64::new(0));
    let observations = Arc::new(AtomicU64::new(0));

    let readers: Vec<_> = (0..4)
        .map(|_| {
            let cells = Arc::clone(&cells);
            let done = Arc::clone(&done);
            let violations = Arc::clone(&violations);
            let observations = Arc::clone(&observations);
            std::thread::spawn(move || {
                let reader = cells.reader();
                let mut last_finished = 0u64;
                while !done.load(Ordering::Relaxed) {
                    let o = reader.observe();
                    observations.fetch_add(1, Ordering::Relaxed);
                    if o.versions.seq < o.finished_seq || o.finished_seq < last_finished {
                        violations.fetch_add(1, Ordering::Relaxed);
                    }
                    last_finished = o.finished_seq;
                }
            })
        })
        .collect();

    let worker = {
        let cells = Arc::clone(&cells);
        std::thread::spawn(move || {
            let mut publisher = cells.commit_publisher();
            let mut fp = StateFingerprint::fresh(EPOCH, FRAME);
            for i in 1..=2_000u64 {
                fp.bump_named(Ns::Local, TRACER);
                let index = publisher
                    .statuses()
                    .with(publisher.next_seq(), BlockId(1), record(i));
                let token = publisher.publish(&fp, index);
                publisher.block_finished(token);
            }
        })
    };

    worker.join().expect("the session worker panicked");
    done.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().expect("a reader panicked");
    }
    assert_eq!(violations.load(Ordering::Relaxed), 0);
    assert!(observations.load(Ordering::Relaxed) > 0);
}

// ---------------------------------------------------------------------------
// The document cell
// ---------------------------------------------------------------------------

fn region(index: u32, hash: CodeHash) -> RegionSummary {
    let start = index * 16;
    RegionSummary {
        index,
        span: stratum_proto::Span {
            start,
            end: start + 15,
        },
        outer_span: stratum_proto::Span {
            start,
            end: start + 15,
        },
        lines: LineRange {
            start: index,
            end: index,
        },
        code_lines: LineRange {
            start: index,
            end: index,
        },
        kind: RegionKind::Simple,
        entry_delimiter: stratum_proto::Delimiter::Cr,
        exit_delimiter: stratum_proto::Delimiter::Cr,
        code_hash: hash,
        hash_ordinal: 0,
        canonical: None,
        is_estimation: false,
        has_macro_in_head: false,
        section: None,
    }
}

fn snapshot(generation: u64, hashes: &[CodeHash], blocks: &[BlockId]) -> DocumentSnapshot {
    let regions: Vec<RegionSummary> = hashes
        .iter()
        .enumerate()
        .map(|(i, h)| region(i as u32, *h))
        .collect();
    DocumentSnapshot {
        doc: DOC,
        generation,
        doc_version: generation,
        text: Arc::from("gen a = 1\ngen b = 2\ngen c = 3\n"),
        regions: Arc::from(regions),
        blocks: Arc::from(blocks.to_vec()),
    }
}

#[test]
fn the_worker_runs_the_text_the_user_pressed_run_on() {
    // The document cell is published by control and read by the worker when it
    // dequeues, which is what stops a queued block running whatever the editor
    // happens to hold by the time the queue drains.
    let cells = Arc::new(SnapshotCells::new());
    let publisher = cells.document_publisher();
    let reader = cells.reader();
    assert!(reader.document(DOC).is_none());

    let hashes = [CodeHash([1; 16]), CodeHash([2; 16])];
    let blocks = [BlockId(1), BlockId(2)];
    publisher.publish(snapshot(1, &hashes, &blocks));
    let held = reader.document(DOC).expect("published");
    assert_eq!(held.generation, 1);
    assert_eq!(held.hashes(), hashes.to_vec());
    assert_eq!(held.region_of(BlockId(2)).map(|r| r.index), Some(1));

    // A later reconcile does not mutate the snapshot the worker is holding.
    publisher.publish(snapshot(2, &[CodeHash([9; 16])], &[BlockId(1)]));
    assert_eq!(held.generation, 1, "the held snapshot is immutable");
    assert_eq!(reader.document(DOC).expect("republished").generation, 2);

    publisher.close(DOC);
    assert!(reader.document(DOC).is_none());
    assert_eq!(held.generation, 1, "and closing does not disturb it either");
}

#[test]
fn documents_are_listed_in_a_stable_order() {
    // `--deterministic` (A8) forbids a hash-map iteration order reaching output,
    // and the sweep walks every open document.
    let cells = Arc::new(SnapshotCells::new());
    let publisher = cells.document_publisher();
    let reader = cells.reader();
    for id in [7u32, 2, 5, 1] {
        let mut s = snapshot(1, &[CodeHash([1; 16])], &[BlockId(1)]);
        s.doc = DocumentId(id);
        publisher.publish(s);
    }
    let ids: Vec<u32> = reader.documents().iter().map(|d| d.doc.0).collect();
    assert_eq!(ids, vec![1, 2, 5, 7]);
}

// ---------------------------------------------------------------------------
// The status index
// ---------------------------------------------------------------------------

#[test]
fn ephemeral_and_trivia_ids_never_enter_the_status_index() {
    // A3: `latest_by_block[0]` was ambiguous between "the last command-bar run"
    // and "every comment region in the document", so a command-bar
    // `StatusChanged` repainted every comment.
    let index = StatusIndex::default();
    let index = index.with(1, BlockId::EPHEMERAL, record(1));
    let index = index.with(2, BlockId::NONE, record(2));
    assert!(index.is_empty(), "neither id carries block identity");
    let index = index.with(3, BlockId(4), record(3));
    assert_eq!(index.len(), 1);
    assert_eq!(
        index.latest(BlockId(4)).map(|r| r.exec),
        Some(ExecutionId(3))
    );
    assert_eq!(index.seq, 3);
}

#[test]
fn retiring_a_block_removes_it_from_the_index_and_nothing_else() {
    let index =
        StatusIndex::default()
            .with(1, BlockId(1), record(1))
            .with(2, BlockId(2), record(2));
    let after = index.without(3, &[BlockId(2)]);
    assert_eq!(after.seq, 3);
    assert!(after.latest(BlockId(2)).is_none(), "its widget goes");
    assert!(after.latest(BlockId(1)).is_some());
    // The predecessor is untouched: the ledger is append-only and the index is
    // a projection of it, not the record itself.
    assert!(index.latest(BlockId(2)).is_some());
}

// ---------------------------------------------------------------------------
// The version table's answers
// ---------------------------------------------------------------------------

#[test]
fn an_absent_name_reads_as_version_zero_and_an_absent_variable_is_unresolved() {
    // A block that expanded an undefined local — which Stata expands to nothing,
    // not an error — depends on it STAYING undefined, so defining it later must
    // restale the block. A missing variable is different: re-running would
    // ERROR, which is `Broken`, not `Stale`.
    let mut fp = StateFingerprint::fresh(EPOCH, FRAME);
    let empty = VersionTable::from_fingerprint(1, &fp, &|_| Some("default".into()));
    let key = DepKey::Macro {
        name: "`later".into(),
    };
    assert_eq!(empty.version_of(&key), Version::At(0));
    assert_eq!(
        empty.version_of(&DepKey::Var {
            frame: "default".into(),
            name: "mpg".into()
        }),
        Version::Unresolved
    );

    fp.bump_named(Ns::Local, "later");
    let after = VersionTable::from_fingerprint(2, &fp, &|_| Some("default".into()));
    assert_eq!(
        after.version_of(&key),
        Version::At(1),
        "and the block restales"
    );
}
