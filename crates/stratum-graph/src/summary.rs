//! The aggregates behind `graph box` and `graph bar`.
//!
//! The percentile rule is **Stata's**, not a textbook's, and it is not the one
//! most libraries use. `05-statistics.md` §0 fact F3 established it by
//! experiment against StataMP 18.5:
//!
//! > `j = N·p/100`; if `j` is an integer the percentile is `(x(j)+x(j+1))/2`,
//! > otherwise it is `x(ceil(j))`.
//!
//! Verified there with `x = 1..10 ⇒ p25 = 3, p50 = 5.5, p75 = 8` and
//! `x = 1..5 ⇒ p25 = 2, p50 = 3`. A box plot drawn with the R-7 or the
//! Tukey-hinge rule puts the hinges in visibly different places on small groups,
//! and 05's governing rule applies here too: being *more correct* than Stata is
//! a defect.
//!
//! This module owns no policy about what a group *is* — that is `over()`, and
//! the runtime resolves it. It is handed values and returns numbers.

use stratum_core::missing::is_missing;

/// The five-number summary plus whiskers and outside values — everything
/// `graph box` draws for one group.
#[derive(Clone, PartialEq, Debug)]
pub struct BoxSummary {
    /// 25th percentile — the bottom of the box.
    pub p25: f64,
    /// 50th percentile — the median line.
    pub p50: f64,
    /// 75th percentile — the top of the box.
    pub p75: f64,
    /// Lowest observation at or above `p25 − 1.5·IQR`.
    pub lower_whisker: f64,
    /// Highest observation at or below `p75 + 1.5·IQR`.
    pub upper_whisker: f64,
    /// Observations outside the whiskers, ascending. Stata plots each one.
    pub outside: Vec<f64>,
    /// Observations that survived the missing rule.
    pub n: u64,
    /// Observations the missing rule removed.
    pub dropped: u64,
}

/// Sort a copy of the non-missing values, ascending.
///
/// `total_cmp` rather than `partial_cmp().unwrap()`: NaN is already filtered,
/// but a sort comparator that can panic on data is a sort comparator that will,
/// and `-0.0 < 0.0` under `total_cmp` is harmless at a percentile boundary.
fn sorted_clean(values: &[f64]) -> (Vec<f64>, u64) {
    let mut out: Vec<f64> = Vec::with_capacity(values.len());
    let mut dropped = 0u64;
    for &v in values {
        if is_missing(v) || !v.is_finite() {
            dropped += 1;
        } else {
            out.push(v);
        }
    }
    out.sort_by(f64::total_cmp);
    (out, dropped)
}

/// Stata's percentile of an already-sorted, non-empty slice (fact F3).
#[must_use]
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    debug_assert!(n > 0, "percentile of an empty group");
    if n == 1 {
        return sorted[0];
    }
    let j = n as f64 * p / 100.0;
    let floor = j.floor();
    if (j - floor).abs() < 1e-12 {
        // `j` is an integer: the average of x(j) and x(j+1), 1-based.
        let lo = (floor as usize).clamp(1, n) - 1;
        let hi = (lo + 1).min(n - 1);
        (sorted[lo] + sorted[hi]) / 2.0
    } else {
        let idx = (j.ceil() as usize).clamp(1, n) - 1;
        sorted[idx]
    }
}

/// Summarise one group for `graph box`. `None` when nothing survives the missing
/// rule — an empty `over()` category draws no box rather than a degenerate one.
#[must_use]
pub fn box_summary(values: &[f64]) -> Option<BoxSummary> {
    let (sorted, dropped) = sorted_clean(values);
    if sorted.is_empty() {
        return None;
    }
    let p25 = percentile(&sorted, 25.0);
    let p50 = percentile(&sorted, 50.0);
    let p75 = percentile(&sorted, 75.0);
    let fence = 1.5 * (p75 - p25);
    let (lo_fence, hi_fence) = (p25 - fence, p75 + fence);

    // The whiskers are ADJACENT VALUES — the most extreme observations still
    // inside the fences — not the fences themselves. Drawing the fence is the
    // classic box-plot mistake: it invents a value the data does not contain.
    let mut lower = None;
    let mut upper = None;
    let mut outside = Vec::new();
    for &v in &sorted {
        if v < lo_fence || v > hi_fence {
            outside.push(v);
        } else {
            if lower.is_none() {
                lower = Some(v);
            }
            upper = Some(v);
        }
    }

    Some(BoxSummary {
        p25,
        p50,
        p75,
        lower_whisker: lower.unwrap_or(p25),
        upper_whisker: upper.unwrap_or(p75),
        outside,
        n: sorted.len() as u64,
        dropped,
    })
}

/// `graph bar`'s bar height for one group. `None` when the group is empty after
/// the missing rule — except for `(count)`, which is legitimately zero.
#[must_use]
pub fn bar_stat(values: &[f64], stat: crate::spec::BarStat) -> Option<f64> {
    use crate::spec::BarStat;
    let (sorted, _) = sorted_clean(values);
    let n = sorted.len();
    if stat == BarStat::Count {
        return Some(n as f64);
    }
    if n == 0 {
        return None;
    }
    match stat {
        BarStat::Count => unreachable!("handled above"),
        // Summed in sorted order, which is both deterministic and the
        // better-conditioned order for a mean of same-signed data (ADR-013 asks
        // for the first; numerics asks for the second).
        BarStat::Sum => Some(sorted.iter().sum()),
        BarStat::Mean => Some(sorted.iter().sum::<f64>() / n as f64),
        BarStat::Median => Some(percentile(&sorted, 50.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::BarStat;
    use stratum_core::missing::SYSMISS;

    /// 05 §0 F3, verbatim: the experiment that established the rule.
    #[test]
    fn percentiles_are_statas_not_r7s() {
        let ten: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(percentile(&ten, 10.0), 1.5);
        assert_eq!(percentile(&ten, 25.0), 3.0);
        assert_eq!(percentile(&ten, 50.0), 5.5);
        assert_eq!(percentile(&ten, 75.0), 8.0);
        assert_eq!(percentile(&ten, 90.0), 9.5);

        let five: Vec<f64> = (1..=5).map(f64::from).collect();
        assert_eq!(percentile(&five, 25.0), 2.0);
        assert_eq!(percentile(&five, 50.0), 3.0);
    }

    #[test]
    fn whiskers_are_adjacent_values_not_fences() {
        // 1..9 plus a far outlier. IQR of 1..9,100 -> the outlier is outside and
        // the upper whisker is the largest ordinary value, not p75 + 1.5*IQR.
        let mut v: Vec<f64> = (1..=9).map(f64::from).collect();
        v.push(100.0);
        let s = box_summary(&v).unwrap();
        assert_eq!(s.outside, vec![100.0]);
        assert_eq!(s.upper_whisker, 9.0);
        assert_eq!(s.lower_whisker, 1.0);
    }

    #[test]
    fn no_outliers_means_whiskers_at_the_extremes() {
        let v: Vec<f64> = (1..=9).map(f64::from).collect();
        let s = box_summary(&v).unwrap();
        assert!(s.outside.is_empty());
        assert_eq!((s.lower_whisker, s.upper_whisker), (1.0, 9.0));
    }

    #[test]
    fn missing_values_never_reach_a_quantile() {
        let v = vec![1.0, SYSMISS, 2.0, 3.0];
        let s = box_summary(&v).unwrap();
        assert_eq!(s.n, 3);
        assert_eq!(s.dropped, 1);
        assert_eq!(s.p50, 2.0);
    }

    #[test]
    fn an_empty_group_draws_nothing() {
        assert!(box_summary(&[]).is_none());
        assert!(box_summary(&[SYSMISS]).is_none());
        assert_eq!(bar_stat(&[SYSMISS], BarStat::Mean), None);
        // ...but `count` of an all-missing group is zero, not absent.
        assert_eq!(bar_stat(&[SYSMISS], BarStat::Count), Some(0.0));
    }

    #[test]
    fn bar_statistics() {
        let v = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(bar_stat(&v, BarStat::Mean), Some(2.5));
        assert_eq!(bar_stat(&v, BarStat::Sum), Some(10.0));
        assert_eq!(bar_stat(&v, BarStat::Count), Some(4.0));
        assert_eq!(bar_stat(&v, BarStat::Median), Some(2.5));
    }

    #[test]
    fn a_single_observation_is_a_flat_box() {
        let s = box_summary(&[7.0]).unwrap();
        assert_eq!((s.p25, s.p50, s.p75), (7.0, 7.0, 7.0));
        assert!(s.outside.is_empty());
    }
}
