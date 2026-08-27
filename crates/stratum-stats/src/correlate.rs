//! `correlate` and `pwcorr` — `05` §11.
//!
//! Both are two-pass: the means are subtracted before the cross-products
//! accumulate, never via the `Σxy − n·x̄·ȳ` shortcut, which loses catastrophically
//! on shifted data (a variable with mean 1e8 and sd 1 has no significant digits
//! left after that cancellation).
//!
//! The two commands differ only in which rows they drop, and that difference is
//! the whole reason both exist: `correlate` deletes **casewise** across the
//! varlist and reports one `N`; `pwcorr` deletes **pairwise** and reports one
//! `N` per pair. On `price mpg rep78` they disagree in the fourth decimal.

use stratum_core::dist::t_sf;
use stratum_core::fmt::{fmt_f, fmt_g};
use stratum_core::math::sqrt;
use stratum_core::missing::{is_missing, SYSMISS};
use stratum_data::sample::Sample;
use stratum_proto::result::{Align, Cell, GenericTable, ResultPayload, StyledRun};

use crate::render::correlate_txt;
use crate::stored::{MatrixValue, ResultKind, ResultSet};
use crate::{gather, StatResult, StatsError, VarRef};

/// `correlate`/`pwcorr` options, `05` §11's v1 subset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CorrOptions {
    /// `covariance` — print covariances instead of correlations.
    pub covariance: bool,
    /// `sig` — a significance sub-row under each `pwcorr` cell.
    pub sig: bool,
    /// `obs` — an observation-count sub-row under each `pwcorr` cell.
    pub obs: bool,
    /// `print(#)` — suppress `pwcorr` cells with `p` above this.
    pub print: Option<f64>,
    /// `star(#)` — asterisk `pwcorr` cells with `p` below this.
    pub star: Option<f64>,
}

/// A correlation (or covariance) matrix, lower triangle only.
#[derive(Clone, PartialEq, Debug)]
pub struct CorrResult {
    /// Variable names, in varlist order.
    pub names: Vec<String>,
    /// Lower triangle **including** the diagonal, row-major: `len == k(k+1)/2`.
    pub r: Vec<f64>,
    /// One entry for `correlate`, one per pair for `pwcorr`.
    pub n: Vec<u64>,
    /// Two-sided p per pair, `pwcorr, sig` only.
    pub p: Option<Vec<f64>>,
    /// True for `pwcorr`.
    pub pairwise: bool,
    /// True under `covariance`.
    pub covariance: bool,
    /// True when `pwcorr, obs` was given.
    pub show_obs: bool,
    /// `star(#)`'s threshold, for the asterisk column.
    pub star: Option<f64>,
}

impl CorrResult {
    /// Index into the packed lower triangle.
    #[must_use]
    pub fn idx(i: usize, j: usize) -> usize {
        debug_assert!(j <= i);
        i * (i + 1) / 2 + j
    }

    /// `r[i][j]` for `j <= i`.
    #[must_use]
    pub fn at(&self, i: usize, j: usize) -> f64 {
        self.r[Self::idx(i, j)]
    }

    /// The observation count behind cell `(i, j)`.
    #[must_use]
    pub fn n_at(&self, i: usize, j: usize) -> u64 {
        if self.pairwise {
            self.n[Self::idx(i, j)]
        } else {
            self.n[0]
        }
    }
}

/// `correlate varlist` — casewise deletion, one `N`.
///
/// # Errors
///
/// [`StatsError::StringVariable`], [`StatsError::InvalidSyntax`] with fewer than
/// two variables, and [`StatsError::NoObservations`].
pub fn correlate(
    vars: &[VarRef<'_>],
    sample: &Sample,
    o: &CorrOptions,
) -> Result<CorrResult, StatsError> {
    let cols = materialise(vars, sample)?;
    let k = cols.len();
    let nsel = cols[0].len();

    let mut keep = vec![true; nsel];
    for col in &cols {
        for (i, slot) in keep.iter_mut().enumerate() {
            *slot &= !is_missing(col[i]);
        }
    }
    let n = keep.iter().filter(|k| **k).count();
    if n == 0 {
        return Err(StatsError::NoObservations);
    }

    let kept: Vec<Vec<f64>> = cols
        .iter()
        .map(|c| {
            c.iter()
                .zip(&keep)
                .filter(|(_, k)| **k)
                .map(|(v, _)| *v)
                .collect()
        })
        .collect();
    let means: Vec<f64> = kept.iter().map(|c| mean(c)).collect();

    let mut r = Vec::with_capacity(k * (k + 1) / 2);
    for i in 0..k {
        for j in 0..=i {
            r.push(pair_stat(
                &kept[i],
                means[i],
                &kept[j],
                means[j],
                o.covariance,
            ));
        }
    }
    Ok(CorrResult {
        names: vars.iter().map(|v| v.name.to_owned()).collect(),
        r,
        n: vec![n as u64],
        p: None,
        pairwise: false,
        covariance: o.covariance,
        show_obs: false,
        star: None,
    })
}

/// `pwcorr varlist` — pairwise deletion, one `N` per pair.
///
/// # Errors
///
/// As [`correlate`].
pub fn pwcorr(
    vars: &[VarRef<'_>],
    sample: &Sample,
    o: &CorrOptions,
) -> Result<CorrResult, StatsError> {
    let cols = materialise(vars, sample)?;
    let k = cols.len();
    let nsel = cols[0].len();

    let mut r = Vec::with_capacity(k * (k + 1) / 2);
    let mut ns = Vec::with_capacity(k * (k + 1) / 2);
    let mut ps = Vec::with_capacity(k * (k + 1) / 2);
    let mut xa = Vec::with_capacity(nsel);
    let mut xb = Vec::with_capacity(nsel);
    for i in 0..k {
        for j in 0..=i {
            xa.clear();
            xb.clear();
            for (&a, &b) in cols[i].iter().zip(&cols[j]) {
                if is_missing(a) || is_missing(b) {
                    continue;
                }
                xa.push(a);
                xb.push(b);
            }
            let n = xa.len();
            ns.push(n as u64);
            if n == 0 {
                r.push(SYSMISS);
                ps.push(SYSMISS);
                continue;
            }
            let (ma, mb) = (mean(&xa), mean(&xb));
            let stat = pair_stat(&xa, ma, &xb, mb, o.covariance);
            r.push(stat);
            ps.push(sig_p(stat, n, i == j, o.covariance));
        }
    }
    if ns.iter().all(|n| *n == 0) {
        return Err(StatsError::NoObservations);
    }
    Ok(CorrResult {
        names: vars.iter().map(|v| v.name.to_owned()).collect(),
        r,
        n: ns,
        p: o.sig.then_some(ps),
        pairwise: true,
        covariance: o.covariance,
        show_obs: o.obs,
        star: o.star,
    })
}

fn materialise(vars: &[VarRef<'_>], sample: &Sample) -> Result<Vec<Vec<f64>>, StatsError> {
    if vars.len() < 2 {
        return Err(StatsError::InvalidSyntax(
            "correlate requires at least two variables".to_owned(),
        ));
    }
    let mut cols = Vec::with_capacity(vars.len());
    for v in vars {
        v.require_numeric()?;
        let mut buf = Vec::new();
        gather(v.col, sample, &mut buf);
        cols.push(buf);
    }
    Ok(cols)
}

fn mean(v: &[f64]) -> f64 {
    let mut s = 0.0;
    for &x in v {
        s += x;
    }
    s / v.len() as f64
}

/// The correlation — or, under `covariance`, the `N−1` covariance — of two
/// equal-length, already-filtered vectors.
fn pair_stat(a: &[f64], ma: f64, b: &[f64], mb: f64, covariance: bool) -> f64 {
    let n = a.len();
    let mut sab = 0.0;
    let mut saa = 0.0;
    let mut sbb = 0.0;
    for t in 0..n {
        let da = a[t] - ma;
        let db = b[t] - mb;
        sab += da * db;
        saa += da * da;
        sbb += db * db;
    }
    if covariance {
        if n < 2 {
            return SYSMISS;
        }
        return sab / (n - 1) as f64;
    }
    let d = sqrt(saa * sbb);
    if d > 0.0 {
        sab / d
    } else {
        SYSMISS
    }
}

/// The two-sided p behind `pwcorr, sig`: `t = r·sqrt((n−2)/(1−r²))` on `n−2` df.
fn sig_p(r: f64, n: usize, diagonal: bool, covariance: bool) -> f64 {
    if diagonal || covariance || n < 3 {
        return SYSMISS;
    }
    // NaN-explicit rather than `!(denom > 0.0)`: a NaN denominator must take
    // the same branch as an exhausted one, and the negated comparison hid that.
    let denom = 1.0 - r * r;
    if denom.is_nan() || denom <= 0.0 {
        return 0.0;
    }
    let df = (n - 2) as f64;
    let t = r * sqrt(df / denom);
    2.0 * t_sf(t.abs(), df)
}

impl StatResult for CorrResult {
    fn classic_text(&self, _linesize: u16) -> Vec<StyledRun> {
        correlate_txt::render(self)
    }

    fn payload(&self) -> ResultPayload {
        let k = self.names.len();
        let mut cells = Vec::with_capacity(k * k);
        for i in 0..k {
            for j in 0..k {
                cells.push(if j <= i {
                    let v = self.at(i, j);
                    Some(Cell::Num {
                        value: v,
                        display: self.display_cell(v),
                    })
                } else {
                    None
                });
            }
        }
        ResultPayload::Table(GenericTable {
            title: Some(if self.covariance {
                "Covariances".to_owned()
            } else {
                "Correlations".to_owned()
            }),
            colnames: self.names.clone(),
            rownames: self.names.clone(),
            cells,
            col_align: vec![Align::Decimal; k],
        })
    }

    fn results(&self) -> (ResultKind, ResultSet) {
        let mut r = ResultSet::new();
        let k = self.names.len();
        r.push_scalar("N", self.n_at(k - 1, 0) as f64);
        // r(rho) is the LAST off-diagonal pair of the lower triangle.
        if k >= 2 {
            r.push_scalar("rho", self.at(k - 1, k - 2));
        }
        if !self.pairwise {
            // 18.5 posts r(C) for `correlate` and NOT for `pwcorr` (05 §11).
            let mut data = vec![0.0f64; k * k];
            for i in 0..k {
                for j in 0..=i {
                    data[i * k + j] = self.at(i, j);
                    data[j * k + i] = self.at(i, j);
                }
            }
            r.push_matrix(
                "C",
                MatrixValue {
                    rows: k,
                    cols: k,
                    data,
                    rownames: self.names.clone(),
                    colnames: self.names.clone(),
                    colstripe: vec![String::new(); k],
                },
            );
        }
        (ResultKind::RClass, r)
    }
}

impl CorrResult {
    /// The display string for one cell, in the same call the classic text uses.
    #[must_use]
    pub fn display_cell(&self, v: f64) -> String {
        if self.covariance {
            // Measured: `correlate, covariance` renders through a NARROWER
            // field than the rest of `05` — `%8.0g` right-aligned in 9, so
            // var(mpg) = 33.47204738985561 prints `33.472` (six significant
            // digits) and not `33.47205`.
            fmt_g(v, 8)
        } else {
            fmt_f(v, 9, 4)
        }
    }
}
