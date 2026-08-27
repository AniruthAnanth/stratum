//! What the undo journal costs, recorded.
//!
//! The A18 gates are counters and live in `tests/journal.rs`. What a duration
//! can still tell you is the thing the audit was actually about: **the ratio
//! between a one-observation `replace` and a whole-column one.**
//!
//! Under the pre-audit flat-`Arc` design those two were the same price, because
//! retaining the previous `Arc` forced `Arc::make_mut` to deep-copy 80 MB on
//! every write. Here the first should be a fixed ~512 KiB of work whatever the
//! column's length, and the second should be one extra pass. If a future change
//! ever makes `replace_in_1` scale with `n`, this bench is where it shows.
//!
//! `no_journal` is the same write outside `begin_command`: the difference
//! between it and `replace_in_1` is exactly what rollbackability costs.
//!
//! Run with `cargo bench -p stratum-data --bench journal`.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use stratum_data::{Frame, StorageType};
use stratum_proto::VarIdx;

fn frame_of(n: u64) -> Frame {
    let mut f = Frame::new("default");
    f.set_n_obs(n);
    f.add_var("x", StorageType::Double).expect("fresh name");
    f
}

fn bench_journal(c: &mut Criterion) {
    let mut group = c.benchmark_group("journal");
    group.sample_size(10);

    for n in [1_000_000u64, 10_000_000] {
        group.bench_function(format!("replace_in_1_n{n}"), |b| {
            b.iter_batched_ref(
                || frame_of(n),
                |f| {
                    f.begin_command();
                    f.col_mut(VarIdx(0))
                        .expect("exists")
                        .set_f64(0, 1.0)
                        .expect("double");
                    f.commit();
                },
                BatchSize::LargeInput,
            );
        });

        group.bench_function(format!("replace_in_1_no_journal_n{n}"), |b| {
            b.iter_batched_ref(
                || frame_of(n),
                |f| {
                    f.col_mut(VarIdx(0))
                        .expect("exists")
                        .set_f64(0, 1.0)
                        .expect("double");
                },
                BatchSize::LargeInput,
            );
        });
    }

    let n = 10_000_000u64;
    group.throughput(Throughput::Elements(n));
    group.bench_function("replace_whole_column_10m", |b| {
        b.iter_batched_ref(
            || frame_of(n),
            |f| {
                f.begin_command();
                let mut c = f.col_mut(VarIdx(0)).expect("exists");
                for ch in 0..c.n_chunks() {
                    c.with_double_chunk(ch, |_, xs| {
                        for v in xs.iter_mut() {
                            *v += 1.0;
                        }
                    });
                }
            },
            BatchSize::LargeInput,
        );
    });

    // Rollback is proportional to work done, which is the property INV-2 needs
    // and the flat-Arc design did not have: this restores 153 chunks.
    group.bench_function("rollback_whole_column_10m", |b| {
        b.iter_batched_ref(
            || {
                let mut f = frame_of(n);
                f.begin_command();
                {
                    let mut c = f.col_mut(VarIdx(0)).expect("exists");
                    for ch in 0..c.n_chunks() {
                        c.with_double_chunk(ch, |_, xs| {
                            for v in xs.iter_mut() {
                                *v += 1.0;
                            }
                        });
                    }
                }
                f
            },
            Frame::rollback,
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_journal);
criterion_main!(benches);
