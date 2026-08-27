//! Q17: what the hand-written Gram kernel costs versus a tuned one (ADR-004).
//!
//! ADR-004 gave up perhaps 2–4× on the k-inner loop by refusing faer, on the
//! grounds that the kernel is memory-bandwidth-bound in `n` and `k` is small by
//! construction. That is a claim, and this bench is the measurement obligation
//! attached to it. The headline case is `n = 10^7, k = 20`.
//!
//! Measured on an Apple M-series (aarch64-apple-darwin, `--release`,
//! `target-cpu=apple-m1`), 2026-08:
//!
//! ```text
//! gram/xtx_n1000000_k5     1.17 ms
//! gram/xtx_n10000000_k20   157.8 ms      (231 dot products of 10^7 terms)
//! gram/dot_1e7             3.82 ms       (2.6 G elements/s in one kernel)
//! ```
//!
//! `regress y x1..x20` on ten million rows is that 157.8 ms plus a `21^3`
//! sweep, which is a few microseconds: the accumulation is essentially the
//! whole numeric cost, and the solve does not appear in the profile at all.
//!
//! Run with `cargo bench -p stratum-core --bench gram`.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use stratum_core::gram::{dot, gram};

/// A deterministic spread with no transcendental in it (ARCHITECTURE §8.11).
fn column(n: usize, seed: u64) -> Vec<f64> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ((s >> 33) as f64) / 4_294_967_296.0 - 0.5
        })
        .collect()
}

fn bench_gram(c: &mut Criterion) {
    let mut group = c.benchmark_group("gram");
    group.sample_size(10);

    for &(n, k) in &[(1_000_000usize, 5usize), (10_000_000, 20)] {
        let cols: Vec<Vec<f64>> = (0..k).map(|j| column(n, j as u64 + 7)).collect();
        let y = column(n, 999);
        group.throughput(criterion::Throughput::Elements((n * k * k / 2) as u64));
        group.bench_function(format!("xtx_n{n}_k{k}"), |b| {
            b.iter_batched(
                || cols.iter().map(Vec::as_slice).collect::<Vec<&[f64]>>(),
                |refs| gram(&refs, &y),
                BatchSize::SmallInput,
            );
        });
    }

    // The inner loop on its own, so a regression can be attributed to the
    // kernel rather than to the blocking around it.
    let u = column(10_000_000, 1);
    let v = column(10_000_000, 2);
    group.throughput(criterion::Throughput::Elements(10_000_000));
    group.bench_function("dot_1e7", |b| b.iter(|| dot(&u, &v)));
    group.finish();
}

criterion_group!(benches, bench_gram);
criterion_main!(benches);
