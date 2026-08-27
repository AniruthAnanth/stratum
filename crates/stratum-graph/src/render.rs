//! `GraphSpec` → SVG.
//!
//! The whole pipeline the design note §3 draws:
//!
//! ```text
//! resolve ──▶ domain ──▶ scale ──▶ layout ──▶ emit
//! ```
//!
//! `prepare` does the first two and produces a [`Figure`] — everything the
//! emitter needs, with no reference to the input data left in it for an
//! aggregate plot. That split is what makes the counter claim checkable: after
//! `prepare` returns, a histogram of ten million observations and a histogram of
//! forty are the same object.

use crate::coord::push_num;
use crate::counters::RenderCounters;
use crate::error::GraphError;
use crate::hist::{self, Binning};
use crate::layout::{self, LayoutInput, Rect};
use crate::raster::{self, Canvas};
use crate::scale::{self, Domain, Scale};
use crate::spec::{BarStat, GraphSpec, Group, Layer, Mark, Plot};
use crate::summary::{self, BoxSummary};
use crate::svg::{Anchor, Doc};
use stratum_core::missing::is_missing;
use stratum_tokens::{Color, Scheme};

/// A rendered figure. One field per `stratum_proto::GraphRef` field, plus the
/// bytes and the counters, so the runtime's `GraphRef` construction is a copy
/// and cannot lose one (design note §1).
#[derive(Clone, PartialEq, Debug)]
pub struct GraphRender {
    /// `GraphRef.name`.
    pub name: String,
    /// The document. Written to the asset store; never inlined in an envelope
    /// (ARCHITECTURE C23).
    pub svg: String,
    /// `AssetRef.mime`.
    pub mime: &'static str,
    /// `GraphRef.intrinsic_pt`.
    pub intrinsic_pt: (f32, f32),
    /// `GraphRef.scheme`.
    pub scheme: String,
    /// `GraphRef.source_cmd`.
    pub source_cmd: String,
    /// ADR-017's evidence.
    pub counters: RenderCounters,
    /// `histogram` only: the three numbers Stata echoes as
    /// `(bin=8, start=1, width=.75)`. The runtime prints the line; this is where
    /// the numbers come from, so the log and the figure cannot disagree.
    pub binning: Option<Binning>,
}

/// The MIME every render carries.
pub const SVG_MIME: &str = "image/svg+xml";

/// Draw `spec`.
pub fn render(spec: &GraphSpec) -> Result<GraphRender, GraphError> {
    let scheme = spec.resolve_scheme()?;
    let figure = prepare(spec, scheme)?;
    emit(spec, scheme, figure)
}

// ---------------------------------------------------------------------------
// prepare
// ---------------------------------------------------------------------------

/// The x axis is either a continuum or a row of category slots. `graph box` and
/// `graph bar` are the second kind and every other plot is the first.
enum XAxis {
    Numeric(Domain),
    Categories(Vec<String>),
}

/// What goes inside the plot region, in data space.
enum Draw {
    /// Histogram bars: `(left, right, height)` in data units.
    HistBars(Vec<(f64, f64, f64)>),
    /// `twoway` layers, resolved to their scheme colours.
    Layers(Vec<Resolved>),
    /// One box per category slot; `None` for an empty category.
    Boxes(Vec<Option<BoxSummary>>),
    /// One bar height per category slot.
    CatBars(Vec<Option<f64>>),
}

struct Resolved {
    mark: Mark,
    color: Color,
    label: Option<String>,
}

struct Figure {
    x: XAxis,
    y: Domain,
    draw: Draw,
    x_title: Option<String>,
    y_title: Option<String>,
    counters: RenderCounters,
    binning: Option<Binning>,
}

fn prepare(spec: &GraphSpec, scheme: &'static Scheme) -> Result<Figure, GraphError> {
    match &spec.plot {
        Plot::Histogram(h) => prepare_histogram(spec, h),
        Plot::Twoway(layers) => prepare_twoway(spec, scheme, layers),
        Plot::Box(b) => prepare_box(spec, &b.groups),
        Plot::Bar(b) => prepare_bar(spec, &b.groups, b.stat),
    }
}

/// No `scheme` argument: a histogram's bar colour is chosen at emit time like
/// every other mark's, so `prepare` here is pure arithmetic over the data.
fn prepare_histogram(spec: &GraphSpec, h: &crate::spec::Histogram) -> Result<Figure, GraphError> {
    let binned = hist::bin(&h.values, h.bins, h.scale, h.discrete)?;
    let b = binned.binning;

    let bars: Vec<(f64, f64, f64)> = binned
        .heights
        .iter()
        .enumerate()
        .map(|(i, &height)| {
            let left = b.start + i as f64 * b.width;
            (left, left + b.width, height)
        })
        .collect();

    let x = Domain {
        lo: b.start,
        hi: b.start + f64::from(b.bins) * b.width,
    };
    let y = Domain {
        lo: 0.0,
        hi: binned.heights.iter().copied().fold(0.0, f64::max),
    }
    .including_zero();

    Ok(Figure {
        x: XAxis::Numeric(x),
        // A histogram's value axis is anchored at zero and padded only at the
        // top: a bar floating above the axis misstates every height on it.
        y: Domain {
            lo: 0.0,
            hi: y.hi * 1.05,
        },
        draw: Draw::HistBars(bars),
        x_title: spec.x.title.clone(),
        y_title: spec
            .y
            .title
            .clone()
            .or_else(|| Some(h.scale.axis_title().to_owned())),
        counters: RenderCounters {
            // hist::bin walks the data exactly twice — range then accumulate.
            data_passes: 2,
            points_input: h.values.len() as u64,
            points_dropped: binned.dropped,
            ..RenderCounters::default()
        },
        binning: Some(b),
    })
}

/// One pass over each layer: the domain, the pairwise missing count, and (for
/// bars) the smallest gap between adjacent x.
struct LayerScan {
    x: Option<Domain>,
    y: Option<Domain>,
    kept: u64,
    dropped: u64,
    min_gap: f64,
}

fn scan(mark: &Mark) -> LayerScan {
    let mut xd: Option<Domain> = None;
    let mut yd: Option<Domain> = None;
    let mut kept = 0u64;
    let mut dropped = 0u64;
    let mut min_gap = f64::INFINITY;
    let mut last_x: Option<f64> = None;

    let mut note = |x: f64, lo: f64, hi: f64| {
        xd = Some(match xd {
            None => Domain { lo: x, hi: x },
            Some(d) => d.union(Domain { lo: x, hi: x }),
        });
        yd = Some(match yd {
            None => Domain {
                lo: lo.min(hi),
                hi: lo.max(hi),
            },
            Some(d) => d.union(Domain {
                lo: lo.min(hi),
                hi: lo.max(hi),
            }),
        });
        if let Some(prev) = last_x {
            let gap = (x - prev).abs();
            if gap > 0.0 && gap < min_gap {
                min_gap = gap;
            }
        }
        last_x = Some(x);
    };

    match mark {
        Mark::Scatter { x, y } | Mark::Line { x, y } | Mark::Connected { x, y } => {
            for (&xi, &yi) in x.iter().zip(y.iter()) {
                if drop_pair(xi, yi) {
                    dropped += 1;
                    continue;
                }
                kept += 1;
                note(xi, yi, yi);
            }
        }
        Mark::Bar { x, y, base, .. } => {
            for (&xi, &yi) in x.iter().zip(y.iter()) {
                if drop_pair(xi, yi) {
                    dropped += 1;
                    continue;
                }
                kept += 1;
                note(xi, yi, *base);
            }
        }
        Mark::Rcap { x, lo, hi } => {
            for ((&xi, &li), &hi_i) in x.iter().zip(lo.iter()).zip(hi.iter()) {
                if drop_pair(xi, li) || is_missing(hi_i) || !hi_i.is_finite() {
                    dropped += 1;
                    continue;
                }
                kept += 1;
                note(xi, li, hi_i);
            }
        }
    }

    LayerScan {
        x: xd,
        y: yd,
        kept,
        dropped,
        min_gap,
    }
}

fn drop_pair(a: f64, b: f64) -> bool {
    is_missing(a) || is_missing(b) || !a.is_finite() || !b.is_finite()
}

fn prepare_twoway(
    spec: &GraphSpec,
    scheme: &'static Scheme,
    layers: &[Layer],
) -> Result<Figure, GraphError> {
    if layers.is_empty() {
        return Err(GraphError::NoObservations);
    }

    let mut xd: Option<Domain> = None;
    let mut yd: Option<Domain> = None;
    let mut counters = RenderCounters {
        data_passes: 2,
        ..RenderCounters::default()
    };
    let mut resolved = Vec::with_capacity(layers.len());
    let mut any_bar = false;

    for (i, layer) in layers.iter().enumerate() {
        layer.mark.check_ragged()?;
        let s = scan(&layer.mark);
        counters.points_input += layer.mark.len() as u64;
        counters.points_dropped += s.dropped;
        if let Some(d) = s.x {
            xd = Some(xd.map_or(d, |a| a.union(d)));
        }
        if let Some(d) = s.y {
            yd = Some(yd.map_or(d, |a| a.union(d)));
        }

        let mut mark = layer.mark.clone();
        if let Mark::Bar { barwidth, .. } = &mut mark {
            any_bar = true;
            if barwidth.is_none() {
                // Stata sizes a bar from the spacing of the data. A single bar
                // has no gap to measure, so it gets a width of 1 — the same
                // default Stata falls back to.
                *barwidth = Some(if s.min_gap.is_finite() {
                    s.min_gap * 0.8
                } else {
                    1.0
                });
            }
        }
        if s.kept > 0 {
            resolved.push(Resolved {
                mark,
                color: series_color(scheme, layer.color.unwrap_or(i)),
                label: layer.label.clone(),
            });
        }
    }

    let (Some(xd), Some(yd)) = (xd, yd) else {
        return Err(GraphError::NoObservations);
    };

    // Bars are widened by half a bar each side so the outermost bar is not cut
    // in half by the plot frame; point marks get the ordinary 2 % padding.
    let x = if any_bar {
        let half = resolved
            .iter()
            .filter_map(|r| match &r.mark {
                Mark::Bar { barwidth, .. } => *barwidth,
                _ => None,
            })
            .fold(0.0f64, f64::max)
            / 2.0;
        Domain {
            lo: xd.lo - half,
            hi: xd.hi + half,
        }
        .padded(0.02)
    } else {
        xd.padded(0.02)
    };
    let y = if any_bar {
        yd.including_zero().padded(0.02)
    } else {
        yd.padded(0.05)
    };

    Ok(Figure {
        x: XAxis::Numeric(x),
        y,
        draw: Draw::Layers(resolved),
        x_title: spec.x.title.clone(),
        y_title: spec.y.title.clone(),
        counters,
        binning: None,
    })
}

fn prepare_box(spec: &GraphSpec, groups: &[Group]) -> Result<Figure, GraphError> {
    if groups.is_empty() {
        return Err(GraphError::NoObservations);
    }
    let mut counters = RenderCounters {
        data_passes: 1,
        ..RenderCounters::default()
    };
    let mut boxes = Vec::with_capacity(groups.len());
    let mut yd: Option<Domain> = None;

    for g in groups {
        counters.points_input += g.values.len() as u64;
        let s = summary::box_summary(&g.values);
        if let Some(s) = &s {
            counters.points_dropped += s.dropped;
            let mut d = Domain {
                lo: s.lower_whisker,
                hi: s.upper_whisker,
            };
            for &o in &s.outside {
                d = d.union(Domain { lo: o, hi: o });
            }
            yd = Some(yd.map_or(d, |a| a.union(d)));
        } else {
            counters.points_dropped += g.values.len() as u64;
        }
        boxes.push(s);
    }

    let Some(yd) = yd else {
        return Err(GraphError::NoObservations);
    };

    Ok(Figure {
        x: XAxis::Categories(groups.iter().map(|g| g.label.clone()).collect()),
        y: yd.padded(0.05),
        draw: Draw::Boxes(boxes),
        x_title: spec.x.title.clone(),
        y_title: spec.y.title.clone(),
        counters,
        binning: None,
    })
}

fn prepare_bar(spec: &GraphSpec, groups: &[Group], stat: BarStat) -> Result<Figure, GraphError> {
    if groups.is_empty() {
        return Err(GraphError::NoObservations);
    }
    let mut counters = RenderCounters {
        data_passes: 1,
        ..RenderCounters::default()
    };
    let mut heights = Vec::with_capacity(groups.len());
    let mut yd: Option<Domain> = None;

    for g in groups {
        counters.points_input += g.values.len() as u64;
        let h = summary::bar_stat(&g.values, stat);
        match h {
            Some(v) => {
                yd = Some(yd.map_or(Domain { lo: v, hi: v }, |a| {
                    a.union(Domain { lo: v, hi: v })
                }))
            }
            None => counters.points_dropped += g.values.len() as u64,
        }
        heights.push(h);
    }

    let Some(yd) = yd else {
        return Err(GraphError::NoObservations);
    };

    Ok(Figure {
        x: XAxis::Categories(groups.iter().map(|g| g.label.clone()).collect()),
        // Anchored at zero, always: a bar chart whose baseline floats misstates
        // every ratio a reader takes off it.
        y: Domain {
            lo: yd.including_zero().lo,
            hi: yd.including_zero().hi * 1.05,
        },
        draw: Draw::CatBars(heights),
        x_title: spec.x.title.clone(),
        y_title: spec.y.title.clone(),
        counters,
        binning: None,
    })
}

fn series_color(scheme: &'static Scheme, i: usize) -> Color {
    scheme.series[i % scheme.series.len()]
}

// ---------------------------------------------------------------------------
// emit
// ---------------------------------------------------------------------------

/// Predicted bytes for the mark layer, used to choose the raster path before
/// anything is written (design note §7).
fn predict_mark_bytes(draw: &Draw) -> usize {
    match draw {
        Draw::HistBars(bars) => bars.len() * raster::MAX_BYTES_PER_BAR,
        Draw::Boxes(b) => b.len() * (raster::MAX_BYTES_PER_BAR + 4 * raster::MAX_BYTES_PER_RCAP),
        Draw::CatBars(b) => b.len() * raster::MAX_BYTES_PER_BAR,
        Draw::Layers(layers) => layers
            .iter()
            .map(|r| match &r.mark {
                Mark::Scatter { x, .. } => x.len() * raster::MAX_BYTES_PER_MARKER,
                Mark::Line { x, .. } => x.len() * raster::MAX_BYTES_PER_VERTEX,
                Mark::Connected { x, .. } => {
                    x.len() * (raster::MAX_BYTES_PER_VERTEX + raster::MAX_BYTES_PER_MARKER)
                }
                Mark::Bar { x, .. } => x.len() * raster::MAX_BYTES_PER_BAR,
                Mark::Rcap { x, .. } => x.len() * raster::MAX_BYTES_PER_RCAP,
            })
            .sum(),
    }
}

fn emit(
    spec: &GraphSpec,
    scheme: &'static Scheme,
    figure: Figure,
) -> Result<GraphRender, GraphError> {
    let (w, h) = (
        f64::from(spec.size.width_pt),
        f64::from(spec.size.height_pt),
    );

    // --- ticks and labels, which the margins depend on ----------------------
    let y_ticks = scale::ticks(figure.y, 5);
    let y_labels: Vec<String> = y_ticks.iter().map(|&v| scale::tick_label(v)).collect();
    let (x_ticks, x_labels): (Vec<f64>, Vec<String>) = match &figure.x {
        XAxis::Numeric(d) => {
            let t = scale::ticks(*d, 6);
            let l = t.iter().map(|&v| scale::tick_label(v)).collect();
            (t, l)
        }
        XAxis::Categories(names) => (Vec::new(), names.clone()),
    };

    let legend: Vec<(String, Color)> = match (&figure.draw, spec.legend) {
        (Draw::Layers(layers), crate::spec::Legend::Bottom) => layers
            .iter()
            .filter_map(|r| r.label.clone().map(|l| (l, r.color)))
            .collect(),
        _ => Vec::new(),
    };

    let widest_y = y_labels
        .iter()
        .max_by_key(|s| s.chars().count())
        .cloned()
        .unwrap_or_default();
    let laid = layout::plan(LayoutInput {
        width: w,
        height: h,
        widest_y_label: &widest_y,
        first_x_label: x_labels.first().map_or("", String::as_str),
        last_x_label: x_labels.last().map_or("", String::as_str),
        has_title: spec.titles.title.is_some(),
        has_subtitle: spec.titles.subtitle.is_some(),
        has_note: spec.titles.note.is_some(),
        has_x_title: figure.x_title.is_some(),
        has_y_title: figure.y_title.is_some(),
        legend_rows: usize::from(!legend.is_empty()),
    })
    .ok_or(GraphError::FigureTooSmall {
        width_pt: spec.size.width_pt,
        height_pt: spec.size.height_pt,
    })?;

    let plot = laid.plot;
    let ys = Scale::new(figure.y, plot.bottom(), plot.y);
    let xs = match &figure.x {
        XAxis::Numeric(d) => Scale::new(*d, plot.x, plot.right()),
        // Categories are laid out as equal slots; the scale is never consulted.
        XAxis::Categories(_) => Scale::new(Domain { lo: 0.0, hi: 1.0 }, plot.x, plot.right()),
    };

    // --- document -----------------------------------------------------------
    // Built with `coord::push_num` rather than `format!("{w}x{h}")`, so the
    // claim `svg.rs` makes — that no float in this crate is formatted outside
    // `coord` — is checkable by grep and not by argument. ARCHITECTURE §8.7's
    // ban is about user-visible numbers and this seed is never shown to anyone,
    // but a rule with one documented exception is a rule nobody can grep for.
    let mut seed = String::with_capacity(spec.name.len() + spec.source_cmd.len() + 24);
    seed.push_str(&spec.name);
    seed.push('|');
    seed.push_str(&spec.source_cmd);
    seed.push('|');
    push_num(&mut seed, w);
    seed.push('x');
    push_num(&mut seed, h);
    let mut doc = Doc::open(w, h, &seed);
    doc.title(&spec.source_cmd);
    doc.rect(laid.figure, scheme.background);
    doc.rect(plot, scheme.plot_background);

    // Grid first, so marks sit on top of it.
    if spec.y.grid {
        for &t in &y_ticks {
            let y = ys.map(t);
            doc.line(plot.x, y, plot.right(), y, scheme.grid, 0.5);
        }
    }
    if spec.x.grid {
        for &t in &x_ticks {
            let x = xs.map(t);
            doc.line(x, plot.y, x, plot.bottom(), scheme.grid, 0.5);
        }
    }

    let mut counters = figure.counters;
    // `marks_emitted` is the DATA marks and nothing else, so it is read as a
    // delta across the mark layer. Counting the figure ground, the axes and the
    // ticks in it would make "a histogram of ten million rows emits `bins`
    // rectangles" untrue by an amount that depends on how many ticks the axis
    // happened to want.
    let marks_before = doc.marks();
    let clip = doc.clip(plot);
    doc.group_open(Some(&clip));
    let rasterized = if raster::over_budget(predict_mark_bytes(&figure.draw)) {
        emit_marks_raster(&mut doc, &figure, plot, &xs, &ys, &mut counters)
    } else {
        false
    };
    if !rasterized {
        emit_marks_vector(&mut doc, &figure, scheme, plot, &xs, &ys);
    }
    doc.group_close();
    counters.marks_emitted = doc.marks() - marks_before;

    // --- axes ---------------------------------------------------------------
    doc.line(
        plot.x,
        plot.bottom(),
        plot.right(),
        plot.bottom(),
        scheme.axis,
        0.75,
    );
    doc.line(plot.x, plot.y, plot.x, plot.bottom(), scheme.axis, 0.75);

    for (&t, label) in y_ticks.iter().zip(y_labels.iter()) {
        let y = ys.map(t);
        doc.line(plot.x - layout::TICK_LEN, y, plot.x, y, scheme.axis, 0.75);
        // `+ 0.35 em` puts the cap-height centre of the label on the tick;
        // `dominant-baseline` is inconsistent across the three webviews we ship
        // on, and this arithmetic is not.
        let baseline = y + layout::FS_TICK * 0.35;
        doc.text(
            plot.x - layout::TICK_LEN - layout::TICK_GAP,
            baseline,
            Anchor::End,
            layout::FS_TICK,
            scheme.text,
            label,
        );
    }

    let baseline = plot.bottom() + layout::TICK_LEN + layout::TICK_GAP + layout::FS_TICK * 0.8;
    match &figure.x {
        XAxis::Numeric(_) => {
            for (&t, label) in x_ticks.iter().zip(x_labels.iter()) {
                let x = xs.map(t);
                doc.line(
                    x,
                    plot.bottom(),
                    x,
                    plot.bottom() + layout::TICK_LEN,
                    scheme.axis,
                    0.75,
                );
                doc.text(
                    x,
                    baseline,
                    Anchor::Middle,
                    layout::FS_TICK,
                    scheme.text,
                    label,
                );
            }
        }
        XAxis::Categories(names) => {
            for (i, label) in names.iter().enumerate() {
                let x = slot_centre(plot, i, names.len());
                doc.text(
                    x,
                    baseline,
                    Anchor::Middle,
                    layout::FS_TICK,
                    scheme.text,
                    label,
                );
            }
        }
    }

    // --- titles -------------------------------------------------------------
    if let (Some(y), Some(text)) = (laid.title_y, spec.titles.title.as_deref()) {
        doc.text_bold(
            plot.x + plot.w / 2.0,
            y,
            Anchor::Middle,
            layout::FS_TITLE,
            scheme.foreground,
            text,
        );
    }
    if let (Some(y), Some(text)) = (laid.subtitle_y, spec.titles.subtitle.as_deref()) {
        doc.text(
            plot.x + plot.w / 2.0,
            y,
            Anchor::Middle,
            layout::FS_META,
            scheme.text_meta,
            text,
        );
    }
    if let (Some(y), Some(text)) = (laid.note_y, spec.titles.note.as_deref()) {
        doc.text(
            layout::PAD,
            y,
            Anchor::Start,
            layout::FS_META,
            scheme.text_meta,
            text,
        );
    }
    if let (Some(y), Some(text)) = (laid.x_title_y, figure.x_title.as_deref()) {
        // `y` is already the baseline — see the band note in `layout::plan`.
        doc.text(
            plot.x + plot.w / 2.0,
            y,
            Anchor::Middle,
            layout::FS_AXIS_TITLE,
            scheme.text,
            text,
        );
    }
    if let (Some(x), Some(text)) = (laid.y_title_x, figure.y_title.as_deref()) {
        doc.text_rotated(
            x,
            plot.y + plot.h / 2.0,
            layout::FS_AXIS_TITLE,
            scheme.text,
            text,
        );
    }

    // --- legend -------------------------------------------------------------
    if let Some(y) = laid.legend_y {
        emit_legend(&mut doc, &legend, plot, y, scheme);
    }

    let svg = doc.finish();
    counters.svg_bytes = svg.len() as u64;

    Ok(GraphRender {
        name: spec.name.clone(),
        svg,
        mime: SVG_MIME,
        intrinsic_pt: (spec.size.width_pt, spec.size.height_pt),
        scheme: spec.scheme.clone(),
        source_cmd: spec.source_cmd.clone(),
        counters,
        binning: figure.binning,
    })
}

/// Centre of category slot `i` of `n`.
fn slot_centre(plot: Rect, i: usize, n: usize) -> f64 {
    plot.x + plot.w * (i as f64 + 0.5) / n as f64
}

/// Marker radius, in user units. 1.6 pt is Stata's `msize(small)`, near enough.
const MARKER_R: f64 = 1.6;
/// Line width for `line`, `connected` and the rcap spine.
const LINE_W: f64 = 1.1;

fn emit_marks_vector(
    doc: &mut Doc,
    figure: &Figure,
    scheme: &'static Scheme,
    plot: Rect,
    xs: &Scale,
    ys: &Scale,
) {
    match &figure.draw {
        Draw::HistBars(bars) => {
            // Series 1, not series 0. The palette is Okabe-Ito, whose first
            // entry is orange, and a solid field of orange bars reads as a
            // warning rather than as a distribution. Series 1 is the sky blue
            // nearest to the navy Stata fills a histogram with, which is what a
            // reader coming from Stata expects a histogram to look like.
            let fill = series_color(scheme, 1);
            let zero = ys.map(0.0);
            for &(lo, hi, height) in bars {
                let (x0, x1) = (xs.map(lo), xs.map(hi));
                let y = ys.map(height);
                doc.bar(
                    Rect {
                        x: x0,
                        y: y.min(zero),
                        w: x1 - x0,
                        h: (zero - y).abs(),
                    },
                    fill,
                    scheme.plot_background,
                );
            }
        }
        Draw::CatBars(heights) => {
            // Same colour as a histogram's bars, for the same reason: both are
            // one undifferentiated series and a reader should not have to ask
            // what the colour means.
            let fill = series_color(scheme, 1);
            let zero = ys.map(0.0);
            let n = heights.len();
            for (i, h) in heights.iter().enumerate() {
                let Some(v) = h else { continue };
                let centre = slot_centre(plot, i, n);
                let half = plot.w / n as f64 * 0.35;
                let y = ys.map(*v);
                doc.bar(
                    Rect {
                        x: centre - half,
                        y: y.min(zero),
                        w: half * 2.0,
                        h: (zero - y).abs(),
                    },
                    fill,
                    scheme.plot_background,
                );
            }
        }
        Draw::Boxes(boxes) => {
            let fill = scheme.plot_background;
            let ink = scheme.foreground;
            let n = boxes.len();
            for (i, b) in boxes.iter().enumerate() {
                let Some(b) = b else { continue };
                let centre = slot_centre(plot, i, n);
                let half = (plot.w / n as f64 * 0.3).min(24.0);
                let (y25, y50, y75) = (ys.map(b.p25), ys.map(b.p50), ys.map(b.p75));
                // Whisker spines first, so the box paints over their ends.
                doc.rcap(centre, ys.map(b.lower_whisker), y25, half * 0.5, ink, 0.75);
                doc.rcap(centre, y75, ys.map(b.upper_whisker), half * 0.5, ink, 0.75);
                doc.bar(
                    Rect {
                        x: centre - half,
                        y: y75.min(y25),
                        w: half * 2.0,
                        h: (y25 - y75).abs(),
                    },
                    fill,
                    ink,
                );
                doc.line(centre - half, y50, centre + half, y50, ink, 1.1);
                for &o in &b.outside {
                    doc.circle(centre, ys.map(o), MARKER_R, ink);
                }
            }
        }
        Draw::Layers(layers) => {
            for r in layers {
                emit_layer_vector(doc, r, scheme, xs, ys);
            }
        }
    }
}

fn emit_layer_vector(doc: &mut Doc, r: &Resolved, scheme: &'static Scheme, xs: &Scale, ys: &Scale) {
    match &r.mark {
        Mark::Scatter { x, y } => {
            for (&xi, &yi) in x.iter().zip(y.iter()) {
                if drop_pair(xi, yi) {
                    continue;
                }
                doc.circle(xs.map(xi), ys.map(yi), MARKER_R, r.color);
            }
        }
        Mark::Line { x, y } | Mark::Connected { x, y } => {
            let points = sorted_points(x, y, xs, ys);
            doc.polyline(&points, r.color, LINE_W);
            if matches!(r.mark, Mark::Connected { .. }) {
                for &(px, py) in &points {
                    doc.circle(px, py, MARKER_R, r.color);
                }
            }
        }
        Mark::Bar {
            x,
            y,
            base,
            barwidth,
        } => {
            let half = barwidth.unwrap_or(1.0) / 2.0;
            let y0 = ys.map(*base);
            for (&xi, &yi) in x.iter().zip(y.iter()) {
                if drop_pair(xi, yi) {
                    continue;
                }
                let (x0, x1) = (xs.map(xi - half), xs.map(xi + half));
                let y1 = ys.map(yi);
                doc.bar(
                    Rect {
                        x: x0,
                        y: y1.min(y0),
                        w: x1 - x0,
                        h: (y0 - y1).abs(),
                    },
                    r.color,
                    scheme.plot_background,
                );
            }
        }
        Mark::Rcap { x, lo, hi } => {
            for ((&xi, &li), &hi_i) in x.iter().zip(lo.iter()).zip(hi.iter()) {
                if drop_pair(xi, li) || is_missing(hi_i) || !hi_i.is_finite() {
                    continue;
                }
                doc.rcap(xs.map(xi), ys.map(li), ys.map(hi_i), 3.0, r.color, LINE_W);
            }
        }
    }
}

/// `twoway line` connects observations **in x order**, which is what Stata does
/// and the difference between a line and a scribble.
fn sorted_points(x: &[f64], y: &[f64], xs: &Scale, ys: &Scale) -> Vec<(f64, f64)> {
    let mut pairs: Vec<(f64, f64)> = x
        .iter()
        .zip(y.iter())
        .filter(|(&a, &b)| !drop_pair(a, b))
        .map(|(&a, &b)| (a, b))
        .collect();
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    pairs
        .into_iter()
        .map(|(a, b)| (xs.map(a), ys.map(b)))
        .collect()
}

/// The fallback. Returns `false` when the canvas could not be allocated, in
/// which case the caller draws vector and the budget is missed rather than the
/// figure being lost — a too-large figure beats no figure.
fn emit_marks_raster(
    doc: &mut Doc,
    figure: &Figure,
    plot: Rect,
    xs: &Scale,
    ys: &Scale,
    counters: &mut RenderCounters,
) -> bool {
    let Draw::Layers(layers) = &figure.draw else {
        // Aggregate plots emit O(bins) marks and can never reach the budget;
        // reaching here would mean `predict_mark_bytes` disagreed with reality.
        debug_assert!(false, "only twoway layers are ever rasterised");
        return false;
    };
    let Some(mut canvas) = Canvas::new(plot) else {
        return false;
    };

    // Canvas coordinates are relative to the plot region's corner.
    let ox = plot.x;
    let oy = plot.y;
    for r in layers {
        match &r.mark {
            Mark::Scatter { x, y } => {
                for (&xi, &yi) in x.iter().zip(y.iter()) {
                    if drop_pair(xi, yi) {
                        continue;
                    }
                    canvas.disc(xs.map(xi) - ox, ys.map(yi) - oy, MARKER_R, r.color);
                }
            }
            Mark::Line { x, y } | Mark::Connected { x, y } => {
                let points = sorted_points(x, y, xs, ys);
                for pair in points.windows(2) {
                    canvas.segment(
                        pair[0].0 - ox,
                        pair[0].1 - oy,
                        pair[1].0 - ox,
                        pair[1].1 - oy,
                        LINE_W,
                        r.color,
                    );
                }
                if matches!(r.mark, Mark::Connected { .. }) {
                    for &(px, py) in &points {
                        canvas.disc(px - ox, py - oy, MARKER_R, r.color);
                    }
                }
            }
            Mark::Bar {
                x,
                y,
                base,
                barwidth,
            } => {
                let half = barwidth.unwrap_or(1.0) / 2.0;
                let y0 = ys.map(*base);
                for (&xi, &yi) in x.iter().zip(y.iter()) {
                    if drop_pair(xi, yi) {
                        continue;
                    }
                    let (x0, x1) = (xs.map(xi - half), xs.map(xi + half));
                    let y1 = ys.map(yi);
                    canvas.rect(
                        Rect {
                            x: x0 - ox,
                            y: y1.min(y0) - oy,
                            w: x1 - x0,
                            h: (y0 - y1).abs(),
                        },
                        r.color,
                    );
                }
            }
            Mark::Rcap { x, lo, hi } => {
                for ((&xi, &li), &hi_i) in x.iter().zip(lo.iter()).zip(hi.iter()) {
                    if drop_pair(xi, li) || is_missing(hi_i) || !hi_i.is_finite() {
                        continue;
                    }
                    let cx = xs.map(xi) - ox;
                    let (a, b) = (ys.map(li) - oy, ys.map(hi_i) - oy);
                    canvas.segment(cx, a, cx, b, LINE_W, r.color);
                    for yy in [a, b] {
                        canvas.segment(cx - 3.0, yy, cx + 3.0, yy, LINE_W, r.color);
                    }
                }
            }
        }
    }

    counters.raster_pixels = canvas.pixels();
    doc.raster_image(plot, &canvas.into_data_uri());
    true
}

fn emit_legend(
    doc: &mut Doc,
    entries: &[(String, Color)],
    plot: Rect,
    y: f64,
    scheme: &'static Scheme,
) {
    if entries.is_empty() {
        return;
    }
    let widths: Vec<f64> = entries
        .iter()
        .map(|(label, _)| layout::text_width(label, layout::FS_TICK) + 20.0)
        .collect();
    let total: f64 = widths.iter().sum();
    let mut x = plot.x + (plot.w - total) / 2.0;
    let cy = y + layout::LEGEND_ROW / 2.0;
    for ((label, color), w) in entries.iter().zip(widths.iter()) {
        doc.circle(x + 5.0, cy, MARKER_R + 0.4, *color);
        doc.text(
            x + 13.0,
            cy + layout::FS_TICK * 0.35,
            Anchor::Start,
            layout::FS_TICK,
            scheme.text,
            label,
        );
        x += w;
    }
}
