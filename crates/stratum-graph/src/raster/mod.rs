//! The raster fallback — design note §7, acceptance bullet 1.
//!
//! ADR-007 sizes a graph at "up to 1.5 MB of SVG", and above that the
//! acceptance asks for a raster fallback. **What is rasterised here is the mark
//! layer only.** Axes, ticks, labels, titles, legend and the document's
//! `<title>` stay vector, so the figure that triggered the fallback — the dense
//! one, the one somebody is squinting at — keeps legible, selectable,
//! screen-reader-reachable text. A whole-figure bitmap would also need a font
//! rasteriser, which ARCHITECTURE §8.14 forbids this crate the filesystem to
//! have.
//!
//! The decision is made **before** anything is emitted, by comparing
//! `marks × MAX_BYTES_PER_*` against the budget. That prediction is safe rather
//! than optimistic because [`crate::coord`] bounds a coordinate at ten bytes and
//! `tests/render.rs` asserts the per-mark ceilings against real output at
//! extreme coordinates.

pub mod base64;
pub mod deflate;
pub mod png;

use crate::layout::Rect;
use stratum_tokens::{Color, Rgb};

/// ADR-007's ceiling.
pub const SVG_BUDGET_BYTES: usize = 1_500_000;

/// Reserved for everything that is not a mark: the document header, the plot
/// frame, up to 32 ticks a side with labels, three titles and a legend. Measured
/// at ~4 KB for a busy figure; the reservation is eight times that because
/// under-reserving here is what would let a figure sneak past the budget.
pub const FURNITURE_BYTES: usize = 32_768;

/// What the marks may spend.
pub const MARK_BUDGET_BYTES: usize = SVG_BUDGET_BYTES - FURNITURE_BYTES;

/// Upper bound on the bytes one `<circle>` marker can occupy.
pub const MAX_BYTES_PER_MARKER: usize = 72;
/// Upper bound on the bytes one polyline vertex can occupy.
pub const MAX_BYTES_PER_VERTEX: usize = 24;
/// Upper bound on the bytes one stroked `<rect>` bar can occupy.
pub const MAX_BYTES_PER_BAR: usize = 136;
/// Upper bound on the bytes one `<path>` range-cap can occupy.
pub const MAX_BYTES_PER_RCAP: usize = 224;

/// Device pixels per user unit in the raster layer. Two, not one: the layer is
/// resampled by the browser to whatever the card's width is, and §0a is explicit
/// that a larger artifact is the acceptable price of the better result.
pub const RASTER_SCALE: f64 = 2.0;

/// A hard ceiling on the raster buffer, so a pathological figure size cannot ask
/// for a gigabyte. 8 M pixels is a 2000 × 4000 plot region at 2×.
pub const MAX_RASTER_PIXELS: usize = 8_000_000;

/// Whether the mark layer must be rasterised, given its predicted size.
#[must_use]
pub fn over_budget(predicted_mark_bytes: usize) -> bool {
    predicted_mark_bytes > MARK_BUDGET_BYTES
}

/// An RGBA8 canvas with source-over compositing and analytic coverage.
///
/// Coordinates are user units **relative to the plot region's top-left corner**;
/// the canvas applies the scale. Callers therefore never see device pixels,
/// which is what lets the vector and raster paths share one set of scale
/// mappings.
pub struct Canvas {
    w: usize,
    h: usize,
    scale: f64,
    px: Vec<u8>,
}

impl Canvas {
    /// A transparent canvas covering `region` at [`RASTER_SCALE`].
    ///
    /// `None` when the region would exceed [`MAX_RASTER_PIXELS`] or has no area.
    #[must_use]
    pub fn new(region: Rect) -> Option<Canvas> {
        let w = (region.w * RASTER_SCALE).ceil().max(0.0) as usize;
        let h = (region.h * RASTER_SCALE).ceil().max(0.0) as usize;
        if w == 0 || h == 0 || w.saturating_mul(h) > MAX_RASTER_PIXELS {
            return None;
        }
        Some(Canvas {
            w,
            h,
            scale: RASTER_SCALE,
            px: vec![0u8; w * h * 4],
        })
    }

    /// Device pixels in the buffer — `RenderCounters::raster_pixels`.
    #[must_use]
    pub fn pixels(&self) -> u64 {
        (self.w * self.h) as u64
    }

    fn blend(&mut self, x: usize, y: usize, c: Rgb, a: f32) {
        if a <= 0.0 || x >= self.w || y >= self.h {
            return;
        }
        let a = a.min(1.0);
        let i = (y * self.w + x) * 4;
        let da = f32::from(self.px[i + 3]) / 255.0;
        let na = a + da * (1.0 - a);
        if na <= 0.0 {
            return;
        }
        for (k, s) in [c.r, c.g, c.b].into_iter().enumerate() {
            let src = f32::from(s);
            let dst = f32::from(self.px[i + k]);
            self.px[i + k] = ((src * a + dst * da * (1.0 - a)) / na)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
        self.px[i + 3] = (na * 255.0).round().clamp(0.0, 255.0) as u8;
    }

    /// A filled disc — the scatter marker.
    pub fn disc(&mut self, cx: f64, cy: f64, r: f64, color: Color) {
        let (cx, cy, r) = (cx * self.scale, cy * self.scale, r * self.scale);
        let x0 = (cx - r - 1.0).floor().max(0.0) as usize;
        let y0 = (cy - r - 1.0).floor().max(0.0) as usize;
        let x1 = ((cx + r + 1.0).ceil().max(0.0) as usize).min(self.w);
        let y1 = ((cy + r + 1.0).ceil().max(0.0) as usize).min(self.h);
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = x as f64 + 0.5 - cx;
                let dy = y as f64 + 0.5 - cy;
                // Analytic-ish coverage: the signed distance to the edge,
                // clamped to one pixel of feather. Exact enough for a 3-pixel
                // marker and branch-free.
                let d = (dx * dx + dy * dy).sqrt();
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                self.blend(x, y, color.rgb, cov as f32);
            }
        }
    }

    /// A stroked line segment of total width `width`.
    pub fn segment(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, width: f64, color: Color) {
        let (x1, y1) = (x1 * self.scale, y1 * self.scale);
        let (x2, y2) = (x2 * self.scale, y2 * self.scale);
        let hw = (width * self.scale / 2.0).max(0.35);
        let x0 = (x1.min(x2) - hw - 1.0).floor().max(0.0) as usize;
        let y0 = (y1.min(y2) - hw - 1.0).floor().max(0.0) as usize;
        let xe = ((x1.max(x2) + hw + 1.0).ceil().max(0.0) as usize).min(self.w);
        let ye = ((y1.max(y2) + hw + 1.0).ceil().max(0.0) as usize).min(self.h);
        let (dx, dy) = (x2 - x1, y2 - y1);
        let len2 = dx * dx + dy * dy;
        for y in y0..ye {
            for x in x0..xe {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let t = if len2 > 0.0 {
                    (((px - x1) * dx + (py - y1) * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (qx, qy) = (x1 + t * dx, y1 + t * dy);
                let d = ((px - qx) * (px - qx) + (py - qy) * (py - qy)).sqrt();
                let cov = (hw + 0.5 - d).clamp(0.0, 1.0);
                self.blend(x, y, color.rgb, cov as f32);
            }
        }
    }

    /// An axis-aligned filled rectangle, with exact per-pixel coverage.
    pub fn rect(&mut self, r: Rect, color: Color) {
        let (rx, ry) = (r.x * self.scale, r.y * self.scale);
        let (rw, rh) = (r.w * self.scale, r.h * self.scale);
        let x0 = rx.floor().max(0.0) as usize;
        let y0 = ry.floor().max(0.0) as usize;
        let x1 = ((rx + rw).ceil().max(0.0) as usize).min(self.w);
        let y1 = ((ry + rh).ceil().max(0.0) as usize).min(self.h);
        for y in y0..y1 {
            let cy = ((ry + rh).min(y as f64 + 1.0) - ry.max(y as f64)).clamp(0.0, 1.0);
            for x in x0..x1 {
                let cx = ((rx + rw).min(x as f64 + 1.0) - rx.max(x as f64)).clamp(0.0, 1.0);
                self.blend(x, y, color.rgb, (cx * cy) as f32);
            }
        }
    }

    /// Encode as a `data:` URI ready for an SVG `<image href>`.
    #[must_use]
    pub fn into_data_uri(self) -> String {
        let png = png::encode_rgba(self.w as u32, self.h as u32, &self.px);
        let mut uri = String::with_capacity(png.len() * 4 / 3 + 32);
        uri.push_str("data:image/png;base64,");
        base64::push_encoded(&mut uri, &png);
        uri
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color::new("#FF0000", 255, 0, 0);

    fn region() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 20.0,
            h: 20.0,
        }
    }

    fn alpha_at(c: &Canvas, x: usize, y: usize) -> u8 {
        c.px[(y * c.w + x) * 4 + 3]
    }

    #[test]
    fn a_fresh_canvas_is_fully_transparent() {
        let c = Canvas::new(region()).unwrap();
        assert!(c.px.iter().all(|&b| b == 0));
        assert_eq!(c.pixels(), 40 * 40);
    }

    #[test]
    fn a_disc_is_opaque_at_its_centre_and_clear_outside() {
        let mut c = Canvas::new(region()).unwrap();
        c.disc(10.0, 10.0, 2.0, RED);
        assert_eq!(alpha_at(&c, 20, 20), 255);
        assert_eq!(alpha_at(&c, 0, 0), 0);
    }

    #[test]
    fn a_segment_covers_its_own_line() {
        let mut c = Canvas::new(region()).unwrap();
        c.segment(0.0, 10.0, 20.0, 10.0, 1.0, RED);
        assert!(alpha_at(&c, 20, 20) > 200);
        assert_eq!(alpha_at(&c, 20, 0), 0);
    }

    #[test]
    fn a_rect_covers_exactly_its_own_area() {
        let mut c = Canvas::new(region()).unwrap();
        c.rect(
            Rect {
                x: 5.0,
                y: 5.0,
                w: 5.0,
                h: 5.0,
            },
            RED,
        );
        assert_eq!(alpha_at(&c, 15, 15), 255);
        assert_eq!(alpha_at(&c, 5, 5), 0);
    }

    #[test]
    fn marks_outside_the_canvas_are_dropped_not_wrapped() {
        let mut c = Canvas::new(region()).unwrap();
        c.disc(-50.0, -50.0, 2.0, RED);
        c.disc(500.0, 500.0, 2.0, RED);
        assert!(c.px.iter().all(|&b| b == 0));
    }

    #[test]
    fn an_absurd_region_is_refused_rather_than_allocated() {
        assert!(Canvas::new(Rect {
            x: 0.0,
            y: 0.0,
            w: 1e6,
            h: 1e6
        })
        .is_none());
        assert!(Canvas::new(Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 10.0
        })
        .is_none());
    }

    #[test]
    fn the_budget_arithmetic_is_the_adrs() {
        assert_eq!(SVG_BUDGET_BYTES, 1_500_000);
        assert!(!over_budget(MARK_BUDGET_BYTES));
        assert!(over_budget(MARK_BUDGET_BYTES + 1));
    }

    #[test]
    fn the_data_uri_is_a_png() {
        let mut c = Canvas::new(region()).unwrap();
        c.disc(10.0, 10.0, 3.0, RED);
        let uri = c.into_data_uri();
        assert!(uri.starts_with("data:image/png;base64,iVBORw0KGgo"));
    }
}
