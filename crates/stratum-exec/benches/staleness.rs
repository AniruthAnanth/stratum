//! Recorded durations for the sweep — ADR-017, `03` §6.5.
//!
//! **Nothing here is a gate.** ADR-017 asserts performance with COUNTERS, which
//! live in `DepIndex::stats()` and in the tests that read them; a wall-clock
//! number is machine-dependent, and a red build caused by a busy CI runner
//! teaches everyone to ignore red builds. What a duration is good for is a
//! moving line: if the incremental path starts tracking document size, that
//! shows up here before a user notices the gutter lagging their typing.
//!
//! Two shapes, because they answer different questions:
//!
//! * `full/2000` — the O(n²) reference sweep over a document where every block
//!   has run and is `Current`. Settled blocks leave C7's `pending` list empty,
//!   so this is the realistic open-a-file cost rather than the pathological one.
//! * `incremental/2000` — one block re-classified after a commit. This is the
//!   number the 16 ms editing budget was ever really about, and it must not grow
//!   with the 2 000.
//!
//! Verification is turned OFF for the incremental bench (`set_verify(false)`).
//! `--verify-staleness` runs the full sweep beside the incremental one by
//! design; leaving it on here would measure the verifier, not the engine, and a
//! shipped release engine does not carry it.

use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion};
use stratum_proto::{
    BlockId, DatasetStateId, DocumentId, ExecOrigin, ExecStatus, ExecutionId, ExecutionRecord,
    RunId, SessionEpoch, SessionId, StateId, Taint,
};

use stratum_exec::testkit::{code_hash, doc};
use stratum_exec::{
    sweep, AnalysedDoc, Committed, DepIndex, ExecutionLedger, RecordedReads, RecordedWrites,
    RunState, SweepInput, Versions,
};

/// Blocks in the benchmark document. `03` §6.5's stated target size.
const BLOCKS: usize = 2_000;

/// A document of `n` blocks, and a ledger in which every one of them has run in
/// its current form and succeeded — so the sweep answers `Current` for all of
/// them and C7 has an empty pending list, which is what an open file looks like
/// a moment after "Run all".
fn corpus(n: usize) -> (AnalysedDoc, ExecutionLedger) {
    let hashes: Vec<_> = (0..n as u64).map(code_hash).collect();
    let document = doc(DocumentId(1), 1, &hashes);
    let mut ledger = ExecutionLedger::new();
    for (i, hash) in hashes.iter().enumerate() {
        let exec = ExecutionId(i as u64 + 1);
        ledger.append(Committed {
            record: ExecutionRecord {
                exec,
                seq: 0,
                session: SessionId(1),
                epoch: SessionEpoch(0),
                run: RunId(1),
                block: BlockId(i as u64 + 1),
                doc: Some(DocumentId(1)),
                origin: ExecOrigin::Editor,
                code_hash: *hash,
                source: String::new(),
                input_state: StateId(0),
                output_state: StateId(0),
                input_dataset: DatasetStateId(0),
                output_dataset: DatasetStateId(0),
                result: None,
                status: ExecStatus::Succeeded,
                started_at_ms: 0,
                duration_us: 0,
                stale_on_arrival: false,
                taint: Taint::empty(),
            },
            reads: Arc::new(RecordedReads::default()),
            writes: Arc::new(RecordedWrites::default()),
        });
    }
    (document, ledger)
}

fn sweeps(c: &mut Criterion) {
    let (document, ledger) = corpus(BLOCKS);
    let versions = Versions::new("default");
    let run = RunState::default();
    let input = SweepInput {
        doc: &document,
        versions: &versions,
        ledger: &ledger,
        epoch: SessionEpoch(0),
        run: &run,
    };

    c.bench_function("full/2000", |b| b.iter(|| sweep(&input)));

    let mut index = DepIndex::new();
    index.set_verify(false);
    let previous = index.full(&input);
    let candidates = [BLOCKS as u32 - 1];
    c.bench_function("incremental/2000", |b| {
        b.iter(|| index.incremental(&input, &previous, &candidates));
    });
}

criterion_group!(benches, sweeps);
criterion_main!(benches);
