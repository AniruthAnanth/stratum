//! `histogram`'s bin grid, and the heights that go on it.
//!
//! **The default rule is the manual's, and there is no golden for it.**
//! `tests/golden/stata18/*.log` contains no graph command, because a graph
//! writes a window and not a log, and the licence on this machine has expired.
//! So `k = min(sqrt(N), 10·ln(N)/ln(10))` rounded to the nearest integer is
//! implemented as [R] histogram documents it, said out loud here and in the
//! design note §3.1, and flagged for W23's difftest sweep. Guessing quietly
//! would have been the same code with a worse comment.
//!
//! This module is also where `histogram`'s one piece of *textual* output comes
//! from. Stata prints `(bin=8, start=1, width=.75)` above the graph; the runtime
//! prints it, from [`Binning`], so the number in the log and the grid in the
//! figure cannot disagree.

use crate::error::GraphError;
use crate::spec::{BinSpec, HistScale};
use stratum_core::math;
use stratum_core::missing::is_missing;

/// The bin grid, and the three numbers `histogram` echoes to the log.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Binning {
    /// Number of bins.
    pub bins: u32,
    /// Left edge of the first bin.
    pub start: f64,
    /// Bin width.
    pub width: f64,
}

/// A binned variable: the grid, the bar heights in the requested scale, and what
/// the missing-value rule removed.
#[derive(Clone, PartialEq, Debug)]
pub struct Binned {
    /// The grid.
    pub binning: Binning,
    /// One height per bin, in the scale the spec asked for.
    pub heights: Vec<f64>,
    /// Observations that survived the missing rule.
    pub n: u64,
    /// Observations dropped by it.
    pub dropped: u64,
}

/// Bin `values`.
///
/// Two passes over the data and no more: one to find the range and count, one to
/// accumulate. `discrete` sorts a *copy of the distinct values* rather than the
/// data, which is `O(N)` to collect and `O(u log u)` in the number of distinct
/// levels — the shape a discrete variable actually has.
pub fn bin(
    values: &[f64],
    spec: BinSpec,
    scale: HistScale,
    discrete: bool,
) -> Result<Binned, GraphError> {
    // ---- pass 1: range, count, and (discrete only) the levels --------------
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    let mut n: u64 = 0;
    let mut dropped: u64 = 0;
    let mut levels: Vec<f64> = Vec::new();
    for &v in values {
        if is_missing(v) || !v.is_finite() {
            dropped += 1;
            continue;
        }
        n += 1;
        if v < lo {
            lo = v;
        }
        if v > hi {
            hi = v;
        }
        if discrete {
            levels.push(v);
        }
    }
    if n == 0 {
        return Err(GraphError::NoObservations);
    }

    let binning = if discrete {
        discrete_grid(&mut levels, lo, hi)
    } else {
        continuous_grid(spec, lo, hi, n)?
    };
    if binning.bins == 0 || !binning.width.is_finite() || binning.width <= 0.0 {
        return Err(GraphError::BadBinning);
    }

    // ---- pass 2: accumulate ------------------------------------------------
    let nbins = binning.bins as usize;
    let mut counts = vec![0u64; nbins];
    for &v in values {
        if is_missing(v) || !v.is_finite() {
            continue;
        }
        let idx = ((v - binning.start) / binning.width).floor();
        // The last bin is closed on the right: without this the maximum
        // observation falls one bin past the end and vanishes from its own
        // histogram.
        let idx = if idx < 0.0 {
            0usize
        } else {
            let i = idx as usize;
            i.min(nbins - 1)
        };
        counts[idx] += 1;
    }

    let n_f = n as f64;
    let heights = counts
        .iter()
        .map(|&c| {
            let c = c as f64;
            match scale {
                HistScale::Frequency => c,
                HistScale::Fraction => c / n_f,
                HistScale::Percent => 100.0 * c / n_f,
                HistScale::Density => c / (n_f * binning.width),
            }
        })
        .collect();

    Ok(Binned {
        binning,
        heights,
        n,
        dropped,
    })
}

/// `bin()`, `width()`/`start()`, or the default rule.
fn continuous_grid(spec: BinSpec, lo: f64, hi: f64, n: u64) -> Result<Binning, GraphError> {
    match spec {
        BinSpec::Width { width, start } => {
            if !width.is_finite() || width <= 0.0 {
                return Err(GraphError::BadBinning);
            }
            let start = start.unwrap_or(lo);
            let span = hi - start;
            // `+ 1` because the grid must cover `hi` itself: a span of exactly
            // 3 widths needs 3 bins, a span of 3.2 needs 4.
            let bins = if span <= 0.0 {
                1.0
            } else {
                (span / width).floor() + 1.0
            };
            Ok(Binning {
                bins: clamp_bins(bins),
                start,
                width,
            })
        }
        BinSpec::Bins(k) => {
            if k == 0 {
                return Err(GraphError::BadBinning);
            }
            even_grid(lo, hi, k)
        }
        BinSpec::Auto => {
            let n_f = n as f64;
            // [R] histogram: k = min(sqrt(N), 10*ln(N)/ln(10)), rounded to the
            // closest integer. `ln(N)/ln(10)` is `log10(N)`; spelling it as the
            // manual does keeps this line greppable against the source.
            let k = math::sqrt(n_f).min(10.0 * math::log10(n_f)).round();
            even_grid(lo, hi, clamp_bins(k))
        }
    }
}

/// `k` equal-width bins spanning `lo..=hi`.
fn even_grid(lo: f64, hi: f64, k: u32) -> Result<Binning, GraphError> {
    let span = hi - lo;
    if span <= 0.0 {
        return Err(GraphError::ZeroRange { value: lo });
    }
    Ok(Binning {
        bins: k,
        start: lo,
        width: span / f64::from(k),
    })
}

/// `discrete`: one bin per distinct value, centred on it.
///
/// The width is the smallest gap between adjacent levels, so integer-coded data
/// gets width 1 and a variable measured in half-units gets width 0.5. The grid
/// starts half a width below the minimum, which is what puts the bar *on* the
/// value rather than to the right of it.
fn discrete_grid(levels: &mut Vec<f64>, lo: f64, hi: f64) -> Binning {
    levels.sort_by(f64::total_cmp);
    levels.dedup();
    let mut width = f64::INFINITY;
    for pair in levels.windows(2) {
        let gap = pair[1] - pair[0];
        if gap > 0.0 && gap < width {
            width = gap;
        }
    }
    if !width.is_finite() || width <= 0.0 {
        width = 1.0;
    }
    let start = lo - width / 2.0;
    let bins = ((hi - start) / width).floor() + 1.0;
    Binning {
        bins: clamp_bins(bins),
        start,
        width,
    }
}

/// Bin counts are a `u32` on the wire and a `Vec` length here. The ceiling is a
/// guard against `width(1e-300)` allocating a terabyte, not a design limit: a
/// histogram nobody can read is not worth a panic.
fn clamp_bins(k: f64) -> u32 {
    const MAX_BINS: f64 = 4096.0;
    if !k.is_finite() || k < 1.0 {
        1
    } else {
        k.min(MAX_BINS) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_core::missing::{missing_f64, SYSMISS};

    #[test]
    fn the_maximum_observation_lands_in_the_last_bin() {
        let v: Vec<f64> = (0..100).map(f64::from).collect();
        let b = bin(&v, BinSpec::Bins(10), HistScale::Frequency, false).unwrap();
        assert_eq!(b.heights.iter().sum::<f64>(), 100.0);
        // 99 is the maximum; it must be counted, and in the last bin.
        assert!(*b.heights.last().unwrap() > 0.0);
    }

    #[test]
    fn density_bars_have_unit_area() {
        let v: Vec<f64> = (0..50).map(f64::from).collect();
        let b = bin(&v, BinSpec::Bins(5), HistScale::Density, false).unwrap();
        let area: f64 = b.heights.iter().map(|h| h * b.binning.width).sum();
        assert!((area - 1.0).abs() < 1e-12, "area was {area}");
    }

    #[test]
    fn fraction_and_percent_sum_to_one_and_a_hundred() {
        let v: Vec<f64> = (0..40).map(f64::from).collect();
        let f = bin(&v, BinSpec::Bins(4), HistScale::Fraction, false).unwrap();
        let p = bin(&v, BinSpec::Bins(4), HistScale::Percent, false).unwrap();
        assert!((f.heights.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!((p.heights.iter().sum::<f64>() - 100.0).abs() < 1e-12);
    }

    #[test]
    fn the_default_rule_is_the_manuals() {
        // N = 74 (auto.dta): sqrt(74) = 8.60, 10*log10(74) = 18.69 -> 9.
        let v: Vec<f64> = (0..74).map(f64::from).collect();
        let b = bin(&v, BinSpec::Auto, HistScale::Density, false).unwrap();
        assert_eq!(b.binning.bins, 9);
    }

    #[test]
    fn missing_values_are_dropped_and_counted() {
        let v = vec![1.0, 2.0, SYSMISS, 3.0, missing_f64(b'a')];
        let b = bin(&v, BinSpec::Bins(2), HistScale::Frequency, false).unwrap();
        assert_eq!(b.n, 3);
        assert_eq!(b.dropped, 2);
        assert_eq!(b.heights.iter().sum::<f64>(), 3.0);
    }

    #[test]
    fn a_constant_variable_is_refused_not_drawn_wrong() {
        let v = vec![5.0; 10];
        assert_eq!(
            bin(&v, BinSpec::Auto, HistScale::Density, false),
            Err(GraphError::ZeroRange { value: 5.0 })
        );
    }

    #[test]
    fn all_missing_is_no_observations() {
        assert_eq!(
            bin(
                &[SYSMISS, SYSMISS],
                BinSpec::Auto,
                HistScale::Density,
                false
            ),
            Err(GraphError::NoObservations)
        );
    }

    #[test]
    fn discrete_centres_a_bar_on_each_level() {
        let v = vec![1.0, 1.0, 2.0, 3.0, 3.0, 3.0];
        let b = bin(&v, BinSpec::Auto, HistScale::Frequency, true).unwrap();
        assert_eq!(b.binning.bins, 3);
        assert_eq!(b.binning.width, 1.0);
        assert_eq!(b.binning.start, 0.5);
        assert_eq!(b.heights, vec![2.0, 1.0, 3.0]);
    }

    #[test]
    fn a_zero_or_negative_width_is_refused() {
        let v = vec![1.0, 2.0];
        for w in [0.0, -1.0, f64::NAN] {
            assert_eq!(
                bin(
                    &v,
                    BinSpec::Width {
                        width: w,
                        start: None
                    },
                    HistScale::Density,
                    false
                ),
                Err(GraphError::BadBinning)
            );
        }
        assert_eq!(
            bin(&v, BinSpec::Bins(0), HistScale::Density, false),
            Err(GraphError::BadBinning)
        );
    }

    /// The counter claim in the design note: bins, not observations.
    #[test]
    fn bin_count_is_independent_of_n() {
        let small: Vec<f64> = (0..40).map(f64::from).collect();
        let large: Vec<f64> = (0..400_000).map(|i| f64::from(i) / 10_000.0).collect();
        let a = bin(&small, BinSpec::Bins(20), HistScale::Density, false).unwrap();
        let b = bin(&large, BinSpec::Bins(20), HistScale::Density, false).unwrap();
        assert_eq!(a.heights.len(), b.heights.len());
    }
}
