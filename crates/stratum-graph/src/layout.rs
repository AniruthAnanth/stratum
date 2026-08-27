//! Where the plot region ends up, given text this crate cannot measure.
//!
//! # Why there is a model here at all
//!
//! ARCHITECTURE §8.14 forbids this crate a filesystem, so it cannot open a font,
//! so it cannot measure a string. But the left margin has to be wide enough for
//! `-12,345.6` or the y-axis labels are clipped, and that width depends on the
//! glyphs.
//!
//! The model below is a per-character-class fraction of the em for IBM Plex
//! Sans, **biased upward**. Its entire error budget is "a margin slightly wider
//! than necessary": every string in the figure is positioned by `text-anchor`
//! (`end` for y labels, `middle` for x labels and titles), so a wrong width can
//! never move a label off its tick. That is the property that makes an estimate
//! acceptable here and would not make one acceptable in, say, a table layout.
//!
//! # Units
//!
//! One SVG user unit is one point, and the document declares `width="396pt"`
//! with a matching unitless `viewBox`, so the same file is a 5.5 × 4 in figure
//! in a paper and a 396-px-wide figure in a card (06 §6.7 caps the card at
//! `min(pane width, 720 px)`).
//!
//! Type sizes are the design system's, in those units, so the graph's smallest
//! text is the same size as the app's smallest text — `--fs-micro`, 11.
//! `stratum_tokens::typography::sizes` is the source; a literal here would be
//! the "graph that does not match the app" A14 calls a bug.

use stratum_tokens::typography::sizes;

/// An axis-aligned rectangle in user units, y growing downwards.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// Right edge.
    #[must_use]
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    /// Bottom edge.
    #[must_use]
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// Tick labels and legend keys — `--fs-micro`.
pub const FS_TICK: f64 = sizes::MICRO.px as f64;
/// Axis titles — `--fs-small`.
pub const FS_AXIS_TITLE: f64 = sizes::SMALL.px as f64;
/// The figure title — `--fs-body`.
pub const FS_TITLE: f64 = sizes::BODY.px as f64;
/// Subtitle and note — `--fs-micro`.
pub const FS_META: f64 = sizes::MICRO.px as f64;

/// Tick mark length.
pub const TICK_LEN: f64 = 4.0;
/// Gap between a tick and its label.
pub const TICK_GAP: f64 = 3.0;
/// The figure's outer padding.
pub const PAD: f64 = 8.0;
/// One legend row.
pub const LEGEND_ROW: f64 = 16.0;

/// Advance width of one character, as a fraction of the em.
///
/// Biased upward: every class is rounded up from the measured IBM Plex Sans
/// value, so the model over-reserves and never under-reserves.
fn advance_em(c: char) -> f64 {
    match c {
        ' ' => 0.28,
        '.' | ',' | ':' | ';' | '\'' | '!' | '|' | '(' | ')' | '[' | ']' => 0.36,
        'i' | 'l' | 'j' | 'I' | 'f' | 't' | 'r' => 0.38,
        // Plex's digits are tabular: every one of them is the same width, which
        // is exactly what an axis of numbers needs. The measured advance is
        // 600/1000 em exactly; 0.62 is that plus the upward bias every class
        // here carries, and it is what makes `text_width` provably a ceiling
        // rather than an equality that float accumulation can undercut.
        '0'..='9' => 0.62,
        'm' | 'w' | 'M' | 'W' => 0.90,
        'A'..='Z' => 0.72,
        'a'..='z' => 0.58,
        '-' | '+' | '=' | '/' | '%' | '<' | '>' => 0.60,
        // Anything else — punctuation, accents, CJK — is charged a full em. A
        // label in a script this table does not cover gets a generous margin
        // rather than a clipped one.
        _ => 1.0,
    }
}

/// Estimated width of `text` at `size` user units.
#[must_use]
pub fn text_width(text: &str, size: f64) -> f64 {
    text.chars().map(advance_em).sum::<f64>() * size
}

/// What the emitter needs to know about where things go.
#[derive(Clone, PartialEq, Debug)]
pub struct Layout {
    /// The whole figure.
    pub figure: Rect,
    /// The data area, inside the axes.
    pub plot: Rect,
    /// Baseline y of the title, when there is one.
    pub title_y: Option<f64>,
    /// Baseline y of the subtitle, when there is one.
    pub subtitle_y: Option<f64>,
    /// Baseline y of the note, when there is one.
    pub note_y: Option<f64>,
    /// Baseline y of the x-axis title, when there is one.
    pub x_title_y: Option<f64>,
    /// Centre x of the rotated y-axis title, when there is one.
    pub y_title_x: Option<f64>,
    /// Top y of the legend block, when there is one.
    pub legend_y: Option<f64>,
}

/// Everything the margin arithmetic depends on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LayoutInput<'a> {
    /// Figure width in user units.
    pub width: f64,
    /// Figure height in user units.
    pub height: f64,
    /// The widest y tick label, already formatted.
    pub widest_y_label: &'a str,
    /// The first and last x tick labels, which are the two that can overhang.
    pub first_x_label: &'a str,
    /// See `first_x_label`.
    pub last_x_label: &'a str,
    /// Whether the figure has a title / subtitle / note.
    pub has_title: bool,
    /// See `has_title`.
    pub has_subtitle: bool,
    /// See `has_title`.
    pub has_note: bool,
    /// Whether either axis has a title.
    pub has_x_title: bool,
    /// See `has_x_title`.
    pub has_y_title: bool,
    /// Legend rows; `0` for no legend.
    pub legend_rows: usize,
}

/// The smallest plot region worth drawing. Below this the figure is refused
/// rather than emitted with a negative-width rectangle.
const MIN_PLOT: f64 = 24.0;

/// Solve the margins.
#[must_use]
pub fn plan(input: LayoutInput<'_>) -> Option<Layout> {
    let mut top = PAD;
    let title_y = input.has_title.then(|| {
        top += FS_TITLE;
        let y = top;
        top += 4.0;
        y
    });
    let subtitle_y = input.has_subtitle.then(|| {
        top += FS_META;
        let y = top;
        top += 4.0;
        y
    });
    top += 4.0;

    let mut bottom = PAD;
    let note_y = input.has_note.then(|| {
        let y = input.height - bottom;
        bottom += FS_META + 4.0;
        y
    });
    let legend_y = (input.legend_rows > 0).then(|| {
        bottom += LEGEND_ROW * input.legend_rows as f64;
        input.height - bottom
    });
    // Every `*_y` returned from here is a text BASELINE except `legend_y`, which
    // is the top of the legend band because a legend row is a strip of markers
    // rather than one string. The x-axis title reserves its band and then places
    // its baseline 0.8 em down inside it; deriving that baseline at the call
    // site instead is how the title came out drawn across the legend.
    let x_title_y = input.has_x_title.then(|| {
        bottom += 2.0 + FS_AXIS_TITLE;
        input.height - bottom + FS_AXIS_TITLE * 0.8
    });
    bottom += FS_TICK + TICK_GAP + TICK_LEN;

    // The left margin is the whole reason the advance model exists.
    let mut left = PAD;
    let y_title_x = input.has_y_title.then(|| {
        let x = left + FS_AXIS_TITLE;
        left += FS_AXIS_TITLE + 4.0;
        x
    });
    left += text_width(input.widest_y_label, FS_TICK) + TICK_GAP + TICK_LEN;

    // Half the first x label hangs left of the axis and half the last one hangs
    // right of it, both because they are `text-anchor="middle"` on their tick.
    let overhang_left = text_width(input.first_x_label, FS_TICK) / 2.0;
    let overhang_right = text_width(input.last_x_label, FS_TICK) / 2.0;
    left = left.max(PAD + overhang_left);
    let right = PAD + overhang_right;

    let plot = Rect {
        x: left,
        y: top,
        w: input.width - left - right,
        h: input.height - top - bottom,
    };
    if plot.w < MIN_PLOT || plot.h < MIN_PLOT {
        return None;
    }

    Some(Layout {
        figure: Rect {
            x: 0.0,
            y: 0.0,
            w: input.width,
            h: input.height,
        },
        plot,
        title_y,
        subtitle_y,
        note_y,
        x_title_y,
        y_title_x,
        legend_y,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> LayoutInput<'static> {
        LayoutInput {
            width: 396.0,
            height: 288.0,
            widest_y_label: "0",
            first_x_label: "0",
            last_x_label: "0",
            has_title: false,
            has_subtitle: false,
            has_note: false,
            has_x_title: false,
            has_y_title: false,
            legend_rows: 0,
        }
    }

    #[test]
    fn type_sizes_come_from_the_design_system() {
        assert_eq!(FS_TICK, 11.0);
        assert_eq!(FS_AXIS_TITLE, 12.0);
        assert_eq!(FS_TITLE, 13.0);
    }

    #[test]
    fn a_wider_label_takes_a_wider_margin() {
        let narrow = plan(base()).unwrap();
        let wide = plan(LayoutInput {
            widest_y_label: "-12,345.6",
            ..base()
        })
        .unwrap();
        assert!(wide.plot.x > narrow.plot.x);
        assert!(wide.plot.w < narrow.plot.w);
    }

    #[test]
    fn the_model_over_reserves_rather_than_clipping() {
        // Ten tabular digits at 11 units is 66; the model must not come in under
        // the real advance, and Plex's digit advance is 0.6 em exactly.
        assert!(text_width("0123456789", 11.0) >= 0.6 * 10.0 * 11.0);
        // An unknown script is charged a full em rather than guessed at.
        assert_eq!(text_width("日", 11.0), 11.0);
    }

    #[test]
    fn every_furniture_item_takes_space_from_the_plot() {
        let bare = plan(base()).unwrap();
        for input in [
            LayoutInput {
                has_title: true,
                ..base()
            },
            LayoutInput {
                has_subtitle: true,
                ..base()
            },
            LayoutInput {
                has_note: true,
                ..base()
            },
            LayoutInput {
                has_x_title: true,
                ..base()
            },
            LayoutInput {
                has_y_title: true,
                ..base()
            },
            LayoutInput {
                legend_rows: 1,
                ..base()
            },
        ] {
            let laid = plan(input).unwrap();
            assert!(laid.plot.w * laid.plot.h < bare.plot.w * bare.plot.h);
        }
    }

    #[test]
    fn a_figure_smaller_than_its_margins_is_refused_not_drawn_inside_out() {
        assert!(plan(LayoutInput {
            width: 30.0,
            height: 30.0,
            ..base()
        })
        .is_none());
        let laid = plan(LayoutInput {
            width: 30.0,
            height: 30.0,
            ..base()
        });
        assert!(
            laid.is_none(),
            "a negative-width plot region must never be emitted"
        );
    }

    /// The furniture below the plot region is a stack of bands, and every band
    /// has to stay in its own. This is a regression test: `plan` used to return
    /// the x-title band's TOP while the emitter treated it as a baseline and
    /// added 0.8 em, which drew "Mileage (mpg)" straight through the legend.
    #[test]
    fn the_x_title_sits_between_the_tick_labels_and_the_legend() {
        let l = plan(LayoutInput {
            has_x_title: true,
            legend_rows: 1,
            has_note: true,
            ..base()
        })
        .unwrap();
        let x_title = l.x_title_y.expect("an x title was asked for");
        let legend_top = l.legend_y.expect("a legend row was asked for");
        let tick_baseline = l.plot.bottom() + TICK_LEN + TICK_GAP + FS_TICK * 0.8;

        assert!(
            tick_baseline < x_title,
            "the x title must clear the tick labels"
        );
        assert!(
            x_title < legend_top,
            "the x title must clear the legend band"
        );
        assert!(
            legend_top + LEGEND_ROW <= l.note_y.expect("a note was asked for"),
            "the legend must clear the note"
        );
    }

    #[test]
    fn the_plot_region_stays_inside_the_figure() {
        let l = plan(LayoutInput {
            widest_y_label: "-1,234,567.8",
            first_x_label: "1970m1",
            last_x_label: "2026m12",
            has_title: true,
            has_subtitle: true,
            has_note: true,
            has_x_title: true,
            has_y_title: true,
            legend_rows: 2,
            ..base()
        })
        .unwrap();
        assert!(l.plot.x >= 0.0 && l.plot.y >= 0.0);
        assert!(l.plot.right() <= l.figure.w);
        assert!(l.plot.bottom() <= l.figure.h);
    }
}
