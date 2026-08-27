//! The document writer.
//!
//! Everything is appended to one `String`; there is no element tree, because a
//! figure is written once, front to back, and a tree would allocate a node per
//! mark on exactly the path the design note promises stays cheap.
//!
//! Two rules this module enforces so that no caller has to remember them:
//!
//! * **Every number goes through [`crate::coord`].** No `format!("{}", f64)`
//!   exists in this crate (ARCHITECTURE §8.7), and the byte bound the raster
//!   decision rests on is only true because of that.
//! * **Every string is escaped.** A variable label is user data — it reaches the
//!   figure as a title, an axis title or a legend key, and a `<` in one must not
//!   open an element. `tests/render.rs` asserts it with a label full of markup.

use crate::coord::{push_num, push_u64};
use crate::layout::Rect;
use stratum_tokens::Color;

/// Appends SVG source, counting the primitives it writes.
pub struct Doc {
    out: String,
    marks: u64,
    /// Document-unique suffix for `id` attributes. See [`Doc::open`].
    id: String,
}

impl Doc {
    /// Start a document `w × h` user units, declared in points.
    ///
    /// `id_seed` becomes a suffix on every `id` in the document. It has to:
    /// these SVGs are injected into a card with `innerHTML`, `id` is
    /// document-global in HTML, and two graph cards on one screen would
    /// otherwise share a `clipPath` — the second figure clipped to the first
    /// one's plot region. The suffix is a hash of the spec, so it is stable
    /// across runs (ADR-013) and different between figures.
    pub fn open(w: f64, h: f64, id_seed: &str) -> Doc {
        let mut out = String::with_capacity(4096);
        out.push_str(r#"<svg xmlns="http://www.w3.org/2000/svg" version="1.1" width=""#);
        push_num(&mut out, w);
        out.push_str(r#"pt" height=""#);
        push_num(&mut out, h);
        out.push_str(r#"pt" viewBox="0 0 "#);
        push_num(&mut out, w);
        out.push(' ');
        push_num(&mut out, h);
        out.push_str(r#"" role="img">"#);
        Doc {
            out,
            marks: 0,
            id: fnv1a_hex(id_seed),
        }
    }

    /// `<title>` — the accessible name of the figure. First child of `<svg>`, as
    /// SVG-AAM requires, so a screen reader announces the command that drew it
    /// even when the card's own `aria-label` is not read.
    pub fn title(&mut self, text: &str) {
        self.out.push_str("<title>");
        push_escaped_text(&mut self.out, text);
        self.out.push_str("</title>");
    }

    /// A `clipPath` around `r`, returning the `url(#…)` that references it.
    pub fn clip(&mut self, r: Rect) -> String {
        let name = format!("clip-{}", self.id);
        self.out.push_str(r#"<defs><clipPath id=""#);
        self.out.push_str(&name);
        self.out.push_str(r#""><rect "#);
        self.rect_attrs(r);
        self.out.push_str("/></clipPath></defs>");
        format!("url(#{name})")
    }

    /// Open a `<g>` with an optional clip and an optional stroke/fill default.
    pub fn group_open(&mut self, clip: Option<&str>) {
        self.out.push_str("<g");
        if let Some(c) = clip {
            self.out.push_str(r#" clip-path=""#);
            self.out.push_str(c);
            self.out.push('"');
        }
        self.out.push('>');
    }

    /// Close the most recent `<g>`.
    pub fn group_close(&mut self) {
        self.out.push_str("</g>");
    }

    fn rect_attrs(&mut self, r: Rect) {
        self.out.push_str(r#"x=""#);
        push_num(&mut self.out, r.x);
        self.out.push_str(r#"" y=""#);
        push_num(&mut self.out, r.y);
        self.out.push_str(r#"" width=""#);
        push_num(&mut self.out, r.w.max(0.0));
        self.out.push_str(r#"" height=""#);
        push_num(&mut self.out, r.h.max(0.0));
        self.out.push('"');
    }

    /// A filled rectangle.
    pub fn rect(&mut self, r: Rect, fill: Color) {
        self.out.push_str("<rect ");
        self.rect_attrs(r);
        self.out.push_str(r#" fill=""#);
        self.out.push_str(fill.hex);
        self.out.push_str(r#""/>"#);
        self.marks += 1;
    }

    /// A filled rectangle with a stroked edge — a histogram or bar-chart bar.
    pub fn bar(&mut self, r: Rect, fill: Color, edge: Color) {
        self.out.push_str("<rect ");
        self.rect_attrs(r);
        self.out.push_str(r#" fill=""#);
        self.out.push_str(fill.hex);
        self.out.push_str(r#"" stroke=""#);
        self.out.push_str(edge.hex);
        self.out.push_str(r#"" stroke-width="0.6"/>"#);
        self.marks += 1;
    }

    /// A straight line.
    pub fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, stroke: Color, width: f64) {
        self.out.push_str(r#"<line x1=""#);
        push_num(&mut self.out, x1);
        self.out.push_str(r#"" y1=""#);
        push_num(&mut self.out, y1);
        self.out.push_str(r#"" x2=""#);
        push_num(&mut self.out, x2);
        self.out.push_str(r#"" y2=""#);
        push_num(&mut self.out, y2);
        self.out.push_str(r#"" stroke=""#);
        self.out.push_str(stroke.hex);
        self.out.push_str(r#"" stroke-width=""#);
        push_num(&mut self.out, width);
        self.out.push_str(r#""/>"#);
        self.marks += 1;
    }

    /// A filled circle — the one marker symbol pass 1 draws.
    pub fn circle(&mut self, cx: f64, cy: f64, r: f64, fill: Color) {
        self.out.push_str(r#"<circle cx=""#);
        push_num(&mut self.out, cx);
        self.out.push_str(r#"" cy=""#);
        push_num(&mut self.out, cy);
        self.out.push_str(r#"" r=""#);
        push_num(&mut self.out, r);
        self.out.push_str(r#"" fill=""#);
        self.out.push_str(fill.hex);
        self.out.push_str(r#""/>"#);
        self.marks += 1;
    }

    /// A polyline through `points`, already in user units.
    pub fn polyline(&mut self, points: &[(f64, f64)], stroke: Color, width: f64) {
        if points.len() < 2 {
            return;
        }
        self.out.push_str(r#"<polyline fill="none" stroke=""#);
        self.out.push_str(stroke.hex);
        self.out.push_str(r#"" stroke-width=""#);
        push_num(&mut self.out, width);
        self.out.push_str(r#"" stroke-linejoin="round" points=""#);
        for (i, &(x, y)) in points.iter().enumerate() {
            if i > 0 {
                self.out.push(' ');
            }
            push_num(&mut self.out, x);
            self.out.push(',');
            push_num(&mut self.out, y);
            self.marks += 1;
        }
        self.out.push_str(r#""/>"#);
    }

    /// One range-cap: a vertical spine with a horizontal cap at each end.
    ///
    /// Emitted as a single `<path>` rather than three `<line>`s because a
    /// coefficient plot has one of these per coefficient and three elements per
    /// cap is three times the DOM for the same picture.
    pub fn rcap(&mut self, x: f64, y_lo: f64, y_hi: f64, half: f64, stroke: Color, width: f64) {
        self.out.push_str(r#"<path fill="none" stroke=""#);
        self.out.push_str(stroke.hex);
        self.out.push_str(r#"" stroke-width=""#);
        push_num(&mut self.out, width);
        self.out.push_str(r#"" d="M"#);
        push_num(&mut self.out, x);
        self.out.push(' ');
        push_num(&mut self.out, y_lo);
        self.out.push('L');
        push_num(&mut self.out, x);
        self.out.push(' ');
        push_num(&mut self.out, y_hi);
        for y in [y_lo, y_hi] {
            self.out.push('M');
            push_num(&mut self.out, x - half);
            self.out.push(' ');
            push_num(&mut self.out, y);
            self.out.push('L');
            push_num(&mut self.out, x + half);
            self.out.push(' ');
            push_num(&mut self.out, y);
        }
        self.out.push_str(r#""/>"#);
        self.marks += 3;
    }

    /// Text at a baseline, anchored.
    pub fn text(&mut self, x: f64, y: f64, anchor: Anchor, size: f64, fill: Color, s: &str) {
        self.text_inner(x, y, anchor, size, fill, s, None, 400);
    }

    /// Text in a heavier weight — the figure title, and nothing else.
    pub fn text_bold(&mut self, x: f64, y: f64, anchor: Anchor, size: f64, fill: Color, s: &str) {
        let weight = stratum_tokens::typography::weights::SEMIBOLD.value;
        self.text_inner(x, y, anchor, size, fill, s, None, weight);
    }

    /// Text rotated a quarter turn anticlockwise about `(x, y)` — the y-axis
    /// title, and nothing else.
    pub fn text_rotated(&mut self, x: f64, y: f64, size: f64, fill: Color, s: &str) {
        self.text_inner(x, y, Anchor::Middle, size, fill, s, Some(-90.0), 400);
    }

    #[allow(clippy::too_many_arguments)] // one private writer beats six wrappers
    fn text_inner(
        &mut self,
        x: f64,
        y: f64,
        anchor: Anchor,
        size: f64,
        fill: Color,
        s: &str,
        rotate: Option<f64>,
        weight: u16,
    ) {
        self.out.push_str(r#"<text x=""#);
        push_num(&mut self.out, x);
        self.out.push_str(r#"" y=""#);
        push_num(&mut self.out, y);
        self.out.push_str(r#"" font-family=""#);
        push_font_stack(&mut self.out);
        self.out.push_str(r#"" font-size=""#);
        push_num(&mut self.out, size);
        self.out.push_str(r#"" fill=""#);
        self.out.push_str(fill.hex);
        if weight != 400 {
            self.out.push_str(r#"" font-weight=""#);
            push_u64(&mut self.out, u64::from(weight));
        }
        self.out.push_str(r#"" text-anchor=""#);
        self.out.push_str(anchor.as_str());
        if let Some(deg) = rotate {
            self.out.push_str(r#"" transform="rotate("#);
            push_num(&mut self.out, deg);
            self.out.push(' ');
            push_num(&mut self.out, x);
            self.out.push(' ');
            push_num(&mut self.out, y);
            self.out.push(')');
        }
        self.out.push_str(r#"">"#);
        push_escaped_text(&mut self.out, s);
        self.out.push_str("</text>");
    }

    /// The rasterised mark layer (design note §7): one PNG, positioned over the
    /// plot region, `image-rendering: auto` so the browser resamples it.
    pub fn raster_image(&mut self, r: Rect, data_uri: &str) {
        self.out.push_str(r#"<image "#);
        self.rect_attrs(r);
        self.out.push_str(r#" preserveAspectRatio="none" href=""#);
        self.out.push_str(data_uri);
        self.out.push_str(r#""/>"#);
        self.marks += 1;
    }

    /// Close the document and take the source.
    #[must_use]
    pub fn finish(mut self) -> String {
        self.out.push_str("</svg>");
        self.out
    }

    /// Primitives written so far.
    ///
    /// Read as a *delta* across the mark layer rather than as a total: the
    /// counter the design note §8 publishes is data marks, and the figure ground,
    /// the axes and the ticks are fixed furniture that would drown the signal.
    #[must_use]
    pub fn marks(&self) -> u64 {
        self.marks
    }

    /// Bytes written so far — the raster decision's post-check.
    #[must_use]
    pub fn len(&self) -> usize {
        self.out.len()
    }

    /// Whether nothing has been written. Present because clippy asks for it
    /// beside `len`; a `Doc` is never actually empty after `open`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }
}

/// `text-anchor`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Anchor {
    /// Left-aligned.
    Start,
    /// Centred — every tick label on the x axis, every title.
    Middle,
    /// Right-aligned — every tick label on the y axis.
    End,
}

impl Anchor {
    fn as_str(self) -> &'static str {
        match self {
            Anchor::Start => "start",
            Anchor::Middle => "middle",
            Anchor::End => "end",
        }
    }
}

/// The design system's sans stack, comma-separated, each family quoted when it
/// contains a space — the CSS `font-family` grammar SVG inherits.
fn push_font_stack(out: &mut String) {
    for (i, family) in stratum_tokens::typography::families::SANS
        .stack
        .iter()
        .enumerate()
    {
        if i > 0 {
            out.push_str(", ");
        }
        // `&quot;` and not `'`: the attribute is already delimited with `"`, and
        // an apostrophe inside a font name would then need escaping too.
        if family.contains(' ') {
            out.push_str("&quot;");
            out.push_str(family);
            out.push_str("&quot;");
        } else {
            out.push_str(family);
        }
    }
}

/// XML text escaping. `&` first, or the ampersands of the other replacements are
/// escaped a second time.
pub fn push_escaped_text(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            // XML 1.0 has no spelling for these at all, escaped or not; a NUL in
            // a variable label would make the document unparseable.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' => out.push(' '),
            c => out.push(c),
        }
    }
}

/// FNV-1a over the seed, hex — a document-unique `id` suffix that is the same on
/// every machine and in every run.
fn fnv1a_hex(seed: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut s = String::with_capacity(16);
    for i in (0..16).rev() {
        let nibble = ((h >> (i * 4)) & 0xf) as u8;
        s.push(char::from(if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        }));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_in_a_variable_label_cannot_open_an_element() {
        let mut s = String::new();
        push_escaped_text(&mut s, r#"<script>alert("x" & 'y')</script>"#);
        assert!(!s.contains('<'));
        assert!(!s.contains('>'));
        assert_eq!(
            s,
            "&lt;script&gt;alert(&quot;x&quot; &amp; &#39;y&#39;)&lt;/script&gt;"
        );
    }

    #[test]
    fn ampersand_is_escaped_once() {
        let mut s = String::new();
        push_escaped_text(&mut s, "R&D <5");
        assert_eq!(s, "R&amp;D &lt;5");
    }

    #[test]
    fn control_characters_become_spaces() {
        let mut s = String::new();
        push_escaped_text(&mut s, "a\u{0}b\u{1}c");
        assert_eq!(s, "a b c");
    }

    #[test]
    fn ids_are_deterministic_and_distinct() {
        assert_eq!(fnv1a_hex("histogram price"), fnv1a_hex("histogram price"));
        assert_ne!(fnv1a_hex("histogram price"), fnv1a_hex("histogram mpg"));
        assert_eq!(fnv1a_hex("x").len(), 16);
    }

    #[test]
    fn two_documents_do_not_share_a_clip_path_id() {
        let mut a = Doc::open(10.0, 10.0, "a");
        let mut b = Doc::open(10.0, 10.0, "b");
        let ca = a.clip(Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        });
        let cb = b.clip(Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        });
        assert_ne!(ca, cb);
    }

    #[test]
    fn the_font_stack_is_the_design_systems() {
        let mut s = String::new();
        push_font_stack(&mut s);
        assert_eq!(s, "&quot;IBM Plex Sans&quot;, system-ui, sans-serif");
    }
}
