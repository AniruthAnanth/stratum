//! Design tokens, generated from `design/tokens.json` by `cargo xtask tokens`.
//!
//! DO NOT EDIT. This file is a committed build artifact: `cargo xtask tokens
//! --check` regenerates it into a buffer and fails CI if the bytes differ
//! (ARCHITECTURE §8.14). Change `design/tokens.json` and regenerate.
//!
//! The emission rules are specified in `crates/stratum-tokens/README.md`.

use crate::{
    Accent, Color, DataPalette, Elevation, FontFamily, FontSize, Gradient, Metric, Motion, Scheme,
    Semantic, StatePalette, StateToken, Theme, Token, Weight,
};

/// `schema` from `design/tokens.json`. Bumped when the file's shape changes.
pub const SCHEMA: u32 = 1;

/// Type families, sizes and weights.
pub mod typography {
    use super::*;

    /// Bundled woff2 stacks. Nothing that must align in columns falls back to a
    /// system font.
    pub mod families {
        use super::*;

        /// all UI text
        pub const SANS: FontFamily = FontFamily {
            var: "--font-sans",
            stack: &["IBM Plex Sans", "system-ui", "sans-serif"],
        };

        /// editor, classic output, every numeral in a table
        pub const MONO: FontFamily = FontFamily {
            var: "--font-mono",
            stack: &["IBM Plex Mono", "ui-monospace", "monospace"],
        };

        /// rendered narrative only
        pub const SERIF: FontFamily = FontFamily {
            var: "--font-serif",
            stack: &["IBM Plex Serif", "Georgia", "serif"],
        };
    }

    /// Six sizes, and no pane shows more than three of them.
    pub mod sizes {
        use super::*;

        /// meta, state readout, section eyebrows
        pub const MICRO: FontSize = FontSize {
            var: "--fs-micro",
            px: 11.0,
            line_height_px: 14.0,
        };
        /// `letter_spacing_em` of the `micro` size.
        pub const MICRO_LETTER_SPACING_EM: f32 = 0.06;
        /// `uppercase` of the `micro` size.
        pub const MICRO_UPPERCASE: bool = true;

        /// table numerals in cards, dense grids, card header
        pub const SMALL: FontSize = FontSize {
            var: "--fs-small",
            px: 12.0,
            line_height_px: 16.0,
        };

        /// all UI text, labels, menus
        pub const BODY: FontSize = FontSize {
            var: "--fs-body",
            px: 13.0,
            line_height_px: 18.0,
        };

        /// editor; size is user-adjustable, the ratio is locked
        pub const CODE: FontSize = FontSize {
            var: "--fs-code",
            px: 13.0,
            line_height_px: 20.0,
        };
        /// `user_min_px` of the `code` size.
        pub const CODE_USER_MIN_PX: f32 = 11.0;
        /// `user_max_px` of the `code` size.
        pub const CODE_USER_MAX_PX: f32 = 18.0;
        /// `line_height_ratio` of the `code` size.
        pub const CODE_LINE_HEIGHT_RATIO: f32 = 1.54;

        /// rendered narrative (serif)
        pub const MID: FontSize = FontSize {
            var: "--fs-mid",
            px: 15.0,
            line_height_px: 25.0,
        };
        /// `measure_ch` of the `mid` size.
        pub const MID_MEASURE_CH: f32 = 68.0;

        /// modal titles and empty states only
        pub const LARGE: FontSize = FontSize {
            var: "--fs-large",
            px: 18.0,
            line_height_px: 24.0,
        };
    }

    /// 400, 500, 600 only. No 700: bold headings fight monospace output.
    pub mod weights {
        use super::*;

        /// `--fw-regular` = 400.
        pub const REGULAR: Weight = Weight {
            var: "--fw-regular",
            value: 400,
        };

        /// `--fw-medium` = 500.
        pub const MEDIUM: Weight = Weight {
            var: "--fw-medium",
            value: 500,
        };

        /// `--fw-semibold` = 600.
        pub const SEMIBOLD: Weight = Weight {
            var: "--fw-semibold",
            value: 600,
        };
    }

    /// The custom property carrying the numeral shaping below.
    pub const NUMERALS_VAR: &str = "--font-numeric";
    /// Global `font-variant-numeric`. Decimal alignment in result tables is
    /// `text-align: right` plus a per-column ch padding derived from the Stata
    /// display format — never a `text-align: "."` hack, never per-cell
    /// measurement.
    pub const NUMERALS: &str = "tabular-nums lining-nums";

    /// Acceptable substitutes for the mono family, in preference order.
    pub const MONO_ALTERNATES: &[&str] = &["Iosevka Term", "JetBrains Mono"];
}

/// 4 px base with a 2 px half-step for gutters and inline gaps.
pub mod space {
    /// Custom properties are `--sp-` followed by the step, e.g. `--sp-12`.
    pub const VAR_PREFIX: &str = "--sp-";
    /// The whole scale, ascending, in CSS px.
    pub const STEPS: [u16; 8] = [2, 4, 6, 8, 12, 16, 24, 32];
}

/// Fixed chrome heights. Density is the point.
pub mod metrics {
    use super::*;

    /// `--h-pane-header`.
    pub const PANE_HEADER: Metric = Metric {
        var: "--h-pane-header",
        px: 28.0,
    };

    /// `--h-toolbar`.
    pub const TOOLBAR: Metric = Metric {
        var: "--h-toolbar",
        px: 32.0,
    };

    /// `--h-top-bar`.
    pub const TOP_BAR: Metric = Metric {
        var: "--h-top-bar",
        px: 38.0,
    };

    /// `--h-status-bar`.
    pub const STATUS_BAR: Metric = Metric {
        var: "--h-status-bar",
        px: 22.0,
    };

    /// `--h-grid-row`.
    pub const GRID_ROW_DENSE: Metric = Metric {
        var: "--h-grid-row",
        px: 22.0,
    };

    /// `--h-grid-row-comfortable`.
    pub const GRID_ROW_COMFORTABLE: Metric = Metric {
        var: "--h-grid-row-comfortable",
        px: 26.0,
    };

    /// `--w-gutter`.
    pub const GUTTER: Metric = Metric {
        var: "--w-gutter",
        px: 18.0,
    };

    /// `--w-state-rail`.
    pub const STATE_RAIL: Metric = Metric {
        var: "--w-state-rail",
        px: 2.0,
    };

    /// `--card-pad-y`.
    pub const CARD_PADDING_Y: Metric = Metric {
        var: "--card-pad-y",
        px: 8.0,
    };

    /// `--card-pad-x`.
    pub const CARD_PADDING_X: Metric = Metric {
        var: "--card-pad-x",
        px: 12.0,
    };

    /// `--card-gap-above`.
    pub const CARD_GAP_ABOVE: Metric = Metric {
        var: "--card-gap-above",
        px: 8.0,
    };

    /// `--card-gap-below`.
    pub const CARD_GAP_BELOW: Metric = Metric {
        var: "--card-gap-below",
        px: 12.0,
    };

    /// `--hairline`.
    pub const HAIRLINE: Metric = Metric {
        var: "--hairline",
        px: 1.0,
    };
}

/// Nothing above 3 px exists in the product.
pub mod radius {
    use super::*;

    /// panes, rules, the state rail
    pub const NONE: Metric = Metric {
        var: "--radius-none",
        px: 0.0,
    };

    /// buttons, inputs, chips, card bodies
    pub const CONTROL: Metric = Metric {
        var: "--radius-control",
        px: 3.0,
    };
}

/// Exactly two levels. No result surface ever has a shadow.
pub mod elevation {
    use super::*;

    /// `--elev-flat`.
    pub const FLAT: Elevation = Elevation {
        var: "--elev-flat",
        light: "none",
        dark: "none",
    };

    /// `--elev-overlay`.
    pub const OVERLAY: Elevation = Elevation {
        var: "--elev-overlay",
        light: "0 1px 2px rgba(16,20,26,.10), 0 8px 24px rgba(16,20,26,.14)",
        dark: "0 12px 32px rgba(0,0,0,.5)",
    };

    /// Dark gets a 1 px outline instead of a soft shadow; soft shadows on dark
    /// read as smudges.
    pub const OVERLAY_DARK_OUTLINE_WIDTH_PX: f32 = 1.0;
    /// Which semantic token that outline is drawn in.
    pub const OVERLAY_DARK_OUTLINE_TOKEN: &str = "border_strong";
}

/// Near zero, and that is a feature.
pub mod motion {
    use super::*;

    /// glyph state changes and hover
    pub const STATE_MS: Motion = Motion {
        var: "--motion-state",
        ms: 90,
        easing: "ease-out",
    };

    /// card collapse/expand height only
    pub const COLLAPSE_MS: Motion = Motion {
        var: "--motion-collapse",
        ms: 120,
        easing: "cubic-bezier(.2,0,0,1)",
    };

    /// The custom property for the shuttle below.
    pub const SHUTTLE_S_VAR: &str = "--motion-shuttle";
    /// indeterminate work on the 1px running hairline. In SECONDS, as `design/tokens.json` spells it.
    pub const SHUTTLE_S: f32 = 2.0;

    /// Frontend contract for `prefers-reduced-motion: reduce`.
    pub const REDUCED_MOTION: &str = "Under prefers-reduced-motion:reduce both transitions are removed and the running hairline freezes at 50%.";
}

/// Square caps are the deliberate difference from Feather/Lucide's round caps.
pub mod icon {
    /// The design grid every glyph is drawn on, in CSS px.
    pub const GRID_PX: f32 = 14.0;
    /// Stroke width, in CSS px.
    pub const STROKE_PX: f32 = 1.25;
    /// SVG `stroke-linecap`.
    pub const LINECAP: &str = "square";
    /// SVG `stroke-linejoin`.
    pub const LINEJOIN: &str = "square";
    /// How many glyphs the set contains.
    pub const COUNT: u32 = 40;
}

/// The two full themes. `light` and `dark` have identical key sets at every
/// depth, so a consumer can iterate one and index the other.
pub mod color {
    use super::*;

    /// "Broadsheet Light".
    pub static LIGHT: Theme = Theme {
        id: "light",
        name: "Broadsheet Light",
        neutral: [
            Token::new("--n0", "#FFFFFF", 255, 255, 255),
            Token::new("--n1", "#FAFAFB", 250, 250, 251),
            Token::new("--n2", "#F4F5F6", 244, 245, 246),
            Token::new("--n3", "#EDEEF0", 237, 238, 240),
            Token::new("--n4", "#E3E5E8", 227, 229, 232),
            Token::new("--n5", "#D3D6DA", 211, 214, 218),
            Token::new("--n6", "#B4B9C0", 180, 185, 192),
            Token::new("--n7", "#696F79", 105, 111, 121),
            Token::new("--n8", "#5C636D", 92, 99, 109),
            Token::new("--n9", "#3A414B", 58, 65, 75),
            Token::new("--n10", "#232830", 35, 40, 48),
            Token::new("--n11", "#12151A", 18, 21, 26),
        ],
        semantic: Semantic {
            app_background: Token::new("--app-bg", "#E3E5E8", 227, 229, 232),
            canvas: Token::new("--canvas", "#FAFAFB", 250, 250, 251),
            surface: Token::new("--surface", "#F4F5F6", 244, 245, 246),
            overlay: Token::new("--overlay", "#FFFFFF", 255, 255, 255),
            input: Token::new("--input", "#EDEEF0", 237, 238, 240),
            border: Token::new("--border", "#E3E5E8", 227, 229, 232),
            border_strong: Token::new("--border-strong", "#D3D6DA", 211, 214, 218),
            rule_mid: Token::new("--rule-mid", "#D3D6DA", 211, 214, 218),
            rule_edge: Token::new("--rule-edge", "#12151A", 18, 21, 26),
            text_disabled: Token::new("--text-disabled", "#B4B9C0", 180, 185, 192),
            text_meta: Token::new("--text-meta", "#696F79", 105, 111, 121),
            text_label: Token::new("--text-label", "#5C636D", 92, 99, 109),
            text_secondary: Token::new("--text-secondary", "#3A414B", 58, 65, 75),
            text_body: Token::new("--text-body", "#232830", 35, 40, 48),
            text_heading: Token::new("--text-heading", "#12151A", 18, 21, 26),
        },
        accent: Accent {
            accent: Token::new("--accent", "#116A6A", 17, 106, 106),
            accent_hover: Token::new("--accent-hover", "#0D5757", 13, 87, 87),
            accent_subtle: Token::new("--accent-subtle", "#E2F0EF", 226, 240, 239),
        },
        state: StatePalette {
            ok: StateToken::new("--state-ok", "#2F7D4F", 47, 125, 79, "check"),
            stale: StateToken::new("--state-stale", "#9A6A00", 154, 106, 0, "hollow-dot"),
            failed: StateToken::new("--state-failed", "#B3261E", 179, 38, 30, "cross"),
            interrupted: StateToken::new("--state-interrupted", "#696F79", 105, 111, 121, "cross"),
            never_run: StateToken::new("--state-never-run", "#858D99", 133, 141, 153, "ring"),
        },
        data: DataPalette {
            categorical: [
                Color::new("#E69F00", 230, 159, 0),
                Color::new("#56B4E9", 86, 180, 233),
                Color::new("#009E73", 0, 158, 115),
                Color::new("#F0E442", 240, 228, 66),
                Color::new("#0072B2", 0, 114, 178),
                Color::new("#D55E00", 213, 94, 0),
                Color::new("#CC79A7", 204, 121, 167),
                Color::new("#000000", 0, 0, 0),
            ],
            sequential: Gradient {
                from: Color::new("#E2F0EF", 226, 240, 239),
                to: Color::new("#116A6A", 17, 106, 106),
            },
        },
    };

    /// "Broadsheet Dark".
    pub static DARK: Theme = Theme {
        id: "dark",
        name: "Broadsheet Dark",
        neutral: [
            Token::new("--n0", "#181D24", 24, 29, 36),
            Token::new("--n1", "#14181E", 20, 24, 30),
            Token::new("--n2", "#191E25", 25, 30, 37),
            Token::new("--n3", "#1F2530", 31, 37, 48),
            Token::new("--n4", "#2A313C", 42, 49, 60),
            Token::new("--n5", "#3A4350", 58, 67, 80),
            Token::new("--n6", "#59636F", 89, 99, 111),
            Token::new("--n7", "#7C8794", 124, 135, 148),
            Token::new("--n8", "#9AA4B0", 154, 164, 176),
            Token::new("--n9", "#B7C0CA", 183, 192, 202),
            Token::new("--n10", "#D6DCE3", 214, 220, 227),
            Token::new("--n11", "#EDF1F5", 237, 241, 245),
        ],
        semantic: Semantic {
            app_background: Token::new("--app-bg", "#0E1116", 14, 17, 22),
            canvas: Token::new("--canvas", "#14181E", 20, 24, 30),
            surface: Token::new("--surface", "#191E25", 25, 30, 37),
            overlay: Token::new("--overlay", "#181D24", 24, 29, 36),
            input: Token::new("--input", "#1F2530", 31, 37, 48),
            border: Token::new("--border", "#2A313C", 42, 49, 60),
            border_strong: Token::new("--border-strong", "#3A4350", 58, 67, 80),
            rule_mid: Token::new("--rule-mid", "#3A4350", 58, 67, 80),
            rule_edge: Token::new("--rule-edge", "#EDF1F5", 237, 241, 245),
            text_disabled: Token::new("--text-disabled", "#59636F", 89, 99, 111),
            text_meta: Token::new("--text-meta", "#7C8794", 124, 135, 148),
            text_label: Token::new("--text-label", "#9AA4B0", 154, 164, 176),
            text_secondary: Token::new("--text-secondary", "#B7C0CA", 183, 192, 202),
            text_body: Token::new("--text-body", "#D6DCE3", 214, 220, 227),
            text_heading: Token::new("--text-heading", "#EDF1F5", 237, 241, 245),
        },
        accent: Accent {
            accent: Token::new("--accent", "#3FB0A8", 63, 176, 168),
            accent_hover: Token::new("--accent-hover", "#5AC4BC", 90, 196, 188),
            accent_subtle: Token::new("--accent-subtle", "#12312F", 18, 49, 47),
        },
        state: StatePalette {
            ok: StateToken::new("--state-ok", "#4CAF7A", 76, 175, 122, "check"),
            stale: StateToken::new("--state-stale", "#D4A03C", 212, 160, 60, "hollow-dot"),
            failed: StateToken::new("--state-failed", "#F2726A", 242, 114, 106, "cross"),
            interrupted: StateToken::new("--state-interrupted", "#7C8794", 124, 135, 148, "cross"),
            never_run: StateToken::new("--state-never-run", "#606A77", 96, 106, 119, "ring"),
        },
        data: DataPalette {
            categorical: [
                Color::new("#FFBB24", 255, 187, 36),
                Color::new("#8CCCF0", 140, 204, 240),
                Color::new("#00DBA0", 0, 219, 160),
                Color::new("#F4EC7B", 244, 236, 123),
                Color::new("#0099EF", 0, 153, 239),
                Color::new("#FF7B13", 255, 123, 19),
                Color::new("#DDA5C4", 221, 165, 196),
                Color::new("#EDF1F5", 237, 241, 245),
            ],
            sequential: Gradient {
                from: Color::new("#12312F", 18, 49, 47),
                to: Color::new("#3FB0A8", 63, 176, 168),
            },
        },
    };

    /// Index order is stable: `[0]` is light, `[1]` is dark.
    pub static THEMES: [&Theme; 2] = [&LIGHT, &DARK];
}

/// Compiled into the binary and consumed by `stratum-graph`, which is L1 and
/// never reads a file at runtime (ARCHITECTURE C47 / §8.14).
pub mod graph {
    use super::*;

    /// The scheme used when the user names none.
    pub const DEFAULT_SCHEME: &str = "stratum";

    /// Ordered as `design/tokens.json` declares them.
    pub static SCHEMES: [Scheme; 3] = [
        Scheme {
            id: "stratum",
            theme: "light",
            background: Color::new("#FAFAFB", 250, 250, 251),
            plot_background: Color::new("#FAFAFB", 250, 250, 251),
            foreground: Color::new("#12151A", 18, 21, 26),
            axis: Color::new("#232830", 35, 40, 48),
            grid: Color::new("#E3E5E8", 227, 229, 232),
            rule: Color::new("#D3D6DA", 211, 214, 218),
            text: Color::new("#232830", 35, 40, 48),
            text_meta: Color::new("#696F79", 105, 111, 121),
            series: [
                Color::new("#E69F00", 230, 159, 0),
                Color::new("#56B4E9", 86, 180, 233),
                Color::new("#009E73", 0, 158, 115),
                Color::new("#F0E442", 240, 228, 66),
                Color::new("#0072B2", 0, 114, 178),
                Color::new("#D55E00", 213, 94, 0),
                Color::new("#CC79A7", 204, 121, 167),
                Color::new("#000000", 0, 0, 0),
            ],
            sequential: Gradient {
                from: Color::new("#E2F0EF", 226, 240, 239),
                to: Color::new("#116A6A", 17, 106, 106),
            },
        },
        Scheme {
            id: "stratum-dark",
            theme: "dark",
            background: Color::new("#14181E", 20, 24, 30),
            plot_background: Color::new("#14181E", 20, 24, 30),
            foreground: Color::new("#EDF1F5", 237, 241, 245),
            axis: Color::new("#D6DCE3", 214, 220, 227),
            grid: Color::new("#2A313C", 42, 49, 60),
            rule: Color::new("#3A4350", 58, 67, 80),
            text: Color::new("#D6DCE3", 214, 220, 227),
            text_meta: Color::new("#7C8794", 124, 135, 148),
            series: [
                Color::new("#FFBB24", 255, 187, 36),
                Color::new("#8CCCF0", 140, 204, 240),
                Color::new("#00DBA0", 0, 219, 160),
                Color::new("#F4EC7B", 244, 236, 123),
                Color::new("#0099EF", 0, 153, 239),
                Color::new("#FF7B13", 255, 123, 19),
                Color::new("#DDA5C4", 221, 165, 196),
                Color::new("#EDF1F5", 237, 241, 245),
            ],
            sequential: Gradient {
                from: Color::new("#12312F", 18, 49, 47),
                to: Color::new("#3FB0A8", 63, 176, 168),
            },
        },
        Scheme {
            id: "print",
            theme: "light",
            background: Color::new("#FFFFFF", 255, 255, 255),
            plot_background: Color::new("#FFFFFF", 255, 255, 255),
            foreground: Color::new("#000000", 0, 0, 0),
            axis: Color::new("#000000", 0, 0, 0),
            grid: Color::new("#D3D6DA", 211, 214, 218),
            rule: Color::new("#000000", 0, 0, 0),
            text: Color::new("#000000", 0, 0, 0),
            text_meta: Color::new("#5C636D", 92, 99, 109),
            series: [
                Color::new("#E69F00", 230, 159, 0),
                Color::new("#56B4E9", 86, 180, 233),
                Color::new("#009E73", 0, 158, 115),
                Color::new("#F0E442", 240, 228, 66),
                Color::new("#0072B2", 0, 114, 178),
                Color::new("#D55E00", 213, 94, 0),
                Color::new("#CC79A7", 204, 121, 167),
                Color::new("#000000", 0, 0, 0),
            ],
            sequential: Gradient {
                from: Color::new("#E2F0EF", 226, 240, 239),
                to: Color::new("#116A6A", 17, 106, 106),
            },
        },
    ];
}

/// WCAG 2.1 relative-luminance floors. The check that enforces them walks
/// `design/tokens.json` inside `cargo xtask tokens`; these constants exist so a
/// Rust consumer can state the same policy without re-reading the file.
pub mod a11y {
    /// Body text against its own surface.
    pub const MIN_CONTRAST_BODY_TEXT: f32 = 7.0;
    /// Meta text against its own surface.
    pub const MIN_CONTRAST_META_TEXT: f32 = 4.5;
    /// State glyphs against their own surface.
    pub const MIN_CONTRAST_GLYPH: f32 = 3.0;
    /// Rules that carry meaning against their own surface. Decorative
    /// separators are explicitly out of scope; see `a11y.known_exceptions` in
    /// `design/tokens.json`.
    pub const MIN_CONTRAST_RULE: f32 = 3.0;
}
