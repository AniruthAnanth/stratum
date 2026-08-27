//! Per-class numeric tolerances — `docs/design/05-statistics.md` §17.3,
//! transcribed as code.
//!
//! Classic text is byte-exact and never comes here. Everything else is
//! classified by result name and compared under its class's tolerance.
//! Missing values never reach a tolerance at all: `capture::Value::Missing`
//! compares by code upstream, because `.a` and `.b` are one ulp apart and any
//! tolerance would call them equal.
//!
//! # Q4 stands
//!
//! The `e(V)` floor (`Coef`, rel 1e-11) is §17.3's placeholder for
//! well-conditioned designs, **not a measurement**. IMPLEMENTATION_PLAN §6
//! keeps Q4 open until it is re-derived against a live Stata over the
//! ill-conditioned suite; the constant lives in exactly one place so that
//! re-derivation is a one-line change.

/// A tolerance class of §17.3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Integers and order statistics: `==`, no tolerance at all.
    Exact,
    /// Moments — mean, sd, Var, sum, skewness, kurtosis: rel 1e-13.
    Moment,
    /// `e(b)`, `e(V)`, se, t, CI on designs with κ(X) < 1e6: rel 1e-11 (Q4).
    Coef,
    /// p-values and tail probabilities: abs 1e-14 above 1e-6, rel 1e-9 below.
    PValue,
    /// r2, rmse, ll, F and friends — derived from the sweep: rel 1e-12.
    Derived,
    /// chi2 and correlations: rel 1e-13.
    Chi2Corr,
}

impl Class {
    /// Classify a result by its **plain** name (`N`, not `e(N)`).
    #[must_use]
    pub fn of_name(name: &str) -> Class {
        match name {
            // Counts, dof, ranks, tabulate frequencies, order statistics.
            "N" | "N_1" | "N_2" | "N_clust" | "N_over" | "df" | "df_m" | "df_r" | "df_t"
            | "rank" | "level" | "min" | "max" | "sum_w" | "r" | "c" => Class::Exact,
            n if n.starts_with('p')
                && n[1..].bytes().all(|b| b.is_ascii_digit())
                && n.len() > 1 =>
            {
                // Percentiles p1..p99 are order statistics: exact.
                Class::Exact
            }
            "mean" | "sd" | "Var" | "sum" | "skewness" | "kurtosis" | "sd_1" | "sd_2" | "mu_1"
            | "mu_2" => Class::Moment,
            "p" | "p_l" | "p_u" | "p_exact" => Class::PValue,
            "chi2" | "chi2_adj" | "rho" | "corr" => Class::Chi2Corr,
            "r2" | "r2_a" | "rmse" | "mss" | "rss" | "ll" | "ll_0" | "F" | "t" | "se" | "t_1"
            | "t_2" => Class::Derived,
            // Everything else — coefficients, V cells, CI bounds and names we
            // have not tabulated — takes the coefficient floor.
            _ => Class::Coef,
        }
    }

    /// Do `got` and `want` agree under this class?
    ///
    /// The relative scale is `max(|want|, 1)` — §17.3's tolerances are stated
    /// against Stata's value, and a denominator that vanishes near zero would
    /// turn a relative test into an impossible absolute one.
    #[must_use]
    pub fn matches(self, got: f64, want: f64) -> bool {
        if got == want {
            return true;
        }
        let diff = (got - want).abs();
        match self {
            Class::Exact => false, // got == want already failed
            Class::Moment => diff <= 1e-13 * want.abs().max(1.0),
            Class::Coef => diff <= 1e-11 * want.abs().max(1.0),
            Class::Derived => diff <= 1e-12 * want.abs().max(1.0),
            Class::Chi2Corr => diff <= 1e-13 * want.abs().max(1.0),
            Class::PValue => {
                if want.abs() > 1e-6 {
                    diff <= 1e-14
                } else {
                    diff <= 1e-9 * want.abs()
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::excessive_precision)] // the literal transcribes a %21.17g string.
mod tests {
    use super::*;

    #[test]
    fn exact_class_admits_no_drift_at_all() {
        assert_eq!(Class::of_name("N"), Class::Exact);
        assert_eq!(Class::of_name("df_r"), Class::Exact);
        assert_eq!(Class::of_name("p50"), Class::Exact, "percentile");
        assert!(Class::Exact.matches(74.0, 74.0));
        assert!(!Class::Exact.matches(74.0, 74.000_000_000_000_01));
    }

    #[test]
    fn p_alone_is_a_p_value_not_a_percentile() {
        assert_eq!(Class::of_name("p"), Class::PValue);
        assert_eq!(Class::of_name("p_l"), Class::PValue);
        assert!(Class::PValue.matches(0.05 + 4e-15, 0.05));
        assert!(!Class::PValue.matches(0.05 + 2e-14, 0.05));
    }

    #[test]
    fn moments_take_their_relative_band_and_no_more() {
        let want = 21.297_297_297_297_297;
        assert!(Class::Moment.matches(want * (1.0 + 1e-14), want));
        assert!(!Class::Moment.matches(want * (1.0 + 1e-12), want));
    }

    #[test]
    fn unknown_names_fall_to_the_coefficient_floor() {
        assert_eq!(Class::of_name("b"), Class::Coef);
        assert_eq!(Class::of_name("V"), Class::Coef);
        assert_eq!(Class::of_name("something_new"), Class::Coef);
    }
}
