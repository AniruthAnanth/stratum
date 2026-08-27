//! What a render did, as numbers — DECISIONS.md ADR-017.
//!
//! "A performance acceptance bullet must assert a *counter* — work done,
//! allocations, regions re-hashed, bytes copied — and not a duration."
//!
//! So this crate's performance claims are these six fields and nothing else. The
//! claim "a histogram of ten million observations is the same forty rectangles
//! as a histogram of forty" is `marks_emitted == bins` in both cases; the claim
//! "no interaction path walks the frame twice" is `data_passes <= 2`. Neither
//! sentence needs a stopwatch, and neither one moves when the machine is busy.

/// Per-render instrumentation. Returned with every figure, asserted by tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RenderCounters {
    /// Times the renderer walked an input series end to end.
    ///
    /// The design note's central efficiency claim: **at most 2** for every plot
    /// kind. Pass 1 computes the domain (and, for an aggregate plot, the
    /// aggregate); pass 2 emits. A third pass would mean a 10 M-row scatter read
    /// three times to draw one figure.
    pub data_passes: u32,
    /// Observations handed to the renderer, summed over layers.
    pub points_input: u64,
    /// Observations dropped by the missing-value rule (§3.2 of the note).
    /// `points_input - points_dropped` is what the figure represents, and the
    /// difference is why a scatter can have fewer points than the frame has rows.
    pub points_dropped: u64,
    /// Drawing primitives written **in the mark layer** — circles, path
    /// vertices, rectangles, caps.
    ///
    /// The figure ground, the axes, the ticks and the legend are deliberately
    /// NOT counted. They are fixed furniture whose size depends on how many
    /// ticks the axis wanted, and including them would make the claim below
    /// false by a variable amount for a reason that has nothing to do with the
    /// data.
    ///
    /// For `histogram`, `graph box` and `graph bar` this is `O(bins)` /
    /// `O(groups)` and independent of the observation count. When the raster
    /// fallback fires it is `1`, because the whole mark layer became one
    /// `<image>`.
    pub marks_emitted: u64,
    /// Final size of the SVG document.
    pub svg_bytes: u64,
    /// Device pixels in the embedded raster layer; `0` when the figure is
    /// entirely vector, which is the ordinary case.
    pub raster_pixels: u64,
}

impl RenderCounters {
    /// Whether the mark layer was rasterised (design note §7).
    #[must_use]
    pub fn rasterized(&self) -> bool {
        self.raster_pixels > 0
    }
}
