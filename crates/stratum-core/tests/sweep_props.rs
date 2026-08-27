//! Properties of the sweep solver that a fixed example cannot pin (`05` §6).
//!
//! The interesting failures here are not "the answer is a bit off"; they are
//! "the collinearity rule dropped a different column from the one Stata drops",
//! which changes which coefficients a paper reports.

use proptest::prelude::*;
use stratum_core::gram::{gram, Gram};
use stratum_core::sweep::{solve_gram, GramSolve, COLLIN_TOL};

/// A deterministic PRNG. `rand` is not a dependency of this crate and a
/// transcendental is banned (§8.11), so the spread is integer-derived.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed | 1)
    }
    /// Uniform-ish in [-1, 1).
    fn next_f64(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64) / f64::from(1u32 << 31) / 4_194_304.0 - 1.0
    }
}

fn design(seed: u64, n: usize, k: usize) -> Vec<Vec<f64>> {
    let mut r = Lcg::new(seed);
    (0..k)
        .map(|_| (0..n).map(|_| r.next_f64()).collect())
        .collect()
}

fn refs(cols: &[Vec<f64>]) -> Vec<&[f64]> {
    cols.iter().map(Vec::as_slice).collect()
}

/// `max diag / min diag` of the raw cross-product matrix. A LOWER bound on the
/// 2-norm condition number of a symmetric positive-definite matrix, so using it
/// as the tolerance scale makes the assertion stricter than `05` §17.6 asks.
fn kappa_lower_bound(g: &Gram) -> f64 {
    let d = g.diagonal();
    let hi = d.iter().cloned().fold(f64::MIN, f64::max);
    let lo = d.iter().cloned().fold(f64::MAX, f64::min);
    if lo > 0.0 {
        hi / lo
    } else {
        f64::INFINITY
    }
}

/// `‖(X'X)(X'X)⁻¹ − I‖∞` over the surviving block.
fn inverse_residual(g: &Gram, s: &GramSolve) -> f64 {
    let k = g.k;
    let inv = s.xtx_inv();
    let mut worst = 0.0f64;
    for i in 0..k {
        if s.plan.omitted[i] {
            continue;
        }
        for j in 0..k {
            if s.plan.omitted[j] {
                continue;
            }
            let mut acc = 0.0;
            for (t, o) in s.plan.omitted.iter().enumerate() {
                if !o {
                    acc += g.get(i, t) * inv[t * k + j];
                }
            }
            let want = if i == j { 1.0 } else { 0.0 };
            worst = worst.max((acc - want).abs());
        }
    }
    worst
}

#[test]
fn f6_dynamic_pivoting_drops_v() {
    // THE experiment that separates the dynamic rule from a static pre-sort of
    // the original diagonal (05 §6.3). u = 3e1, v = 10e2, w = 0.5u + v.
    //
    //   original diagonals: u 9, v 100, w 102.25   -> static order w, v, u
    //   static rule sweeps w then v, finds u collinear, DROPS u
    //   Stata drops v, and so does the current-diagonal rule:
    //     sweep w (102.25); u's diagonal falls to 8.80, v's to 2.20;
    //     sweep u; v is now exactly collinear and goes.
    let u = [3.0f64, 0.0];
    let v = [0.0f64, 10.0];
    let w = [1.5f64, 10.0];
    let y = [1.0f64, 2.0];
    let g = gram(&[&u, &v, &w], &y);
    let s = solve_gram(&g, false);

    assert_eq!(s.plan.omitted, vec![false, true, false], "must drop v");
    assert_eq!(s.plan.order, vec![2, 0], "w first on the current diagonal");
    assert_eq!(s.rank(), 2);
    assert_eq!(s.beta()[1], 0.0, "an omitted column has beta exactly zero");

    // A static pre-sort of the ORIGINAL diagonal would have swept w, then v.
    let d0 = g.diagonal();
    let mut static_order: Vec<usize> = (0..3).collect();
    static_order.sort_by(|a, b| d0[*b].partial_cmp(&d0[*a]).expect("finite"));
    assert_eq!(static_order, vec![2, 1, 0], "static order is w, v, u");
    assert_ne!(static_order[1], s.plan.order[1], "the two rules disagree");
}

#[test]
fn duplicate_column_is_dropped_exactly_once() {
    for seed in [1u64, 7, 99, 12345] {
        let cols = design(seed, 400, 3);
        let ones = vec![1.0f64; 400];
        let y: Vec<f64> = (0..400)
            .map(|i| 2.0 * cols[0][i] - cols[1][i] + 0.5 * cols[2][i] + 3.0)
            .collect();

        let base_cols: Vec<&[f64]> = {
            let mut v = refs(&cols);
            v.push(&ones);
            v
        };
        let base = solve_gram(&gram(&base_cols, &y), true);

        // Append an exact duplicate of column 0, before the constant.
        let mut dup_cols: Vec<&[f64]> = refs(&cols);
        dup_cols.push(&cols[0]);
        dup_cols.push(&ones);
        let dup = solve_gram(&gram(&dup_cols, &y), true);

        assert_eq!(
            dup.plan.omitted.iter().filter(|o| **o).count(),
            base.plan.omitted.iter().filter(|o| **o).count() + 1,
            "seed {seed}: exactly one more omitted column"
        );
        assert_eq!(dup.rank(), base.rank(), "seed {seed}: rank is unchanged");
        assert!(
            (dup.rss() - base.rss()).abs() <= 1e-8 * base.rss().abs().max(1e-12),
            "seed {seed}: rss {} vs {}",
            dup.rss(),
            base.rss()
        );

        let surviving: Vec<f64> = dup
            .beta()
            .into_iter()
            .zip(dup.plan.omitted.iter())
            .filter(|(_, o)| !**o)
            .map(|(b, _)| b)
            .collect();
        for (a, b) in base.beta().iter().zip(surviving.iter()) {
            assert!((a - b).abs() < 1e-8, "seed {seed}: beta {a} vs {b}");
        }
    }
}

#[test]
fn permuting_columns_does_not_change_the_omitted_set() {
    // c = a + b, with three distinct column norms so that no pivot decision is
    // ever a tie. (With an EXACT duplicate the two candidates have identical
    // diagonals and the rule breaks the tie by column index, so the identity of
    // the dropped column genuinely does depend on the order — which is why this
    // property is stated over a design with no tie in it.)
    let n = 300usize;
    let cols = design(4242, n, 2);
    let a: Vec<f64> = cols[0].iter().map(|v| v * 3.0).collect();
    let b: Vec<f64> = cols[1].iter().map(|v| v * 7.0).collect();
    let c: Vec<f64> = a.iter().zip(b.iter()).map(|(x, y)| x + y).collect();
    let y: Vec<f64> = (0..n).map(|i| a[i] - 2.0 * b[i]).collect();

    let named: [(&str, &[f64]); 3] = [("a", &a), ("b", &b), ("c", &c)];
    let perms = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];

    let mut reference: Option<Vec<&str>> = None;
    for p in perms {
        let cols: Vec<&[f64]> = p.iter().map(|i| named[*i].1).collect();
        let s = solve_gram(&gram(&cols, &y), false);
        let mut dropped: Vec<&str> = s
            .plan
            .omitted
            .iter()
            .enumerate()
            .filter(|(_, o)| **o)
            .map(|(i, _)| named[p[i]].0)
            .collect();
        dropped.sort_unstable();
        match &reference {
            None => reference = Some(dropped),
            Some(r) => assert_eq!(&dropped, r, "permutation {p:?} changed the omitted set"),
        }
        assert_eq!(s.rank(), 2, "permutation {p:?}");
    }
    assert_eq!(reference.expect("at least one permutation").len(), 1);
}

#[test]
fn the_constant_is_never_omitted() {
    // F8. Two identical dummies plus a constant: the dummies are collinear with
    // the intercept, and the intercept must survive anyway.
    let n = 100usize;
    let d: Vec<f64> = (0..n).map(|i| f64::from(u8::from(i % 2 == 0))).collect();
    let e = d.clone();
    let ones = vec![1.0f64; n];
    let y: Vec<f64> = (0..n).map(|i| 1.0 + 2.0 * d[i]).collect();
    let s = solve_gram(&gram(&[&d, &e, &ones], &y), true);
    assert!(!s.plan.omitted[2], "the constant survives");
    assert_eq!(s.plan.order[0], 2, "and it is swept first");
    assert_eq!(s.plan.omitted.iter().filter(|o| **o).count(), 1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// `05` §17.6 — the inverse really is the inverse.
    #[test]
    fn inverse_is_an_inverse(seed in any::<u64>(), k in 1usize..=6, n in 40usize..300) {
        let cols = design(seed, n, k);
        let y: Vec<f64> = (0..n).map(|i| f64::from(i as u32 % 11) - 5.0).collect();
        let g = gram(&refs(&cols), &y);
        let s = solve_gram(&g, false);
        prop_assume!(s.rank() == k);
        let kappa = kappa_lower_bound(&g);
        let resid = inverse_residual(&g, &s);
        prop_assert!(
            resid < 1e-8 * kappa,
            "residual {resid} exceeds 1e-8 * kappa ({kappa})"
        );
    }

    /// RSS is the residual sum of squares, computed independently.
    #[test]
    fn rss_matches_a_direct_residual_sum(seed in any::<u64>(), k in 1usize..=4, n in 60usize..200) {
        let cols = design(seed, n, k);
        let ones = vec![1.0f64; n];
        let mut all: Vec<&[f64]> = refs(&cols);
        all.push(&ones);
        let y: Vec<f64> = (0..n).map(|i| f64::from(i as u32 % 7) * 0.5).collect();
        let g = gram(&all, &y);
        let s = solve_gram(&g, true);
        prop_assume!(s.rank() == k + 1);
        let b = s.beta();
        let mut direct = 0.0f64;
        for i in 0..n {
            let mut fit = 0.0;
            for (j, c) in all.iter().enumerate() {
                fit += b[j] * c[i];
            }
            let r = y[i] - fit;
            direct += r * r;
        }
        let tss: f64 = y.iter().map(|v| v * v).sum();
        prop_assert!(
            (direct - s.rss()).abs() < 1e-8 * tss,
            "direct {direct} vs rss {}",
            s.rss()
        );
    }

    /// Solving twice gives BITWISE identical answers (ADR-013).
    #[test]
    fn solving_is_deterministic(seed in any::<u64>(), k in 1usize..=5, n in 40usize..400) {
        let cols = design(seed, n, k);
        let y: Vec<f64> = (0..n).map(|i| f64::from(i as u32 % 13)).collect();
        let a = solve_gram(&gram(&refs(&cols), &y), false);
        let b = solve_gram(&gram(&refs(&cols), &y), false);
        prop_assert_eq!(
            a.beta().iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b.beta().iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
        prop_assert_eq!(a.rss().to_bits(), b.rss().to_bits());
        prop_assert_eq!(a.plan.omitted, b.plan.omitted);
    }
}

#[test]
fn tolerance_is_the_f7_constant() {
    assert_eq!(COLLIN_TOL, 1e-9);
}
