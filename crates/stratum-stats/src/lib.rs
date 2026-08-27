//! Stratum's statistical core: `summarize`, `regress`, `predict`, `tabulate`,
//! `correlate`/`pwcorr` and `ttest`, with byte-exact classic renderers.
//!
//! # The governing rule
//!
//! `docs/design/05-statistics.md` states it and this crate obeys it: **being
//! more accurate than Stata is a defect.** Every formula here was chosen by
//! reverse-engineering StataMP 18.5, and where our answer would differ from the
//! committed golden the golden wins. That is why `regress` solves the normal
//! equations through a sweep rather than a QR (05 §5.2), why the collinearity
//! rule is dynamic max-*current*-diagonal against a **raw uncentered**
//! denominator (F5–F7), and why the percentile rule is the averaged order
//! statistic with an **exact** integrality test and no epsilon (F3).
//!
//! # `classic_text` returns runs, not a `String` (A12)
//!
//! [`StatResult::classic_text`] returns `Vec<StyledRun>`. Given a byte-exact
//! 78-column `regress` table as flat text, nothing downstream can recover which
//! spans were result values and which were labels, so the Classic pane could
//! not print them in Stata's ink. Byte-exactness is asserted on
//! [`stratum_proto::styled::to_plain`] of the runs, so styling can never move a
//! golden.
//!
//! The styling convention, which `tests/golden/*.runs.json` pins:
//!
//! * [`StyleId::Result`] covers exactly the characters of a **computed number**
//!   — the formatted value, never its padding.
//! * [`StyleId::Text`] covers everything else: literals, headers, variable
//!   names, rules, separators, padding and newlines.
//!
//! # What `classic_text` does and does not include
//!
//! It is the command's own output block: the lines Stata writes between the
//! `. cmd` echo and the blank line the log inserts before the next echo, minus
//! a single leading blank line where Stata emits one. So `regress` starts at
//! `      Source |` and `correlate` starts at `(obs=74)` and keeps its trailing
//! blank line, exactly as `tests/golden/stata18/core_surface.log` has them. The
//! spacing *around* a command's output belongs to the runtime.
//!
//! # Scope
//!
//! `05` §15's v1 list, and nothing beyond it. Weights, factor-variable
//! notation, `hc2`/`hc3`, and the non-linear estimators are out of scope here
//! and are not stubbed — a `todo!()` on a shipped path is worse than a command
//! the registry honestly says it does not implement.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod correlate;
pub mod effects;
pub mod predict;
pub mod regress;
pub mod render;
pub mod stored;
pub mod summarize;
pub mod tabulate;
pub mod ttest;

use stratum_data::column::Column;
use stratum_data::labels::ValueLabel;
use stratum_data::sample::{Run, Sample};
use stratum_proto::result::{ResultPayload, StyledRun};

pub use correlate::{correlate, pwcorr, CorrOptions, CorrResult};
pub use predict::{predict, PredictKind};
pub use regress::{regress, Anova, Coef, RegressResult, RegressSpec, Vce, VceKind, VceSpec};
pub use stored::{MatrixValue, ResultKind, ResultSet, RowSet};
pub use summarize::{summarize, SummarizeDetail, SummarizeResult, SummarizeSpec, SummarizeVar};
pub use tabulate::{
    tabulate_oneway, tabulate_twoway, Chi2, OneWayTab, TabOptions, TabShow, TwoWayTab,
};
pub use ttest::{ttest, TTestGroup, TTestKind, TTestResult, TTestSpec};

/// The only linesize `05`'s layouts are pinned at. `05` §20 records that
/// Stata's widening rule above 80 has not been reverse-engineered; A16 makes
/// `set linesize n` with `n != 80` an error in the runtime, so this is the only
/// value `classic_text` can be handed.
pub const LINESIZE: u16 = 80;

/// What went wrong, with the Stata return code the runtime must report.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum StatsError {
    /// `r(2000)` — the sample has no observations.
    #[error("no observations")]
    NoObservations,
    /// `r(2001)` — fewer observations than parameters.
    #[error("insufficient observations")]
    InsufficientObservations,
    /// `r(301)` — `predict` with no active estimates.
    #[error("last estimates not found")]
    NoEstimates,
    /// `r(109)` — a string variable where a numeric one is required.
    #[error("type mismatch: {0} is a string variable")]
    StringVariable(String),
    /// `r(198)` — the option combination is not valid.
    #[error("{0}")]
    InvalidSyntax(String),
    /// `r(430)` / `r(2002)` — `by()` did not produce exactly two groups.
    #[error("{0}")]
    GroupCount(String),
}

impl StatsError {
    /// The Stata return code. `Diagnostic.rc` is built from this.
    #[must_use]
    pub fn rc(&self) -> u16 {
        match self {
            StatsError::NoObservations => 2000,
            StatsError::InsufficientObservations => 2001,
            StatsError::NoEstimates => 301,
            StatsError::StringVariable(_) => 109,
            StatsError::InvalidSyntax(_) => 198,
            StatsError::GroupCount(_) => 430,
        }
    }
}

/// One resolved variable, as the statistics commands see it.
///
/// `05` §2 asked for a private `NumericColumn` trait so this crate could be
/// tested without the data engine. ARCHITECTURE §1 (L/T) ruled against it: a
/// structurally identical twin of `Column` with no conversion between the two
/// is exactly the duplication A10 bans. So this is a borrow of the real column
/// plus the metadata the renderers need, and it is cheap to build — the caller
/// (the runtime's varlist resolution) already holds all four pieces.
#[derive(Clone, Copy)]
pub struct VarRef<'a> {
    /// The variable's name, as it prints in the stub column.
    pub name: &'a str,
    /// `Variable::label`, empty when unset.
    pub label: &'a str,
    /// The Stata display format, e.g. `"%8.0gc"`. Carried into the payload so
    /// the card can align decimals; the classic renderers never consult it,
    /// because Stata's tables use their own fixed formats (F14).
    pub format: &'a str,
    /// The storage.
    pub col: &'a Column,
    /// The attached value-label table, for `tabulate` headers and `by()` group
    /// names.
    pub value_label: Option<&'a ValueLabel>,
}

/// Deliberately hand-written rather than derived. `VarRef` borrows a whole
/// `Column`, and `Column: Debug` prints every element — a derived `Debug` on a
/// 10 M-row variable would turn one `{:?}` in a test failure message into
/// hundreds of megabytes of output. The identity of the variable plus its
/// length is what a debugging reader actually wants.
impl std::fmt::Debug for VarRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VarRef")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("format", &self.format)
            .field("len", &self.col.len())
            .field("value_label", &self.value_label.map(ValueLabel::len))
            .finish()
    }
}

impl<'a> VarRef<'a> {
    /// A numeric-only borrow, erroring on a string variable.
    ///
    /// `summarize` deliberately does *not* use this: Stata prints a string
    /// variable as a row with `Obs 0` rather than refusing the command
    /// (`core_surface.log`, the `make` row).
    pub fn require_numeric(&self) -> Result<(), StatsError> {
        if self.col.is_numeric() {
            Ok(())
        } else {
            Err(StatsError::StringVariable(self.name.to_owned()))
        }
    }

    /// The label if there is one, else the name. This is the `summarize,
    /// detail` title and the `tabulate` stub header.
    #[must_use]
    pub fn label_or_name(&self) -> &'a str {
        if self.label.is_empty() {
            self.name
        } else {
            self.label
        }
    }
}

/// Every statistical result: one struct, three views that cannot disagree
/// because they are generated from it.
pub trait StatResult {
    /// Byte-exact traditional Stata output, as styled runs (A12).
    ///
    /// `linesize` is accepted for forward compatibility and is 80 in every code
    /// path today; see [`LINESIZE`].
    fn classic_text(&self, linesize: u16) -> Vec<StyledRun>;

    /// The structured payload the inline card consumes. Every number in it
    /// arrives twice — as an `f64` and as the display string
    /// `stratum_core::fmt` produced for the classic text (A6) — so the card and
    /// the Classic pane print the same bytes.
    fn payload(&self) -> ResultPayload;

    /// The `r()` or `e()` contribution, in the insertion order `ereturn list`
    /// prints.
    fn results(&self) -> (ResultKind, ResultSet);
}

/// Flatten a result's classic text. A convenience over
/// [`stratum_proto::styled::to_plain`] for callers that want the bytes.
#[must_use]
pub fn classic_plain(r: &impl StatResult, linesize: u16) -> String {
    stratum_proto::styled::to_plain(&r.classic_text(linesize))
}

// ---------------------------------------------------------------------------
// Sample iteration
// ---------------------------------------------------------------------------

/// The selected observations as a run list.
///
/// `Sample::contains` is `O(log n)` for an `Index` sample, so calling it once
/// per row inside a chunk map would put a binary search on a scan that is
/// otherwise memory-bandwidth bound. Materialising the runs once turns the
/// whole traversal into `O(runs + rows)` and, for the overwhelmingly common
/// `All` sample, into a single run that costs nothing at all.
#[derive(Clone, Debug)]
pub(crate) struct Selection {
    runs: Vec<Run>,
    nsel: u64,
    nobs: u64,
}

impl Selection {
    pub(crate) fn new(s: &Sample) -> Self {
        Self {
            runs: s.runs().collect(),
            nsel: s.len(),
            nobs: s.nobs(),
        }
    }

    /// How many observations are selected.
    pub(crate) fn len(&self) -> u64 {
        self.nsel
    }

    /// How many observations the frame has.
    pub(crate) fn nobs(&self) -> u64 {
        self.nobs
    }

    /// Call `f(local_start, local_end)` for each selected span inside the
    /// half-open chunk `[row0, row0 + len)`, ascending, with offsets local to
    /// the chunk.
    pub(crate) fn spans_in<F: FnMut(usize, usize)>(&self, row0: u64, len: usize, mut f: F) {
        let hi = row0 + len as u64;
        // The runs are ascending and disjoint, so the first candidate is the
        // last run starting at or before `row0`.
        let start = self.runs.partition_point(|r| r.start + r.len <= row0);
        for r in &self.runs[start..] {
            if r.start >= hi {
                break;
            }
            let a = r.start.max(row0);
            let b = (r.start + r.len).min(hi);
            if a < b {
                f((a - row0) as usize, (b - row0) as usize);
            }
        }
    }

    /// The selected observation ids, ascending. Only for paths that must map a
    /// position in a gathered buffer back to a row number — `e(sample)` and the
    /// cluster/by-group index.
    pub(crate) fn for_each_obs<F: FnMut(u64)>(&self, mut f: F) {
        for r in &self.runs {
            for obs in r.start..r.start + r.len {
                f(obs);
            }
        }
    }
}

/// Gather one column's values over the sample into `out`, widened to `f64`.
///
/// Missing values arrive in Stata's sentinel encoding, never converted — the
/// casewise filters below are the only place they are dropped.
pub(crate) fn gather(col: &Column, sample: &Sample, out: &mut Vec<f64>) {
    col.gather_f64(sample, out);
}
