//! What chunking costs a scan — the control the `summarize` budget needs.
//!
//! `04` §12.1 budgets `summarize` on one `double` column of 10 M rows at 6 ms,
//! and W02's acceptance adds "chunked iteration must not regress this —
//! measured against a flat-slice control". ADR-017 makes that a *recorded*
//! number rather than a gate, so this bench measures three things side by side
//! and the counters in `tests/sample.rs` are what CI actually asserts:
//!
//! `flat_seq` is a plain `&[f64]` fold and `chunked_seq` is the same fold through
//! `Column::for_each_chunk_f64`: that pair is the literal "chunked iteration
//! must not regress a flat slice" control, both sequential.
//!
//! `flat_par` (`stratum_core::reduce::sum_f64`) and `chunked_par`
//! (`Column::map_reduce_f64`) are the pair that matters for the 6 ms
//! `summarize` budget, because 80 MB in 6 ms is 13 GB/s and no single core on
//! this machine reaches it. Both fold sequentially in ascending chunk index, so
//! they return the same bits as their sequential twins (ADR-013).
//!
//! `chunked_int_par` pays the widening pass that native storage buys residency
//! with.
//!
//! Run with `cargo bench -p stratum-data --bench widen`.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use stratum_data::column::NumCol;
use stratum_data::Column;

/// A deterministic spread with no transcendental in it (ARCHITECTURE §8.11).
fn spread(n: usize, seed: u64) -> Vec<f64> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ((s >> 33) as f64) / 4_294_967_296.0 - 0.5
        })
        .collect()
}

fn fold_flat(xs: &[f64]) -> f64 {
    // Chunked exactly like `map_reduce_blocks` so the comparison is fold-for-fold
    // and not "one association order versus another".
    stratum_core::reduce::sum_f64(xs)
}

/// The same association order, single-threaded, so the chunking comparison is
/// fold-for-fold and not "parallel versus not".
fn fold_flat_seq(xs: &[f64]) -> f64 {
    let mut acc = 0.0f64;
    for i in 0..stratum_core::reduce::n_chunks(xs.len()) {
        let (s, e) = stratum_core::reduce::chunk_range(i, xs.len());
        let mut part = 0.0;
        for &x in &xs[s..e] {
            part += x;
        }
        acc += part;
    }
    acc
}

/// The parallel, deterministic reduction every column kernel is built on.
fn fold_map_reduce(col: &Column) -> f64 {
    col.map_reduce_f64(
        0.0f64,
        |_, xs| {
            let mut part = 0.0;
            for &x in xs {
                part += x;
            }
            part
        },
        |acc, p| *acc += *p,
    )
}

fn fold_chunked(col: &Column, scratch: &mut Vec<f64>) -> f64 {
    let mut acc = 0.0f64;
    col.for_each_chunk_f64(scratch, |_, xs| {
        let mut part = 0.0;
        for &x in xs {
            part += x;
        }
        acc += part;
    });
    acc
}

fn bench_widen(c: &mut Criterion) {
    let n = 10_000_000usize;
    let flat = spread(n, 7);
    let dbl = Column::Double(NumCol::from_slice(&flat));
    let ints: Vec<i16> = (0..n).map(|i| (i % 30_000) as i16 - 15_000).collect();
    let int = Column::Int(NumCol::from_slice(&ints));

    let mut group = c.benchmark_group("scan_10m");
    group.sample_size(10);
    group.throughput(Throughput::Elements(n as u64));

    group.bench_function("flat_seq", |b| b.iter(|| fold_flat_seq(&flat)));

    let mut scratch = Vec::new();
    group.bench_function("chunked_seq", |b| {
        b.iter(|| fold_chunked(&dbl, &mut scratch));
    });

    group.bench_function("flat_par", |b| b.iter(|| fold_flat(&flat)));

    group.bench_function("chunked_par", |b| b.iter(|| fold_map_reduce(&dbl)));

    group.bench_function("chunked_int_par", |b| b.iter(|| fold_map_reduce(&int)));

    group.finish();
}

criterion_group!(benches, bench_widen);
criterion_main!(benches);
