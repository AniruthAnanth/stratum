//! `predict` after `regress` — `05` §9.
//!
//! v1 ships `xb`, `residuals` and `stdp`. `stdf`, `cooksd`, `leverage`,
//! `rstudent` and `dfbeta` are `05` §9's deferred list and are not stubbed.
//!
//! The rule that surprises people: `xb` is computed **outside** the estimation
//! sample wherever the regressors are non-missing. That is Stata's behaviour and
//! it is what makes out-of-sample prediction work at all, so `e(sample)` is not
//! consulted here.

use stratum_core::math::sqrt;
use stratum_core::missing::{is_missing, SYSMISS};

use crate::regress::RegressResult;
use crate::{StatsError, VarRef};

/// What to predict.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PredictKind {
    /// Fitted values. The default, and the one Stata announces.
    #[default]
    Xb,
    /// `y − xb`.
    Residuals,
    /// `sqrt(x' V x)` — the standard error of the linear prediction.
    Stdp,
}

impl PredictKind {
    /// The line Stata prints when `predict` is given no option at all.
    #[must_use]
    pub fn assumed_note(self) -> Option<&'static str> {
        matches!(self, PredictKind::Xb).then_some("(option xb assumed; fitted values)")
    }
}

/// Compute a prediction over **every** observation of the frame.
///
/// `xs` must be the regressor columns re-resolved by name in `e(b)`'s order,
/// excluding `_cons`; the caller owns that resolution because only it has the
/// frame. `y` is required for [`PredictKind::Residuals`] and ignored otherwise.
///
/// The result has one entry per observation, with the Stata missing sentinel
/// wherever the prediction is undefined.
///
/// # Errors
///
/// * [`StatsError::NoEstimates`] — no active estimates (the runtime raises
///   `r(301)` from this).
/// * [`StatsError::InvalidSyntax`] — the regressor list does not match `e(b)`,
///   or `residuals` was asked for without a dependent variable.
pub fn predict(
    est: Option<&RegressResult>,
    xs: &[VarRef<'_>],
    y: Option<&VarRef<'_>>,
    kind: PredictKind,
    nobs: u64,
) -> Result<Vec<f64>, StatsError> {
    let est = est.ok_or(StatsError::NoEstimates)?;
    let slopes: Vec<&crate::regress::Coef> =
        est.coefs.iter().filter(|c| c.name != "_cons").collect();
    if xs.len() != slopes.len() {
        return Err(StatsError::InvalidSyntax(format!(
            "predict expects {} regressor(s), got {}",
            slopes.len(),
            xs.len()
        )));
    }
    for (x, c) in xs.iter().zip(&slopes) {
        x.require_numeric()?;
        if x.name != c.name {
            return Err(StatsError::InvalidSyntax(format!(
                "variable {} is not the {} that e(b) was fitted on",
                x.name, c.name
            )));
        }
    }
    if kind == PredictKind::Residuals && y.is_none() {
        return Err(StatsError::InvalidSyntax(
            "predict, residuals needs the dependent variable".to_owned(),
        ));
    }

    let k = est.coefs.len();
    let cons = if est.has_cons {
        est.coefs.last().map_or(0.0, |c| c.b)
    } else {
        0.0
    };

    let n = usize::try_from(nobs).unwrap_or(usize::MAX);
    let mut out = vec![SYSMISS; n];
    let mut row = vec![0.0f64; k];

    for i in 0..nobs {
        let mut ok = true;
        for (j, x) in xs.iter().enumerate() {
            let v = x.col.get_f64(i).unwrap_or(SYSMISS);
            if is_missing(v) {
                ok = false;
                break;
            }
            row[j] = v;
        }
        if !ok {
            continue;
        }
        if est.has_cons {
            row[k - 1] = 1.0;
        }
        match kind {
            PredictKind::Xb | PredictKind::Residuals => {
                let mut xb = cons;
                for (j, c) in slopes.iter().enumerate() {
                    xb += c.b * row[j];
                }
                out[i as usize] = match kind {
                    PredictKind::Residuals => {
                        let yv = y.expect("checked above").col.get_f64(i).unwrap_or(SYSMISS);
                        if is_missing(yv) {
                            SYSMISS
                        } else {
                            yv - xb
                        }
                    }
                    _ => xb,
                };
            }
            PredictKind::Stdp => {
                // The MODEL-BASED variance, even under robust/cluster — Stata
                // uses e(V_modelbased) here.
                let v = &est.v_modelbased;
                let mut q = 0.0;
                for a in 0..k {
                    if row[a] == 0.0 {
                        continue;
                    }
                    let mut inner = 0.0;
                    for b in 0..k {
                        inner += v[a * k + b] * row[b];
                    }
                    q += row[a] * inner;
                }
                out[i as usize] = if q >= 0.0 { sqrt(q) } else { SYSMISS };
            }
        }
    }
    Ok(out)
}
