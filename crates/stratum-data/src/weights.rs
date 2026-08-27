//! Weights — `04` §5.5, and why the *kind* is in the type.
//!
//! Four weight kinds give four different answers from the same numbers, and the
//! difference is not a scale factor: `fweight` treats `Σw` as the sample size
//! for degrees of freedom while `aweight` treats `n` as the sample size, so the
//! same data under the two spellings differ in every standard error. A
//! statistics author handed `Option<&[f64]>` cannot get that right, and the ones
//! who get it wrong get it *silently* wrong. So this module hands out
//! [`EvaluatedWeights`], which carries its [`WeightKind`] and cannot be
//! destructured into a bare slice without naming it.
//!
//! # The order of operations, which is load-bearing
//!
//! `04` §5.5: *"observations with a missing or non-positive weight are removed
//! from the sample before normalisation."* Two consequences the implementation
//! has to get right and one the design doc leaves implicit:
//!
//! 1. Removal happens **before** the `aweight` rescale, so `Σw* = n` counts the
//!    surviving observations, not the ones that were dropped.
//! 2. A **negative** weight is an error, not a silent removal. Reading
//!    "non-positive are removed" as covering negatives would make
//!    `summarize x [aw = -1]` quietly summarise nothing at all, which is the
//!    failure mode `04` §5.5's own `w > 0` validation column exists to stop.
//!    Zero is the removal case; negative is [`WeightError::Negative`].
//! 3. `iweight` validates nothing (`04` §5.5 table: "any"), so a negative
//!    importance weight passes — that is Stata's behaviour and the reason
//!    `iweight` exists.

use std::sync::Arc;

use stratum_core::is_missing;

use crate::bitset::BitSet;
use crate::sample::Sample;

/// Which of Stata's four weight spellings a command was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WeightKind {
    /// `[fweight = w]` — each observation stands for `w` identical ones.
    Frequency,
    /// `[aweight = w]` — inversely proportional to the variance of the cell.
    Analytic,
    /// `[pweight = w]` — sampling weights; forces a robust VCE.
    Probability,
    /// `[iweight = w]` — importance weights, validated by nothing.
    Importance,
}

impl WeightKind {
    /// The Stata spelling, for diagnostics and for `e(wtype)`.
    #[must_use]
    pub fn spelling(self) -> &'static str {
        match self {
            WeightKind::Frequency => "fweight",
            WeightKind::Analytic => "aweight",
            WeightKind::Probability => "pweight",
            WeightKind::Importance => "iweight",
        }
    }
}

/// The unevaluated declaration: `[fweight = pop]` before `pop` is read.
///
/// `expr` is an `Arc<str>` for the same reason a variable name is: a command
/// clones its input repeatedly and none of those clones should allocate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeightSpec {
    /// Which spelling.
    pub kind: WeightKind,
    /// The expression as written, kept verbatim for `e(wexp)`.
    pub expr: Arc<str>,
}

impl WeightSpec {
    /// A spec from its two parts.
    #[must_use]
    pub fn new(kind: WeightKind, expr: &str) -> Self {
        Self {
            kind,
            expr: Arc::from(expr),
        }
    }
}

/// Weights evaluated against a sample, with the sample already narrowed to the
/// observations that survived.
#[derive(Clone, Debug, PartialEq)]
pub struct EvaluatedWeights {
    /// Which spelling produced these. **Branch on this, never on
    /// `Option::is_some`** (`04` §5.5).
    pub kind: WeightKind,
    /// Already restricted to the sample, in sample order. `len() == sample.len()`
    /// of the *returned* sample.
    pub w: Vec<f64>,
    /// `Σw` over the surviving sample, before any normalisation.
    pub sum_raw: f64,
    /// Effective N a command should report: `Σw` for `fweight`/`iweight`, the
    /// surviving observation count for `aweight`/`pweight`.
    pub eff_n: f64,
    /// True for `pweight`; commands must force a robust/sandwich VCE.
    pub requires_robust_vce: bool,
    /// **`04` §5.5's open question, answered.** `f64` stops counting exactly at
    /// `2^53`, so a frequency-weighted `_N` above that is silently wrong. For
    /// [`WeightKind::Frequency`] — the only kind whose weights are integers by
    /// definition — the sum is *also* accumulated in `u128`, and
    /// [`WeightError::FrequencySumTooLarge`] is raised above `2^63`. `None` for
    /// every other kind, where the sum is a real number and no exact
    /// counterpart exists.
    pub sum_exact: Option<u128>,
}

/// Why a weight expression could not be used.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum WeightError {
    /// `[fweight = 1.5]`. Stata: "may not use noninteger frequency weights".
    #[error("may not use noninteger frequency weights ({0} is not an integer)")]
    NonInteger(f64),
    /// A negative weight under a kind that forbids one.
    #[error("negative weights encountered ({1} at observation {0})")]
    Negative(u64, f64),
    /// `Σ fweight` past `2^63`; see [`EvaluatedWeights::sum_exact`].
    #[error("frequency weights sum to {0}, which cannot be counted exactly")]
    FrequencySumTooLarge(u128),
    /// The weight vector does not line up with the sample it was gathered over.
    #[error("{got} weight values for a sample of {want} observations")]
    LengthMismatch {
        /// What was passed.
        got: usize,
        /// What the sample holds.
        want: u64,
    },
    /// Every observation had a missing or zero weight.
    #[error("no observations left after dropping zero and missing weights")]
    NoObservations,
}

impl WeightError {
    /// Stata's return code for this failure.
    ///
    /// `04` §5.5 calls the non-integer case "`r(401)`-class"; 402 is the
    /// negative-weight code that pairs with it, and 2000 is Stata's own "no
    /// observations".
    #[must_use]
    pub fn rc(self) -> u16 {
        match self {
            WeightError::NonInteger(_) | WeightError::FrequencySumTooLarge(_) => 401,
            WeightError::Negative(..) => 402,
            WeightError::LengthMismatch { .. } => 198,
            WeightError::NoObservations => 2000,
        }
    }
}

impl EvaluatedWeights {
    /// Validate, narrow and normalise.
    ///
    /// `values` is the weight expression **already gathered over `sample`**, in
    /// sample order — exactly what
    /// [`Column::gather_f64`](crate::column::Column::gather_f64) writes — so
    /// this function never touches the frame and never re-evaluates anything.
    ///
    /// Returns the narrowed sample alongside the weights, because the two must
    /// not be able to drift apart: an `aweight` normalised over `n` while its
    /// sample still holds the zero-weight rows is a wrong `Σw*`.
    ///
    /// # Errors
    ///
    /// [`WeightError`].
    pub fn build(
        spec: &WeightSpec,
        values: &[f64],
        sample: &Sample,
    ) -> Result<(Sample, EvaluatedWeights), WeightError> {
        if values.len() as u64 != sample.len() {
            return Err(WeightError::LengthMismatch {
                got: values.len(),
                want: sample.len(),
            });
        }
        let kind = spec.kind;

        // Pass 1 — validate, and do it against PHYSICAL observation numbers so
        // the diagnostic names the row the user can go and look at. Walking
        // `runs()` costs one iterator step per run, not per observation.
        if kind != WeightKind::Importance {
            let mut i = 0usize;
            for run in sample.runs() {
                for obs in run.start..run.start + run.len {
                    let v = values[i];
                    i += 1;
                    if is_missing(v) {
                        continue;
                    }
                    if v < 0.0 {
                        return Err(WeightError::Negative(obs, v));
                    }
                    if kind == WeightKind::Frequency && v.fract() != 0.0 {
                        return Err(WeightError::NonInteger(v));
                    }
                }
            }
        }

        // Pass 2 — narrow. A missing or zero weight leaves the sample entirely;
        // `04` §5.5's "removed before normalisation".
        let mut keep = BitSet::new(sample.nobs());
        let mut w = Vec::with_capacity(values.len());
        let mut sum_raw = 0.0f64;
        let mut sum_exact: Option<u128> = (kind == WeightKind::Frequency).then_some(0u128);
        let mut i = 0usize;
        for run in sample.runs() {
            for obs in run.start..run.start + run.len {
                let v = values[i];
                i += 1;
                if is_missing(v) || v == 0.0 {
                    continue;
                }
                keep.set(obs, true);
                w.push(v);
                sum_raw += v;
                if let Some(acc) = sum_exact.as_mut() {
                    // Validated non-negative and integral above, so the cast is
                    // exact for every value `f64` can hold as an integer.
                    *acc = acc.saturating_add(v as u128);
                }
            }
        }
        if w.is_empty() {
            return Err(WeightError::NoObservations);
        }
        if let Some(acc) = sum_exact {
            if acc > 1u128 << 63 {
                return Err(WeightError::FrequencySumTooLarge(acc));
            }
        }

        let n = w.len() as f64;
        // `aweight` is the only kind that rescales: `w*ᵢ = wᵢ · n / Σw`, so
        // `Σw* == n` (`04` §5.5).
        if kind == WeightKind::Analytic {
            let scale = n / sum_raw;
            for x in &mut w {
                *x *= scale;
            }
        }

        let eff_n = match kind {
            WeightKind::Frequency | WeightKind::Importance => sum_raw,
            WeightKind::Analytic | WeightKind::Probability => n,
        };

        // Nothing was dropped ⇒ hand the ORIGINAL sample back. `Sample::All` and
        // `Sample::Range` are what let a kernel take contiguous slices (`04`
        // §5.1); turning an unrestricted 10 M-row `summarize [fw=n]` into a
        // `Mask` because weights were present would make it gather-bound for no
        // reason at all.
        let narrowed = if w.len() as u64 == sample.len() {
            sample.clone()
        } else {
            Sample::mask(sample.nobs(), keep)
        };

        Ok((
            narrowed,
            EvaluatedWeights {
                kind,
                w,
                sum_raw,
                eff_n,
                requires_robust_vce: kind == WeightKind::Probability,
                sum_exact,
            },
        ))
    }

    /// The weights as a slice. Named rather than `Deref` so a caller cannot
    /// reach the numbers without having gone past [`Self::kind`].
    #[must_use]
    pub fn values(&self) -> &[f64] {
        &self.w
    }

    /// How many observations survived.
    #[must_use]
    pub fn len(&self) -> usize {
        self.w.len()
    }

    /// True when nothing survived. Unreachable through [`Self::build`], which
    /// errors instead; present because `len` without `is_empty` is a lint.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.w.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_core::missing::{missing_f64, SYSMISS};

    fn spec(kind: WeightKind) -> WeightSpec {
        WeightSpec::new(kind, "w")
    }

    #[test]
    fn a_missing_or_zero_weight_leaves_the_sample_before_normalisation() {
        let s = Sample::all(4);
        let v = [2.0, SYSMISS, 0.0, 6.0];
        let (narrowed, ew) = EvaluatedWeights::build(&spec(WeightKind::Analytic), &v, &s)
            .expect("two positive weights survive");
        assert_eq!(narrowed.len(), 2);
        assert!(narrowed.contains(0) && narrowed.contains(3));
        // Σw* == n over the SURVIVORS: 2/8*2 + 6/8*2 = 2.
        let sum: f64 = ew.values().iter().sum();
        assert!((sum - 2.0).abs() < 1e-12, "Σw* = {sum}");
        assert_eq!(ew.eff_n, 2.0);
        assert_eq!(ew.sum_raw, 8.0);
    }

    #[test]
    fn the_four_kinds_report_four_different_effective_ns() {
        let s = Sample::all(3);
        let v = [1.0, 2.0, 3.0];
        for (kind, want, robust) in [
            (WeightKind::Frequency, 6.0, false),
            (WeightKind::Analytic, 3.0, false),
            (WeightKind::Probability, 3.0, true),
            (WeightKind::Importance, 6.0, false),
        ] {
            let (_, ew) = EvaluatedWeights::build(&spec(kind), &v, &s).expect("valid");
            assert_eq!(ew.eff_n, want, "{}", kind.spelling());
            assert_eq!(ew.requires_robust_vce, robust, "{}", kind.spelling());
        }
    }

    #[test]
    fn a_noninteger_frequency_weight_is_rc_401() {
        let s = Sample::all(2);
        let e = EvaluatedWeights::build(&spec(WeightKind::Frequency), &[1.0, 1.5], &s)
            .expect_err("1.5 is not a count");
        assert_eq!(e, WeightError::NonInteger(1.5));
        assert_eq!(e.rc(), 401);
        // The same values are fine as analytic weights.
        assert!(EvaluatedWeights::build(&spec(WeightKind::Analytic), &[1.0, 1.5], &s).is_ok());
    }

    #[test]
    fn a_negative_weight_is_an_error_and_not_a_silent_removal() {
        let s = Sample::all(3);
        for kind in [
            WeightKind::Frequency,
            WeightKind::Analytic,
            WeightKind::Probability,
        ] {
            let e = EvaluatedWeights::build(&spec(kind), &[1.0, -1.0, 1.0], &s)
                .expect_err("negative is rejected");
            assert_eq!(e, WeightError::Negative(1, -1.0));
            assert_eq!(e.rc(), 402);
        }
        // `iweight` validates nothing.
        let (_, ew) = EvaluatedWeights::build(&spec(WeightKind::Importance), &[1.0, -1.0, 1.0], &s)
            .expect("iweight takes anything");
        assert_eq!(ew.sum_raw, 1.0);
    }

    #[test]
    fn extended_missing_weights_are_dropped_like_plain_missing() {
        let s = Sample::all(3);
        let v = [missing_f64(1), 4.0, missing_f64(26)];
        let (narrowed, ew) =
            EvaluatedWeights::build(&spec(WeightKind::Frequency), &v, &s).expect("one survives");
        assert_eq!(narrowed.len(), 1);
        assert_eq!(ew.sum_exact, Some(4));
    }

    #[test]
    fn frequency_weights_carry_an_exact_sum_and_refuse_an_uncountable_one() {
        let s = Sample::all(2);
        let big = 9.0e18_f64.trunc();
        let e = EvaluatedWeights::build(&spec(WeightKind::Frequency), &[big, big], &s)
            .expect_err("1.8e19 cannot be counted");
        assert!(matches!(e, WeightError::FrequencySumTooLarge(_)));
        assert_eq!(e.rc(), 401);
    }

    #[test]
    fn weights_are_gathered_in_sample_order_not_frame_order() {
        // `in 3/4` on a 5-observation frame: the two values line up with
        // observations 2 and 3, and the narrowed mask has to say so.
        let s = Sample::range(5, 2, 4);
        let (narrowed, _) =
            EvaluatedWeights::build(&spec(WeightKind::Frequency), &[3.0, 0.0], &s).expect("valid");
        assert_eq!(narrowed.len(), 1);
        assert!(narrowed.contains(2));
        assert!(!narrowed.contains(3));
    }

    #[test]
    fn a_length_mismatch_is_caught_rather_than_read_off_the_end() {
        let s = Sample::all(3);
        let e = EvaluatedWeights::build(&spec(WeightKind::Frequency), &[1.0], &s)
            .expect_err("one value, three observations");
        assert_eq!(e, WeightError::LengthMismatch { got: 1, want: 3 });
    }

    #[test]
    fn everything_dropped_is_an_error_and_not_an_empty_success() {
        let s = Sample::all(2);
        let e = EvaluatedWeights::build(&spec(WeightKind::Analytic), &[0.0, SYSMISS], &s)
            .expect_err("nothing survives");
        assert_eq!(e, WeightError::NoObservations);
        assert_eq!(e.rc(), 2000);
    }
}
