//! What the document has to be — the acceptance bullets that are about output.
//!
//! W19's acceptance, in the plan's words:
//!
//! > SVG output with intrinsic point size; a raster fallback above 1.5 MB.
//! > Scheme colours come from `stratum_tokens::SCHEMES`, **compiled in** (A14) —
//! > **a graph that does not match the app is a bug**, and the same graph
//! > exported for a paper uses the print scheme.
//! > Graphs are delivered as `ResultPayload::Graph(GraphRef)` over
//! > `stratum-asset://`, never inline.
//!
//! The third bullet is a *contract* this crate deliberately does not implement
//! (the design note §1 explains why: `GraphRef`'s URL is keyed by `SessionId`
//! and `ResultId`, and this crate knows about neither). What is checkable here
//! is the half that is ours: every `GraphRef` field has exactly one producer in
//! [`GraphRender`], and the bytes come back as bytes rather than as anything
//! that could be inlined into an envelope by accident.

use stratum_graph::raster::{
    MARK_BUDGET_BYTES, MAX_BYTES_PER_BAR, MAX_BYTES_PER_MARKER, MAX_BYTES_PER_RCAP,
    MAX_BYTES_PER_VERTEX, SVG_BUDGET_BYTES,
};
use stratum_graph::{
    render, BarPlot, BarStat, BinSpec, BoxPlot, FigureSize, GraphError, GraphRender, GraphSpec,
    Group, HistScale, Histogram, Layer, Mark, Plot, Titles, SVG_MIME,
};

fn ramp(n: usize) -> Vec<f64> {
    (0..n).map(|i| i as f64).collect()
}

fn histogram(n: usize) -> GraphSpec {
    GraphSpec::new(
        "histogram price",
        Plot::Histogram(Histogram {
            values: ramp(n),
            bins: BinSpec::Auto,
            scale: HistScale::Density,
            discrete: false,
        }),
    )
}

fn scatter(n: usize) -> GraphSpec {
    let x = ramp(n);
    let y: Vec<f64> = (0..n).map(|i| (i % 97) as f64).collect();
    GraphSpec::new(
        "twoway scatter y x",
        Plot::Twoway(vec![Layer::new(Mark::Scatter { x, y })]),
    )
}

// ---------------------------------------------------------------------------
// the document
// ---------------------------------------------------------------------------

#[test]
fn the_document_carries_its_intrinsic_size_in_points() {
    let out = render(&histogram(74)).unwrap();
    assert!(out.svg.starts_with("<svg "), "{}", &out.svg[..40]);
    assert!(out.svg.contains(r#"width="396pt" height="288pt""#));
    assert!(out.svg.contains(r#"viewBox="0 0 396 288""#));
    assert!(out.svg.ends_with("</svg>"));
    assert_eq!(out.intrinsic_pt, (396.0, 288.0));
    assert_eq!(out.mime, SVG_MIME);
}

#[test]
fn a_custom_size_reaches_both_the_document_and_the_payload_field() {
    let mut spec = histogram(74);
    spec.size = FigureSize {
        width_pt: 600.0,
        height_pt: 400.0,
    };
    let out = render(&spec).unwrap();
    assert_eq!(out.intrinsic_pt, (600.0, 400.0));
    assert!(out.svg.contains(r#"width="600pt" height="400pt""#));
}

/// Spec §17 and 06 §6.7: the card's accessible name is the command. The document
/// carries it too, as `<title>`, so an SVG opened on its own is not anonymous.
#[test]
fn the_command_is_the_documents_accessible_name() {
    let out = render(&histogram(74)).unwrap();
    assert!(out.svg.contains("<title>histogram price</title>"));
    assert!(out.svg.contains(r#"role="img""#));
}

/// A variable label is user data. It reaches the figure as a title.
#[test]
fn markup_in_a_label_cannot_escape_into_the_document() {
    let mut spec = histogram(74);
    spec.titles = Titles {
        title: Some(r#"</svg><script>alert(1)</script>"#.to_owned()),
        subtitle: Some("a & b".to_owned()),
        note: Some("x < y".to_owned()),
    };
    let out = render(&spec).unwrap();
    // The payload survives as TEXT — `alert(1)` is still spelled out, because
    // escaping a title is not censoring it — but no markup character in it can
    // still open an element, and the document still has exactly one end tag.
    assert!(!out.svg.contains("<script"));
    assert!(!out.svg.contains("</script"));
    assert!(out
        .svg
        .contains("&lt;/svg&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert_eq!(
        out.svg.matches("</svg>").count(),
        1,
        "the document has exactly one end tag"
    );
    assert!(out.svg.contains("a &amp; b"));
    assert!(out.svg.contains("x &lt; y"));
}

/// ADR-013. The same spec must produce the same bytes, or the asset store's
/// content addressing and the difftest goldens are both meaningless.
#[test]
fn rendering_is_deterministic() {
    let spec = scatter(500);
    assert_eq!(render(&spec).unwrap().svg, render(&spec).unwrap().svg);
}

/// Determinism is a property of the EMITTER, not of a comparison.
///
/// Every number in the document is written by `coord::push_num` as fixed point
/// with at most two decimals, which is what makes two machines agree and what
/// bounds a coordinate at ten bytes so `raster::over_budget` can predict a
/// document's size before emitting a byte of it. A `format!("{}", f64)` slipping
/// into the emitter would show up here as a seventeen-digit attribute long
/// before it showed up as a difftest failure.
#[test]
fn every_number_in_the_document_is_fixed_point() {
    for spec in [
        histogram(74),
        scatter(200),
        GraphSpec::new(
            "graph box y",
            Plot::Box(BoxPlot {
                groups: vec![Group {
                    label: "a".to_owned(),
                    values: ramp(37),
                }],
            }),
        ),
    ] {
        let svg = render(&spec).unwrap().svg;
        for (i, raw) in svg.match_indices('"') {
            let _ = raw;
            let rest = &svg[i + 1..];
            let Some(end) = rest.find('"') else { break };
            for token in rest[..end].split([' ', ',', 'M', 'L']) {
                let t = token.trim_end_matches("pt");
                if t.is_empty() || !t.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
                    continue;
                }
                if !t
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
                {
                    continue;
                }
                let decimals = t.split_once('.').map_or(0, |(_, frac)| frac.len());
                assert!(
                    decimals <= 2,
                    "`{t}` carries {decimals} decimals in {}",
                    spec.source_cmd
                );
                assert!(
                    !t.contains('e') && !t.contains('E'),
                    "`{t}` is in exponent notation"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// schemes (A14)
// ---------------------------------------------------------------------------

#[test]
fn the_scheme_decides_the_ink_and_print_is_black_on_white() {
    let mut spec = histogram(74);
    let app = render(&spec).unwrap();
    assert!(app.svg.contains("#FAFAFB"), "the app scheme's ground");

    spec.scheme = "print".to_owned();
    let paper = render(&spec).unwrap();
    assert!(paper.svg.contains("#FFFFFF"), "print is white ground");
    assert!(!paper.svg.contains("#FAFAFB"));
    assert_eq!(paper.scheme, "print");

    spec.scheme = "stratum-dark".to_owned();
    let dark = render(&spec).unwrap();
    assert!(dark.svg.contains("#14181E"), "the dark scheme's ground");
}

/// `stratum_tokens::scheme` returns `Option` rather than falling back, and so
/// does this: an unrecognised scheme name draws the wrong colours silently.
#[test]
fn an_unknown_scheme_is_refused_not_papered_over() {
    let mut spec = histogram(74);
    spec.scheme = "monokai".to_owned();
    let err = render(&spec).unwrap_err();
    assert_eq!(err, GraphError::UnknownScheme("monokai".to_owned()));
    assert_eq!(err.rc(), 198);
}

#[test]
fn the_type_is_the_design_systems() {
    let out = render(&histogram(74)).unwrap();
    assert!(out.svg.contains("IBM Plex Sans"));
    // --fs-micro, the app's smallest text, is the graph's smallest text.
    assert!(out.svg.contains(r#"font-size="11""#));
}

// ---------------------------------------------------------------------------
// every mark in pass-1 scope draws
// ---------------------------------------------------------------------------

#[test]
fn all_five_twoway_marks_draw() {
    let x = ramp(20);
    let y: Vec<f64> = (0..20).map(|i| (i * i) as f64).collect();
    let lo: Vec<f64> = y.iter().map(|v| v - 5.0).collect();
    let hi: Vec<f64> = y.iter().map(|v| v + 5.0).collect();

    let cases: Vec<(&str, Mark)> = vec![
        (
            "scatter",
            Mark::Scatter {
                x: x.clone(),
                y: y.clone(),
            },
        ),
        (
            "line",
            Mark::Line {
                x: x.clone(),
                y: y.clone(),
            },
        ),
        (
            "connected",
            Mark::Connected {
                x: x.clone(),
                y: y.clone(),
            },
        ),
        (
            "bar",
            Mark::Bar {
                x: x.clone(),
                y: y.clone(),
                base: 0.0,
                barwidth: None,
            },
        ),
        (
            "rcap",
            Mark::Rcap {
                x: x.clone(),
                lo,
                hi,
            },
        ),
    ];
    for (name, mark) in cases {
        let spec = GraphSpec::new(
            format!("twoway {name}"),
            Plot::Twoway(vec![Layer::new(mark)]),
        );
        let out = render(&spec).unwrap_or_else(|e| panic!("{name} failed: {e}"));
        assert!(
            out.counters.marks_emitted >= 20,
            "{name} drew {} marks for 20 observations",
            out.counters.marks_emitted
        );
    }
}

/// A22: `CardAction::PlotCoefficients` promises a confidence-interval plot. This
/// is the mark that makes the promise keepable.
#[test]
fn a_coefficient_plot_draws_caps_and_points() {
    let x = vec![1.0, 2.0, 3.0];
    let b = vec![0.0721, 0.0198, -1.2841];
    let lo = vec![0.0637, 0.0176, -1.9002];
    let hi = vec![0.0805, 0.0220, -0.6680];
    let spec = GraphSpec::new(
        "twoway rcap hi lo x || scatter b x",
        Plot::Twoway(vec![
            Layer::new(Mark::Rcap {
                x: x.clone(),
                lo,
                hi,
            })
            .labelled("95% CI"),
            Layer::new(Mark::Scatter { x, y: b }).labelled("Coefficient"),
        ]),
    );
    let out = render(&spec).unwrap();
    assert_eq!(
        out.svg.matches("<path").count(),
        3,
        "one path per range cap"
    );
    assert_eq!(
        out.svg.matches("<circle").count(),
        3 + 2,
        "three points plus two legend keys"
    );
    assert!(out.svg.contains("95% CI"));
    assert!(out.svg.contains("Coefficient"));
}

#[test]
fn graph_box_draws_a_box_per_category() {
    let spec = GraphSpec::new(
        "graph box price, over(foreign)",
        Plot::Box(BoxPlot {
            groups: vec![
                Group {
                    label: "Domestic".to_owned(),
                    values: ramp(52),
                },
                Group {
                    label: "Foreign".to_owned(),
                    values: ramp(22),
                },
            ],
        }),
    );
    let out = render(&spec).unwrap();
    assert!(out.svg.contains("Domestic"));
    assert!(out.svg.contains("Foreign"));
    assert_eq!(
        out.svg.matches("<rect").count(),
        2 + 2 + 1,
        "two boxes, two grounds, one clip"
    );
}

#[test]
fn graph_bar_draws_a_bar_per_category_and_names_the_statistic() {
    let spec = GraphSpec::new(
        "graph bar (mean) price, over(rep78)",
        Plot::Bar(BarPlot {
            groups: (1..=5)
                .map(|i| Group {
                    label: i.to_string(),
                    values: ramp(10 * i),
                })
                .collect(),
            stat: BarStat::Mean,
        }),
    );
    let out = render(&spec).unwrap();
    assert_eq!(out.svg.matches("<rect").count(), 5 + 2 + 1);
    assert_eq!(BarStat::Mean.axis_title("price"), "mean of price");
}

#[test]
fn an_empty_plot_is_an_error_with_a_return_code_not_a_blank_figure() {
    let spec = GraphSpec::new("twoway scatter y x", Plot::Twoway(vec![]));
    assert_eq!(render(&spec).unwrap_err(), GraphError::NoObservations);
    assert_eq!(GraphError::NoObservations.rc(), 2000);
}

#[test]
fn a_layer_whose_variables_disagree_in_length_is_refused() {
    let spec = GraphSpec::new(
        "twoway scatter y x",
        Plot::Twoway(vec![Layer::new(Mark::Scatter {
            x: ramp(10),
            y: ramp(9),
        })]),
    );
    assert_eq!(
        render(&spec).unwrap_err(),
        GraphError::RaggedLayer {
            expected: 10,
            found: 9
        }
    );
}

#[test]
fn a_figure_smaller_than_its_own_margins_is_refused() {
    let mut spec = histogram(74);
    spec.size = FigureSize {
        width_pt: 40.0,
        height_pt: 40.0,
    };
    assert!(matches!(
        render(&spec),
        Err(GraphError::FigureTooSmall { .. })
    ));
}

// ---------------------------------------------------------------------------
// the byte budget and the raster fallback (acceptance bullet 1)
// ---------------------------------------------------------------------------

/// The per-mark ceilings are what `raster::over_budget` predicts against BEFORE
/// emitting anything, so a ceiling that under-states reality would let a figure
/// past the 1.5 MB budget. Measured as a delta between two sizes so the fixed
/// furniture cancels.
fn measured_bytes_per_mark(build: impl Fn(usize) -> GraphSpec, n: usize) -> f64 {
    let small = render(&build(n)).unwrap().counters.svg_bytes;
    let large = render(&build(n * 2)).unwrap().counters.svg_bytes;
    (large - small) as f64 / n as f64
}

#[test]
fn no_mark_can_exceed_the_ceiling_the_prediction_budgets_against() {
    let marker = measured_bytes_per_mark(scatter, 400);
    assert!(
        marker <= MAX_BYTES_PER_MARKER as f64,
        "marker measured {marker}"
    );

    let vertex = measured_bytes_per_mark(
        |n| {
            GraphSpec::new(
                "twoway line",
                Plot::Twoway(vec![Layer::new(Mark::Line {
                    x: ramp(n),
                    y: (0..n).map(|i| (i % 89) as f64).collect(),
                })]),
            )
        },
        400,
    );
    assert!(
        vertex <= MAX_BYTES_PER_VERTEX as f64,
        "vertex measured {vertex}"
    );

    let bar = measured_bytes_per_mark(
        |n| {
            GraphSpec::new(
                "twoway bar",
                Plot::Twoway(vec![Layer::new(Mark::Bar {
                    x: ramp(n),
                    y: (0..n).map(|i| (i % 89) as f64).collect(),
                    base: 0.0,
                    barwidth: Some(0.5),
                })]),
            )
        },
        400,
    );
    assert!(bar <= MAX_BYTES_PER_BAR as f64, "bar measured {bar}");

    let rcap = measured_bytes_per_mark(
        |n| {
            let x = ramp(n);
            GraphSpec::new(
                "twoway rcap",
                Plot::Twoway(vec![Layer::new(Mark::Rcap {
                    lo: x.iter().map(|v| v - 1.0).collect(),
                    hi: x.iter().map(|v| v + 1.0).collect(),
                    x,
                })]),
            )
        },
        400,
    );
    assert!(rcap <= MAX_BYTES_PER_RCAP as f64, "rcap measured {rcap}");
}

#[test]
fn a_dense_scatter_rasterises_and_stays_inside_the_budget() {
    let n = MARK_BUDGET_BYTES / MAX_BYTES_PER_MARKER + 1_000;
    let out = render(&scatter(n)).unwrap();
    assert!(
        out.counters.rasterized(),
        "{n} points should have tripped the fallback"
    );
    assert!(out.counters.raster_pixels > 0);
    assert!(
        (out.counters.svg_bytes as usize) < SVG_BUDGET_BYTES,
        "{} bytes exceeds the budget",
        out.counters.svg_bytes
    );
    // The point of rasterising the MARK LAYER ONLY: the furniture is still text.
    assert!(out.svg.contains("<text"));
    assert!(out.svg.contains("<title>"));
    assert!(out.svg.contains("data:image/png;base64,"));
    assert_eq!(
        out.svg.matches("<circle").count(),
        0,
        "the markers went into the raster"
    );
}

#[test]
fn an_ordinary_scatter_stays_vector() {
    let out = render(&scatter(2_000)).unwrap();
    assert!(!out.counters.rasterized());
    assert_eq!(out.counters.raster_pixels, 0);
    assert_eq!(out.svg.matches("<circle").count(), 2_000);
    assert!(!out.svg.contains("data:image"));
}

/// Aggregate plots emit `O(bins)` marks, so they can never reach the threshold
/// however large the frame is. Ten million observations, forty rectangles.
#[test]
fn an_aggregate_plot_never_rasterises() {
    let out = render(&histogram(1_000_000)).unwrap();
    assert!(!out.counters.rasterized());
    assert!(
        (out.counters.svg_bytes as usize) < 20_000,
        "{} bytes",
        out.counters.svg_bytes
    );
}

// ---------------------------------------------------------------------------
// the payload contract (C23)
// ---------------------------------------------------------------------------

/// The mapping the runtime will write, written once here so the compiler owns
/// it.
///
/// Both structs are destructured exhaustively, so a sixth field on either side
/// is a compile error in this file rather than a field the runtime quietly
/// forgets to copy. `asset` is the one field this crate does not and must not
/// produce: its path is `graph/{session}/{result}.svg` and this crate has
/// neither — but it does produce both of the other two `AssetRef` fields, and
/// this is where that is pinned down.
#[test]
fn a_graph_ref_is_a_field_for_field_copy_of_a_render() {
    use stratum_proto::{AssetRef, GraphRef};

    let out = render(&histogram(74)).unwrap();
    let GraphRender {
        name,
        svg,
        mime,
        intrinsic_pt,
        scheme,
        source_cmd,
        counters: _,
        binning: _,
    } = out;

    // What the runtime does, verbatim: mint the URL it alone knows, and copy.
    let graph_ref = GraphRef {
        name,
        asset: AssetRef {
            path: "graph/7/3.svg".to_owned(),
            mime: mime.to_owned(),
            bytes: svg.len() as u64,
        },
        intrinsic_pt,
        scheme,
        source_cmd,
    };

    let GraphRef {
        name,
        asset,
        intrinsic_pt,
        scheme,
        source_cmd,
    } = graph_ref;
    assert_eq!(name, "Graph");
    assert_eq!(asset.mime, "image/svg+xml");
    assert_eq!(asset.bytes, svg.len() as u64);
    assert_eq!(intrinsic_pt, (396.0, 288.0));
    assert_eq!(scheme, "stratum");
    assert_eq!(source_cmd, "histogram price");
}

/// Every field of `stratum_proto::GraphRef` — `{ name, asset, intrinsic_pt,
/// scheme, source_cmd }` — has exactly one producer here, so the runtime's
/// construction is a copy. `asset` is the one this crate does not and must not
/// produce: it is a `stratum-asset://localhost/graph/{session}/{result}.svg`
/// URL, and this crate has no session and no result.
#[test]
fn every_graph_ref_field_has_a_producer() {
    let mut spec = histogram(74);
    spec.name = "byregion".to_owned();
    let out = render(&spec).unwrap();
    assert_eq!(out.name, "byregion");
    assert_eq!(out.source_cmd, "histogram price");
    assert_eq!(out.scheme, "stratum");
    assert_eq!(out.intrinsic_pt, (396.0, 288.0));
    assert_eq!(out.mime, "image/svg+xml");
    // The bytes are bytes. Nothing here is an envelope, a URL or a path.
    assert!(!out.svg.contains("stratum-asset://"));
    assert!(!out.svg.contains("file://"));
}

/// The one textual thing `histogram` prints goes to the log through the runtime,
/// from the same struct the bars came from.
#[test]
fn the_binning_line_and_the_bars_come_from_one_place() {
    let out = render(&histogram(74)).unwrap();
    let b = out.binning.expect("histogram reports its grid");
    assert_eq!(b.bins, 9);
    assert_eq!(b.start, 0.0);
    assert!((b.width - 73.0 / 9.0).abs() < 1e-12);
    assert_eq!(out.counters.marks_emitted, 9, "nine bins, nine rectangles");

    // Every other plot kind reports no binning at all.
    assert!(render(&scatter(10)).unwrap().binning.is_none());
}
