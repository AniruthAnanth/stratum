//! Structural checks on the committed artifact.
//!
//! These are deliberately *not* the contrast checks. WCAG relative luminance
//! needs `powf`, which ARCHITECTURE §8.11 (A19) bans everywhere under
//! `crates/`, and `IMPLEMENTATION_PLAN` W12 puts the contrast policy in
//! `cargo xtask tokens` — which lives outside `crates/` and reads
//! `design/tokens.json` directly. What is left for this crate to prove is that
//! the emitter did not lie: that every `hex` agrees with its `rgb`, that the
//! two themes really are the same shape, and that the lookups resolve.

use stratum_tokens::{color, graph, Color, Scheme, StateToken, Theme, Token};

/// Parse `#RRGGBB` with integer arithmetic only.
fn parse_hex(hex: &str) -> (u8, u8, u8) {
    let b = hex.as_bytes();
    assert_eq!(b.len(), 7, "not #RRGGBB: {hex}");
    assert_eq!(b[0], b'#', "not #RRGGBB: {hex}");
    let nib = |c: u8| -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'A'..=b'F' => c - b'A' + 10,
            _ => panic!("hex must be uppercase and 0-9A-F: {hex}"),
        }
    };
    (
        nib(b[1]) * 16 + nib(b[2]),
        nib(b[3]) * 16 + nib(b[4]),
        nib(b[5]) * 16 + nib(b[6]),
    )
}

fn check_color(c: &Color, whence: &str) {
    let (r, g, b) = parse_hex(c.hex);
    assert_eq!(
        (r, g, b),
        (c.rgb.r, c.rgb.g, c.rgb.b),
        "{whence}: {} does not match its rgb triple",
        c.hex
    );
}

fn check_token(t: &Token, whence: &str) {
    assert!(
        t.var.starts_with("--"),
        "{whence}: `{}` is not a custom property",
        t.var
    );
    check_color(&t.color, whence);
    assert_eq!(t.hex(), t.color.hex);
}

fn check_state(t: &StateToken, whence: &str) {
    assert!(
        t.var.starts_with("--"),
        "{whence}: `{}` is not a custom property",
        t.var
    );
    check_color(&t.color, whence);
    assert!(
        matches!(t.glyph, "check" | "hollow-dot" | "cross" | "ring"),
        "{whence}: unknown glyph `{}`",
        t.glyph
    );
}

fn semantic_tokens(t: &'static Theme) -> [&'static Token; 15] {
    let s = &t.semantic;
    [
        &s.app_background,
        &s.canvas,
        &s.surface,
        &s.overlay,
        &s.input,
        &s.border,
        &s.border_strong,
        &s.rule_mid,
        &s.rule_edge,
        &s.text_disabled,
        &s.text_meta,
        &s.text_label,
        &s.text_secondary,
        &s.text_body,
        &s.text_heading,
    ]
}

#[test]
fn every_hex_agrees_with_its_rgb() {
    for theme in color::THEMES {
        for (i, t) in theme.neutral.iter().enumerate() {
            check_token(t, &format!("{}.neutral[{i}]", theme.id));
            assert_eq!(t.var, format!("--n{i}"), "the ramp is positional");
        }
        for t in semantic_tokens(theme) {
            check_token(t, &format!("{}.semantic", theme.id));
        }
        for t in [
            &theme.accent.accent,
            &theme.accent.accent_hover,
            &theme.accent.accent_subtle,
        ] {
            check_token(t, &format!("{}.accent", theme.id));
        }
        let st = &theme.state;
        for t in [
            &st.ok,
            &st.stale,
            &st.failed,
            &st.interrupted,
            &st.never_run,
        ] {
            check_state(t, &format!("{}.state", theme.id));
        }
        for c in &theme.data.categorical {
            check_color(c, &format!("{}.data.categorical", theme.id));
        }
        check_color(&theme.data.sequential.from, "data.sequential.from");
        check_color(&theme.data.sequential.to, "data.sequential.to");
    }

    for s in &graph::SCHEMES {
        for c in scheme_colors(s) {
            check_color(c, &format!("scheme {}", s.id));
        }
    }
}

fn scheme_colors(s: &'static Scheme) -> Vec<&'static Color> {
    let mut v: Vec<&Color> = vec![
        &s.background,
        &s.plot_background,
        &s.foreground,
        &s.axis,
        &s.grid,
        &s.rule,
        &s.text,
        &s.text_meta,
        &s.sequential.from,
        &s.sequential.to,
    ];
    v.extend(s.series.iter());
    v
}

/// `design/tokens.json`'s own convention: "`light` and `dark` have identical
/// key sets at every depth, so a generator can iterate one and index the
/// other." A consumer that walks one theme and indexes the other by `var` would
/// break silently if that ever stopped being true.
#[test]
fn the_two_themes_are_the_same_shape() {
    let (l, d) = (&color::LIGHT, &color::DARK);
    assert_eq!(l.id, "light");
    assert_eq!(d.id, "dark");

    for i in 0..12 {
        assert_eq!(l.neutral[i].var, d.neutral[i].var);
    }
    for (a, b) in semantic_tokens(l).into_iter().zip(semantic_tokens(d)) {
        assert_eq!(a.var, b.var);
    }
    assert_eq!(l.accent.accent.var, d.accent.accent.var);
    assert_eq!(l.accent.accent_hover.var, d.accent.accent_hover.var);
    assert_eq!(l.accent.accent_subtle.var, d.accent.accent_subtle.var);
    assert_eq!(l.state.ok.var, d.state.ok.var);
    assert_eq!(l.state.ok.glyph, d.state.ok.glyph);
    assert_eq!(l.state.never_run.glyph, d.state.never_run.glyph);
    assert_eq!(l.data.categorical.len(), d.data.categorical.len());
}

/// Every `var` in a theme is unique, because the CSS artifact emits one
/// declaration per token into a single block and a collision would silently
/// take the last one.
#[test]
fn theme_custom_properties_are_unique() {
    for theme in color::THEMES {
        let mut seen: Vec<&str> = theme.neutral.iter().map(|t| t.var).collect();
        seen.extend(semantic_tokens(theme).iter().map(|t| t.var));
        seen.extend([
            theme.accent.accent.var,
            theme.accent.accent_hover.var,
            theme.accent.accent_subtle.var,
            theme.state.ok.var,
            theme.state.stale.var,
            theme.state.failed.var,
            theme.state.interrupted.var,
            theme.state.never_run.var,
        ]);
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            before,
            seen.len(),
            "{}: duplicate custom property",
            theme.id
        );
    }
}

#[test]
fn lookups_resolve() {
    assert_eq!(stratum_tokens::theme("light").unwrap().id, "light");
    assert_eq!(stratum_tokens::theme("dark").unwrap().id, "dark");
    assert!(
        stratum_tokens::theme("Light").is_none(),
        "lookup is case-sensitive"
    );
    assert!(
        stratum_tokens::theme("light_hc").is_none(),
        "the HC variants are not specified yet"
    );

    for s in &graph::SCHEMES {
        assert_eq!(stratum_tokens::scheme(s.id).unwrap().id, s.id);
        assert!(matches!(s.theme, "light" | "dark"));
    }
    assert!(
        stratum_tokens::scheme(graph::DEFAULT_SCHEME).is_some(),
        "the default scheme must be one of the declared schemes"
    );
    assert!(stratum_tokens::scheme("s2color").is_none());
}

/// `print` is the one scheme that must not track the user's theme: a figure
/// exported into a paper is white ground and black ink whatever the app looks
/// like. Asserting it here is cheaper than discovering it in a PDF.
#[test]
fn print_is_paper() {
    let p = stratum_tokens::scheme("print").expect("print scheme");
    assert_eq!(p.background.hex, "#FFFFFF");
    assert_eq!(p.plot_background.hex, "#FFFFFF");
    assert_eq!(p.foreground.hex, "#000000");
    assert_eq!(p.axis.hex, "#000000");
    let light = stratum_tokens::scheme("stratum").expect("stratum scheme");
    assert_eq!(
        p.series, light.series,
        "print and the light app scheme share unmodified Okabe-Ito"
    );
}
