//! `dist` against two independent oracles (`05` §13).
//!
//! * `tests/data/dist_grid.json` — mpmath at 60 decimal digits, rounded to the
//!   nearest double. It is the reference: an arbitrary-precision library has no
//!   shared ancestry with our continued fractions, so agreement is evidence
//!   rather than a coincidence of method.
//! * `statrs` — a second Rust implementation, dev-dependency only. It never
//!   links into a shipping binary because it calls `std`'s transcendentals,
//!   which are the platform's libm and therefore not bitwise portable (§3).
//!
//! Values travel as big-endian IEEE-754 hex so the grid round-trips exactly.

use std::path::PathBuf;

use serde_json::Value as J;
use statrs::distribution::{ChiSquared, ContinuousCDF, FisherSnedecor, Normal, StudentsT};
use stratum_core::dist::{betai, chi2_sf, f_sf, gammap, normal_cdf, normal_sf, t_sf};

fn grid() -> J {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/data/dist_grid.json");
    let text = std::fs::read_to_string(&p).expect("tests/data/dist_grid.json");
    serde_json::from_str(&text).expect("dist_grid.json is JSON")
}

fn f(v: &J, key: &str) -> f64 {
    let s = v[key]
        .as_str()
        .unwrap_or_else(|| panic!("missing key {key}"));
    f64::from_bits(u64::from_str_radix(s, 16).expect("16 hex digits"))
}

/// Relative error, with an absolute floor so that a true zero is comparable.
fn rel(got: f64, want: f64) -> f64 {
    if got == want {
        return 0.0;
    }
    (got - want).abs() / want.abs().max(1e-320)
}

/// `05` §13's stated target: 1e-13 relative for `p > 1e-12`, and 1e-9 relative
/// once the answer is down in the 1e-300 tail, where a double's own spacing is
/// most of the budget.
fn tolerance(want: f64) -> f64 {
    if want.abs() > 1e-12 {
        1e-13
    } else {
        1e-9
    }
}

fn check(name: &str, rows: &[J], mut f_of: impl FnMut(&J) -> f64) {
    for r in rows {
        let want = f(r, "p");
        let got = f_of(r);
        let e = rel(got, want);
        let tol = tolerance(want);
        assert!(
            e < tol,
            "{name}: relative error {e} exceeds {tol}\n  at {r} -> got {got}, want {want}"
        );
    }
}

/// The most `statrs` and this crate may differ before one of them has to be
/// shown wrong. `05` §13's number, kept at full strength.
const CROSS: f64 = 1e-12;

/// Grid rows where `statrs` is further than [`CROSS`] from us AND further than
/// us from the 60-digit reference. All 21 are `normal_cdf`/`normal_sf`; see
/// [`agrees_with_statrs`].
const EXPECTED_STATRS_DISAGREEMENTS: usize = 21;

/// One distribution's worth of the three-way cross-check. Returns the number of
/// rows where `statrs` disagreed by more than [`CROSS`] and was demonstrated to
/// be the further of the two from the 60-digit reference.
fn cross(
    name: &str,
    rows: &[J],
    mut ours: impl FnMut(&J) -> f64,
    mut theirs: impl FnMut(&J) -> (f64, f64),
) -> usize {
    let mut disputed = 0usize;
    for r in rows {
        let want = f(r, "p");
        let got = ours(r);
        let (a, b) = theirs(r);
        // statrs gets whichever of `sf` and `1 - cdf` lands closer to the
        // reference. Deep in a tail the subtraction cancels to zero while the
        // directed routine does not, and beating the worse of the two would
        // prove nothing.
        let alt = if rel(a, want) <= rel(b, want) { a } else { b };
        let apart = rel(got, alt);
        if apart < CROSS {
            continue;
        }
        disputed += 1;
        let (e_ours, e_alt) = (rel(got, want), rel(alt, want));
        assert!(
            e_ours < e_alt,
            "{name}: {apart} apart from statrs, and OURS is the further from \
             the 60-digit reference ({e_ours} vs {e_alt})\n  \
             at {r}\n  ours {got}, statrs {alt}, mpmath {want}"
        );
    }
    disputed
}

#[test]
fn agrees_with_mpmath_at_fifty_digits() {
    let g = grid();
    let rows = |k: &str| -> Vec<J> {
        g[k].as_array()
            .unwrap_or_else(|| panic!("{k} is an array"))
            .clone()
    };

    // 05 §13's target: 1e-13 relative for p > 1e-12.
    check("normal_cdf", &rows("normal_cdf"), |r| normal_cdf(f(r, "z")));
    check("normal_sf", &rows("normal_sf"), |r| normal_sf(f(r, "z")));
    check("chi2_sf", &rows("chi2_sf"), |r| {
        chi2_sf(f(r, "x"), f(r, "df"))
    });
    check("t_sf", &rows("t_sf"), |r| t_sf(f(r, "t"), f(r, "df")));
    check("f_sf", &rows("f_sf"), |r| {
        f_sf(f(r, "f"), f(r, "d1"), f(r, "d2"))
    });
    check("betai", &rows("betai"), |r| {
        betai(f(r, "a"), f(r, "b"), f(r, "x"))
    });
    check("gammap", &rows("gammap"), |r| gammap(f(r, "a"), f(r, "x")));
}

#[test]
fn the_deep_tail_keeps_nine_digits() {
    // The `normal_sf` grid runs out to z = 37, where the answer is ~5.7e-300.
    // `1 - cdf` returns exactly 0 there; a directed tail must not.
    let g = grid();
    let rows = g["normal_sf"].as_array().expect("array").clone();
    for r in &rows {
        let z = f(r, "z");
        if z < 20.0 {
            continue;
        }
        let want = f(r, "p");
        let got = normal_sf(z);
        assert!(got > 0.0, "normal_sf({z}) underflowed to zero");
        assert!(rel(got, want) < 1e-9, "normal_sf({z}): {got} vs {want}");
    }
}

/// A second implementation, by a different route — and, where it disagrees, a
/// proof of which side is wrong.
///
/// `05` §13 asks for agreement with `statrs` to 1e-12. **`statrs` 0.19 is not
/// accurate to 1e-12.** Its `erf`/`erfc` is a rational approximation good to
/// about 1e-10, so `Normal::cdf(-1)` is `0.15865525394505725` where mpmath at
/// 60 digits says `0.158655253931457051414…`, whose nearest double is
/// `0.15865525393145705`. The gap is 8.6e-11 and it is statrs's: this crate is
/// one ulp from the correctly rounded value.
///
/// Asserting a looser tolerance would hide that, so the cross-check runs on the
/// same mpmath grid instead. Every row then has three numbers, and the
/// assertion is the honest form of the bullet: **we agree with `statrs` to
/// 1e-12 wherever `statrs` is itself accurate to 1e-12, and wherever we do not,
/// our error against the 60-digit reference is strictly the smaller of the
/// two.** That is stronger than a 1e-9 tolerance, not weaker — a wrong formula,
/// a swapped argument or a mis-parameterised distribution still fails, because
/// it would put our error on the losing side of the comparison.
///
/// `statrs` is given the better of `sf(x)` and `1 - cdf(x)` at every point,
/// judged against the reference. Handing the opposing implementation its best
/// shot is what makes the conclusion mean anything.
///
/// The measured outcome, pinned below: **236 of the grid's 257 rows agree with
/// `statrs` at the full 1e-12**, and all 21 that do not are `normal_cdf` and
/// `normal_sf` — the erfc, exactly as diagnosed. `chi2_sf`, `t_sf` and `f_sf`
/// clear 1e-12 outright, so nothing here needs a weaker number than `05` §13's.
#[test]
fn agrees_with_statrs() {
    /// mpmath 1.3.0 at 60 dps: `erfc(1/sqrt(2))/2`, rounded to nearest double.
    const MP_CDF_M1: f64 = 0.158_655_253_931_457_05;

    let g = grid();
    let rows = |k: &str| -> Vec<J> {
        g[k].as_array()
            .unwrap_or_else(|| panic!("{k} is an array"))
            .clone()
    };
    let normal = Normal::new(0.0, 1.0).expect("standard normal");

    // The documented case, asserted rather than asserted-in-prose, so that a
    // `cargo update` that changes statrs's accuracy fails here and the
    // deviation gets re-derived instead of being carried on trust.
    let gap = rel(normal.cdf(-1.0), MP_CDF_M1);
    assert!(
        (8e-11..9e-11).contains(&gap),
        "statrs Normal::cdf(-1) is no longer 8.6e-11 from the 60-digit value \
         (gap {gap}); re-derive this test's premise"
    );
    assert!(
        rel(normal_cdf(-1.0), MP_CDF_M1) < 3e-16,
        "normal_cdf(-1) drifted from the 60-digit value"
    );

    let mut disputed = 0usize;
    disputed += cross(
        "normal_cdf",
        &rows("normal_cdf"),
        |r| normal_cdf(f(r, "z")),
        |r| {
            let z = f(r, "z");
            (normal.cdf(z), 1.0 - normal.sf(z))
        },
    );
    disputed += cross(
        "normal_sf",
        &rows("normal_sf"),
        |r| normal_sf(f(r, "z")),
        |r| {
            let z = f(r, "z");
            (normal.sf(z), 1.0 - normal.cdf(z))
        },
    );
    disputed += cross(
        "chi2_sf",
        &rows("chi2_sf"),
        |r| chi2_sf(f(r, "x"), f(r, "df")),
        |r| {
            let (x, df) = (f(r, "x"), f(r, "df"));
            let c = ChiSquared::new(df).expect("chi2");
            (c.sf(x), 1.0 - c.cdf(x))
        },
    );
    disputed += cross(
        "t_sf",
        &rows("t_sf"),
        |r| t_sf(f(r, "t"), f(r, "df")),
        |r| {
            let (x, df) = (f(r, "t"), f(r, "df"));
            let t = StudentsT::new(0.0, 1.0, df).expect("t");
            (t.sf(x), 1.0 - t.cdf(x))
        },
    );
    disputed += cross(
        "f_sf",
        &rows("f_sf"),
        |r| f_sf(f(r, "f"), f(r, "d1"), f(r, "d2")),
        |r| {
            let (x, d1, d2) = (f(r, "f"), f(r, "d1"), f(r, "d2"));
            let fd = FisherSnedecor::new(d1, d2).expect("F");
            (fd.sf(x), 1.0 - fd.cdf(x))
        },
    );

    // Pinned like `fmt_corpus.rs`'s null count: the deviation may not grow, and
    // if it ever shrinks to zero statrs has become accurate enough to assert
    // `05` §13's flat 1e-12 everywhere and this whole apparatus can go. Both
    // directions want a human to look, so neither passes quietly.
    assert_eq!(
        disputed, EXPECTED_STATRS_DISAGREEMENTS,
        "the statrs disagreement set changed; re-derive which side is wrong \
         before re-pinning this number"
    );
}
