//! The graph model — what the runtime hands this crate.
//!
//! A `GraphSpec` is *already resolved*: varlists have become `Vec<f64>`, `if`
//! and `in` have become the sample the runtime gathered through, option
//! defaults have been applied. This crate does no parsing, has no opinion about
//! Stata syntax, and cannot reach a frame except through [`crate::series`].
//!
//! That split is what makes the CLI and the app draw the same figure: the only
//! thing between a `GraphSpec` and an SVG is arithmetic.
//!
//! # What is deliberately absent
//!
//! Log axes, `by()` panels, marker symbols other than the solid circle, `graph
//! combine`, and every suboption not spelled here. Pass 1 scope is fixed by
//! ARCHITECTURE §9 and the design note names the deferrals; an option that is
//! accepted and then ignored is worse than one that is absent, so there is no
//! `other: HashMap<String, String>` catch-all to be silently dropped.

use crate::error::GraphError;

/// Stata's own default graph: 5.5 in × 4 in, in points.
pub const DEFAULT_WIDTH_PT: f32 = 396.0;
/// See [`DEFAULT_WIDTH_PT`].
pub const DEFAULT_HEIGHT_PT: f32 = 288.0;

/// One figure, fully resolved.
#[derive(Clone, PartialEq, Debug)]
pub struct GraphSpec {
    /// The Stata graph name — `Graph` unless `name()` was given. Becomes
    /// `GraphRef.name`.
    pub name: String,
    /// The command as submitted, after macro expansion. Becomes
    /// `GraphRef.source_cmd`, and is what the card's `aria-label` reads.
    pub source_cmd: String,
    /// A `stratum_tokens::graph::SCHEMES` id. `stratum` inline, `print` for a
    /// figure going into a paper.
    pub scheme: String,
    /// Intrinsic size in points. Becomes `GraphRef.intrinsic_pt`.
    pub size: FigureSize,
    /// Title, subtitle, note.
    pub titles: Titles,
    /// Horizontal axis furniture.
    pub x: Axis,
    /// Vertical axis furniture.
    pub y: Axis,
    /// Legend placement. Drawn only when at least one layer is labelled.
    pub legend: Legend,
    /// What to draw.
    pub plot: Plot,
}

impl GraphSpec {
    /// A spec with Stata's defaults and no titles, for the given plot.
    #[must_use]
    pub fn new(source_cmd: impl Into<String>, plot: Plot) -> Self {
        GraphSpec {
            name: "Graph".to_owned(),
            source_cmd: source_cmd.into(),
            scheme: stratum_tokens::graph::DEFAULT_SCHEME.to_owned(),
            size: FigureSize::default(),
            titles: Titles::default(),
            x: Axis::default(),
            // Stata's default scheme draws horizontal gridlines and no vertical
            // ones. The asymmetry is real and readers expect it: a value read
            // off the y axis is compared across the figure, and an x position
            // is read against its own tick.
            y: Axis::default().with_grid(),
            legend: Legend::default(),
            plot,
        }
    }

    /// Resolve the scheme name, or refuse.
    pub(crate) fn resolve_scheme(&self) -> Result<&'static stratum_tokens::Scheme, GraphError> {
        stratum_tokens::scheme(&self.scheme)
            .ok_or_else(|| GraphError::UnknownScheme(self.scheme.clone()))
    }
}

/// Intrinsic size, in points (1/72 in). The SVG carries these as its `width`,
/// `height` and `viewBox`, so the frontend's `aspect-ratio` box (06 §6.7) and
/// the document agree by construction.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FigureSize {
    /// Width in points.
    pub width_pt: f32,
    /// Height in points.
    pub height_pt: f32,
}

impl Default for FigureSize {
    fn default() -> Self {
        FigureSize {
            width_pt: DEFAULT_WIDTH_PT,
            height_pt: DEFAULT_HEIGHT_PT,
        }
    }
}

/// `title()`, `subtitle()`, `note()`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Titles {
    /// Above the plot region.
    pub title: Option<String>,
    /// Below the title, in meta ink.
    pub subtitle: Option<String>,
    /// Bottom-left, in meta ink.
    pub note: Option<String>,
}

/// One axis's furniture. The *domain* is computed from the data and is not
/// settable in pass 1 — `xscale(range())` is a deferral, not an omission.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Axis {
    /// `xtitle()` / `ytitle()`. `None` draws no title, which is what Stata does
    /// when the variable has no label and the caller passed no title.
    pub title: Option<String>,
    /// Grid lines at the major ticks. Off by default; [`GraphSpec::new`] turns
    /// it on for the *y* axis only, which is the asymmetry Stata ships.
    pub grid: bool,
}

impl Axis {
    /// An axis with a title.
    #[must_use]
    pub fn titled(title: impl Into<String>) -> Self {
        Axis {
            title: Some(title.into()),
            grid: false,
        }
    }

    /// Turn major gridlines on.
    #[must_use]
    pub fn with_grid(mut self) -> Self {
        self.grid = true;
        self
    }
}

/// Where the legend goes. `Off` is not "no legend": a single unlabelled layer
/// draws none regardless.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Legend {
    /// Below the plot region, centred — Stata's default position.
    #[default]
    Bottom,
    /// Suppressed even when layers are labelled (`legend(off)`).
    Off,
}

/// The five things pass 1 can draw.
#[derive(Clone, PartialEq, Debug)]
pub enum Plot {
    /// `histogram`.
    Histogram(Histogram),
    /// `twoway` — one or more overlaid layers, drawn back to front.
    Twoway(Vec<Layer>),
    /// `graph box`.
    Box(BoxPlot),
    /// `graph bar`.
    Bar(BarPlot),
}

// ---------------------------------------------------------------------------
// histogram
// ---------------------------------------------------------------------------

/// `histogram varname`.
#[derive(Clone, PartialEq, Debug)]
pub struct Histogram {
    /// The variable, gathered through the sample. Missing values are dropped
    /// here, not by the caller.
    pub values: Vec<f64>,
    /// `bin()` / `width()` / `start()`, or Stata's default rule.
    pub bins: BinSpec,
    /// What the height of a bar means.
    pub scale: HistScale,
    /// `discrete` — bin on the distinct values rather than on a width.
    pub discrete: bool,
}

/// How the bin grid is chosen. Stata's precedence: `width()` beats `bin()`,
/// `bin()` beats the default rule.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum BinSpec {
    /// `k = min(sqrt(N), 10*ln(N)/ln(10))`, rounded to the nearest integer.
    #[default]
    Auto,
    /// `bin(k)`.
    Bins(u32),
    /// `width(w)` and optionally `start(s)`.
    Width {
        /// Bin width.
        width: f64,
        /// Left edge of the first bin; the data minimum when `None`.
        start: Option<f64>,
    },
}

/// `histogram`'s y-axis meaning. Stata's default for continuous data is density.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HistScale {
    /// Bar area sums to 1.
    #[default]
    Density,
    /// Bar heights sum to 1.
    Fraction,
    /// Counts.
    Frequency,
    /// Bar heights sum to 100.
    Percent,
}

impl HistScale {
    /// The y-axis title Stata prints for this scale.
    #[must_use]
    pub fn axis_title(self) -> &'static str {
        match self {
            HistScale::Density => "Density",
            HistScale::Fraction => "Fraction",
            HistScale::Frequency => "Frequency",
            HistScale::Percent => "Percent",
        }
    }
}

// ---------------------------------------------------------------------------
// twoway
// ---------------------------------------------------------------------------

/// One `||`-separated plot in a `twoway` command.
#[derive(Clone, PartialEq, Debug)]
pub struct Layer {
    /// The mark and its data.
    pub mark: Mark,
    /// Legend key. `None` keeps the layer out of the legend entirely — which is
    /// how a fitted line drawn under a scatter stays unlabelled.
    pub label: Option<String>,
    /// Index into `Scheme::series`, wrapping past eight. `None` uses the layer's
    /// own position, which is what makes two overlaid scatters differ in colour
    /// without the caller saying anything.
    pub color: Option<usize>,
}

impl Layer {
    /// An unlabelled layer in the scheme's positional colour.
    #[must_use]
    pub fn new(mark: Mark) -> Self {
        Layer {
            mark,
            label: None,
            color: None,
        }
    }

    /// Give the layer a legend key.
    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// The five pass-1 `twoway` marks. `Rcap` is here because audit item A22 put
/// `CardAction::PlotCoefficients` on the `regress` card, and a coefficient plot
/// is a range-cap plot.
#[derive(Clone, PartialEq, Debug)]
pub enum Mark {
    /// `twoway scatter y x`.
    Scatter {
        /// Horizontal positions.
        x: Vec<f64>,
        /// Vertical positions.
        y: Vec<f64>,
    },
    /// `twoway line y x` — sorted on `x`, as Stata sorts it.
    Line {
        /// Horizontal positions.
        x: Vec<f64>,
        /// Vertical positions.
        y: Vec<f64>,
    },
    /// `twoway connected y x` — a line with the markers still drawn.
    Connected {
        /// Horizontal positions.
        x: Vec<f64>,
        /// Vertical positions.
        y: Vec<f64>,
    },
    /// `twoway bar y x` — a bar from `base` to `y` at each `x`.
    Bar {
        /// Bar centres.
        x: Vec<f64>,
        /// Bar tops.
        y: Vec<f64>,
        /// Where a bar starts. `0.0` unless the caller says otherwise.
        base: f64,
        /// Bar width in data units. `None` derives it from the smallest gap
        /// between adjacent `x`, which is what makes a bar chart of years look
        /// right without the caller measuring anything.
        barwidth: Option<f64>,
    },
    /// `twoway rcap hi lo x` — the confidence-interval mark.
    Rcap {
        /// Cap positions.
        x: Vec<f64>,
        /// Lower ends.
        lo: Vec<f64>,
        /// Upper ends.
        hi: Vec<f64>,
    },
}

impl Mark {
    /// The observation count this mark was handed, before the missing rule.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Mark::Scatter { x, .. }
            | Mark::Line { x, .. }
            | Mark::Connected { x, .. }
            | Mark::Bar { x, .. }
            | Mark::Rcap { x, .. } => x.len(),
        }
    }

    /// Whether the mark was handed nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every variable in the layer must have the same length: the runtime
    /// gathers them all through one `Sample`, so a disagreement is a contract
    /// break and not a user error we can draw around.
    pub(crate) fn check_ragged(&self) -> Result<(), GraphError> {
        let (first, rest): (usize, [usize; 2]) = match self {
            Mark::Scatter { x, y } | Mark::Line { x, y } | Mark::Connected { x, y } => {
                (x.len(), [y.len(), x.len()])
            }
            Mark::Bar { x, y, .. } => (x.len(), [y.len(), x.len()]),
            Mark::Rcap { x, lo, hi } => (x.len(), [lo.len(), hi.len()]),
        };
        for found in rest {
            if found != first {
                return Err(GraphError::RaggedLayer {
                    expected: first,
                    found,
                });
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// graph box / graph bar
// ---------------------------------------------------------------------------

/// One `over()` category.
#[derive(Clone, PartialEq, Debug)]
pub struct Group {
    /// The category's display name — its value label when it has one, otherwise
    /// the numeric level. The runtime attaches it; this crate never formats a
    /// level itself, because a value label is a `ValueLabelSet` lookup and that
    /// table lives with the frame.
    pub label: String,
    /// The observations in this category, missing values included; the
    /// summariser drops them.
    pub values: Vec<f64>,
}

/// `graph box yvar [, over(g)]`.
#[derive(Clone, PartialEq, Debug)]
pub struct BoxPlot {
    /// One box per group; a bare `graph box y` is one group with an empty label.
    pub groups: Vec<Group>,
}

/// `graph bar (stat) yvar [, over(g)]`.
#[derive(Clone, PartialEq, Debug)]
pub struct BarPlot {
    /// One bar per group.
    pub groups: Vec<Group>,
    /// Which statistic the bar height is.
    pub stat: BarStat,
}

/// `graph bar`'s `(mean)`, `(sum)`, `(count)`, `(median)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BarStat {
    /// `(mean)` — Stata's default.
    #[default]
    Mean,
    /// `(sum)`.
    Sum,
    /// `(count)`.
    Count,
    /// `(median)`.
    Median,
}

impl BarStat {
    /// The y-axis title Stata prints, given the variable name.
    #[must_use]
    pub fn axis_title(self, var: &str) -> String {
        match self {
            BarStat::Mean => format!("mean of {var}"),
            BarStat::Sum => format!("sum of {var}"),
            BarStat::Count => format!("count of {var}"),
            BarStat::Median => format!("median of {var}"),
        }
    }
}
