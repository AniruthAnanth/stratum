//! `regress` — `05` §8.
//!
//! OLS on the casewise-complete sample, solved by a symmetric sweep on the raw
//! uncentered cross-product matrix (`stratum_core::sweep`). The sweep is not the
//! best algorithm available and that is the point: F4–F7 show Stata's answers
//! carry the squared condition number and that its collinearity omissions fall
//! out of the pivot magnitude, so a QR would be *right* and would print a
//! different number of rows than the software our users' referees run.
//!
//! Everything numerically interesting lives in `stratum-core`. What lives here
//! is the sample construction, the VCE variants, the fit statistics and the
//! exact insertion order of `e()`.

use stratum_core::dist::{f_sf, t_inv, t_sf};
use stratum_core::fmt::{fmt_f, fmt_fc, fmt_g, fmt_g5};
use stratum_core::math::{ln, sqrt};
use stratum_core::missing::{is_missing, SYSMISS};
use stratum_core::sweep::GramSolve;
use stratum_data::sample::Sample;
use stratum_proto::diagnostic::Severity;
use stratum_proto::result::{
    AnovaTable, EstimationPayload, ModelFlag, ResultPayload, StyledRun, Term,
};

use crate::render::regress_txt;
use crate::stored::{MatrixValue, ResultKind, ResultSet, RowSet};
use crate::{gather, Selection, StatResult, StatsError, VarRef};

/// Which variance estimator to use. `05` §8.3's v1 row set.
#[derive(Clone, Copy, Debug)]
pub enum VceSpec<'a> {
    /// `s² (X'X)⁻¹`.
    Ols,
    /// HC1, `q = N/(N−k)` (F9).
    Robust,
    /// Clustered, `q = (N−1)/(N−k)·G/(G−1)`, `df_r = G−1` (F10).
    Cluster(VarRef<'a>),
}

/// The tag `e(vce)` carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VceKind {
    /// `vce(ols)`.
    Ols,
    /// `vce(robust)`.
    Robust,
    /// `vce(cluster clustvar)`.
    Cluster,
}

/// The resolved variance estimator, as `e()` reports it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Vce {
    /// Which estimator.
    pub kind: VceKind,
    /// `Some(name)` only for [`VceKind::Cluster`].
    pub clustvar: Option<String>,
    /// `Some(G)` only for [`VceKind::Cluster`].
    pub n_clust: Option<u64>,
}

impl Vce {
    /// `e(vce)`.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self.kind {
            VceKind::Ols => "ols",
            VceKind::Robust => "robust",
            VceKind::Cluster => "cluster",
        }
    }

    /// `e(vcetype)`. Absent under OLS; `"Robust"` under **both** robust and
    /// cluster (F10's `ereturn list`).
    #[must_use]
    pub fn vcetype(&self) -> Option<&'static str> {
        match self.kind {
            VceKind::Ols => None,
            VceKind::Robust | VceKind::Cluster => Some("Robust"),
        }
    }

    /// True when the ANOVA block is suppressed and the `Robust` banner printed.
    #[must_use]
    pub fn is_robust(&self) -> bool {
        !matches!(self.kind, VceKind::Ols)
    }
}

/// Everything the parser has to hand `regress`.
#[derive(Clone, Debug)]
pub struct RegressSpec<'a> {
    /// The command as submitted, after macro expansion. Stored as `e(cmdline)`.
    pub cmdline: String,
    /// The dependent variable.
    pub depvar: VarRef<'a>,
    /// The regressors, in varlist order. `_cons` is appended by us.
    pub indeps: Vec<VarRef<'a>>,
    /// `noconstant`.
    pub noconstant: bool,
    /// `level(#)`, default 95.
    pub level: f64,
    /// `beta` — print standardized coefficients instead of the CI columns.
    pub beta: bool,
    /// `vce()`.
    pub vce: VceSpec<'a>,
}

impl<'a> RegressSpec<'a> {
    /// A default-option spec: OLS, with a constant, at the 95% level.
    #[must_use]
    pub fn new(cmdline: impl Into<String>, depvar: VarRef<'a>, indeps: Vec<VarRef<'a>>) -> Self {
        Self {
            cmdline: cmdline.into(),
            depvar,
            indeps,
            noconstant: false,
            level: 95.0,
            beta: false,
            vce: VceSpec::Ols,
        }
    }
}

/// One row of the coefficient table.
#[derive(Clone, PartialEq, Debug)]
pub struct Coef {
    /// `_cons` for the intercept.
    pub name: String,
    /// Exactly `0.0` when omitted.
    pub b: f64,
    /// Stata missing when the residual variance is zero.
    pub se: f64,
    /// `b / se`.
    pub t: f64,
    /// Two-sided `P>|t|`.
    pub p: f64,
    /// Lower confidence limit.
    pub ci_lo: f64,
    /// Upper confidence limit.
    pub ci_hi: f64,
    /// Renders as `0  (omitted)`.
    pub omitted: bool,
    /// Standardized coefficient. `None` for `_cons` and under `vce(cluster)`.
    pub beta: Option<f64>,
}

/// The ANOVA block. Absent under robust and cluster, which print no such block.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Anova {
    /// Model sum of squares.
    pub mss: f64,
    /// Model degrees of freedom.
    pub df_m: f64,
    /// `mss / df_m`.
    pub ms_m: f64,
    /// Residual sum of squares.
    pub rss: f64,
    /// Residual degrees of freedom.
    pub df_r: f64,
    /// `rss / df_r`.
    pub ms_r: f64,
    /// Total sum of squares — centred with a constant, raw without one.
    pub tss: f64,
    /// Total degrees of freedom.
    pub df_t: f64,
    /// `tss / df_t`.
    pub ms_t: f64,
}

impl Anova {
    /// The nine display strings, row-major in field order (A6).
    #[must_use]
    pub fn display(&self) -> [String; 9] {
        [
            fmt_g(self.mss, 11),
            fmt_f(self.df_m, 10, 0),
            fmt_g(self.ms_m, 11),
            fmt_g(self.rss, 11),
            fmt_f(self.df_r, 10, 0),
            fmt_g(self.ms_r, 11),
            fmt_g(self.tss, 11),
            fmt_f(self.df_t, 10, 0),
            fmt_g(self.ms_t, 11),
        ]
    }
}

/// A fitted linear regression.
#[derive(Clone, PartialEq, Debug)]
pub struct RegressResult {
    /// `e(cmdline)`.
    pub cmdline: String,
    /// `e(depvar)`.
    pub depvar: String,
    /// `e(N)`.
    pub n: u64,
    /// `e(rank)` — non-omitted columns, the constant included.
    pub rank: usize,
    /// Whether `_cons` is in the model.
    pub has_cons: bool,
    /// `None` under robust/cluster.
    pub anova: Option<Anova>,
    /// `e(F)`.
    pub f: f64,
    /// `Prob > F`.
    pub p_f: f64,
    /// `e(df_m)`.
    pub df_m: f64,
    /// `e(df_r)`.
    pub df_r: f64,
    /// `e(r2)`.
    pub r2: f64,
    /// `e(r2_a)`.
    pub r2_a: f64,
    /// `e(rmse)`.
    pub rmse: f64,
    /// `e(mss)`.
    pub mss: f64,
    /// `e(rss)`.
    pub rss: f64,
    /// `e(ll)`.
    pub ll: f64,
    /// `e(ll_0)`.
    pub ll_0: f64,
    /// Confidence level, 95 by default.
    pub level: f64,
    /// Whether `beta` was requested.
    pub show_beta: bool,
    /// The variance estimator.
    pub vce: Vce,
    /// Coefficients in varlist order, `_cons` last.
    pub coefs: Vec<Coef>,
    /// `k x k`, row-major, symmetric, zero rows/cols at omitted columns.
    pub v: Vec<f64>,
    /// `s²(X'X)⁻¹`. Stored as `e(V_modelbased)` under robust/cluster, and used
    /// by `predict, stdp` under every VCE.
    pub v_modelbased: Vec<f64>,
    /// `e(sample)`.
    pub sample: RowSet,
    /// Omitted regressor names, in **varlist** order rather than detection
    /// order — verified: `regress y c b a` printed the notes for `c` then `a`.
    pub omitted_names: Vec<String>,
    /// Diagnostics only, never used to compute anything reported. `None` in
    /// v1: `05` §5.2(a) specified a separate non-load-bearing SVD, and A19
    /// removed the only linear-algebra crate in the workspace. Reporting `None`
    /// is honest; reporting a number we did not compute would not be.
    pub cond_number: Option<f64>,
}

/// Fit `y` on `indeps` over `sample`.
///
/// # Errors
///
/// * [`StatsError::StringVariable`] — a string variable in the model.
/// * [`StatsError::NoObservations`] — the casewise-complete sample is empty.
/// * [`StatsError::InsufficientObservations`] — fewer observations than
///   parameters.
pub fn regress(spec: &RegressSpec<'_>, sample: &Sample) -> Result<RegressResult, StatsError> {
    spec.depvar.require_numeric()?;
    for x in &spec.indeps {
        x.require_numeric()?;
    }
    if let VceSpec::Cluster(c) = &spec.vce {
        c.require_numeric()?;
    }

    let sel = Selection::new(sample);
    let p = spec.indeps.len();

    // ---- materialise the sample -------------------------------------------
    // The Gram kernel wants contiguous f64 columns, so the design is
    // materialised once and every later pass (the robust meat, the residuals,
    // the standardized coefficients) reads that buffer rather than the frame.
    let mut y = Vec::new();
    gather(spec.depvar.col, sample, &mut y);
    let mut xs: Vec<Vec<f64>> = Vec::with_capacity(p + 1);
    for x in &spec.indeps {
        let mut buf = Vec::new();
        gather(x.col, sample, &mut buf);
        xs.push(buf);
    }
    let mut clust = Vec::new();
    if let VceSpec::Cluster(c) = &spec.vce {
        // F15: the cluster variable participates in casewise deletion, which is
        // why `regress price mpg weight, vce(cluster rep78)` reports e(N) = 69
        // and not 74.
        gather(c.col, sample, &mut clust);
    }

    let nsel = usize::try_from(sel.len()).unwrap_or(usize::MAX);
    let mut keep = vec![true; nsel];
    for (i, slot) in keep.iter_mut().enumerate() {
        let mut ok = !is_missing(y[i]);
        for col in &xs {
            ok &= !is_missing(col[i]);
        }
        if !clust.is_empty() {
            ok &= !is_missing(clust[i]);
        }
        *slot = ok;
    }

    // e(sample) is built while compacting, so the two can never disagree about
    // which rows the estimate used.
    let mut esample = RowSet::new(sel.nobs());
    {
        let mut i = 0usize;
        sel.for_each_obs(|obs| {
            if keep[i] {
                esample.set(obs);
            }
            i += 1;
        });
    }

    compact(&mut y, &keep);
    for col in &mut xs {
        compact(col, &keep);
    }
    if !clust.is_empty() {
        compact(&mut clust, &keep);
    }

    let n = y.len();
    if n == 0 {
        return Err(StatsError::NoObservations);
    }
    let has_cons = !spec.noconstant;
    if has_cons {
        // `_cons` is appended LAST, matching Stata's coefficient ordering.
        xs.push(vec![1.0; n]);
    }
    let k = xs.len();
    if k == 0 || n < k {
        return Err(StatsError::InsufficientObservations);
    }

    // ---- solve -------------------------------------------------------------
    let refs: Vec<&[f64]> = xs.iter().map(Vec::as_slice).collect();
    let g = stratum_core::gram::gram(&refs, &y);
    let d0 = g.diagonal();
    let solved = GramSolve::solve(g.a.clone(), k, &d0, has_cons);
    let beta = solved.beta();
    // A residual sum of squares cannot be negative. The sweep drives the
    // augmented (y,y) cell to `y'y - b'X'y`, and on a perfect fit that
    // cancellation lands an ulp below zero — for `regress exact mpg` with
    // `exact = 2*mpg + 3` it comes out at -2.91e-11 against a `y'y` of 1.6e5,
    // i.e. one ulp of rounding and not a number. StataMP 18.5 reports
    // `Residual 0`, `MS 0` and `Root MSE 0` for exactly that model
    // (`extended_surface.log`, "regress perfect fit"), so the rounding is
    // clamped away here rather than propagated into a `sqrt` of a negative.
    // Written as a comparison and not `max(0.0)` on purpose: `f64::max`
    // returns the *other* operand for a NaN, which would turn a genuinely
    // undefined fit into a confident zero.
    let rss = solved.rss();
    let rss = if rss < 0.0 { 0.0 } else { rss };
    let xtx_inv = solved.xtx_inv();
    let rank = solved.rank();
    let omitted = solved.plan.omitted.clone();

    let nf = n as f64;
    let df_r_ols = nf - rank as f64;
    let df_m = (rank - usize::from(has_cons)) as f64;

    let (tss, df_t) = if has_cons {
        let mean = mean_of(&y);
        let mut s = 0.0;
        for &v in &y {
            let d = v - mean;
            s += d * d;
        }
        (s, nf - 1.0)
    } else {
        // Uncentred, on df = N. Verified against `regress price mpg weight,
        // noconstant`: Total SS 3.4478e+09 on 74 df.
        (stratum_core::gram::dot(&y, &y), nf)
    };
    let mss = tss - rss;
    let ms_r = rss / df_r_ols;
    let rmse = sqrt(ms_r);
    let r2 = mss / tss;
    let r2_a = if has_cons {
        1.0 - (1.0 - r2) * (nf - 1.0) / df_r_ols
    } else {
        1.0 - (1.0 - r2) * nf / df_r_ols
    };
    let ll = loglik(nf, rss);
    let ll_0 = loglik(nf, tss);

    // ---- variance ----------------------------------------------------------
    let v_modelbased = scale(&xtx_inv, ms_r);
    let (v, df_r, n_clust) = match &spec.vce {
        VceSpec::Ols => (v_modelbased.clone(), df_r_ols, None),
        VceSpec::Robust => {
            let meat = robust_meat(&xs, &y, &beta, k, n);
            let q = nf / df_r_ols;
            (sandwich(&xtx_inv, &meat, k, q), df_r_ols, None)
        }
        VceSpec::Cluster(_) => {
            let (meat, gcount) = cluster_meat(&xs, &y, &beta, &clust, k, n);
            let gf = gcount as f64;
            let q = (nf - 1.0) / df_r_ols * gf / (gf - 1.0);
            (sandwich(&xtx_inv, &meat, k, q), gf - 1.0, Some(gcount))
        }
    };

    let vce = Vce {
        kind: match &spec.vce {
            VceSpec::Ols => VceKind::Ols,
            VceSpec::Robust => VceKind::Robust,
            VceSpec::Cluster(_) => VceKind::Cluster,
        },
        clustvar: match &spec.vce {
            VceSpec::Cluster(c) => Some(c.name.to_owned()),
            _ => None,
        },
        n_clust,
    };

    // ---- inference ---------------------------------------------------------
    let tcrit = t_inv(1.0 - (1.0 - spec.level / 100.0) / 2.0, df_r);
    let sd_y = sd_of(&y);
    let mut coefs = Vec::with_capacity(k);
    for j in 0..k {
        let is_cons = has_cons && j == k - 1;
        let name = if is_cons {
            "_cons".to_owned()
        } else {
            spec.indeps[j].name.to_owned()
        };
        let vjj = v[j * k + j];
        // Perfect fit: `regress exact mpg` on `exact = 2*mpg + 3` prints the
        // coefficients and a `.` in every inference cell. A zero variance is
        // "no estimate", not "an estimate of zero".
        // `vjj.is_nan() || vjj <= 0.0` and not `!(vjj > 0.0)`: identical truth
        // table, but it says out loud that a NaN variance is "no estimate" too.
        let (se, t, pv, lo, hi) = if omitted[j] || vjj.is_nan() || vjj <= 0.0 {
            (SYSMISS, SYSMISS, SYSMISS, SYSMISS, SYSMISS)
        } else {
            let se = sqrt(vjj);
            let t = beta[j] / se;
            (
                se,
                t,
                2.0 * t_sf(t.abs(), df_r),
                beta[j] - tcrit * se,
                beta[j] + tcrit * se,
            )
        };
        let std_beta = if is_cons || vce.kind == VceKind::Cluster {
            None
        } else {
            Some(beta[j] * sd_of(&xs[j]) / sd_y)
        };
        coefs.push(Coef {
            name,
            b: beta[j],
            se,
            t,
            p: pv,
            ci_lo: lo,
            ci_hi: hi,
            omitted: omitted[j],
            beta: std_beta,
        });
    }

    // ---- F -----------------------------------------------------------------
    let (f, p_f) = if vce.is_robust() {
        // F11: under robust and cluster the reported F is a Wald test on the
        // k−1 slopes using the ROBUST V, not MS_m/MS_r. Mata replication put it
        // at 15.23207976 and 36.9108818 exactly.
        wald(&coefs, &v, k, df_m, df_r)
    } else if ms_r > 0.0 && df_m > 0.0 {
        let f = (mss / df_m) / ms_r;
        (f, f_sf(f, df_m, df_r))
    } else {
        (SYSMISS, SYSMISS)
    };

    let omitted_names = spec
        .indeps
        .iter()
        .enumerate()
        .filter(|(j, _)| omitted[*j])
        .map(|(_, x)| x.name.to_owned())
        .collect();

    Ok(RegressResult {
        cmdline: spec.cmdline.clone(),
        depvar: spec.depvar.name.to_owned(),
        n: n as u64,
        rank,
        has_cons,
        anova: (!vce.is_robust()).then_some(Anova {
            mss,
            df_m,
            ms_m: mss / df_m,
            rss,
            df_r: df_r_ols,
            ms_r,
            tss,
            df_t,
            ms_t: tss / df_t,
        }),
        f,
        p_f,
        df_m,
        df_r,
        r2,
        r2_a,
        rmse,
        mss,
        rss,
        ll,
        ll_0,
        level: spec.level,
        show_beta: spec.beta,
        vce,
        coefs,
        v,
        v_modelbased,
        sample: esample,
        omitted_names,
        cond_number: None,
    })
}

fn compact(v: &mut Vec<f64>, keep: &[bool]) {
    let mut w = 0usize;
    for i in 0..v.len() {
        if keep[i] {
            v[w] = v[i];
            w += 1;
        }
    }
    v.truncate(w);
}

fn mean_of(v: &[f64]) -> f64 {
    let mut s = 0.0;
    for &x in v {
        s += x;
    }
    s / v.len() as f64
}

/// The N−1 standard deviation, for the standardized coefficients of `05` §8.2.
fn sd_of(v: &[f64]) -> f64 {
    let n = v.len();
    if n < 2 {
        return 0.0;
    }
    let m = mean_of(v);
    let mut s = 0.0;
    for &x in v {
        let d = x - m;
        s += d * d;
    }
    sqrt(s / (n - 1) as f64)
}

/// F12: `ll = −N/2·(ln 2π + ln(RSS/N) + 1)`.
fn loglik(n: f64, ss: f64) -> f64 {
    const LN_2PI: f64 = 1.837_877_066_409_345_3;
    if ss.is_nan() || ss <= 0.0 {
        return SYSMISS;
    }
    -n / 2.0 * (LN_2PI + ln(ss / n) + 1.0)
}

fn scale(m: &[f64], s: f64) -> Vec<f64> {
    m.iter().map(|x| x * s).collect()
}

/// `Σ e_i² x_i x_i'`, accumulated in row order so the sum is a function of the
/// data alone.
fn robust_meat(xs: &[Vec<f64>], y: &[f64], b: &[f64], k: usize, n: usize) -> Vec<f64> {
    let mut m = vec![0.0f64; k * k];
    let mut xi = vec![0.0f64; k];
    for i in 0..n {
        let mut e = y[i];
        for j in 0..k {
            xi[j] = xs[j][i];
            e -= b[j] * xi[j];
        }
        let e2 = e * e;
        for a in 0..k {
            let w = e2 * xi[a];
            for c in 0..=a {
                m[a * k + c] += w * xi[c];
            }
        }
    }
    mirror(&mut m, k);
    m
}

/// `Σ_g u_g u_g'` with `u_g = Σ_{i∈g} e_i x_i`.
///
/// The cluster ids are collected in ascending observation order and the groups
/// summed in ascending *id* order, so neither the group count nor the meat
/// depends on how the frame happens to be sorted.
fn cluster_meat(
    xs: &[Vec<f64>],
    y: &[f64],
    b: &[f64],
    clust: &[f64],
    k: usize,
    n: usize,
) -> (Vec<f64>, u64) {
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &c| clust[a].total_cmp(&clust[c]).then(a.cmp(&c)));

    let mut m = vec![0.0f64; k * k];
    let mut u = vec![0.0f64; k];
    let mut groups = 0u64;
    let mut i = 0usize;
    while i < n {
        let id = clust[order[i]];
        u.iter_mut().for_each(|x| *x = 0.0);
        while i < n && clust[order[i]] == id {
            let r = order[i];
            let mut e = y[r];
            for j in 0..k {
                e -= b[j] * xs[j][r];
            }
            for (j, slot) in u.iter_mut().enumerate() {
                *slot += e * xs[j][r];
            }
            i += 1;
        }
        for a in 0..k {
            for c in 0..=a {
                m[a * k + c] += u[a] * u[c];
            }
        }
        groups += 1;
    }
    mirror(&mut m, k);
    (m, groups)
}

fn mirror(m: &mut [f64], k: usize) {
    for a in 0..k {
        for c in (a + 1)..k {
            m[a * k + c] = m[c * k + a];
        }
    }
}

/// `q · (X'X)⁻¹ M (X'X)⁻¹`.
fn sandwich(xtx_inv: &[f64], meat: &[f64], k: usize, q: f64) -> Vec<f64> {
    let mid = matmul(xtx_inv, meat, k);
    let full = matmul(&mid, xtx_inv, k);
    full.iter().map(|x| x * q).collect()
}

fn matmul(a: &[f64], b: &[f64], k: usize) -> Vec<f64> {
    let mut out = vec![0.0f64; k * k];
    for i in 0..k {
        for l in 0..k {
            let ail = a[i * k + l];
            if ail == 0.0 {
                continue;
            }
            for j in 0..k {
                out[i * k + j] += ail * b[l * k + j];
            }
        }
    }
    out
}

/// F11's Wald test on the non-constant, non-omitted coefficients.
///
/// `(RVR')⁻¹` goes through the same sweep the estimator uses, so there is one
/// inversion code path in the crate and not two.
fn wald(coefs: &[Coef], v: &[f64], k: usize, df_m: f64, df_r: f64) -> (f64, f64) {
    let idx: Vec<usize> = (0..k)
        .filter(|&j| !coefs[j].omitted && coefs[j].name != "_cons")
        .collect();
    let m = idx.len();
    if m == 0 || df_m <= 0.0 {
        return (SYSMISS, SYSMISS);
    }
    let dim = m + 1;
    let mut a = vec![0.0f64; dim * dim];
    for (i, &gi) in idx.iter().enumerate() {
        for (j, &gj) in idx.iter().enumerate() {
            a[i * dim + j] = v[gi * k + gj];
        }
        a[i * dim + m] = coefs[gi].b;
        a[m * dim + i] = coefs[gi].b;
    }
    let d0: Vec<f64> = (0..m).map(|i| a[i * dim + i]).collect();
    let solved = GramSolve::solve(a, m, &d0, false);
    // The augmented cell walks from 0 down to −β'(RVR')⁻¹β.
    let chi = -solved.rss();
    if !(chi.is_finite() && chi > 0.0) {
        return (SYSMISS, SYSMISS);
    }
    let f = chi / m as f64;
    (f, f_sf(f, m as f64, df_r))
}

impl RegressResult {
    /// Coefficient names in `e(b)` column order, with the `o.` stripe on
    /// omitted columns — what `matrix list e(b)` prints above the name.
    #[must_use]
    pub fn colnames(&self) -> Vec<String> {
        self.coefs.iter().map(|c| c.name.clone()).collect()
    }

    fn colstripe(&self) -> Vec<String> {
        self.coefs
            .iter()
            .map(|c| {
                if c.omitted {
                    "o.".to_owned()
                } else {
                    String::new()
                }
            })
            .collect()
    }

    /// `e()`, in the exact insertion order `ereturn list` prints (`05` §8.7,
    /// verified against `core_surface.log` and `extended_surface.log`).
    #[must_use]
    pub fn to_eresults(&self) -> ResultSet {
        let mut e = ResultSet::new();
        if let Some(g) = self.vce.n_clust {
            // Cluster inserts N_clust FIRST.
            e.push_scalar("N_clust", g as f64);
        }
        e.push_scalar("N", self.n as f64);
        e.push_scalar("df_m", self.df_m);
        e.push_scalar("df_r", self.df_r);
        e.push_scalar("F", self.f);
        e.push_scalar("r2", self.r2);
        e.push_scalar("rmse", self.rmse);
        e.push_scalar("mss", self.mss);
        e.push_scalar("rss", self.rss);
        e.push_scalar("r2_a", self.r2_a);
        e.push_scalar("ll", self.ll);
        e.push_scalar("ll_0", self.ll_0);
        e.push_scalar("rank", self.rank as f64);

        e.push_macro("cmdline", self.cmdline.clone());
        e.push_macro("title", "Linear regression");
        // `05` §8.7 also lists e(marginsprop). StataNow 18.5's `ereturn list`
        // does NOT print it (core_surface.log, the OLS listing), so it is not
        // stored; the design section predates the capture.
        e.push_macro("marginsok", "XB default");
        e.push_macro("vce", self.vce.tag());
        e.push_macro("depvar", self.depvar.clone());
        e.push_macro("cmd", "regress");
        e.push_macro("properties", "b V");
        e.push_macro("predict", "regres_p");
        e.push_macro("model", "ols");
        e.push_macro("estat_cmd", "regress_estat");
        if let Some(t) = self.vce.vcetype() {
            e.push_macro("vcetype", t);
        }
        if let Some(c) = &self.vce.clustvar {
            e.push_macro("clustvar", c.clone());
        }

        let names = self.colnames();
        let k = names.len();
        let mut b = MatrixValue::row_vector(
            "y1",
            self.coefs.iter().map(|c| c.b).collect(),
            names.clone(),
        );
        b.colstripe = self.colstripe();
        e.push_matrix("b", b);
        e.push_matrix(
            "V",
            MatrixValue {
                rows: k,
                cols: k,
                data: self.v.clone(),
                rownames: names.clone(),
                colnames: names.clone(),
                colstripe: self.colstripe(),
            },
        );
        // e(beta) is present for OLS and robust and ABSENT for cluster
        // (verified in extended_surface.log's two `ereturn list` blocks).
        if self.vce.kind != VceKind::Cluster {
            let slopes: Vec<f64> = self
                .coefs
                .iter()
                .filter(|c| c.name != "_cons")
                .map(|c| c.beta.unwrap_or(0.0))
                .collect();
            if !slopes.is_empty() {
                let cols: Vec<String> = self
                    .coefs
                    .iter()
                    .filter(|c| c.name != "_cons")
                    .map(|c| c.name.clone())
                    .collect();
                e.push_matrix("beta", MatrixValue::row_vector("y1", slopes, cols));
            }
        }
        if self.vce.is_robust() {
            e.push_matrix(
                "V_modelbased",
                MatrixValue {
                    rows: k,
                    cols: k,
                    data: self.v_modelbased.clone(),
                    rownames: names.clone(),
                    colnames: names,
                    colstripe: self.colstripe(),
                },
            );
        }
        e.push_function("sample", self.sample.clone());
        e
    }
}

impl StatResult for RegressResult {
    fn classic_text(&self, _linesize: u16) -> Vec<StyledRun> {
        regress_txt::render(self)
    }

    fn payload(&self) -> ResultPayload {
        let e = self.to_eresults();
        let terms = self
            .coefs
            .iter()
            .map(|c| Term {
                eq: 0,
                name: if c.omitted {
                    format!("o.{}", c.name)
                } else {
                    c.name.clone()
                },
                display: c.name.clone(),
                b: c.b,
                se: c.se,
                t: c.t,
                p: c.p,
                ci_lo: c.ci_lo,
                ci_hi: c.ci_hi,
                display_num: [
                    fmt_g(c.b, 9),
                    fmt_g(c.se, 9),
                    fmt_f(c.t, 8, 2),
                    fmt_f(c.p, 5, 3),
                    fmt_g(c.ci_lo, 9),
                    fmt_g(c.ci_hi, 9),
                ],
                beta: c.beta,
                omitted: c.omitted,
                base: false,
                empty: false,
            })
            .collect();
        let diagnostics = self
            .omitted_names
            .iter()
            .map(|n| ModelFlag {
                code: "STRATUM_COLLINEAR".to_owned(),
                message: format!("{n} omitted because of collinearity."),
                vars: vec![n.clone()],
                severity: Severity::Note,
            })
            .collect();
        ResultPayload::Estimation(EstimationPayload {
            cmd: "regress".to_owned(),
            cmdline: self.cmdline.clone(),
            depvar: self.depvar.clone(),
            n: self.n,
            rank: self.rank as u32,
            eq_names: vec![String::new()],
            terms,
            scalars: e.scalars().to_vec(),
            macros: e.macros().to_vec(),
            anova: self.anova.map(|a| AnovaTable {
                mss: a.mss,
                df_m: a.df_m,
                ms_m: a.ms_m,
                rss: a.rss,
                df_r: a.df_r,
                ms_r: a.ms_r,
                tss: a.tss,
                df_t: a.df_t,
                ms_t: a.ms_t,
                display: a.display(),
            }),
            vce: match &self.vce.clustvar {
                Some(c) => format!("cluster {c}"),
                None => self.vce.tag().to_owned(),
            },
            ci_level: self.level,
            estimates_name: None,
            sample_hash: self.sample.hash64(),
            diagnostics,
            cond_number: self.cond_number,
        })
    }

    fn results(&self) -> (ResultKind, ResultSet) {
        (ResultKind::EClass, self.to_eresults())
    }
}

/// The six right-block rows of the ANOVA header, as label/value pairs.
pub(crate) fn header_rows(r: &RegressResult) -> [(String, String); 6] {
    [
        ("Number of obs".to_owned(), fmt_fc(r.n as f64, 10, 0)),
        (
            format!("F({}, {})", fmt_int(r.df_m), fmt_int(r.df_r)),
            fmt_f(r.f, 10, 2),
        ),
        ("Prob > F".to_owned(), fmt_f(r.p_f, 10, 4)),
        ("R-squared".to_owned(), fmt_f(r.r2, 10, 4)),
        ("Adj R-squared".to_owned(), fmt_f(r.r2_a, 10, 4)),
        ("Root MSE".to_owned(), fmt_g5(r.rmse, 10)),
    ]
}

/// The five right-block rows of the robust/cluster header. No ANOVA block, so
/// `Adj R-squared` is dropped and the value field is one column wider.
pub(crate) fn header_rows_robust(r: &RegressResult) -> [(String, String); 5] {
    [
        ("Number of obs".to_owned(), fmt_fc(r.n as f64, 11, 0)),
        (
            format!("F({}, {})", fmt_int(r.df_m), fmt_int(r.df_r)),
            fmt_f(r.f, 11, 2),
        ),
        ("Prob > F".to_owned(), fmt_f(r.p_f, 11, 4)),
        ("R-squared".to_owned(), fmt_f(r.r2, 11, 4)),
        ("Root MSE".to_owned(), fmt_g5(r.rmse, 11)),
    ]
}

/// The degrees of freedom inside the `F(#, #)` label. Always integral in v1
/// (`df_r` is `N−k` or `G−1`), and printed as an integer rather than through
/// `%g` so that a 1e7-observation model prints `F(3, 9999996)` and not
/// `F(3, 1.0e+07)`.
fn fmt_int(x: f64) -> String {
    if x.is_finite() && x == x.trunc() {
        format!("{}", x as i64)
    } else {
        fmt_g(x, 9).trim().to_owned()
    }
}
