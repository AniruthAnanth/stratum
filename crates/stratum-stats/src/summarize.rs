//! `summarize` and `summarize, detail` — `05` §7.
//!
//! Two-pass, not Welford. Stata's `summarize` is two-pass and mean-corrected,
//! and the two differ in the last ulps; the second scan also lets the three
//! central moments and the sparkline histogram share one traversal, so it costs
//! one extra memory-bandwidth-bound read and nothing else.
//!
//! `summarize` is **not** casewise across the varlist: each variable is
//! summarized over its own non-missing rows, which is why `summarize rep78 mpg`
//! reports 69 and 74 (`core_surface.log`).

use stratum_core::fmt::{fmt_fc, fmt_g};
use stratum_core::missing::{is_missing, SYSMISS};
use stratum_data::sample::Sample;
use stratum_proto::result::{
    ResultPayload, StyledRun, SummarizeDetail as PayloadDetail, SummarizeDisplay, SummarizePayload,
    SummarizeRow, VarKind,
};

use crate::render::summarize_txt;
use crate::stored::{ResultKind, ResultSet};
use crate::{Selection, StatResult, VarRef};

/// The nine percentiles `summarize, detail` reports, in printed order.
pub const PERCENTILES: [u32; 9] = [1, 5, 10, 25, 50, 75, 90, 95, 99];

/// How many bins the sidebar sparkline carries. Fixed so the histogram is a
/// deterministic function of the data (spec §20).
pub const SPARK_BINS: usize = 24;

/// `summarize`'s option bag, `05` §15's v1 subset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SummarizeSpec {
    /// `detail` — percentiles, the four extremes, skewness and kurtosis.
    pub detail: bool,
    /// `meanonly` — compute without printing, and skip the second pass.
    pub meanonly: bool,
}

/// The extras `detail` adds.
#[derive(Clone, PartialEq, Debug)]
pub struct SummarizeDetail {
    /// `m3 / m2^1.5`. A **biased** moment ratio, matching F2.
    pub skewness: f64,
    /// `m4 / m2^2`. NOT excess kurtosis: a normal gives 3, and `x = 1..4`
    /// gives exactly 1.64 (F2).
    pub kurtosis: f64,
    /// p1 p5 p10 p25 p50 p75 p90 p95 p99, in that order.
    pub percentiles: [f64; 9],
    /// Ascending. Slots beyond `n` hold the Stata missing sentinel.
    pub smallest4: [f64; 4],
    /// Ascending. Slots before `n - 3` hold the Stata missing sentinel.
    pub largest4: [f64; 4],
}

/// One row of the `summarize` table.
#[derive(Clone, PartialEq, Debug)]
pub struct SummarizeVar {
    /// Variable name.
    pub name: String,
    /// Variable label, empty when unset.
    pub label: String,
    /// The Stata display format, carried through to the card.
    pub format: String,
    /// Non-missing observations in the sample.
    pub n: u64,
    /// Sample observations that were missing.
    pub n_missing: u64,
    /// `Σw`; equals `n` unweighted.
    pub sum_w: f64,
    /// Arithmetic mean.
    pub mean: f64,
    /// `Σ(x − mean)² / (N − 1)` (F1).
    pub var: f64,
    /// `sqrt(var)`.
    pub sd: f64,
    /// Smallest non-missing value.
    pub min: f64,
    /// Largest non-missing value.
    pub max: f64,
    /// `Σx`.
    pub sum: f64,
    /// Present under `detail`.
    pub detail: Option<SummarizeDetail>,
    /// 24-bin histogram over `[min, max]`, accumulated in the same pass as the
    /// central moments so the sidebar never pays for a scan of its own.
    pub spark: Option<Vec<u32>>,
    /// What the card should draw this as.
    pub kind: VarKind,
}

/// The whole `summarize` result.
#[derive(Clone, PartialEq, Debug)]
pub struct SummarizeResult {
    /// One entry per variable, in varlist order.
    pub vars: Vec<SummarizeVar>,
    /// Whether `detail` was requested.
    pub detail: bool,
    /// Whether `meanonly` was requested; the classic text is then empty.
    pub meanonly: bool,
    /// The `if`/`in` qualifier as the user typed it, for the card subtitle.
    pub qualifier: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct Pass1 {
    n: u64,
    sum: f64,
    min: f64,
    max: f64,
    binary: bool,
}

impl Default for Pass1 {
    fn default() -> Self {
        Self {
            n: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            binary: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Pass2 {
    m2: f64,
    m3: f64,
    m4: f64,
    spark: Vec<u32>,
}

/// Run `summarize` over `vars` on `sample`.
///
/// Never fails: a string variable reports `Obs 0` and an empty sample reports
/// zeros, exactly as Stata does. `r(2000)` belongs to the estimation commands.
#[must_use]
pub fn summarize(vars: &[VarRef<'_>], sample: &Sample, spec: &SummarizeSpec) -> SummarizeResult {
    let sel = Selection::new(sample);
    let out = vars
        .iter()
        .map(|v| summarize_one(v, &sel, spec))
        .collect::<Vec<_>>();
    SummarizeResult {
        vars: out,
        detail: spec.detail,
        meanonly: spec.meanonly,
        qualifier: None,
    }
}

fn summarize_one(v: &VarRef<'_>, sel: &Selection, spec: &SummarizeSpec) -> SummarizeVar {
    let p1 = v.col.map_reduce_f64(
        Pass1::default(),
        |row0, xs| {
            let mut a = Pass1::default();
            sel.spans_in(row0, xs.len(), |s, e| {
                for &x in &xs[s..e] {
                    if is_missing(x) {
                        continue;
                    }
                    a.n += 1;
                    a.sum += x;
                    if x < a.min {
                        a.min = x;
                    }
                    if x > a.max {
                        a.max = x;
                    }
                    if x != 0.0 && x != 1.0 {
                        a.binary = false;
                    }
                }
            });
            a
        },
        |acc, p| {
            acc.n += p.n;
            acc.sum += p.sum;
            if p.min < acc.min {
                acc.min = p.min;
            }
            if p.max > acc.max {
                acc.max = p.max;
            }
            acc.binary &= p.binary;
        },
    );

    let n = p1.n;
    let mean = if n == 0 { SYSMISS } else { p1.sum / n as f64 };
    let (min, max) = if n == 0 {
        (SYSMISS, SYSMISS)
    } else {
        (p1.min, p1.max)
    };

    let mut var = SYSMISS;
    let mut sd = SYSMISS;
    let mut detail = None;
    let mut spark = None;

    if n > 0 && !spec.meanonly {
        let width = max - min;
        let p2 = v.col.map_reduce_f64(
            Pass2 {
                spark: vec![0; SPARK_BINS],
                ..Pass2::default()
            },
            |row0, xs| {
                let mut a = Pass2 {
                    spark: vec![0; SPARK_BINS],
                    ..Pass2::default()
                };
                sel.spans_in(row0, xs.len(), |s, e| {
                    for &x in &xs[s..e] {
                        if is_missing(x) {
                            continue;
                        }
                        let d = x - mean;
                        let d2 = d * d;
                        a.m2 += d2;
                        a.m3 += d2 * d;
                        a.m4 += d2 * d2;
                        a.spark[bin(x, min, width)] += 1;
                    }
                });
                a
            },
            |acc, p| {
                acc.m2 += p.m2;
                acc.m3 += p.m3;
                acc.m4 += p.m4;
                for (a, b) in acc.spark.iter_mut().zip(p.spark.iter()) {
                    *a += *b;
                }
            },
        );

        if n > 1 {
            var = p2.m2 / (n - 1) as f64;
            sd = stratum_core::math::sqrt(var);
        } else {
            // Stata reports a single-observation variance as 0, not missing.
            var = 0.0;
            sd = 0.0;
        }
        spark = Some(p2.spark);

        if spec.detail {
            let nf = n as f64;
            let m2 = p2.m2 / nf;
            let m3 = p2.m3 / nf;
            let m4 = p2.m4 / nf;
            let mut values = Vec::new();
            gather_nonmissing(v, sel, &mut values);
            values.sort_unstable_by(f64::total_cmp);
            detail = Some(SummarizeDetail {
                skewness: if m2 > 0.0 {
                    m3 / (m2 * stratum_core::math::sqrt(m2))
                } else {
                    SYSMISS
                },
                kurtosis: if m2 > 0.0 { m4 / (m2 * m2) } else { SYSMISS },
                percentiles: percentiles(&values),
                smallest4: extremes_low(&values),
                largest4: extremes_high(&values),
            });
        }
    }

    let kind = if !v.col.is_numeric() {
        VarKind::String
    } else if v.value_label.is_some() {
        VarKind::Labeled
    } else if n > 0 && p1.binary {
        VarKind::Binary
    } else {
        VarKind::Numeric
    };

    SummarizeVar {
        name: v.name.to_owned(),
        label: v.label.to_owned(),
        format: v.format.to_owned(),
        n,
        n_missing: sel.len().saturating_sub(n),
        sum_w: n as f64,
        mean,
        var,
        sd,
        min,
        max,
        sum: if n == 0 { 0.0 } else { p1.sum },
        detail,
        spark,
        kind,
    }
}

#[inline]
fn bin(x: f64, min: f64, width: f64) -> usize {
    if width <= 0.0 {
        return 0;
    }
    let k = ((x - min) / width * SPARK_BINS as f64) as isize;
    k.clamp(0, SPARK_BINS as isize - 1) as usize
}

fn gather_nonmissing(v: &VarRef<'_>, sel: &Selection, out: &mut Vec<f64>) {
    out.clear();
    let mut scratch = Vec::new();
    v.col.for_each_chunk_f64(&mut scratch, |row0, xs| {
        sel.spans_in(row0, xs.len(), |s, e| {
            for &x in &xs[s..e] {
                if !is_missing(x) {
                    out.push(x);
                }
            }
        });
    });
}

/// F3, stated plainly and implemented literally.
///
/// `j = N·p/100`. If `j` is an integer the percentile is `(x(j) + x(j+1))/2`,
/// and `x(N)` when `j = N`; otherwise it is `x(⌈j⌉)`. The integrality test is
/// `j == j.floor()` with **no epsilon** — an epsilon here is exactly how a
/// clone silently switches to linear interpolation on a boundary case.
#[must_use]
pub fn percentile(sorted: &[f64], p: u32) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return SYSMISS;
    }
    let j = n as f64 * f64::from(p) / 100.0;
    if j == j.floor() {
        let ji = j as usize;
        if ji == 0 {
            return sorted[0];
        }
        if ji >= n {
            return sorted[n - 1];
        }
        return (sorted[ji - 1] + sorted[ji]) / 2.0;
    }
    let ji = j.ceil() as usize;
    sorted[ji.clamp(1, n) - 1]
}

fn percentiles(sorted: &[f64]) -> [f64; 9] {
    let mut out = [SYSMISS; 9];
    for (slot, p) in out.iter_mut().zip(PERCENTILES) {
        *slot = percentile(sorted, p);
    }
    out
}

fn extremes_low(sorted: &[f64]) -> [f64; 4] {
    let mut out = [SYSMISS; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        if i < sorted.len() {
            *slot = sorted[i];
        }
    }
    out
}

fn extremes_high(sorted: &[f64]) -> [f64; 4] {
    // At N = 3 Stata prints the Largest column as `. 1 2 3`, so the padding is
    // at the TOP of the column, not the bottom.
    let mut out = [SYSMISS; 4];
    let n = sorted.len() as i64;
    for (i, slot) in out.iter_mut().enumerate() {
        let j = n - 4 + i as i64;
        if j >= 0 {
            *slot = sorted[j as usize];
        }
    }
    out
}

impl SummarizeVar {
    /// The five display strings the classic table prints, in field order.
    ///
    /// A6: the card gets these very strings, so it cannot disagree with the
    /// Classic pane about a digit.
    #[must_use]
    pub fn display(&self) -> SummarizeDisplay {
        SummarizeDisplay {
            obs: fmt_fc(self.n as f64, 10, 0),
            mean: fmt_g(self.mean, 9),
            sd: fmt_g(self.sd, 9),
            min: fmt_g(self.min, 9),
            max: fmt_g(self.max, 9),
        }
    }
}

impl StatResult for SummarizeResult {
    fn classic_text(&self, _linesize: u16) -> Vec<StyledRun> {
        summarize_txt::render(self)
    }

    fn payload(&self) -> ResultPayload {
        ResultPayload::Summarize(SummarizePayload {
            detail: self.detail,
            weight: None,
            qualifier: self.qualifier.clone(),
            rows: self
                .vars
                .iter()
                .map(|v| SummarizeRow {
                    var: v.name.clone(),
                    label: (!v.label.is_empty()).then(|| v.label.clone()),
                    format: v.format.clone(),
                    obs: v.n,
                    missing: v.n_missing,
                    mean: v.mean,
                    sd: v.sd,
                    min: v.min,
                    max: v.max,
                    sum: v.sum,
                    display: v.display(),
                    detail: v.detail.as_ref().map(|d| PayloadDetail {
                        skewness: d.skewness,
                        kurtosis: d.kurtosis,
                        variance: v.var,
                        percentiles: d.percentiles,
                        smallest4: d.smallest4,
                        largest4: d.largest4,
                        display_stats: [
                            fmt_g(d.skewness, 9),
                            fmt_g(d.kurtosis, 9),
                            fmt_g(v.var, 9),
                        ],
                        display_percentiles: d.percentiles.map(|x| fmt_g(x, 9)),
                        display_smallest4: d.smallest4.map(|x| fmt_g(x, 9)),
                        display_largest4: d.largest4.map(|x| fmt_g(x, 9)),
                    }),
                    var_kind: v.kind,
                    sparkline: v.spark.clone(),
                })
                .collect(),
        })
    }

    fn results(&self) -> (ResultKind, ResultSet) {
        let mut r = ResultSet::new();
        // "When several variables are summarized, only the LAST variable's
        // results are left in r()" (05 §7.5).
        let Some(v) = self.vars.last() else {
            return (ResultKind::RClass, r);
        };
        r.push_scalar("N", v.n as f64);
        r.push_scalar("sum_w", v.sum_w);
        r.push_scalar("mean", v.mean);
        r.push_scalar("Var", v.var);
        r.push_scalar("sd", v.sd);
        match &v.detail {
            // The DETAIL ordering is a different sequence, not an extension of
            // the plain one: skewness/kurtosis land between sd and sum, and
            // min/max move after sum. `return list` prints insertion order, so
            // this is output, not bookkeeping.
            Some(d) => {
                r.push_scalar("skewness", d.skewness);
                r.push_scalar("kurtosis", d.kurtosis);
                r.push_scalar("sum", v.sum);
                r.push_scalar("min", v.min);
                r.push_scalar("max", v.max);
                for (p, value) in PERCENTILES.iter().zip(d.percentiles) {
                    r.push_scalar(&format!("p{p}"), value);
                }
            }
            None => {
                r.push_scalar("min", v.min);
                r.push_scalar("max", v.max);
                r.push_scalar("sum", v.sum);
            }
        }
        (ResultKind::RClass, r)
    }
}
