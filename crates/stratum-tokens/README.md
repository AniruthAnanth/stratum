# `stratum-tokens`

The design system as compile-time constants, generated from the repository
root's `design/tokens.json`.

`src/generated.rs` and `apps/desktop/resources/tokens.generated.css` are
**committed build artifacts**. `cargo xtask tokens` writes them;
`cargo xtask tokens --check` regenerates both into buffers and fails when the
bytes differ from what is on disk (ARCHITECTURE §8.14, ADR A14). Never hand-edit
either file — change `design/tokens.json` and regenerate.

Everything else in this crate is hand-written and owned normally: `src/lib.rs`
declares the types `generated.rs` instantiates, and `tests/generated.rs` checks
that the emitter did not lie.

## Why the crate exists at all

`stratum-graph` is an L1 crate that must render headless: the graph drawn inline
in the app and the same graph drawn by `stratum run` on a build server with no
display and no config directory have to come out byte-identical. Before A14 it
was told to read `apps/desktop/design/tokens.json` — a file owned by a frontend
unit, at a path that only exists in a source checkout. Now the colours are
`static` data in the binary, and CI greps `crates/stratum-graph/src` for
`std::fs`, `Utf8Path` and `include_str!` to keep it that way.

That is also why `[dependencies]` is empty and the crate is `#![no_std]`. A
dependency here is a dependency of every graph render.

## The emission contract

Two implementations of the emitter have to agree byte-for-byte or `--check`
fails on a clean tree, so the rules are spelled out rather than left to taste.

### Shared

- **Underscore keys are documentation.** `design/tokens.json`'s own
  `_conventions` say generators must ignore every key beginning with `_`, and
  neither artifact contains one. That includes `_about`, `_note`, `_why`,
  `_rule`, `_thesis`, `_character`, `_deferred` and `_uncontrolled`.
- **Key order is file order.** Both artifacts emit objects in the order
  `design/tokens.json` declares them. Nothing is sorted.
- **`ref` is dropped.** It records which neutral step a semantic role aliases;
  it is provenance for a human reading the JSON, and it is recoverable from the
  hex.
- **`use` becomes a doc comment**, not a field.
- Colours are copied verbatim as uppercase `#RRGGBB`; the emitter derives the
  `Rgb` triple from that string and never the other way round.

### `src/generated.rs`

- Module nesting mirrors object nesting: `typography.sizes.body` →
  `typography::sizes::BODY`. The constant name is `SCREAMING_SNAKE_CASE` of the
  JSON key.
- A leaf object's extra keys become sibling constants named
  `<KEY>_<SUBKEY>`: `sizes.code.user_min_px` → `sizes::CODE_USER_MIN_PX`.
- Every public item carries a doc comment: the token's `use` string when it has
  one, otherwise a fixed phrase chosen by the emitter. `missing_docs` is a
  warning in this crate and the build is warning-clean.
- Colours are emitted through `Color::new` / `Token::new` / `StateToken::new`
  rather than as struct literals. This is not cosmetic: spelled as literals,
  rustfmt expands every nested `Rgb` across five lines and the file goes from
  ~510 lines to ~1710. The whole point of committing a generated artifact is
  that a human reads its diff.
- Floats are emitted with an explicit fractional part (`11` → `11.0`) in the
  shortest form that round-trips.
- **The emitter does not format.** It writes the file and then runs `rustfmt
  --edition 2021` against the workspace `rustfmt.toml`. Trying to hit
  `max_width = 100` by hand is the one rule two independent implementations
  will not agree on.

### `apps/desktop/resources/tokens.generated.css`

- A custom property is emitted for every token that `design/tokens.json` gives a
  `var` for. `var` is authoritative and never derived: the names in
  `06-ui-architecture.md` §14.5 are irregular — `--n7`, `--text-meta` and
  `--accent` are the same shape of thing with three different naming schemes.
- Five families of property have no `var` in the JSON. Their names are fixed
  here, and **this is a W00 ruling, not something `06` states**:

  | Family | Name | From |
  |---|---|---|
  | line height | `--lh-*` | the size's `var` with `--fs-` swapped for `--lh-` |
  | letter spacing | `--ls-*` | same swap |
  | motion easing | `--motion-*-easing` | the motion token's `var` plus `-easing` |
  | spacing steps | `--sp-<step>` | `space.var_prefix` plus the step |
  | icon | `--icon-grid`, `--icon-stroke`, `--icon-linecap`, `--icon-linejoin` | `icon.*`, trailing `_px` dropped |
  | data palette | `--data-1`…`--data-8`, `--data-seq-from`, `--data-seq-to` | `color.themes.*.data` |

- `icon.count` is not emitted. It is a fact about the icon set, not a style
  value, and a CSS custom property nothing can consume is dead weight in a file
  people review.
- `elevation.overlay.dark_outline` is not emitted either. It is a rule about how
  the overlay is *composed* in dark — a 1 px `--border-strong` outline instead
  of a soft shadow — not a value, and turning it into a property means the
  emitter deciding the shorthand. It reaches Rust as
  `elevation::OVERLAY_DARK_OUTLINE_WIDTH_PX` / `_TOKEN` and reaches the
  frontend through this note.
- Theme selection is the three-state pattern, also a W00 ruling — `06` names
  two themes and never says how one is selected:

  ```
  :root                                        { /* light  */ }
  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"])            { /* dark   */ }
  }
  :root[data-theme="dark"]                     { /* dark   */ }
  ```

  `LayoutSpec.defaults.theme` (`CONTRACTS.md` §9.1) is exactly
  `"light" | "dark" | "system"`, and "system" is the attribute being absent. The
  theme-independent tokens — typography, space, metrics, radius, motion, icon —
  are defined once on bare `:root` and never redefined, so a theme switch
  touches only colour. Each block also sets `color-scheme`, which is what makes
  native scrollbars and form controls follow.

## Known open items

- **`--n7` misses its own contrast floor in light.** `06` §14.5 states meta text
  ≥ 4.5:1; `#8A9099` on `#FAFAFB` measures 3.08:1 (dark clears it at 4.88). It
  is recorded in `a11y.known_exceptions` with the measured value rather than
  papered over. Resolving it is a design ruling — either `--n7` darkens to about
  `#767C85` or the policy line changes — and belongs to `06` / W12, so the value
  shipped here is the one the design doc states.
- **The high-contrast variants are absent, not invented.** `06` §14.5 promises
  one per theme and gives no values. Whoever specifies them adds `light_hc` and
  `dark_hc` under `color.themes` with identical key sets; both artifacts pick
  them up with no code change, and `stratum_tokens::theme("light_hc")` starts
  resolving.
- **`light.semantic.app_background` (`#E3E5E8`) is inferred.** `06` gives the
  app background behind panes only for dark (`#0E1116`). A step darker than pane
  chrome is the light-theme analogue. Confirm with W12.
- **The contrast check is not in this crate.** It needs `powf` for the sRGB
  transfer function, which ARCHITECTURE §8.11 (A19) bans everywhere under
  `crates/`. `IMPLEMENTATION_PLAN` W12 puts it in `cargo xtask tokens`, which
  lives outside `crates/` and reads `design/tokens.json` directly. What this
  crate's tests prove instead is that every `hex` agrees with its `rgb`, that
  the two themes are the same shape, and that no `var` collides.
