//! The sort budget, recorded.
//!
//! `04` §12.1 budgets "radix sort, 1 numeric key, 10 M rows" at 350 ms. Per
//! ADR-017 that duration is **recorded here, not asserted anywhere**: the gate
//! is the counter in `tests/sort.rs` — passes over the key, and `n` rows
//! scattered per pass — which is machine-independent and cannot pass on an idle
//! laptop and fail on a busy one.
//!
//! What is worth watching in the numbers:
//!
//! * `radix_10m_double` against `comparator_10m_double` is the reason the hybrid
//!   exists at all.
//! * `radix_10m_byte` shows the pass-skipping working: a one-byte key is one
//!   pass, so this is close to the floor for "reorder 10 M `u32`s".
//! * `two_keys` is the composite-key path, which is one sort over a wider key
//!   and not two sorts.
//!
//! Run with `cargo bench -p stratum-data --bench sort`.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use stratum_data::column::NumCol;
use stratum_data::sort::{permutation, Strategy};
use stratum_data::Column;
use stratum_proto::SortDir;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn bench_sort(c: &mut Criterion) {
    let n = 10_000_000u64;
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);

    let doubles: Vec<f64> = (0..n).map(|_| (rng.next() >> 11) as f64).collect();
    let dbl = Column::Double(NumCol::from_slice(&doubles));
    let bytes: Vec<i8> = (0..n).map(|_| (rng.next() % 100) as i8).collect();
    let byte = Column::Byte(NumCol::from_slice(&bytes));
    let ints: Vec<i16> = (0..n).map(|_| (rng.next() % 30_000) as i16).collect();
    let int = Column::Int(NumCol::from_slice(&ints));

    let mut group = c.benchmark_group("sort_10m");
    group.sample_size(10);
    group.throughput(Throughput::Elements(n));

    group.bench_function("radix_10m_double", |b| {
        b.iter(|| permutation(&[(&dbl, SortDir::Asc)], n, Strategy::Radix));
    });
    group.bench_function("radix_10m_byte", |b| {
        b.iter(|| permutation(&[(&byte, SortDir::Asc)], n, Strategy::Radix));
    });
    group.bench_function("radix_10m_two_keys", |b| {
        b.iter(|| {
            permutation(
                &[(&byte, SortDir::Asc), (&int, SortDir::Asc)],
                n,
                Strategy::Radix,
            )
        });
    });
    group.finish();

    // The comparator on the same data, at a tenth the size: at 10 M it is slow
    // enough to make `cargo bench` unpleasant, and the shape is already clear.
    let m = 1_000_000u64;
    let small = Column::Double(NumCol::from_slice(&doubles[..m as usize]));
    let mut group = c.benchmark_group("sort_1m_paths");
    group.sample_size(10);
    group.throughput(Throughput::Elements(m));
    group.bench_function("radix", |b| {
        b.iter(|| permutation(&[(&small, SortDir::Asc)], m, Strategy::Radix));
    });
    group.bench_function("comparator", |b| {
        b.iter(|| permutation(&[(&small, SortDir::Asc)], m, Strategy::Comparator));
    });
    group.finish();

    // Applying the permutation: one gather pass per column, which is what makes
    // `sort` on a wide frame linear in cells rather than in cells x log n.
    let mut group = c.benchmark_group("apply_permutation_1m");
    group.sample_size(10);
    group.throughput(Throughput::Elements(m));
    let perm = permutation(&[(&small, SortDir::Asc)], m, Strategy::Radix).expect("double key");
    group.bench_function("permute_double_column", |b| {
        b.iter_batched(
            || (),
            |()| stratum_data::sort::permute_column(&small, &perm),
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_sort);
criterion_main!(benches);
