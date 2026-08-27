//! The design system as compile-time constants.
//!
//! `design/tokens.json` at the repository root is the single source of design
//! truth. `cargo xtask tokens` compiles it into two **committed** artifacts —
//! this crate's [`generated`] module and `apps/desktop/resources/
//! tokens.generated.css` — and `cargo xtask tokens --check` fails CI when
//! either has drifted (ARCHITECTURE §8.14).
//!
//! The point of the crate is that `stratum-graph` is an L1 crate that must
//! render headless, in-process, with no filesystem: a graph drawn inline in the
//! app and the same graph drawn by `stratum run` on a build server have to come
//! out identical. So the scheme colours are `static` data in the binary, and
//! CI greps `crates/stratum-graph/src` for `std::fs`, `Utf8Path` and
//! `include_str!` to keep it that way (ARCHITECTURE §8.14, ADR A14).
//!
//! This crate has no dependencies, on purpose. Everything here is plain data
//! and two linear scans over arrays of three and two elements.

#![no_std]

mod generated;

pub use generated::*;

/// A colour as both the string a renderer writes and the channels an
/// interpolator needs.
///
/// The two are always consistent: `hex` is the uppercase `#RRGGBB` spelling of
/// `rgb`, and a test asserts it for every colour in the crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    /// Uppercase `#RRGGBB`, ready to write straight into an SVG attribute.
    pub hex: &'static str,
    /// The same colour as 8-bit channels, for gradient interpolation and for
    /// the contrast arithmetic in `cargo xtask tokens`.
    pub rgb: Rgb,
}

/// 8-bit sRGB channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

/// A colour that the frontend also reaches by CSS custom property.
///
/// `var` is carried rather than derived because the names in
/// `06-ui-architecture.md` §14.5 are irregular — `--n7`, `--text-meta` and
/// `--accent` are all the same shape of thing — and cannot be computed from the
/// JSON key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    /// The CSS custom-property name, e.g. `--text-body`.
    pub var: &'static str,
    /// The colour itself.
    pub color: Color,
}

/// A block-state colour, which is also the colour of that block's gutter glyph
/// and state rail (`06` §4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateToken {
    /// The CSS custom-property name, e.g. `--state-ok`.
    pub var: &'static str,
    /// The colour itself.
    pub color: Color,
    /// Which of the icon set's glyphs renders this state: `check`,
    /// `hollow-dot`, `cross` or `ring`.
    pub glyph: &'static str,
}

/// A two-stop single-hue ramp. Consumers interpolate in whatever space they
/// have decided is correct; this crate takes no position and does no arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gradient {
    /// The low end.
    pub from: Color,
    /// The high end.
    pub to: Color,
}

/// A font family and the fallback chain behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontFamily {
    /// The CSS custom-property name, e.g. `--font-mono`.
    pub var: &'static str,
    /// Most-preferred first. The bundled woff2 is always `stack[0]`.
    pub stack: &'static [&'static str],
}

/// A type size and its locked line height, both in CSS px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontSize {
    /// The CSS custom-property name, e.g. `--fs-body`.
    pub var: &'static str,
    /// Size in CSS px.
    pub px: f32,
    /// Line height in CSS px.
    pub line_height_px: f32,
}

/// One of the three permitted weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Weight {
    /// The CSS custom-property name, e.g. `--fw-medium`.
    pub var: &'static str,
    /// The CSS `font-weight` number.
    pub value: u16,
}

/// A fixed length in CSS px — a chrome height, a gutter width, a radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metric {
    /// The CSS custom-property name, e.g. `--h-top-bar`.
    pub var: &'static str,
    /// The length in CSS px.
    pub px: f32,
}

/// A transition duration and its easing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Motion {
    /// The CSS custom-property name, e.g. `--motion-state`.
    pub var: &'static str,
    /// Duration in milliseconds.
    pub ms: u32,
    /// A CSS `<easing-function>`.
    pub easing: &'static str,
}

/// A shadow level. There are exactly two, and no result surface uses either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Elevation {
    /// The CSS custom-property name, e.g. `--elev-overlay`.
    pub var: &'static str,
    /// The `box-shadow` value in the light theme.
    pub light: &'static str,
    /// The `box-shadow` value in the dark theme.
    pub dark: &'static str,
}

/// Role aliases onto the neutral ramp. Both themes carry all fifteen; only the
/// step each one points at differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Semantic {
    /// The ground the panes sit on.
    pub app_background: Token,
    /// Editor canvas and card body.
    pub canvas: Token,
    /// Pane chrome, sidebars, top bar.
    pub surface: Token,
    /// Menus, the palette, tooltips, popovers.
    pub overlay: Token,
    /// Inputs and hover.
    pub input: Token,
    /// The one hairline colour.
    pub border: Token,
    /// Emphasised borders and the dark overlay outline.
    pub border_strong: Token,
    /// The interior rule of a booktabs table.
    pub rule_mid: Token,
    /// The booktabs top and bottom rules.
    pub rule_edge: Token,
    /// Disabled glyphs.
    pub text_disabled: Token,
    /// Meta text.
    pub text_meta: Token,
    /// Labels.
    pub text_label: Token,
    /// Secondary body text.
    pub text_secondary: Token,
    /// Body text.
    pub text_body: Token,
    /// Headings.
    pub text_heading: Token,
}

/// The single ink-teal accent and its two derivatives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accent {
    /// The accent itself.
    pub accent: Token,
    /// Hover and active.
    pub accent_hover: Token,
    /// Tinted backgrounds; also the low end of the sequential ramp.
    pub accent_subtle: Token,
}

/// The five block states that carry a colour. The sixth and seventh states in
/// `BlockStatus` (`queued`, `running`) are motion, not colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatePalette {
    /// Current and verified.
    pub ok: StateToken,
    /// Stale.
    pub stale: StateToken,
    /// Failed or broken.
    pub failed: StateToken,
    /// Interrupted.
    pub interrupted: StateToken,
    /// Never run — the absence of a state.
    pub never_run: StateToken,
}

/// Okabe–Ito, plus the single-hue sequential ramp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataPalette {
    /// Eight discriminable hues, in the order Okabe–Ito defines them.
    pub categorical: [Color; 8],
    /// Interpolated from `--accent-subtle` to `--accent`.
    pub sequential: Gradient,
}

/// One complete theme. `LIGHT` and `DARK` have identical shapes, so a consumer
/// can iterate one and index the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// `"light"` or `"dark"`; matches the key in `design/tokens.json` and the
    /// value of `LayoutSpec.defaults.theme`.
    pub id: &'static str,
    /// The human name, e.g. `"Broadsheet Light"`.
    pub name: &'static str,
    /// The neutral ramp. Index `i` is the token spelled `--n{i}`, darkest-on-
    /// light at `[11]` and lightest-on-dark at `[11]` — the ramp is ordered by
    /// *role*, not by luminance, which is why the dark theme reads as the
    /// mirror of the light one rather than as its inverse.
    pub neutral: [Token; 12],
    /// Role aliases onto that ramp.
    pub semantic: Semantic,
    /// The accent triple.
    pub accent: Accent,
    /// The five state colours.
    pub state: StatePalette,
    /// The data palette.
    pub data: DataPalette,
}

/// A graph scheme: everything `stratum-graph` needs to draw one figure.
///
/// A scheme is deliberately *not* a `Theme`. `print` has no theme to belong to:
/// it is pure white ground and black ink whatever the user's app looks like,
/// because the figure is going into a paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scheme {
    /// The name a user writes in `scheme()`, e.g. `"stratum-dark"`.
    pub id: &'static str,
    /// Which app theme this scheme is meant to sit inside — `"light"` or
    /// `"dark"`. `print` declares `"light"` and is still never chosen for the
    /// user by theme.
    pub theme: &'static str,
    /// Behind the whole figure, including margins and titles.
    pub background: Color,
    /// Behind the plot region only.
    pub plot_background: Color,
    /// Maximum-contrast ink: titles, the outermost frame.
    pub foreground: Color,
    /// Axis lines and tick marks.
    pub axis: Color,
    /// Grid lines.
    pub grid: Color,
    /// Reference rules.
    pub rule: Color,
    /// Axis labels and tick text.
    pub text: Color,
    /// Notes, footers, subtitles.
    pub text_meta: Color,
    /// Series colours, taken in order and wrapping past eight.
    pub series: [Color; 8],
    /// The ramp for continuous encodings.
    pub sequential: Gradient,
}

// The three constructors below exist so that `generated.rs` stays one line per
// token. Spelled as struct literals, rustfmt expands every nested `Rgb` across
// five lines and the committed artifact triples in size — which matters,
// because the whole reason it is committed is that a human reviews its diff.

impl Color {
    /// `hex` must be the uppercase `#RRGGBB` spelling of `r`, `g`, `b`; the
    /// emitter derives one from the other and a test re-checks every pair.
    #[must_use]
    pub const fn new(hex: &'static str, r: u8, g: u8, b: u8) -> Self {
        Self {
            hex,
            rgb: Rgb { r, g, b },
        }
    }
}

impl Token {
    /// See [`Color::new`].
    #[must_use]
    pub const fn new(var: &'static str, hex: &'static str, r: u8, g: u8, b: u8) -> Self {
        Self {
            var,
            color: Color::new(hex, r, g, b),
        }
    }

    /// The `#RRGGBB` spelling, for writing into markup.
    #[must_use]
    pub const fn hex(&self) -> &'static str {
        self.color.hex
    }
}

impl StateToken {
    /// See [`Color::new`].
    #[must_use]
    pub const fn new(
        var: &'static str,
        hex: &'static str,
        r: u8,
        g: u8,
        b: u8,
        glyph: &'static str,
    ) -> Self {
        Self {
            var,
            color: Color::new(hex, r, g, b),
            glyph,
        }
    }

    /// The `#RRGGBB` spelling, for writing into markup.
    #[must_use]
    pub const fn hex(&self) -> &'static str {
        self.color.hex
    }
}

/// Look up a graph scheme by the name a user writes in `scheme()`.
///
/// Returns `None` rather than falling back to [`graph::DEFAULT_SCHEME`]: an
/// unrecognised scheme name is a user error that `stratum-graph` must report
/// with a diagnostic, not paper over by drawing the wrong colours.
#[must_use]
pub fn scheme(id: &str) -> Option<&'static Scheme> {
    let mut i = 0;
    while i < graph::SCHEMES.len() {
        if str_eq(graph::SCHEMES[i].id, id) {
            return Some(&graph::SCHEMES[i]);
        }
        i += 1;
    }
    None
}

/// Look up a theme by `"light"` or `"dark"`.
#[must_use]
pub fn theme(id: &str) -> Option<&'static Theme> {
    let mut i = 0;
    while i < color::THEMES.len() {
        if str_eq(color::THEMES[i].id, id) {
            return Some(color::THEMES[i]);
        }
        i += 1;
    }
    None
}

/// `str::eq` without pulling in `core::cmp` trait dispatch, so the lookups above
/// stay usable from a `const`-adjacent context and the crate keeps no trait
/// surface at all.
fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}
