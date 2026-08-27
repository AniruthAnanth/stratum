//! The engine's ONLY parallel primitive (`05` §3, ADR-013).
//!
//! Floating-point addition is not associative, so `par_iter().sum()` returns a
//! different number depending on how many cores the machine has and how the
//! work stealer happened to split it. That is not a rounding curiosity: it is
//! the difference between a published standard error reproducing and not.
//!
//! [`map_reduce_blocks`] splits `n` rows into fixed [`CHUNK_ROWS`]-row chunks,
//! maps each chunk in parallel, and folds the partial results **sequentially,
//! in ascending chunk index**. The answer therefore depends on `n` and on
//! nothing else — not on thread count, not on scheduling, not on whether rayon
//! is compiled in at all.
//!
//! **[`CHUNK_ROWS`] is part of the wire format.** Changing it changes results
//! in the last few ulps, which invalidates every committed golden, so
//! `chunk_size_is_frozen` asserts it.

/// Rows per chunk. The SAME constant `stratum-data` chunks columns by, so a
/// chunk boundary in the reduction is a chunk boundary in memory.
pub const CHUNK_ROWS: usize = 65_536;

/// Number of chunks `n` rows split into.
#[inline]
#[must_use]
pub fn n_chunks(n: usize) -> usize {
    n.div_ceil(CHUNK_ROWS)
}

/// The half-open row range of chunk `i`.
#[inline]
#[must_use]
pub fn chunk_range(i: usize, n: usize) -> (usize, usize) {
    let start = i * CHUNK_ROWS;
    (start, (start + CHUNK_ROWS).min(n))
}

/// Map `n` rows chunk-wise in parallel, then fold in ascending chunk order.
///
/// * `map(start, end)` computes one chunk's partial result. It must be a pure
///   function of the row range, because it may run on any thread in any order.
/// * `fold(acc, partial)` accumulates `partial` into `acc`. It runs
///   **sequentially**, chunk 0 first, so its association order is fixed.
///
/// The single-threaded path is not an approximation of the parallel one: both
/// perform exactly the same sequence of folds.
pub fn map_reduce_blocks<T, M, F>(n: usize, init: T, map: M, fold: F) -> T
where
    T: Send + Clone,
    M: Fn(usize, usize) -> T + Sync,
    F: Fn(&mut T, &T),
{
    let chunks = n_chunks(n);
    if chunks == 0 {
        return init;
    }

    // Below a couple of chunks the rayon dispatch costs more than the work, and
    // the result is identical either way, so this is a pure performance branch.
    let partials: Vec<T> = if chunks == 1 {
        let (s, e) = chunk_range(0, n);
        vec![map(s, e)]
    } else {
        #[cfg(feature = "parallel")]
        {
            use rayon::prelude::*;
            (0..chunks)
                .into_par_iter()
                .map(|i| {
                    let (s, e) = chunk_range(i, n);
                    map(s, e)
                })
                .collect()
        }
        #[cfg(not(feature = "parallel"))]
        {
            (0..chunks)
                .map(|i| {
                    let (s, e) = chunk_range(i, n);
                    map(s, e)
                })
                .collect()
        }
    };

    let mut acc = init;
    for p in &partials {
        fold(&mut acc, p);
    }
    acc
}

/// Deterministic sum of a slice, chunked exactly like every other reduction.
///
/// Provided so that a caller who "just wants a sum" cannot accidentally reach
/// for `iter().sum()` and get a different association order from the one the
/// goldens were captured under.
#[must_use]
pub fn sum_f64(xs: &[f64]) -> f64 {
    map_reduce_blocks(
        xs.len(),
        0.0f64,
        |s, e| {
            let mut acc = 0.0;
            for &x in &xs[s..e] {
                acc += x;
            }
            acc
        },
        |acc, p| *acc += *p,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_size_is_frozen() {
        // Part of the wire format (05 §3). Changing this invalidates every
        // committed golden in stratum-stats.
        assert_eq!(CHUNK_ROWS, 65_536);
    }

    #[test]
    fn ranges_tile_exactly() {
        for n in [0usize, 1, 65_535, 65_536, 65_537, 200_000] {
            let mut covered = 0usize;
            for i in 0..n_chunks(n) {
                let (s, e) = chunk_range(i, n);
                assert_eq!(s, covered);
                assert!(e > s);
                covered = e;
            }
            assert_eq!(covered, n);
        }
    }

    /// The acceptance bullet in the letter: `RAYON_NUM_THREADS in {1, 2, 8}`
    /// give the SAME BITS. The env var cannot be set per-test (rayon's global
    /// pool initialises once per process), so the pools are built explicitly,
    /// which tests the same property with no ordering hazard between tests.
    #[cfg(feature = "parallel")]
    #[test]
    fn thread_count_does_not_change_a_single_bit() {
        let n = crate::reduce::CHUNK_ROWS * 5 + 777;
        let xs: Vec<f64> = (0..n)
            .map(|i| if i % 3 == 0 { 1.0 } else { 1e-17 })
            .collect();

        let run = || {
            map_reduce_blocks(
                xs.len(),
                0.0f64,
                |s, e| {
                    let mut acc = 0.0;
                    for &x in &xs[s..e] {
                        acc += x;
                    }
                    acc
                },
                |acc, p| *acc += *p,
            )
        };

        let mut seen: Option<u64> = None;
        for threads in [1usize, 2, 8] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .expect("thread pool");
            let bits = pool.install(run).to_bits();
            match seen {
                None => seen = Some(bits),
                Some(b) => assert_eq!(bits, b, "{threads} threads changed the answer"),
            }
        }
    }

    #[test]
    fn result_is_bitwise_independent_of_thread_count() {
        // The values are chosen so that a different association order really
        // does change the answer: 1.0 plus many 1e-17s.
        let mut xs = vec![1.0f64];
        xs.extend(std::iter::repeat_n(1e-17f64, 300_000));
        let reference = sum_f64(&xs);

        // Same input, forced through a single chunk-mapping thread: identical.
        let sequential = {
            let chunks = n_chunks(xs.len());
            let mut acc = 0.0f64;
            for i in 0..chunks {
                let (s, e) = chunk_range(i, xs.len());
                let mut p = 0.0;
                for &x in &xs[s..e] {
                    p += x;
                }
                acc += p;
            }
            acc
        };
        assert_eq!(reference.to_bits(), sequential.to_bits());

        // And it differs from the naive flat sum, which is the whole point.
        let flat = xs.iter().fold(0.0f64, |a, b| a + b);
        assert_ne!(reference.to_bits(), flat.to_bits());
    }
}
