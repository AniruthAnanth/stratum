//! DECISIONS.md ADR-017 — the performance claims, as counters.
//!
//! > A performance acceptance bullet must assert a *counter* — work done,
//! > allocations, regions re-hashed, bytes copied — and not a duration.
//! > Durations may be recorded.
//!
//! So there is no `Instant` in this file. Every claim the design note makes
//! about this crate's cost is one of the six numbers in `RenderCounters`, and
//! each one is asserted below against an input a hundred times bigger than the
//! one beside it — because the shape of the claim is "independent of N", and a
//! single measurement cannot express that.

use stratum_core::missing::{missing_f64, SYSMISS};
use stratum_graph::{
    render, BarPlot, BarStat, BinSpec, BoxPlot, GraphSpec, Group, HistScale, Histogram, Layer,
    Mark, Plot,
};

fn ramp(n: usize) -> Vec<f64> {
    (0..n).map(|i| i as f64).collect()
}

fn histogram(n: usize) -> GraphSpec {
    GraphSpec::new(
        "histogram x",
        Plot::Histogram(Histogram {
            values: ramp(n),
            bins: BinSpec::Bins(20),
            scale: HistScale::Density,
            discrete: false,
        }),
    )
}

/// A scatter of `n` points drawn from a **fixed** distribution: x cycles 0..999
/// and y cycles 0..96 whatever `n` is.
///
/// The bounded domain is load-bearing, not incidental. Two scatters of different
/// `n` over `0..n` have different axis labels, so different margins, so a
/// different plot region — and a claim of the form "this number does not depend
/// on N" would then be comparing two differently shaped figures. Fixing the
/// distribution makes the comparison exact.
fn scatter(n: usize) -> GraphSpec {
    GraphSpec::new(
        "twoway scatter y x",
        Plot::Twoway(vec![Layer::new(Mark::Scatter {
            x: (0..n).map(|i| (i % 1_000) as f64).collect(),
            y: (0..n).map(|i| (i % 97) as f64).collect(),
        })]),
    )
}

/// A histogram of `n` observations drawn from the **same twenty-level
/// distribution** however large `n` is: every bin holds `n/20` of them, so the
/// bin grid, every bar height and therefore every byte of the document are
/// identical across `n`. See [`scatter`] for why that matters.
fn tiled_histogram(n: usize) -> GraphSpec {
    GraphSpec::new(
        "histogram x",
        Plot::Histogram(Histogram {
            values: (0..n).map(|i| (i % 20) as f64).collect(),
            bins: BinSpec::Bins(20),
            scale: HistScale::Density,
            discrete: false,
        }),
    )
}

fn boxplot(n: usize) -> GraphSpec {
    GraphSpec::new(
        "graph box y, over(g)",
        Plot::Box(BoxPlot {
            groups: (0..4)
                .map(|i| Group {
                    label: i.to_string(),
                    values: ramp(n),
                })
                .collect(),
        }),
    )
}

fn barchart(n: usize) -> GraphSpec {
    GraphSpec::new(
        "graph bar (mean) y, over(g)",
        Plot::Bar(BarPlot {
            groups: (0..4)
                .map(|i| Group {
                    label: i.to_string(),
                    values: ramp(n),
                })
                .collect(),
            stat: BarStat::Mean,
        }),
    )
}

/// The design note's central efficiency claim. A third walk over a ten-million-
/// row frame to draw one figure is the thing this number exists to stop.
#[test]
fn no_plot_kind_walks_the_data_more_than_twice() {
    for spec in [
        histogram(10_000),
        scatter(10_000),
        boxplot(10_000),
        barchart(10_000),
    ] {
        let out = render(&spec).unwrap();
        assert!(
            out.counters.data_passes <= 2,
            "{} took {} passes",
            out.source_cmd,
            out.counters.data_passes
        );
        assert!(
            out.counters.data_passes >= 1,
            "a figure that read nothing is a bug too"
        );
    }
}

/// "A histogram of ten million observations is the same forty rectangles as a
/// histogram of forty."
#[test]
fn an_aggregate_plots_mark_count_is_independent_of_n() {
    let small = render(&tiled_histogram(100)).unwrap();
    let large = render(&tiled_histogram(1_000_000)).unwrap();
    assert_eq!(small.counters.marks_emitted, 20, "one rectangle per bin");
    assert_eq!(small.counters.marks_emitted, large.counters.marks_emitted);
    // Ten thousand times the data, byte for byte the same figure.
    assert_eq!(small.counters.svg_bytes, large.counters.svg_bytes);
    assert_eq!(small.svg, large.svg);
    assert_eq!(large.counters.points_input, 1_000_000);
}

#[test]
fn a_box_plots_mark_count_is_the_number_of_groups_not_observations() {
    let small = render(&boxplot(50)).unwrap();
    let large = render(&boxplot(500_000)).unwrap();
    assert_eq!(small.counters.marks_emitted, large.counters.marks_emitted);
    assert_eq!(large.counters.points_input, 4 * 500_000);
}

#[test]
fn a_bar_charts_mark_count_is_the_number_of_groups() {
    let out = render(&barchart(100_000)).unwrap();
    assert_eq!(
        out.counters.marks_emitted, 4,
        "four groups, four rectangles"
    );
    assert_eq!(out.counters.points_input, 4 * 100_000);
}

/// The missing-value rule, visibly. `points_input - points_dropped` is what the
/// figure represents, which is the answer to "why does my scatter have fewer
/// points than my dataset has rows?".
#[test]
fn dropped_observations_are_counted_not_silently_absorbed() {
    let x = vec![1.0, 2.0, SYSMISS, 4.0, 5.0];
    let y = vec![1.0, missing_f64(b'a'), 3.0, 4.0, 5.0];
    let spec = GraphSpec::new(
        "twoway scatter y x",
        Plot::Twoway(vec![Layer::new(Mark::Scatter { x, y })]),
    );
    let out = render(&spec).unwrap();
    assert_eq!(out.counters.points_input, 5);
    // Pairwise: obs 2 is missing on y, obs 3 on x. Three pairs survive.
    assert_eq!(out.counters.points_dropped, 2);
    assert_eq!(out.svg.matches("<circle").count(), 3);
}

#[test]
fn a_histograms_dropped_count_is_its_own() {
    let mut values = ramp(90);
    values.extend(std::iter::repeat_n(SYSMISS, 10));
    let spec = GraphSpec::new(
        "histogram x",
        Plot::Histogram(Histogram {
            values,
            bins: BinSpec::Bins(9),
            scale: HistScale::Frequency,
            discrete: false,
        }),
    );
    let out = render(&spec).unwrap();
    assert_eq!(out.counters.points_input, 100);
    assert_eq!(out.counters.points_dropped, 10);
}

/// `svg_bytes` is the 1.5 MB budget expressed as a number rather than as a hope.
#[test]
fn svg_bytes_is_the_documents_real_length() {
    for spec in [
        histogram(1_000),
        scatter(1_000),
        boxplot(1_000),
        barchart(1_000),
    ] {
        let out = render(&spec).unwrap();
        assert_eq!(out.counters.svg_bytes as usize, out.svg.len());
    }
}

#[test]
fn raster_pixels_is_zero_exactly_when_the_figure_is_vector() {
    let vector = render(&scatter(1_000)).unwrap();
    assert_eq!(vector.counters.raster_pixels, 0);
    assert!(!vector.counters.rasterized());

    let raster = render(&scatter(60_000)).unwrap();
    assert!(raster.counters.raster_pixels > 0);
    assert!(raster.counters.rasterized());
    // The whole mark layer collapses to one `<image>`.
    assert_eq!(raster.counters.marks_emitted, 1);
    // At 2x device scale the buffer is four pixels per square point of the plot
    // region — and it does not grow with N, which is the whole point.
    let bigger = render(&scatter(400_000)).unwrap();
    assert_eq!(raster.counters.raster_pixels, bigger.counters.raster_pixels);
    // The documents are NOT byte-identical — 400 000 points cover more of the
    // canvas than 60 000 do, so the PNG genuinely differs — but neither the
    // buffer nor the budget notices how many points went into it.
    assert_eq!(bigger.counters.marks_emitted, 1);
    for out in [&raster, &bigger] {
        assert!(
            (out.counters.svg_bytes as usize) < stratum_graph::raster::SVG_BUDGET_BYTES,
            "{} bytes",
            out.counters.svg_bytes
        );
    }
}
