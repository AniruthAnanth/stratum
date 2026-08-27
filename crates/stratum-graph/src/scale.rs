//! Data space → point space, and where the ticks go.
//!
//! Linear only in pass 1. `log`/`xscale(range())` are named deferrals in the
//! design note §2, not oversights.
//!
//! Every transcendental here goes through [`stratum_core::math`], which is
//! `libm`. ARCHITECTURE §8.11 bans `std`'s f64 transcendentals across the whole
//! workspace because they forward to the host libm and differ bitwise between
//! glibc, musl, Apple's and MSVC's — and a tick that lands on 0.7000000000000001
//! on one platform and 0.7 on another is a byte-different SVG, which is a
//! byte-different asset, which fails ADR-013's determinism gate.

use stratum_core::math;
use stratum_core::missing::is_missing;

/// A closed interval in data space.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Domain {
    /// Lower bound.
    pub lo: f64,
    /// Upper bound.
    pub hi: f64,
}

impl Domain {
    /// The min and max of the non-missing, finite values, or `None` when there
    /// are none.
    ///
    /// This is **pass 1** over a series. Nothing else in the crate walks the
    /// data to find a range; the counter in `RenderCounters::data_passes` is
    /// what holds that claim up.
    #[must_use]
    pub fn of(values: &[f64]) -> Option<Domain> {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for &v in values {
            if is_missing(v) || !v.is_finite() {
                continue;
            }
            if v < lo {
                lo = v;
            }
            if v > hi {
                hi = v;
            }
        }
        if lo > hi {
            None
        } else {
            Some(Domain { lo, hi })
        }
    }

    /// Widen to cover `other`.
    #[must_use]
    pub fn union(self, other: Domain) -> Domain {
        Domain {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }

    /// Include zero — the value axis of a bar chart or a histogram. A bar chart
    /// whose baseline is not zero misstates every ratio a reader takes off it,
    /// and Stata anchors it too.
    #[must_use]
    pub fn including_zero(self) -> Domain {
        Domain {
            lo: self.lo.min(0.0),
            hi: self.hi.max(0.0),
        }
    }

    /// Pad by a fraction of the span on each side, so a point at the extreme
    /// does not sit half-outside the plot region.
    ///
    /// A degenerate domain (one distinct value) is widened to ±0.5 rather than
    /// left at zero width: a `scatter` of a constant is a legitimate figure and
    /// the point belongs in the middle of it.
    #[must_use]
    pub fn padded(self, frac: f64) -> Domain {
        let span = self.hi - self.lo;
        if span <= 0.0 {
            let unit = if self.lo == 0.0 {
                0.5
            } else {
                self.lo.abs() * 0.5
            };
            return Domain {
                lo: self.lo - unit,
                hi: self.hi + unit,
            };
        }
        Domain {
            lo: self.lo - span * frac,
            hi: self.hi + span * frac,
        }
    }

    /// Width in data units.
    #[must_use]
    pub fn span(self) -> f64 {
        self.hi - self.lo
    }
}

/// An affine map from a [`Domain`] onto a point range.
///
/// `r0`/`r1` are the *screen* ends, so the y scale is built with `r0` at the
/// bottom of the plot region and `r1` at the top and the inversion needs no
/// special case anywhere else.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Scale {
    domain: Domain,
    r0: f64,
    r1: f64,
    /// Precomputed, because this multiplies once per plotted point and a
    /// division per point on a 10 M-row scatter is 10 M divisions.
    factor: f64,
}

impl Scale {
    /// Build the map. A zero-width domain maps everything to the range midpoint.
    #[must_use]
    pub fn new(domain: Domain, r0: f64, r1: f64) -> Scale {
        let span = domain.span();
        let factor = if span > 0.0 { (r1 - r0) / span } else { 0.0 };
        Scale {
            domain,
            r0,
            r1,
            factor,
        }
    }

    /// Data value → point.
    #[must_use]
    pub fn map(&self, v: f64) -> f64 {
        if self.factor == 0.0 {
            (self.r0 + self.r1) * 0.5
        } else {
            self.r0 + (v - self.domain.lo) * self.factor
        }
    }

    /// The interval this scale covers.
    #[must_use]
    pub fn domain(&self) -> Domain {
        self.domain
    }
}

/// The largest number of major ticks an axis will ever carry. A guard on the
/// loop below, not a design parameter: without it a domain of `1e308` and a step
/// that underflows to zero is an infinite loop in a render.
const MAX_TICKS: usize = 32;

/// Major tick positions for `domain`, aiming for about `target` of them.
///
/// The classic *nice number* rule: a step of 1, 2, 2.5 or 5 times a power of
/// ten. 2.5 is in the set because it is the only way to get a sane step out of a
/// span like 12 (steps of 2.5 give 5 ticks; 2 gives 7 and 5 gives 3).
///
/// Returned positions are exact multiples of the step, computed as `k * step`
/// rather than by repeated addition, so the hundredth tick has no accumulated
/// error and its label is the one a reader expects.
#[must_use]
pub fn ticks(domain: Domain, target: usize) -> Vec<f64> {
    let span = domain.span();
    if !span.is_finite() || span <= 0.0 || target == 0 {
        return vec![domain.lo];
    }
    let step = nice_step(span / target as f64);
    if !step.is_finite() || step <= 0.0 {
        return vec![domain.lo];
    }

    let first = (domain.lo / step).ceil();
    let mut out = Vec::with_capacity(target + 2);
    let mut k = first;
    while out.len() < MAX_TICKS {
        let t = k * step;
        // A tolerance of a thousandth of a step: without it the last tick of a
        // 0..1 axis with a 0.1 step is dropped, because 10*0.1 is 1.0000000000000002.
        if t > domain.hi + step * 1e-3 {
            break;
        }
        // `-0.0 * anything` is `-0.0`, and `fmt_g` spells that `-0`. A reader
        // seeing "-0" on an axis assumes the figure is broken.
        out.push(if t == 0.0 { 0.0 } else { t });
        k += 1.0;
    }
    if out.is_empty() {
        out.push(domain.lo);
    }
    out
}

/// Round `raw` up to the next 1/2/2.5/5 × 10^k.
fn nice_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 0.0;
    }
    let mag = math::powf(10.0, math::log10(raw).floor());
    let norm = raw / mag;
    let mult = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 2.5 {
        2.5
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    };
    mult * mag
}

/// An axis label: Stata's `%9.0g`, trimmed.
///
/// C12 and ARCHITECTURE §8.7 make this mandatory rather than tasteful. An axis
/// label is a user-visible number, `stratum_core::fmt` is the only place allowed
/// to produce one, and `%9.0g` is the format Stata itself labels axes with.
#[must_use]
pub fn tick_label(v: f64) -> String {
    stratum_core::fmt::fmt_g(v, 9).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_core::missing::{missing_f64, SYSMISS};

    #[test]
    fn domain_skips_missing_and_extended_missing() {
        let vs = [1.0, SYSMISS, 3.0, missing_f64(b'a'), 2.0];
        assert_eq!(Domain::of(&vs), Some(Domain { lo: 1.0, hi: 3.0 }));
    }

    #[test]
    fn domain_of_all_missing_is_none() {
        assert_eq!(Domain::of(&[SYSMISS, missing_f64(b'z')]), None);
    }

    #[test]
    fn ticks_are_round_numbers() {
        let labels: Vec<String> = ticks(Domain { lo: 0.0, hi: 1.0 }, 5)
            .iter()
            .map(|&v| tick_label(v))
            .collect();
        assert_eq!(labels, ["0", ".2", ".4", ".6", ".8", "1"]);
    }

    #[test]
    fn the_last_tick_is_not_lost_to_float_noise() {
        // 10 * 0.1 is 1.0000000000000002; a naive `t > hi` drops the "1" label.
        assert!(ticks(Domain { lo: 0.0, hi: 1.0 }, 10).len() >= 10);
    }

    #[test]
    fn never_labels_negative_zero() {
        for t in ticks(Domain { lo: -5.0, hi: 5.0 }, 5) {
            assert_ne!(tick_label(t), "-0");
        }
    }

    #[test]
    fn a_degenerate_domain_terminates() {
        assert_eq!(ticks(Domain { lo: 3.0, hi: 3.0 }, 5), vec![3.0]);
        assert_eq!(
            ticks(
                Domain {
                    lo: 0.0,
                    hi: f64::INFINITY
                },
                5
            ),
            vec![0.0]
        );
    }

    #[test]
    fn a_constant_series_still_gets_a_plot_region() {
        let d = Domain { lo: 7.0, hi: 7.0 }.padded(0.02);
        assert!(d.span() > 0.0);
        let s = Scale::new(Domain { lo: 7.0, hi: 7.0 }, 0.0, 100.0);
        assert_eq!(s.map(7.0), 50.0);
    }

    proptest::proptest! {
        /// The step is rounded UP to the next nice number, so the realised count
        /// can never run away from the target: at most one more than asked for,
        /// because the first tick lands on a multiple of the step rather than on
        /// the domain's lower bound.
        ///
        /// Stated as a property because the failure mode is not "the axis looks
        /// slightly wrong" — it is an axis with thirty labels overprinting each
        /// other, which is what an unbounded `while` over a badly conditioned
        /// span produces.
        #[test]
        fn the_tick_count_never_exceeds_the_target_by_more_than_one(
            lo in -1e12f64..1e12,
            span in 1e-9f64..1e12,
            target in 3usize..12,
        ) {
            let d = Domain { lo, hi: lo + span };
            let t = ticks(d, target);
            proptest::prop_assert!(!t.is_empty(), "an axis with no ticks has no scale");
            proptest::prop_assert!(
                t.len() <= target + 1,
                "{} ticks for a target of {target} over {lo}..{}",
                t.len(),
                d.hi
            );
            // Ascending, and inside the domain but for the float tolerance the
            // last-tick rule needs.
            for w in t.windows(2) {
                proptest::prop_assert!(w[1] > w[0], "ticks must ascend");
            }
        }
    }

    #[test]
    fn scale_is_affine_and_inverts_for_y() {
        let s = Scale::new(Domain { lo: 0.0, hi: 10.0 }, 200.0, 0.0);
        assert_eq!(s.map(0.0), 200.0);
        assert_eq!(s.map(10.0), 0.0);
        assert_eq!(s.map(5.0), 100.0);
    }
}
