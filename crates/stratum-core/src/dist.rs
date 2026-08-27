//! Distribution functions, implemented here rather than taken from a crate
//! (`05` §13).
//!
//! Three reasons, in order of weight:
//!
//! 1. **Determinism.** `statrs` calls `std`'s transcendentals, which are the
//!    platform's libm. Every evaluation here goes through [`crate::math`], so
//!    `P>|t|` is the same 17 digits on macOS, Linux and Windows (ADR-013).
//! 2. **Directed tails.** The survivor function is primary and the smaller tail
//!    is always the one computed; `1 - cdf` never appears in a tail. That is
//!    the difference between agreeing with Stata to 1e-15 and to 1e-9 out at
//!    `p = 1e-12`, and `P>|t|` is a published number.
//! 3. It is about four hundred lines.
//!
//! `statrs` remains as a **dev-dependency oracle** and
//! `tests/data/dist_grid.json` (mpmath at 50 digits) is the independent one.
//!
//! Accuracy target: relative error `< 1e-13` for `p > 1e-12`, and `< 1e-9`
//! into the 1e-300 tail.

use crate::math;

/// Relative convergence threshold for the two continued fractions.
const EPS: f64 = 3.0e-16;
/// Underflow guard for the modified Lentz algorithm.
const FPMIN: f64 = 1.0e-300;
/// Iteration cap. Both series converge in well under 100 for every argument we
/// reach; the cap exists so a NaN argument cannot spin.
const ITMAX: usize = 300;

/// `sqrt(2)`, exactly representable steps away from the true value but computed
/// once so every caller shares the same rounding.
const SQRT2: f64 = std::f64::consts::SQRT_2;

/// The regularized incomplete beta function `I_x(a, b)`.
///
/// Lentz's modified continued fraction with the standard
/// `x > (a+1)/(a+b+2)` reflection, which keeps the fraction in its
/// fast-converging half. Relative accuracy ~1e-15.
#[must_use]
pub fn betai(a: f64, b: f64, x: f64) -> f64 {
    if a <= 0.0 || b <= 0.0 || a.is_nan() || b.is_nan() || x.is_nan() {
        return f64::NAN;
    }
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    // ln of the leading factor, evaluated in logs so that a = 1e5 does not
    // overflow the gamma functions.
    let front = math::exp(neg_ln_beta(a, b) + a * math::ln(x) + b * math::ln(1.0 - x));
    if x < (a + 1.0) / (a + b + 2.0) {
        front * betacf(a, b, x) / a
    } else {
        1.0 - front * betacf(b, a, 1.0 - x) / b
    }
}

/// `ln(Gamma(a+b) / (Gamma(a) Gamma(b)))`, which is `-ln B(a, b)`.
///
/// The naive `lgamma(a+b) - lgamma(a) - lgamma(b)` subtracts two numbers that
/// are ~2605 apiece at `a = 500`, so its absolute error is ~3e-13 and `exp`
/// carries that straight into the answer: `t_sf(1, 1000)` came out 8e-13 wrong,
/// against a 1e-13 target. Above `a = 20` the ratio is computed from Stirling's
/// series in a form with no large cancelling terms in it.
fn neg_ln_beta(a: f64, b: f64) -> f64 {
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    if hi >= 20.0 {
        ln_gamma_ratio(hi, lo) - math::lgamma(lo)
    } else {
        math::lgamma(a + b) - math::lgamma(a) - math::lgamma(b)
    }
}

/// `ln(Gamma(z + d) / Gamma(z))` for `z >= 20`, to ~1e-16 relative.
///
/// Stirling gives `lnG(z) = (z-1/2)ln z - z + ln(2pi)/2 + 1/(12z) - 1/(360z^3)
/// + 1/(1260z^5) - 1/(1680z^7)`; the difference is regrouped as
/// `d ln z + (z+d-1/2) ln1p(d/z) - d + [corrections]`, so that every term is
/// `O(d)` or smaller and nothing large is subtracted from anything large.
fn ln_gamma_ratio(z: f64, d: f64) -> f64 {
    let u = d / z;
    let w = z + d;
    d * math::ln(z) + (w - 0.5) * math::ln_1p(u) - d + stirling_tail(w) - stirling_tail(z)
}

/// The `1/(12z) - 1/(360z^3) + …` tail of Stirling's series.
fn stirling_tail(z: f64) -> f64 {
    let zi = 1.0 / z;
    let z2 = zi * zi;
    zi * (1.0 / 12.0 + z2 * (-1.0 / 360.0 + z2 * (1.0 / 1260.0 + z2 * (-1.0 / 1680.0))))
}

/// The continued fraction for `betai`, in Lentz's modified form.
fn betacf(a: f64, b: f64, x: f64) -> f64 {
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0f64;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < FPMIN {
        d = FPMIN;
    }
    d = 1.0 / d;
    let mut h = d;

    for m in 1..=ITMAX {
        let m_f = m as f64;
        let m2 = 2.0 * m_f;

        // Even step.
        let aa = m_f * (b - m_f) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        h *= d * c;

        // Odd step.
        let aa = -(a + m_f) * (qab + m_f) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    h
}

/// The regularized lower incomplete gamma function `P(a, x)`.
#[must_use]
pub fn gammap(a: f64, x: f64) -> f64 {
    if a <= 0.0 || a.is_nan() || x < 0.0 || x.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 {
        return 0.0;
    }
    if x < a + 1.0 {
        gser(a, x)
    } else {
        1.0 - gcf(a, x)
    }
}

/// The regularized upper incomplete gamma function `Q(a, x) = 1 - P(a, x)`.
///
/// Computed directly from the continued fraction in the upper region, never as
/// `1 - gammap`, so `chi2_sf` keeps its digits far out in the tail.
#[must_use]
pub fn gammaq(a: f64, x: f64) -> f64 {
    if a <= 0.0 || a.is_nan() || x < 0.0 || x.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 {
        return 1.0;
    }
    if x < a + 1.0 {
        1.0 - gser(a, x)
    } else {
        gcf(a, x)
    }
}

/// Series representation of `P(a, x)`, for `x < a + 1`.
fn gser(a: f64, x: f64) -> f64 {
    let mut ap = a;
    let mut sum = 1.0 / a;
    let mut del = sum;
    for _ in 0..ITMAX {
        ap += 1.0;
        del *= x / ap;
        sum += del;
        if del.abs() < sum.abs() * EPS {
            break;
        }
    }
    sum * math::exp(-x + a * math::ln(x) - math::lgamma(a))
}

/// Continued fraction for `Q(a, x)`, for `x >= a + 1`. Lentz's modified form.
fn gcf(a: f64, x: f64) -> f64 {
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / FPMIN;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..=ITMAX {
        let an = -(i as f64) * (i as f64 - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = b + an / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    math::exp(-x + a * math::ln(x) - math::lgamma(a)) * h
}

/// Standard normal CDF, `P(Z <= z)`.
///
/// `erfc` based rather than `0.5 * (1 + erf(z/√2))`: for `z = -8` the `erf`
/// form cancels away eleven digits, and this one keeps all of them.
#[must_use]
pub fn normal_cdf(z: f64) -> f64 {
    0.5 * math::erfc(-z / SQRT2)
}

/// Standard normal survivor function, `P(Z > z)`.
#[must_use]
pub fn normal_sf(z: f64) -> f64 {
    0.5 * math::erfc(z / SQRT2)
}

/// Standard normal density.
#[must_use]
pub fn normal_pdf(z: f64) -> f64 {
    const INV_SQRT_2PI: f64 = 0.398_942_280_401_432_7;
    INV_SQRT_2PI * math::exp(-0.5 * z * z)
}

/// Standard normal quantile.
///
/// Acklam's rational approximation (relative error ~1.15e-9) followed by two
/// Halley steps against [`normal_cdf`], which lands it at full double
/// precision. Halley rather than Newton because the second derivative is free:
/// `φ'(z) = -z φ(z)`.
#[must_use]
pub fn normal_inv(p: f64) -> f64 {
    if p.is_nan() || !(0.0..=1.0).contains(&p) {
        return f64::NAN;
    }
    if p == 0.0 {
        return f64::NEG_INFINITY;
    }
    if p == 1.0 {
        return f64::INFINITY;
    }

    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239e0,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838e0,
        -2.549_732_539_343_734e0,
        4.374_664_141_464_968e0,
        2.938_163_982_698_783e0,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996e0,
        3.754_408_661_907_416e0,
    ];
    const P_LOW: f64 = 0.024_25;

    let mut z = if p < P_LOW {
        let q = math::sqrt(-2.0 * math::ln(p));
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - P_LOW {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = math::sqrt(-2.0 * math::ln(1.0 - p));
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };

    for _ in 0..2 {
        let e = normal_cdf(z) - p;
        let u = e / normal_pdf(z);
        // Halley: the correction term is exactly z*u/2 for the normal.
        z -= u / (1.0 + 0.5 * z * u);
    }
    z
}

/// Student's t survivor function, `P(T > t)`.
#[must_use]
pub fn t_sf(t: f64, df: f64) -> f64 {
    if df <= 0.0 || t.is_nan() {
        return f64::NAN;
    }
    let half = 0.5 * betai(0.5 * df, 0.5, df / (df + t * t));
    if t > 0.0 {
        half
    } else {
        1.0 - half
    }
}

/// Student's t CDF, `P(T <= t)`. Always evaluated as the smaller tail.
#[must_use]
pub fn t_cdf(t: f64, df: f64) -> f64 {
    if df <= 0.0 || t.is_nan() {
        return f64::NAN;
    }
    let half = 0.5 * betai(0.5 * df, 0.5, df / (df + t * t));
    if t > 0.0 {
        1.0 - half
    } else {
        half
    }
}

/// Two-sided t tail, `P(|T| > |t|)` — the `P>|t|` every estimation table prints.
#[must_use]
pub fn t_tail2(t: f64, df: f64) -> f64 {
    if df <= 0.0 || t.is_nan() {
        return f64::NAN;
    }
    betai(0.5 * df, 0.5, df / (df + t * t))
}

/// Student's t quantile.
///
/// Cornish–Fisher seed off the normal quantile, then Halley on [`t_cdf`]. Four
/// iterations is enough for `df >= 1` across the whole `(0,1)` range; the loop
/// exits on the first step that moves less than an ulp.
#[must_use]
pub fn t_inv(p: f64, df: f64) -> f64 {
    if p.is_nan() || df <= 0.0 || !(0.0..=1.0).contains(&p) {
        return f64::NAN;
    }
    if p == 0.0 {
        return f64::NEG_INFINITY;
    }
    if p == 1.0 {
        return f64::INFINITY;
    }
    if p == 0.5 {
        return 0.0;
    }

    // Two closed forms. They are not an optimisation: the Cornish–Fisher seed
    // is useless in the extreme tail of a Cauchy, where `t_inv(1e-8, 1)` is
    // -3.2e7 and the expansion suggests -5e4.
    if df == 1.0 {
        // The Cauchy quantile as a COTANGENT, not `tan(pi*(p - 0.5))`. Near
        // `p = 0`, `pi*(p - 0.5)` is a number close to `-pi/2` and its last bits
        // are the whole answer: the tangent form loses eight digits at
        // `p = 1e-8`, and this one loses none.
        return if p < 0.5 {
            -1.0 / math::tan(core::f64::consts::PI * p)
        } else {
            1.0 / math::tan(core::f64::consts::PI * (1.0 - p))
        };
    }
    if df == 2.0 {
        return (2.0 * p - 1.0) * math::sqrt(2.0 / (4.0 * p * (1.0 - p)));
    }

    let z = normal_inv(p);
    // Cornish–Fisher expansion of the t quantile in the normal quantile.
    let g1 = (z * z * z + z) / 4.0;
    let g2 = (5.0 * z * z * z * z * z + 16.0 * z * z * z + 3.0 * z) / 96.0;
    let g3 =
        (3.0 * math::powi(z, 7) + 19.0 * math::powi(z, 5) + 17.0 * z * z * z - 15.0 * z) / 384.0;
    let mut t = z + g1 / df + g2 / (df * df) + g3 / (df * df * df);

    // Halley, but bracketed. An unguarded Newton on a heavy tail overshoots and
    // never comes back; the bracket makes every step monotone in the answer.
    let (mut lo, mut hi) = (-1.0e30f64, 1.0e30f64);
    for _ in 0..80 {
        let e = t_cdf(t, df) - p;
        if e == 0.0 {
            // Exactly on the answer. Falling through would set `lo = t`, make
            // the Newton step land on the bound, and bisect the bracket's other
            // end — which is 1e30 away.
            break;
        }
        if e > 0.0 {
            hi = t;
        } else {
            lo = t;
        }
        let pdf = t_pdf(t, df);
        let cand = if pdf > 0.0 {
            let u = e / pdf;
            // t's log-density derivative gives the Halley correction in closed
            // form.
            let dlog = -(df + 1.0) * t / (df + t * t);
            t - u / (1.0 + 0.5 * u * dlog)
        } else {
            f64::NAN
        };
        // Convergence is decided BEFORE the bracket, because the step that
        // lands exactly on the answer also lands exactly on the bound that was
        // just set from it, and rejecting it would bisect a bracket whose far
        // end is 1e30 away.
        if cand.is_finite() {
            let step = cand - t;
            if step == 0.0 || step.abs() <= 1e-15 * t.abs().max(1.0) {
                t = cand;
                break;
            }
        }
        t = if cand.is_finite() && cand > lo && cand < hi {
            cand
        } else {
            0.5 * (lo + hi)
        };
    }
    t
}

/// Student's t density.
#[must_use]
pub fn t_pdf(t: f64, df: f64) -> f64 {
    if df <= 0.0 {
        return f64::NAN;
    }
    let lg = math::lgamma(0.5 * (df + 1.0)) - math::lgamma(0.5 * df);
    let ln_norm = lg - 0.5 * math::ln(df * std::f64::consts::PI);
    math::exp(ln_norm - 0.5 * (df + 1.0) * math::ln(1.0 + t * t / df))
}

/// Chi-squared survivor function, `P(X > x)`.
#[must_use]
pub fn chi2_sf(x: f64, df: f64) -> f64 {
    if df <= 0.0 || x.is_nan() {
        return f64::NAN;
    }
    if x <= 0.0 {
        return 1.0;
    }
    gammaq(0.5 * df, 0.5 * x)
}

/// Chi-squared CDF, `P(X <= x)`.
#[must_use]
pub fn chi2_cdf(x: f64, df: f64) -> f64 {
    if df <= 0.0 || x.is_nan() {
        return f64::NAN;
    }
    if x <= 0.0 {
        return 0.0;
    }
    gammap(0.5 * df, 0.5 * x)
}

/// F survivor function, `P(F > f)` — the `Prob > F` of every ANOVA table.
#[must_use]
pub fn f_sf(f: f64, d1: f64, d2: f64) -> f64 {
    if d1 <= 0.0 || d2 <= 0.0 || f.is_nan() {
        return f64::NAN;
    }
    if f <= 0.0 {
        return 1.0;
    }
    betai(0.5 * d2, 0.5 * d1, d2 / (d2 + d1 * f))
}

/// F CDF, `P(F <= f)`.
#[must_use]
pub fn f_cdf(f: f64, d1: f64, d2: f64) -> f64 {
    if d1 <= 0.0 || d2 <= 0.0 || f.is_nan() {
        return f64::NAN;
    }
    if f <= 0.0 {
        return 0.0;
    }
    betai(0.5 * d1, 0.5 * d2, d1 * f / (d1 * f + d2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, rel: f64) -> bool {
        if a == b {
            return true;
        }
        (a - b).abs() <= rel * b.abs().max(1e-300)
    }

    #[test]
    fn normal_round_trips_through_its_own_quantile() {
        for &p in &[1e-12, 1e-6, 0.001, 0.025, 0.1, 0.5, 0.9, 0.975, 0.999999] {
            let z = normal_inv(p);
            assert!(close(normal_cdf(z), p, 1e-14), "p = {p}, z = {z}");
        }
        assert!(close(normal_cdf(0.0), 0.5, 1e-16));
        assert!(close(normal_inv(0.975), 1.959_963_984_540_054, 1e-13));
    }

    #[test]
    fn tails_are_directed_not_subtracted() {
        // 1 - cdf would return exactly 0 here; the survivor function must not.
        let sf = normal_sf(38.0);
        assert!(sf > 0.0 && sf < 1e-300, "sf = {sf}");
        let t = t_sf(60.0, 4.0);
        assert!(t > 0.0 && t < 1e-6, "t = {t}");
    }

    #[test]
    fn t_and_f_and_chi2_agree_with_their_definitions() {
        // t with 1 df is Cauchy: P(T > 1) = 0.25 exactly.
        assert!(close(t_sf(1.0, 1.0), 0.25, 1e-14));
        // F(1, d) = t(d)^2.
        for &df in &[3.0f64, 10.0, 40.0] {
            for &t in &[0.5f64, 1.0, 2.5] {
                assert!(
                    close(f_sf(t * t, 1.0, df), t_tail2(t, df), 1e-13),
                    "df = {df}, t = {t}"
                );
            }
        }
        // chi2 with 2 df is exponential: P(X > x) = exp(-x/2).
        for &x in &[0.5f64, 2.0, 10.0, 50.0] {
            assert!(
                close(chi2_sf(x, 2.0), crate::math::exp(-0.5 * x), 1e-14),
                "x = {x}"
            );
        }
    }

    #[test]
    fn t_inv_inverts_t_cdf() {
        for &df in &[1.0f64, 2.0, 5.0, 30.0, 1000.0] {
            for &p in &[1e-8, 0.001, 0.025, 0.3, 0.5, 0.7, 0.975, 0.9999] {
                let t = t_inv(p, df);
                assert!(close(t_cdf(t, df), p, 1e-12), "df = {df}, p = {p}, t = {t}");
            }
        }
    }
}
