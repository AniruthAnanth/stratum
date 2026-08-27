//! The row-major → column-major transpose, which is the whole cost of `use`.
//!
//! `04` §12.1: "`.dta` is row-major; we are column-major. That transpose is the
//! dominant cost of loading." [`Column::from_row_major`] is the primitive the
//! reader (W03) drives, and every destination cell is written exactly once —
//! the floor for a layout change. `counters().ingest_cells` records that as a
//! number; this bench records what it costs.
//!
//! The shapes are the ones that actually occur:
//!
//! `auto_shaped` is the 43-byte row of `auto.dta` (`04` §0.1: `str18`, eight
//! `int`, two `float`, one `byte`) scaled to 10 M observations — narrow fields
//! at a wide stride, which is the cache-hostile case. `wide_double` is 20
//! `double` columns, the regression-dataset shape. `one_int_column` isolates the
//! per-column cost so a regression can be attributed.
//!
//! Run with `cargo bench -p stratum-data --bench transpose`.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use stratum_data::{Column, StorageType};

/// A row-major buffer of `nobs` rows of `row_width` bytes, deterministically
/// filled (ARCHITECTURE §8.11: no transcendental anywhere in a fixture).
fn row_major(nobs: usize, row_width: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    let mut v = vec![0u8; nobs * row_width];
    for b in &mut v {
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        *b = (s >> 56) as u8;
    }
    v
}

fn bench_transpose(c: &mut Criterion) {
    let nobs = 10_000_000u64;

    // auto.dta's row: str18, int x8, float x2, byte = 43 bytes. Exact.
    let auto_row = 18 + 8 * 2 + 2 * 4 + 1;
    let auto_fields: &[(StorageType, usize)] = &[
        (StorageType::Str { width: 18 }, 0),
        (StorageType::Int, 18),
        (StorageType::Int, 20),
        (StorageType::Int, 22),
        (StorageType::Int, 24),
        (StorageType::Int, 26),
        (StorageType::Int, 28),
        (StorageType::Int, 30),
        (StorageType::Int, 32),
        (StorageType::Float, 34),
        (StorageType::Float, 38),
        (StorageType::Byte, 42),
    ];

    let src = row_major(nobs as usize, auto_row, 11);
    let mut group = c.benchmark_group("transpose_10m");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(src.len() as u64));
    group.bench_function("auto_shaped_43_byte_row", |b| {
        b.iter(|| {
            let cols: Vec<Column> = auto_fields
                .iter()
                .map(|&(ty, off)| Column::from_row_major(ty, &src, auto_row, off, nobs))
                .collect();
            cols
        });
    });
    group.bench_function("one_int_column_of_the_same_file", |b| {
        b.iter(|| Column::from_row_major(StorageType::Int, &src, auto_row, 18, nobs));
    });
    group.finish();
    drop(src);

    // The regression shape: 20 doubles, 1.6 GB of source. Built at a tenth the
    // observations so `cargo bench` does not need 3 GB resident to report a
    // per-byte number.
    let wide_obs = 1_000_000u64;
    let wide_row = 20 * 8;
    let wide = row_major(wide_obs as usize, wide_row, 13);
    let mut group = c.benchmark_group("transpose_wide_double");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(wide.len() as u64));
    group.bench_function("twenty_doubles_1m", |b| {
        b.iter(|| {
            let cols: Vec<Column> = (0..20)
                .map(|j| {
                    Column::from_row_major(StorageType::Double, &wide, wide_row, j * 8, wide_obs)
                })
                .collect();
            cols
        });
    });
    group.finish();
}

criterion_group!(benches, bench_transpose);
criterion_main!(benches);
