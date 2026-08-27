//! `X'X` and `X'y` — hand-written, scalar, no BLAS (ADR-004 / A19).
//!
//! `05` §3 originally asserted that faer's kernels are "deterministic and
//! identical on every target that has [FMA]". They are not: faer dispatches
//! microkernels at run time through `pulp`, and the register-blocking width
//! changes the summation tree *inside* a chunk, so two x86 machines running the
//! same binary produce different `e(V)`. The committed determinism hash is a
//! declared release blocker, so the kernel is ours.
//!
//! Three properties make it bitwise reproducible on every target:
//!
//! 1. Rust never contracts `a * b + c` into an FMA — there is no default
//!    fp-contract and we never set fast-math — so the only route to one is an
//!    explicit `mul_add`, which `scripts/check-topology.sh` greps for.
//! 2. LLVM will not vectorise an `f64` reduction without `reassoc`/`fast`,
//!    which we never set. **Source order is execution order.**
//! 3. Chunking is [`crate::reduce::CHUNK_ROWS`] and the fold is by ascending
//!    chunk index, so thread count does not enter the answer.
//!
//! The unroll is pinned at 4 and written out explicitly. It is not a hint: an
//! unroll factor the optimiser chooses can change between LLVM versions, and
//! with it the association order.

use crate::reduce::map_reduce_blocks;

/// The augmented cross-product matrix of a regression (`05` §6.1).
///
/// Layout is the full `(k+1) x (k+1)` symmetric matrix in row-major order, with
/// the dependent variable in row/column `k`:
///
/// ```text
/// A = [ X'X   X'y ]
///     [ y'X   y'y ]
/// ```
///
/// Accumulated **raw and uncentered**, because F7's collinearity tolerance uses
/// the raw diagonal as its denominator.
#[derive(Clone, Debug, PartialEq)]
pub struct Gram {
    /// `(k+1)*(k+1)` entries, row-major, symmetric.
    pub a: Vec<f64>,
    /// Number of design columns (the constant, if present, is one of them).
    pub k: usize,
    /// Rows accumulated — the casewise-complete sample size.
    pub n: usize,
}

impl Gram {
    /// `A[i][j]`.
    #[inline]
    #[must_use]
    pub fn get(&self, i: usize, j: usize) -> f64 {
        self.a[i * (self.k + 1) + j]
    }

    /// The pre-sweep diagonal, which is F7's tolerance denominator.
    #[must_use]
    pub fn diagonal(&self) -> Vec<f64> {
        (0..self.k).map(|j| self.get(j, j)).collect()
    }
}

/// Accumulate `A = [X'X X'y; y'X y'y]` from a column-major design.
///
/// `x[j]` is design column `j`, all of length `n`; `y` is the dependent
/// variable, also length `n`. Every column must already be widened to `f64` and
/// casewise-complete — dropping incomplete rows is the caller's job, because
/// only the caller knows the sample.
///
/// Cost is `O(n * k^2 / 2)`; the lower triangle is accumulated and the matrix
/// is mirrored once at the end.
///
/// # Panics
///
/// If any column length differs from `y.len()`.
#[must_use]
pub fn gram(x: &[&[f64]], y: &[f64]) -> Gram {
    let k = x.len();
    let n = y.len();
    for (j, col) in x.iter().enumerate() {
        assert_eq!(col.len(), n, "design column {j} has the wrong length");
    }
    let dim = k + 1;

    let a = map_reduce_blocks(
        n,
        vec![0.0f64; dim * dim],
        |s, e| {
            let mut m = vec![0.0f64; dim * dim];
            for i in 0..dim {
                let ci: &[f64] = if i < k { x[i] } else { y };
                for j in 0..=i {
                    let cj: &[f64] = if j < k { x[j] } else { y };
                    m[i * dim + j] = dot(&ci[s..e], &cj[s..e]);
                }
            }
            m
        },
        |acc, p| {
            for (a, b) in acc.iter_mut().zip(p.iter()) {
                *a += *b;
            }
        },
    );

    let mut a = a;
    for i in 0..dim {
        for j in (i + 1)..dim {
            a[i * dim + j] = a[j * dim + i];
        }
    }
    Gram { a, k, n }
}

/// `sum(u[i] * v[i])` with a pinned unroll of 4 and four independent
/// accumulators.
///
/// Four accumulators rather than one because a single accumulator serialises on
/// the FP add latency (~4 cycles on both aarch64 and x86-64) and costs roughly
/// 3× the throughput. Four is a deliberate constant: it is written into the
/// source, so the association order it implies is part of the golden values and
/// cannot drift with the optimiser.
#[inline]
#[must_use]
pub fn dot(u: &[f64], v: &[f64]) -> f64 {
    debug_assert_eq!(u.len(), v.len());
    let n = u.len();
    let tail = n % 4;
    let body = n - tail;

    let mut a0 = 0.0f64;
    let mut a1 = 0.0f64;
    let mut a2 = 0.0f64;
    let mut a3 = 0.0f64;

    let mut i = 0;
    while i < body {
        a0 += u[i] * v[i];
        a1 += u[i + 1] * v[i + 1];
        a2 += u[i + 2] * v[i + 2];
        a3 += u[i + 3] * v[i + 3];
        i += 4;
    }
    // The tail joins a0 so that a length-1 input and a length-5 input agree on
    // where the extra term lands.
    while i < n {
        a0 += u[i] * v[i];
        i += 1;
    }
    ((a0 + a1) + a2) + a3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_is_the_written_association_order() {
        let u: Vec<f64> = (0..9).map(|i| f64::from(i) + 0.5).collect();
        let v: Vec<f64> = (0..9).map(|i| 1.0 / (f64::from(i) + 1.0)).collect();
        // Reproduce the four-accumulator order by hand.
        let mut a = [0.0f64; 4];
        for i in 0..8 {
            a[i % 4] += u[i] * v[i];
        }
        a[0] += u[8] * v[8];
        let expect = ((a[0] + a[1]) + a[2]) + a[3];
        assert_eq!(dot(&u, &v).to_bits(), expect.to_bits());
    }

    #[test]
    fn gram_is_symmetric_and_matches_the_definition() {
        let x0: Vec<f64> = (0..1000).map(|i| f64::from(i) * 0.5).collect();
        let x1: Vec<f64> = (0..1000).map(|i| 1.0 + f64::from(i % 7)).collect();
        let ones = vec![1.0f64; 1000];
        let y: Vec<f64> = (0..1000).map(|i| 3.0 + f64::from(i) * 0.25).collect();

        let g = gram(&[&x0, &x1, &ones], &y);
        assert_eq!(g.k, 3);
        assert_eq!(g.n, 1000);
        for i in 0..4 {
            for j in 0..4 {
                assert_eq!(g.get(i, j).to_bits(), g.get(j, i).to_bits());
            }
        }
        assert_eq!(g.get(2, 2), 1000.0);
        assert_eq!(g.get(0, 1).to_bits(), dot(&x0, &x1).to_bits());
        assert_eq!(g.get(3, 3).to_bits(), dot(&y, &y).to_bits());
    }

    #[test]
    fn crossing_a_chunk_boundary_changes_nothing_about_determinism() {
        let n = crate::reduce::CHUNK_ROWS * 2 + 13;
        let col: Vec<f64> = (0..n).map(|i| ((i % 97) as f64) * 0.125).collect();
        let y: Vec<f64> = (0..n).map(|i| ((i % 31) as f64) * 0.5).collect();
        let a = gram(&[&col], &y);
        let b = gram(&[&col], &y);
        assert_eq!(
            a.a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b.a.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
}
