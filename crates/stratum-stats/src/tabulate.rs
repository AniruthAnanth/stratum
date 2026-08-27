//! `tabulate`, one-way and two-way — `05` §10.

use stratum_core::dist::chi2_sf;
use stratum_core::fmt::{fmt_f, fmt_fc, fmt_g};
use stratum_core::missing::is_missing;
use stratum_data::sample::Sample;
use stratum_proto::result::{
    AssocTest, CellStat, ResultPayload, StyledRun, TabulatePayload, Truncation,
};

use crate::render::tabulate_txt;
use crate::stored::{ResultKind, ResultSet};
use crate::{Selection, StatResult, StatsError, VarRef};

/// Beyond this many cells the card renders a prefix and offers the table
/// viewer. The classic text is never truncated.
const CARD_CELL_BUDGET: usize = 5_000;

/// How many cells the truncated card shows.
const CARD_CELLS_SHOWN: u32 = 2_000;

/// `tabulate`'s option bag, `05` §10.3's v1 subset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabOptions {
    /// `missing` — give the missing values their own row/column.
    pub missing: bool,
    /// `nolabel` — print numeric levels even where a value label exists.
    pub nolabel: bool,
    /// `chi2` — report Pearson's χ² (two-way only).
    pub chi2: bool,
    /// `row` — row percentages.
    pub row: bool,
    /// `col` — column percentages.
    pub col: bool,
    /// `cell` — cell percentages.
    pub cell: bool,
    /// `nofreq` — suppress the frequency sub-row.
    pub nofreq: bool,
}

/// Which statistics each two-way cell prints, in Stata's fixed order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabShow {
    /// Frequency.
    pub freq: bool,
    /// Row percentage.
    pub row: bool,
    /// Column percentage.
    pub col: bool,
    /// Cell percentage.
    pub cell: bool,
}

impl TabShow {
    fn from(o: &TabOptions) -> Self {
        Self {
            // Stata drops the frequency row only when `nofreq` is given, and
            // shows it alone when nothing else is requested.
            freq: !o.nofreq,
            row: o.row,
            col: o.col,
            cell: o.cell,
        }
    }

    /// The requested statistics, always frequency → row → column → cell.
    #[must_use]
    pub fn list(&self) -> Vec<CellStat> {
        let mut v = Vec::with_capacity(4);
        if self.freq {
            v.push(CellStat::Freq);
        }
        if self.row {
            v.push(CellStat::RowPct);
        }
        if self.col {
            v.push(CellStat::ColPct);
        }
        if self.cell {
            v.push(CellStat::CellPct);
        }
        v
    }

    /// True when the `Key` box is printed: only once more than one statistic
    /// is shown.
    #[must_use]
    pub fn needs_key(&self) -> bool {
        self.list().len() > 1
    }
}

/// A one-way table.
#[derive(Clone, PartialEq, Debug)]
pub struct OneWayTab {
    /// The tabulated variable.
    pub var: String,
    /// Its label, empty when unset.
    pub label: String,
    /// `(level, printed label, frequency)`, ascending by level.
    pub cells: Vec<(f64, Option<String>, u64)>,
    /// Total frequency.
    pub n: u64,
}

/// Pearson's χ².
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Chi2 {
    /// The statistic.
    pub stat: f64,
    /// `(r − 1)(c − 1)`, after empty rows and columns are dropped.
    pub df: u32,
    /// Upper-tail probability.
    pub p: f64,
}

/// A two-way table.
#[derive(Clone, PartialEq, Debug)]
pub struct TwoWayTab {
    /// Row variable name.
    pub row_var: String,
    /// Column variable name.
    pub col_var: String,
    /// Row variable label, empty when unset.
    pub row_label: String,
    /// Column variable label, empty when unset.
    pub col_label: String,
    /// `(level, printed label)` per row, ascending.
    pub row_keys: Vec<(f64, Option<String>)>,
    /// `(level, printed label)` per column, ascending.
    pub col_keys: Vec<(f64, Option<String>)>,
    /// Row-major, `rows * cols`.
    pub freq: Vec<u64>,
    /// Row marginals.
    pub row_tot: Vec<u64>,
    /// Column marginals.
    pub col_tot: Vec<u64>,
    /// Grand total.
    pub n: u64,
    /// Present under `chi2`.
    pub chi2: Option<Chi2>,
    /// Which statistics each cell prints.
    pub show: TabShow,
}

fn label_of(v: &VarRef<'_>, level: f64, o: &TabOptions) -> Option<String> {
    if o.nolabel {
        return None;
    }
    v.value_label
        .and_then(|t| t.get(level))
        .map(std::borrow::ToOwned::to_owned)
}

/// Distinct values of `col` over `sel`, ascending, with per-value counts.
fn levels(v: &VarRef<'_>, sel: &Selection, keep_missing: bool) -> (Vec<f64>, Vec<u64>) {
    let mut vals = Vec::new();
    let mut scratch = Vec::new();
    v.col.for_each_chunk_f64(&mut scratch, |row0, xs| {
        sel.spans_in(row0, xs.len(), |s, e| {
            for &x in &xs[s..e] {
                if keep_missing || !is_missing(x) {
                    vals.push(x);
                }
            }
        });
    });
    vals.sort_unstable_by(f64::total_cmp);
    let mut keys = Vec::new();
    let mut counts = Vec::new();
    for x in vals {
        match keys.last() {
            Some(&k) if k == x => *counts.last_mut().expect("keys and counts stay in step") += 1,
            _ => {
                keys.push(x);
                counts.push(1u64);
            }
        }
    }
    (keys, counts)
}

/// `tabulate var`.
///
/// # Errors
///
/// [`StatsError::StringVariable`] on a string variable, and
/// [`StatsError::NoObservations`] when nothing is selected.
pub fn tabulate_oneway(
    v: &VarRef<'_>,
    sample: &Sample,
    o: &TabOptions,
) -> Result<OneWayTab, StatsError> {
    v.require_numeric()?;
    let sel = Selection::new(sample);
    let (keys, counts) = levels(v, &sel, o.missing);
    let n: u64 = counts.iter().sum();
    if n == 0 {
        return Err(StatsError::NoObservations);
    }
    Ok(OneWayTab {
        var: v.name.to_owned(),
        label: v.label.to_owned(),
        cells: keys
            .iter()
            .zip(&counts)
            .map(|(&k, &c)| (k, label_of(v, k, o), c))
            .collect(),
        n,
    })
}

/// `tabulate rowvar colvar`.
///
/// Casewise across the two variables: an observation missing either one is
/// dropped unless `missing` is given.
///
/// # Errors
///
/// [`StatsError::StringVariable`] on a string variable, and
/// [`StatsError::NoObservations`] when nothing survives.
pub fn tabulate_twoway(
    rv: &VarRef<'_>,
    cv: &VarRef<'_>,
    sample: &Sample,
    o: &TabOptions,
) -> Result<TwoWayTab, StatsError> {
    rv.require_numeric()?;
    cv.require_numeric()?;
    let sel = Selection::new(sample);

    let mut rvals = Vec::new();
    let mut cvals = Vec::new();
    crate::gather(rv.col, sample, &mut rvals);
    crate::gather(cv.col, sample, &mut cvals);
    debug_assert_eq!(rvals.len(), cvals.len());

    let mut pairs: Vec<(f64, f64)> = Vec::with_capacity(rvals.len());
    for (&a, &b) in rvals.iter().zip(&cvals) {
        if !o.missing && (is_missing(a) || is_missing(b)) {
            continue;
        }
        pairs.push((a, b));
    }
    if pairs.is_empty() {
        return Err(StatsError::NoObservations);
    }

    let rkeys = distinct(pairs.iter().map(|p| p.0));
    let ckeys = distinct(pairs.iter().map(|p| p.1));
    let (nr, nc) = (rkeys.len(), ckeys.len());
    let mut freq = vec![0u64; nr * nc];
    for (a, b) in &pairs {
        let i = rkeys
            .binary_search_by(|k| k.total_cmp(a))
            .expect("every observed level is in the key list");
        let j = ckeys
            .binary_search_by(|k| k.total_cmp(b))
            .expect("every observed level is in the key list");
        freq[i * nc + j] += 1;
    }
    let row_tot: Vec<u64> = (0..nr)
        .map(|i| freq[i * nc..(i + 1) * nc].iter().sum())
        .collect();
    let col_tot: Vec<u64> = (0..nc)
        .map(|j| (0..nr).map(|i| freq[i * nc + j]).sum())
        .collect();
    let n: u64 = row_tot.iter().sum();

    let chi2 = o
        .chi2
        .then(|| pearson(&freq, &row_tot, &col_tot, n, nr, nc));

    // `sel` is only needed for the one-way path's streaming scan; the two-way
    // path gathered instead, because it must pair the two columns row by row.
    let _ = sel;

    Ok(TwoWayTab {
        row_var: rv.name.to_owned(),
        col_var: cv.name.to_owned(),
        row_label: rv.label.to_owned(),
        col_label: cv.label.to_owned(),
        row_keys: rkeys.iter().map(|&k| (k, label_of(rv, k, o))).collect(),
        col_keys: ckeys.iter().map(|&k| (k, label_of(cv, k, o))).collect(),
        freq,
        row_tot,
        col_tot,
        n,
        chi2,
        show: TabShow::from(o),
    })
}

fn distinct(it: impl Iterator<Item = f64>) -> Vec<f64> {
    let mut v: Vec<f64> = it.collect();
    v.sort_unstable_by(f64::total_cmp);
    v.dedup_by(|a, b| a.total_cmp(b).is_eq());
    v
}

/// `05` §10.3. Rows and columns whose marginal is zero contribute nothing and
/// are dropped from the degrees of freedom; the sum is accumulated in row-major
/// order so it is a function of the table and nothing else.
fn pearson(freq: &[u64], row_tot: &[u64], col_tot: &[u64], n: u64, nr: usize, nc: usize) -> Chi2 {
    let nf = n as f64;
    let mut stat = 0.0;
    for i in 0..nr {
        if row_tot[i] == 0 {
            continue;
        }
        for j in 0..nc {
            if col_tot[j] == 0 {
                continue;
            }
            let e = row_tot[i] as f64 * col_tot[j] as f64 / nf;
            let d = freq[i * nc + j] as f64 - e;
            stat += d * d / e;
        }
    }
    let r = row_tot.iter().filter(|&&t| t > 0).count();
    let c = col_tot.iter().filter(|&&t| t > 0).count();
    let df = ((r.saturating_sub(1)) * (c.saturating_sub(1))) as u32;
    Chi2 {
        stat,
        df,
        p: chi2_sf(stat, f64::from(df)),
    }
}

impl OneWayTab {
    /// The printed row header for cell `i`: the value label if there is one,
    /// else the level through `%9.0g`.
    #[must_use]
    pub fn row_header(&self, i: usize) -> String {
        match &self.cells[i].1 {
            Some(l) => l.clone(),
            None => fmt_g(self.cells[i].0, 9),
        }
    }

    /// Percentages and the running cumulative, both unrounded.
    #[must_use]
    pub fn percents(&self) -> Vec<(f64, f64)> {
        let n = self.n as f64;
        let mut cum = 0.0;
        self.cells
            .iter()
            .map(|(_, _, f)| {
                let p = 100.0 * *f as f64 / n;
                cum += p;
                (p, cum)
            })
            .collect()
    }
}

impl TwoWayTab {
    /// The printed header for row `i`.
    #[must_use]
    pub fn row_header(&self, i: usize) -> String {
        match &self.row_keys[i].1 {
            Some(l) => l.clone(),
            None => fmt_g(self.row_keys[i].0, 9),
        }
    }

    /// The printed header for column `j`.
    #[must_use]
    pub fn col_header(&self, j: usize) -> String {
        match &self.col_keys[j].1 {
            Some(l) => l.clone(),
            None => fmt_g(self.col_keys[j].0, 9),
        }
    }

    /// `freq[i][j]`.
    #[must_use]
    pub fn at(&self, i: usize, j: usize) -> u64 {
        self.freq[i * self.col_keys.len() + j]
    }
}

impl StatResult for OneWayTab {
    fn classic_text(&self, _linesize: u16) -> Vec<StyledRun> {
        tabulate_txt::render_oneway(self)
    }

    fn payload(&self) -> ResultPayload {
        ResultPayload::Tabulate(TabulatePayload {
            row_var: self.var.clone(),
            col_var: None,
            row_label: (!self.label.is_empty()).then(|| self.label.clone()),
            col_label: None,
            row_keys: self.cells.iter().map(|(v, l, _)| (*v, l.clone())).collect(),
            col_keys: Vec::new(),
            counts: self.cells.iter().map(|(_, _, f)| *f).collect(),
            row_totals: self.cells.iter().map(|(_, _, f)| *f).collect(),
            col_totals: vec![self.n],
            total: self.n,
            requested: vec![CellStat::Freq],
            tests: Vec::new(),
            truncated: None,
        })
    }

    fn results(&self) -> (ResultKind, ResultSet) {
        let mut r = ResultSet::new();
        r.push_scalar("N", self.n as f64);
        r.push_scalar("r", self.cells.len() as f64);
        (ResultKind::RClass, r)
    }
}

impl StatResult for TwoWayTab {
    fn classic_text(&self, _linesize: u16) -> Vec<StyledRun> {
        tabulate_txt::render_twoway(self)
    }

    fn payload(&self) -> ResultPayload {
        let cells = self.row_keys.len() * self.col_keys.len();
        ResultPayload::Tabulate(TabulatePayload {
            row_var: self.row_var.clone(),
            col_var: Some(self.col_var.clone()),
            row_label: (!self.row_label.is_empty()).then(|| self.row_label.clone()),
            col_label: (!self.col_label.is_empty()).then(|| self.col_label.clone()),
            row_keys: self.row_keys.clone(),
            col_keys: self.col_keys.clone(),
            counts: self.freq.clone(),
            row_totals: self.row_tot.clone(),
            col_totals: self.col_tot.clone(),
            total: self.n,
            requested: self.show.list(),
            tests: self
                .chi2
                .iter()
                .map(|c| AssocTest {
                    name: "Pearson chi2".to_owned(),
                    stat: c.stat,
                    df: Some(f64::from(c.df)),
                    p: c.p,
                    display: format!(
                        "Pearson chi2({}) = {}   Pr = {}",
                        c.df,
                        fmt_f(c.stat, 9, 4),
                        fmt_f(c.p, 5, 3)
                    ),
                })
                .collect(),
            truncated: (cells > CARD_CELL_BUDGET).then_some(Truncation {
                shown_cells: CARD_CELLS_SHOWN,
                total_cells: cells as u64,
            }),
        })
    }

    fn results(&self) -> (ResultKind, ResultSet) {
        let mut r = ResultSet::new();
        r.push_scalar("N", self.n as f64);
        r.push_scalar("r", self.row_keys.len() as f64);
        r.push_scalar("c", self.col_keys.len() as f64);
        if let Some(c) = self.chi2 {
            r.push_scalar("chi2", c.stat);
            r.push_scalar("p", c.p);
        }
        (ResultKind::RClass, r)
    }
}

/// `fmt_fc` is the frequency format everywhere in this module; re-exported so
/// the renderer and the payload cannot drift apart.
pub(crate) fn freq_str(f: u64, w: usize) -> String {
    fmt_fc(f as f64, w, 0)
}
