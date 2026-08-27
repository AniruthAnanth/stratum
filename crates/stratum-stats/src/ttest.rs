//! `ttest` — one-sample, two-sample (equal and Satterthwaite) and paired.
//! `05` §12.
//!
//! One degenerate case is load-bearing and is in the golden: when `sd(d) = 0`
//! the paired test prints `t = .` and all three tail probabilities as `.`
//! rather than erroring. Every non-finite statistic here therefore becomes the
//! Stata missing sentinel before it reaches a formatter, never `NaN` or `inf`.

use stratum_core::dist::{t_cdf, t_inv, t_sf};
use stratum_core::fmt::{fmt_f, fmt_fc, fmt_g};
use stratum_core::math::sqrt;
use stratum_core::missing::{is_missing, SYSMISS};
use stratum_data::sample::Sample;
use stratum_proto::result::{Align, Cell, GenericTable, ResultPayload, StyledRun};

use crate::render::ttest_txt;
use crate::stored::{ResultKind, ResultSet};
use crate::{gather, StatResult, StatsError, VarRef};

/// Which of the three tests to run.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TTestKind {
    /// `ttest x == #`.
    OneSample {
        /// The hypothesised mean.
        mu0: f64,
    },
    /// `ttest x, by(g)`.
    TwoSample {
        /// `unequal` — Satterthwaite's approximation.
        unequal: bool,
    },
    /// `ttest x == y`.
    Paired,
}

/// What to run, and on what.
#[derive(Clone, Copy, Debug)]
pub enum TTestSpec<'a> {
    /// `ttest x == #`.
    OneSample {
        /// The variable.
        var: VarRef<'a>,
        /// The hypothesised mean.
        mu0: f64,
    },
    /// `ttest x, by(g)`.
    TwoSample {
        /// The variable.
        var: VarRef<'a>,
        /// The grouping variable; must take exactly two non-missing values.
        by: VarRef<'a>,
        /// `unequal`.
        unequal: bool,
    },
    /// `ttest x == y`.
    Paired {
        /// The first variable.
        x: VarRef<'a>,
        /// The second variable.
        y: VarRef<'a>,
    },
}

/// One row of the `ttest` table.
#[derive(Clone, PartialEq, Debug)]
pub struct TTestGroup {
    /// Row stub: a variable name, a group label, `Combined` or `diff`.
    pub label: String,
    /// Observations.
    pub n: u64,
    /// Mean.
    pub mean: f64,
    /// Standard error of the mean.
    pub se: f64,
    /// `N−1` standard deviation.
    pub sd: f64,
    /// Lower confidence limit.
    pub ci_lo: f64,
    /// Upper confidence limit.
    pub ci_hi: f64,
}

/// A completed t test.
#[derive(Clone, PartialEq, Debug)]
pub struct TTestResult {
    /// Which test.
    pub kind: TTestKind,
    /// The table title, e.g. `Two-sample t test with equal variances`.
    pub title: &'static str,
    /// The variable(s) under test, for the `mean = mean(x)` line.
    pub varnames: Vec<String>,
    /// One row, or two groups plus `Combined`, or two variables.
    pub groups: Vec<TTestGroup>,
    /// The `diff` row; absent for the one-sample test.
    pub diff: Option<TTestGroup>,
    /// The t statistic.
    pub t: f64,
    /// Degrees of freedom.
    pub df: f64,
    /// `Pr(T < t)`.
    pub p_l: f64,
    /// `Pr(|T| > |t|)`.
    pub p: f64,
    /// `Pr(T > t)`.
    pub p_u: f64,
    /// Confidence level.
    pub level: f64,
}

struct Moments {
    n: u64,
    mean: f64,
    sd: f64,
}

fn moments(v: &[f64]) -> Moments {
    let n = v.len();
    if n == 0 {
        return Moments {
            n: 0,
            mean: SYSMISS,
            sd: SYSMISS,
        };
    }
    let mut s = 0.0;
    for &x in v {
        s += x;
    }
    let mean = s / n as f64;
    let mut m2 = 0.0;
    for &x in v {
        let d = x - mean;
        m2 += d * d;
    }
    let sd = if n > 1 {
        sqrt(m2 / (n - 1) as f64)
    } else {
        SYSMISS
    };
    Moments {
        n: n as u64,
        mean,
        sd,
    }
}

/// A group row: mean, se and the CI at `level` on `n−1` df.
fn group_row(label: String, m: &Moments, level: f64) -> TTestGroup {
    let nf = m.n as f64;
    let se = if m.n > 1 { m.sd / sqrt(nf) } else { SYSMISS };
    let (lo, hi) = ci(m.mean, se, nf - 1.0, level);
    TTestGroup {
        label,
        n: m.n,
        mean: m.mean,
        se,
        sd: m.sd,
        ci_lo: lo,
        ci_hi: hi,
    }
}

fn ci(point: f64, se: f64, df: f64, level: f64) -> (f64, f64) {
    if !(se.is_finite() && df > 0.0) {
        return (SYSMISS, SYSMISS);
    }
    let c = t_inv(1.0 - (1.0 - level / 100.0) / 2.0, df);
    (point - c * se, point + c * se)
}

/// The three tails, or three missings when `t` is not a usable statistic.
///
/// `is_missing` first, and not just `is_finite`: Stata's `.` is [`SYSMISS`], a
/// perfectly finite *normal* double a hair under `f64::MAX`, so a plain
/// finiteness test lets the sentinel through and evaluates the t distribution
/// far out in the right tail — which returns 1, 0, 0 rather than three dots.
/// `05` §12.4 records the degenerate paired case (`sd(d) = 0`) printing
/// `t = .` **and** all three probabilities as `.`; `ttest_paired.txt` is that
/// case.
///
/// Named, never spelled: ADR-005 puts the decimal expansion of that sentinel in
/// exactly one file, and `stratum-core`'s
/// `no_decimal_missing_literal_outside_missing_rs` greps every `.rs` line under
/// `crates/` for the digits with no carve-out for comments — deliberately, since
/// a comment carrying them is how the second copy gets typed.
fn tails(t: f64, df: f64) -> (f64, f64, f64) {
    if is_missing(t) || !t.is_finite() || df.is_nan() || df <= 0.0 {
        return (SYSMISS, SYSMISS, SYSMISS);
    }
    let p_l = t_cdf(t, df);
    (p_l, 2.0 * t_sf(t.abs(), df), 1.0 - p_l)
}

/// Run a t test.
///
/// # Errors
///
/// [`StatsError::StringVariable`], [`StatsError::NoObservations`], and
/// [`StatsError::GroupCount`] when `by()` does not yield exactly two groups.
pub fn ttest(spec: &TTestSpec<'_>, sample: &Sample, level: f64) -> Result<TTestResult, StatsError> {
    match spec {
        TTestSpec::OneSample { var, mu0 } => one_sample(var, *mu0, sample, level),
        TTestSpec::TwoSample { var, by, unequal } => two_sample(var, by, *unequal, sample, level),
        TTestSpec::Paired { x, y } => paired(x, y, sample, level),
    }
}

fn one_sample(
    v: &VarRef<'_>,
    mu0: f64,
    sample: &Sample,
    level: f64,
) -> Result<TTestResult, StatsError> {
    v.require_numeric()?;
    let xs = nonmissing(v, sample);
    if xs.is_empty() {
        return Err(StatsError::NoObservations);
    }
    let m = moments(&xs);
    let row = group_row(v.name.to_owned(), &m, level);
    let df = m.n as f64 - 1.0;
    let t = if row.se > 0.0 {
        (m.mean - mu0) / row.se
    } else {
        SYSMISS
    };
    let (p_l, p, p_u) = tails(t, df);
    Ok(TTestResult {
        kind: TTestKind::OneSample { mu0 },
        title: "One-sample t test",
        varnames: vec![v.name.to_owned()],
        groups: vec![row],
        diff: None,
        t,
        df,
        p_l,
        p,
        p_u,
        level,
    })
}

fn two_sample(
    v: &VarRef<'_>,
    by: &VarRef<'_>,
    unequal: bool,
    sample: &Sample,
    level: f64,
) -> Result<TTestResult, StatsError> {
    v.require_numeric()?;
    by.require_numeric()?;
    let mut xs = Vec::new();
    let mut gs = Vec::new();
    gather(v.col, sample, &mut xs);
    gather(by.col, sample, &mut gs);

    let mut levels: Vec<f64> = Vec::new();
    for (x, g) in xs.iter().zip(&gs) {
        if is_missing(*x) || is_missing(*g) {
            continue;
        }
        if !levels.iter().any(|l| l == g) {
            levels.push(*g);
        }
        if levels.len() > 2 {
            break;
        }
    }
    levels.sort_by(f64::total_cmp);
    if levels.len() != 2 {
        return Err(StatsError::GroupCount(format!(
            "variable {} takes {} values, 2 required",
            by.name,
            levels.len()
        )));
    }

    let mut a = Vec::new();
    let mut b = Vec::new();
    let mut all = Vec::new();
    for (x, g) in xs.iter().zip(&gs) {
        if is_missing(*x) || is_missing(*g) {
            continue;
        }
        all.push(*x);
        if *g == levels[0] {
            a.push(*x);
        } else {
            b.push(*x);
        }
    }
    if a.is_empty() || b.is_empty() {
        return Err(StatsError::NoObservations);
    }

    let ma = moments(&a);
    let mb = moments(&b);
    let mall = moments(&all);
    let label = |lv: f64| -> String {
        by.value_label.and_then(|t| t.get(lv)).map_or_else(
            || fmt_g(lv, 9).trim().to_owned(),
            std::borrow::ToOwned::to_owned,
        )
    };

    let (n1, n2) = (ma.n as f64, mb.n as f64);
    let (v1, v2) = (ma.sd * ma.sd, mb.sd * mb.sd);
    let (se, df) = if unequal {
        // Satterthwaite.
        let (a1, a2) = (v1 / n1, v2 / n2);
        let se = sqrt(a1 + a2);
        let df = (a1 + a2) * (a1 + a2) / (a1 * a1 / (n1 - 1.0) + a2 * a2 / (n2 - 1.0));
        (se, df)
    } else {
        let sp2 = ((n1 - 1.0) * v1 + (n2 - 1.0) * v2) / (n1 + n2 - 2.0);
        (sqrt(sp2 * (1.0 / n1 + 1.0 / n2)), n1 + n2 - 2.0)
    };
    let d = ma.mean - mb.mean;
    let (lo, hi) = ci(d, se, df, level);
    let t = if se > 0.0 { d / se } else { SYSMISS };
    let (p_l, p, p_u) = tails(t, df);

    Ok(TTestResult {
        kind: TTestKind::TwoSample { unequal },
        title: if unequal {
            "Two-sample t test with unequal variances"
        } else {
            "Two-sample t test with equal variances"
        },
        varnames: vec![label(levels[0]), label(levels[1])],
        groups: vec![
            group_row(label(levels[0]), &ma, level),
            group_row(label(levels[1]), &mb, level),
            group_row("Combined".to_owned(), &mall, level),
        ],
        diff: Some(TTestGroup {
            label: "diff".to_owned(),
            n: 0,
            mean: d,
            se,
            sd: SYSMISS,
            ci_lo: lo,
            ci_hi: hi,
        }),
        t,
        df,
        p_l,
        p,
        p_u,
        level,
    })
}

fn paired(
    x: &VarRef<'_>,
    y: &VarRef<'_>,
    sample: &Sample,
    level: f64,
) -> Result<TTestResult, StatsError> {
    x.require_numeric()?;
    y.require_numeric()?;
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    gather(x.col, sample, &mut xs);
    gather(y.col, sample, &mut ys);
    let mut a = Vec::new();
    let mut b = Vec::new();
    let mut d = Vec::new();
    for (u, v) in xs.iter().zip(&ys) {
        if is_missing(*u) || is_missing(*v) {
            continue;
        }
        a.push(*u);
        b.push(*v);
        d.push(*u - *v);
    }
    if d.is_empty() {
        return Err(StatsError::NoObservations);
    }
    let ma = moments(&a);
    let mb = moments(&b);
    let md = moments(&d);
    let diff = group_row("diff".to_owned(), &md, level);
    let df = md.n as f64 - 1.0;
    let t = if diff.se > 0.0 {
        md.mean / diff.se
    } else {
        SYSMISS
    };
    let (p_l, p, p_u) = tails(t, df);
    Ok(TTestResult {
        kind: TTestKind::Paired,
        title: "Paired t test",
        varnames: vec![x.name.to_owned(), y.name.to_owned()],
        groups: vec![
            group_row(x.name.to_owned(), &ma, level),
            group_row(y.name.to_owned(), &mb, level),
        ],
        diff: Some(diff),
        t,
        df,
        p_l,
        p,
        p_u,
        level,
    })
}

fn nonmissing(v: &VarRef<'_>, sample: &Sample) -> Vec<f64> {
    let mut buf = Vec::new();
    gather(v.col, sample, &mut buf);
    buf.retain(|x| !is_missing(*x));
    buf
}

impl StatResult for TTestResult {
    fn classic_text(&self, _linesize: u16) -> Vec<StyledRun> {
        ttest_txt::render(self)
    }

    fn payload(&self) -> ResultPayload {
        let mut rows: Vec<&TTestGroup> = self.groups.iter().collect();
        if let Some(d) = &self.diff {
            rows.push(d);
        }
        let mut cells = Vec::with_capacity(rows.len() * 6);
        for g in &rows {
            cells.push(Some(Cell::Num {
                value: g.n as f64,
                display: fmt_fc(g.n as f64, 7, 0),
            }));
            for v in [g.mean, g.se, g.sd, g.ci_lo, g.ci_hi] {
                cells.push(Some(Cell::Num {
                    value: v,
                    display: fmt_g(v, 9),
                }));
            }
        }
        ResultPayload::Table(GenericTable {
            title: Some(self.title.to_owned()),
            colnames: vec![
                "Obs".to_owned(),
                "Mean".to_owned(),
                "Std. err.".to_owned(),
                "Std. dev.".to_owned(),
                "ci_lo".to_owned(),
                "ci_hi".to_owned(),
            ],
            rownames: rows.iter().map(|g| g.label.clone()).collect(),
            cells,
            col_align: vec![Align::Decimal; 6],
        })
    }

    fn results(&self) -> (ResultKind, ResultSet) {
        let mut r = ResultSet::new();
        r.push_scalar("level", self.level);
        match self.kind {
            // `05` §12.4: one-sample posts a shorter, differently ordered set.
            TTestKind::OneSample { .. } => {
                let g = &self.groups[0];
                r.push_scalar("sd_1", g.sd);
                r.push_scalar("se", g.se);
                r.push_scalar("p_u", self.p_u);
                r.push_scalar("p_l", self.p_l);
                r.push_scalar("p", self.p);
                r.push_scalar("t", self.t);
                r.push_scalar("df_t", self.df);
                r.push_scalar("mu_1", g.mean);
                r.push_scalar("N_1", g.n as f64);
            }
            _ => {
                let diff = self
                    .diff
                    .as_ref()
                    .expect("two-sample and paired have a diff");
                let g1 = &self.groups[0];
                let g2 = &self.groups[1];
                // `sd` is the pooled/difference standard deviation; for the
                // two-sample test Stata reports the pooled one, which is
                // `se * sqrt(1/n1 + 1/n2)` inverted — we carry the value the
                // diff row would show, and the paired diff row shows sd(d).
                let sd = if matches!(self.kind, TTestKind::Paired) {
                    diff.sd
                } else {
                    pooled_sd(g1, g2)
                };
                r.push_scalar("sd", sd);
                r.push_scalar("sd_2", g2.sd);
                r.push_scalar("sd_1", g1.sd);
                r.push_scalar("se", diff.se);
                r.push_scalar("p_u", self.p_u);
                r.push_scalar("p_l", self.p_l);
                r.push_scalar("p", self.p);
                r.push_scalar("t", self.t);
                r.push_scalar("df_t", self.df);
                r.push_scalar("mu_2", g2.mean);
                r.push_scalar("N_2", g2.n as f64);
                r.push_scalar("mu_1", g1.mean);
                r.push_scalar("N_1", g1.n as f64);
            }
        }
        (ResultKind::RClass, r)
    }
}

fn pooled_sd(a: &TTestGroup, b: &TTestGroup) -> f64 {
    let (n1, n2) = (a.n as f64, b.n as f64);
    if n1 + n2 <= 2.0 {
        return SYSMISS;
    }
    sqrt(((n1 - 1.0) * a.sd * a.sd + (n2 - 1.0) * b.sd * b.sd) / (n1 + n2 - 2.0))
}

/// The `%6.4f` tail probabilities the footer prints.
pub(crate) fn tail_str(p: f64) -> String {
    fmt_f(p, 6, 4)
}
