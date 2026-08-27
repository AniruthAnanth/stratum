//! Stratum's statistical graphics: a `GraphSpec` becomes an SVG document.
//!
//! The design is `docs/design/09-graph.md`, written before this directory
//! existed because ARCHITECTURE §9 records that no design document covered this
//! crate. Read it first; this comment is the summary.
//!
//! # Why the renderer is in Rust
//!
//! Spec §30: the engine works with no GUI, so `stratum run analysis.do` on a
//! build server has to draw the same figure the app draws. One renderer, in
//! Rust, serves the inline card, the Graph Deck, `graph export` and the CLI. The
//! alternative — a Rust model plus a TypeScript drawing layer — is two renderers
//! that drift, and the CLI gets neither.
//!
//! # The three consequences that shape everything here
//!
//! 1. **No I/O.** ARCHITECTURE §8.14 / A14: `rg 'std::fs|Utf8Path|include_str!'`
//!    over this directory is empty, and `tests/no_io.rs` asserts it rather than
//!    trusting it. Scheme colours are [`stratum_tokens::graph::SCHEMES`],
//!    compiled into the binary, so a figure drawn on a machine with no `apps/`
//!    directory is the figure the app draws — and the same figure exported for a
//!    paper is the `print` scheme.
//! 2. **No fonts.** A crate that cannot open a file cannot measure a string, so
//!    [`layout`] carries a deliberately conservative advance-width model whose
//!    entire error budget is a slightly generous margin. Alignment is
//!    `text-anchor`, never arithmetic.
//! 3. **Bytes leave by `AssetRef`.** ARCHITECTURE C23: a 1.5 MB SVG inside a
//!    MessagePack event blows the 16 ms event-coalescing budget for every
//!    subscribed window. [`GraphRender`] carries one field per
//!    `stratum_proto::GraphRef` field plus the bytes; the runtime writes the
//!    bytes to the asset store and mints the `GraphRef`. This crate never sees a
//!    `SessionId`, a `ResultId` or a URL.
//!
//! # Scope
//!
//! Pass 1 is `histogram`, `twoway scatter|line|connected|bar|rcap`, `graph box`
//! and `graph bar`. `rcap` is in that list because audit item A22 put
//! `CardAction::PlotCoefficients` on the `regress` card, and a coefficient plot
//! is a range-cap plot; promising the action and shipping an engine that cannot
//! draw it is how a quick action becomes an exit-10 error.
//!
//! # Example
//!
//! ```
//! use stratum_graph::{render, GraphSpec, Histogram, Plot};
//!
//! let values: Vec<f64> = (0..74).map(f64::from).collect();
//! let spec = GraphSpec::new(
//!     "histogram price",
//!     Plot::Histogram(Histogram {
//!         values,
//!         bins: Default::default(),
//!         scale: Default::default(),
//!         discrete: false,
//!     }),
//! );
//! let figure = render(&spec).unwrap();
//! assert!(figure.svg.starts_with("<svg"));
//! assert_eq!(figure.intrinsic_pt, (396.0, 288.0));
//! // The three numbers `histogram` echoes to the log come from the same place
//! // the bars do.
//! assert_eq!(figure.binning.unwrap().bins, 9);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod coord;
pub mod counters;
pub mod error;
pub mod hist;
pub mod layout;
pub mod raster;
pub mod render;
pub mod scale;
pub mod series;
pub mod spec;
pub mod summary;
pub mod svg;

pub use counters::RenderCounters;
pub use error::GraphError;
pub use hist::Binning;
pub use render::{render, GraphRender, SVG_MIME};
pub use scale::Domain;
pub use series::{groups, series};
pub use spec::{
    Axis, BarPlot, BarStat, BinSpec, BoxPlot, FigureSize, GraphSpec, Group, HistScale, Histogram,
    Layer, Legend, Mark, Plot, Titles, DEFAULT_HEIGHT_PT, DEFAULT_WIDTH_PT,
};
pub use summary::BoxSummary;

/// The scheme ids this build knows, in `design/tokens.json` order.
///
/// Exposed so the runtime can answer `graph query, schemes` and so the desktop's
/// Settings pane can list them, without either one re-declaring the list — which
/// is the drift A14 was raised about.
#[must_use]
pub fn scheme_ids() -> Vec<&'static str> {
    stratum_tokens::graph::SCHEMES
        .iter()
        .map(|s| s.id)
        .collect()
}

/// The scheme used when the user names none.
pub const DEFAULT_SCHEME: &str = stratum_tokens::graph::DEFAULT_SCHEME;
