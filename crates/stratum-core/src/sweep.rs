//! The Stata-compatible solver: a symmetric sweep on the cross-product matrix
//! (`05` §6). No LAPACK, no `nalgebra`, no explicit inversion.
//!
//! Sweeping the augmented matrix once yields β̂, RSS **and** `(X'X)⁻¹` from the
//! same pass, which is why it beats a QR here: `e(V)` needs the inverse, and a
//! QR would have to do a second triangular solve to get it. The accuracy cost
//! versus QR is real and accepted (`05` §5.2) because matching Stata's
//! *collinearity behaviour* matters more than the last two digits of a
//! condition-number-10¹² design that no one should be running.

use crate::gram::Gram;

/// F7's collinearity tolerance: a pivot is dead when its current diagonal has
/// fallen to `1e-9` of its pre-sweep value.
pub const COLLIN_TOL: f64 = 1e-9;

/// Which columns were pivoted, in which order, and which were dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SweepPlan {
    /// Column indices in the order they were pivoted.
    pub order: Vec<usize>,
    /// `true` for columns skipped as collinear.
    pub omitted: Vec<bool>,
    /// Number of columns actually swept, the constant included.
    pub rank: usize,
}

/// A fully swept augmented cross-product matrix.
#[derive(Clone, Debug)]
pub struct GramSolve {
    /// `(k+1) x (k+1)`, symmetric, fully swept. Row/column `k` is `y`.
    a: Vec<f64>,
    k: usize,
    /// The pivot order and the omitted set.
    pub plan: SweepPlan,
}

impl GramSolve {
    /// Sweep `a` in place and return the solved system.
    ///
    /// * `a` is the `(k+1)²` row-major augmented matrix from [`crate::gram`].
    /// * `d0` is the **pre-sweep** diagonal, `d0[j] = A[j][j]`, which is F7's
    ///   tolerance denominator. It must be captured before any sweeping.
    /// * `has_cons` marks column `k-1` as the constant: Stata appends `_cons`
    ///   last and sweeps it first and unconditionally (F8), so a model whose
    ///   only "information" is the intercept still reports one.
    ///
    /// # Pivot rule (F5/F6/F7/F8, and the subtlest thing in the design)
    ///
    /// The pivot is `argmax A[j][j]` over the **current** diagonal, after every
    /// previous sweep — not a static pre-sort of the original diagonal. F6 is
    /// the experiment that separates them: on the orthogonal design
    /// `u = 3e₁, v = 10e₂, w = ½u + v`, the static rule drops `u` and Stata
    /// drops `v`. `tests/sweep_props.rs` asserts exactly that case.
    ///
    /// # Panics
    ///
    /// If `a.len() != (k+1)²` or `d0.len() != k`.
    #[must_use]
    pub fn solve(mut a: Vec<f64>, k: usize, d0: &[f64], has_cons: bool) -> GramSolve {
        let dim = k + 1;
        assert_eq!(a.len(), dim * dim, "augmented matrix is not (k+1)^2");
        assert_eq!(d0.len(), k, "d0 must carry one entry per design column");

        let mut omitted = vec![false; k];
        let mut swept = vec![false; k];
        let mut order = Vec::with_capacity(k);

        // F8. The constant is swept first and can never be omitted; without
        // this a model of collinear dummies would drop the intercept and print
        // a table with no `_cons` row.
        if has_cons && k > 0 {
            let c = k - 1;
            sweep_on(&mut a, dim, c);
            swept[c] = true;
            order.push(c);
        }

        loop {
            // argmax over the CURRENT diagonal; ties go to the lower index so
            // that column order, not floating-point luck, decides.
            let mut best: Option<usize> = None;
            for j in 0..k {
                if swept[j] || omitted[j] {
                    continue;
                }
                let d = a[j * dim + j];
                match best {
                    None => best = Some(j),
                    Some(b) if d > a[b * dim + b] => best = Some(j),
                    _ => {}
                }
            }
            let Some(p) = best else { break };

            if !passes_tolerance(a[p * dim + p], d0[p]) {
                // `05` §6.3 stops here on the grounds that nothing left can
                // pass either. That is true when the denominators are alike and
                // false when they are not — a column scaled by 1e-6 has a tiny
                // current diagonal and a tiny `d0`. So every remaining failure
                // is marked and the loop continues; if something does still
                // pass it gets swept, which is what Stata does.
                omitted[p] = true;
                for j in 0..k {
                    if !swept[j] && !omitted[j] && !passes_tolerance(a[j * dim + j], d0[j]) {
                        omitted[j] = true;
                    }
                }
                continue;
            }

            sweep_on(&mut a, dim, p);
            swept[p] = true;
            order.push(p);
        }

        // An omitted column contributes nothing: β̂ = 0 and its row and column
        // of `e(V)` are exactly zero, which is what the classic renderer prints
        // as `0  (omitted)`.
        for (j, &drop) in omitted.iter().enumerate() {
            if drop {
                for i in 0..dim {
                    a[i * dim + j] = 0.0;
                    a[j * dim + i] = 0.0;
                }
            }
        }

        let rank = order.len();
        GramSolve {
            a,
            k,
            plan: SweepPlan {
                order,
                omitted,
                rank,
            },
        }
    }

    /// β̂, length `k`, with exact zeros at omitted columns.
    #[must_use]
    pub fn beta(&self) -> Vec<f64> {
        let dim = self.k + 1;
        (0..self.k).map(|j| self.a[j * dim + self.k]).collect()
    }

    /// The residual sum of squares.
    ///
    /// **`05` §6.2 says `A[k][k]` is `-RSS`. With the operator that section
    /// prescribes, it is `+RSS`.** Each sweep subtracts `A[k][p]^2 / d` from
    /// `A[k][k]`, so the cell walks down from `y'y` to the residual and stops
    /// there; only the design block picks up the sign flip, from the
    /// `A[p][p] = -1/d` step, which is why [`Self::xtx_inv`] negates and this
    /// does not. `tests/sweep_props.rs` cross-checks against a directly summed
    /// residual, which is what caught it.
    #[must_use]
    pub fn rss(&self) -> f64 {
        let dim = self.k + 1;
        self.a[self.k * dim + self.k]
    }

    /// `(X'X)⁻¹`, `k*k` row-major, with exact zero rows and columns at omitted
    /// columns. `e(V) = s² * this` for OLS.
    #[must_use]
    pub fn xtx_inv(&self) -> Vec<f64> {
        let dim = self.k + 1;
        let mut v = vec![0.0f64; self.k * self.k];
        for i in 0..self.k {
            if self.plan.omitted[i] {
                continue;
            }
            for j in 0..self.k {
                if self.plan.omitted[j] {
                    continue;
                }
                v[i * self.k + j] = -self.a[i * dim + j];
            }
        }
        v
    }

    /// Number of columns that survived, the constant included.
    #[must_use]
    pub fn rank(&self) -> usize {
        self.plan.rank
    }

    /// Number of design columns.
    #[must_use]
    pub fn k(&self) -> usize {
        self.k
    }
}

/// Solve straight from a [`Gram`], capturing `d0` for you.
#[must_use]
pub fn solve_gram(g: &Gram, has_cons: bool) -> GramSolve {
    let d0 = g.diagonal();
    GramSolve::solve(g.a.clone(), g.k, &d0, has_cons)
}

/// A pivot survives when its current diagonal is still a real fraction of the
/// original. A non-positive original diagonal means an all-zero column, which
/// is collinear with everything by definition.
#[inline]
fn passes_tolerance(current: f64, original: f64) -> bool {
    original > 0.0 && current > 0.0 && current / original >= COLLIN_TOL
}

/// The sweep operator on pivot `p`, transcribed from `05` §6.2 in that order.
///
/// The order is load-bearing: rows and columns are divided by `d` **before** the
/// rank-one update, so the update reads the already-divided values and
/// multiplies by `d` again. Reassociating it changes the last ulp of `e(V)`.
fn sweep_on(a: &mut [f64], dim: usize, p: usize) {
    let d = a[p * dim + p];
    a[p * dim + p] = -1.0 / d;
    for i in 0..dim {
        if i != p {
            a[i * dim + p] /= d;
        }
    }
    for i in 0..dim {
        if i != p {
            a[p * dim + i] /= d;
        }
    }
    for i in 0..dim {
        if i == p {
            continue;
        }
        let aip = a[i * dim + p];
        for j in 0..dim {
            if j == p {
                continue;
            }
            a[i * dim + j] -= aip * a[p * dim + j] * d;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gram::gram;

    fn fit(cols: &[&[f64]], y: &[f64], has_cons: bool) -> GramSolve {
        let g = gram(cols, y);
        solve_gram(&g, has_cons)
    }

    #[test]
    fn exact_fit_recovers_the_coefficients() {
        let n = 200;
        let x1: Vec<f64> = (0..n).map(|i| f64::from(i) * 0.5).collect();
        let x2: Vec<f64> = (0..n).map(|i| f64::from(i % 13) - 6.0).collect();
        let ones = vec![1.0f64; n as usize];
        let y: Vec<f64> = (0..n as usize)
            .map(|i| 2.0 * x1[i] - 3.0 * x2[i] + 7.0)
            .collect();

        let s = fit(&[&x1, &x2, &ones], &y, true);
        let b = s.beta();
        assert!((b[0] - 2.0).abs() < 1e-9, "b0 = {}", b[0]);
        assert!((b[1] + 3.0).abs() < 1e-9, "b1 = {}", b[1]);
        assert!((b[2] - 7.0).abs() < 1e-9, "b2 = {}", b[2]);
        // A perfect fit leaves cancellation noise, not an exact zero: the
        // cross-products are ~1e10 here and the residual is ~1e-11.
        let tss: f64 = y.iter().map(|v| v * v).sum();
        assert!(s.rss().abs() < 1e-15 * tss, "rss = {}", s.rss());
        assert_eq!(s.rank(), 3);
        assert_eq!(s.plan.omitted, vec![false, false, false]);
    }

    #[test]
    fn f6_dynamic_pivoting_drops_v_not_u() {
        // The experiment that separates the dynamic rule from a static
        // pre-sort of the original diagonal. u = 3e1, v = 10e2, w = 0.5u + v.
        // Static order (w, v, u) drops u; Stata and the dynamic rule drop v.
        let u = [3.0f64, 0.0];
        let v = [0.0f64, 10.0];
        let w = [1.5f64, 10.0];
        let y = [1.0f64, 2.0];
        let s = fit(&[&u, &v, &w], &y, false);
        assert_eq!(
            s.plan.omitted,
            vec![false, true, false],
            "order {:?}",
            s.plan.order
        );
        assert_eq!(s.plan.order[0], 2, "the largest current diagonal is w");
    }

    #[test]
    fn duplicate_column_is_omitted_and_beta_is_unchanged() {
        let n = 500usize;
        let x1: Vec<f64> = (0..n).map(|i| (i as f64).sin_free()).collect();
        let ones = vec![1.0f64; n];
        let y: Vec<f64> = (0..n)
            .map(|i| 1.5 * x1[i] + 4.0 + ((i % 5) as f64))
            .collect();

        let base = fit(&[&x1, &ones], &y, true);
        let dup = fit(&[&x1, &x1, &ones], &y, true);

        assert_eq!(dup.plan.omitted.iter().filter(|o| **o).count(), 1);
        let (b0, b1) = (base.beta(), dup.beta());
        let surviving: Vec<f64> = b1
            .iter()
            .zip(dup.plan.omitted.iter())
            .filter(|(_, o)| !**o)
            .map(|(v, _)| *v)
            .collect();
        for (a, b) in b0.iter().zip(surviving.iter()) {
            assert!((a - b).abs() < 1e-8, "{a} vs {b}");
        }
        assert!((base.rss() - dup.rss()).abs() < 1e-6);
    }

    /// A cheap deterministic spread that is not a transcendental (§8.11).
    trait SinFree {
        fn sin_free(self) -> f64;
    }
    impl SinFree for f64 {
        fn sin_free(self) -> f64 {
            let k = self as i64;
            ((k * 2_654_435_761i64).rem_euclid(1000) as f64) / 250.0 - 2.0
        }
    }
}
