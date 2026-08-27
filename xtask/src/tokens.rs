//! ARCHITECTURE §8.14 / C47 / audit finding A14 — the design-token codegen.
//!
//! `design/tokens.json` is the single source of design truth. This generator
//! compiles it into two **committed** artifacts:
//!
//! * `crates/stratum-tokens/src/generated.rs` — consumed by `stratum-graph`,
//!   which is an L1 crate that must render headless and therefore does no path
//!   resolution and reads no file at run time. `SCHEMES` is why this exists.
//! * `apps/desktop/resources/tokens.generated.css` — the frontend's custom
//!   properties, under the exact `var` names `06` §14.5 pins down.
//!
//! `--check` re-generates in memory and diffs; CI fails on drift, which is what
//! makes "committed generated code" safe.
//!
//! **Ownership.** This module is the generator only. The two artifacts live
//! under `crates/stratum-tokens/**` and `apps/desktop/resources/`, which
//! IMPLEMENTATION_PLAN §8 assigns to W00's tokens agent; whoever owns those
//! paths runs `cargo xtask tokens` and commits the result. The emission rules
//! are that crate's `README.md`, because its hand-written `src/lib.rs` declares
//! the types this file instantiates and its `tests/generated.rs` checks them —
//! so the contract belongs next to the types, and the generator follows it.
//!
//! **Formatting.** The Rust artifact is piped through `rustfmt` before it is
//! written, because `cargo fmt --all --check` is a required PR check and it does
//! not skip generated files. `#![rustfmt::skip]` would be the hermetic answer,
//! but a custom inner attribute is still unstable (rust#54726). The toolchain is
//! pinned in `rust-toolchain.toml`, so piping is deterministic.
//!
//! **Totality.** The generator tracks which parts of `tokens.json` it consumed
//! and fails if any non-underscore leaf was ignored. Without that, adding a
//! token to the JSON would silently produce artifacts that do not contain it,
//! and `--check` would still pass — the drift gate would be checking the
//! generator against itself.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt::Write as _;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::Ctx;

const RUST_ARTIFACT: &str = "crates/stratum-tokens/src/generated.rs";
const CSS_ARTIFACT: &str = "apps/desktop/resources/tokens.generated.css";

#[derive(Args)]
pub struct Cmd {
    /// Verify the committed artifacts instead of writing them. Exits non-zero
    /// on any drift. This is what CI runs (ARCHITECTURE §8.14).
    #[arg(long)]
    pub check: bool,

    /// Read this instead of `design/tokens.json`.
    #[arg(long, value_name = "FILE")]
    pub source: Option<camino::Utf8PathBuf>,
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    let source = cmd
        .source
        .clone()
        .unwrap_or_else(|| ctx.path("design/tokens.json"));
    let text = std::fs::read_to_string(&source).with_context(|| format!("reading {source}"))?;
    let generated =
        generate(&text, &ctx.root).with_context(|| format!("generating from {source}"))?;

    let targets = [
        (ctx.path(RUST_ARTIFACT), generated.rust, RUST_ARTIFACT),
        (ctx.path(CSS_ARTIFACT), generated.css, CSS_ARTIFACT),
    ];

    if !cmd.check {
        for (path, body, rel) in targets {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
            }
            std::fs::write(&path, &body).with_context(|| format!("writing {path}"))?;
            println!("tokens: wrote {rel} ({} bytes)", body.len());
        }
        return Ok(());
    }

    let mut drift = Vec::new();
    for (path, want, rel) in targets {
        match std::fs::read_to_string(&path) {
            Ok(got) if got == want => {}
            Ok(got) => drift.push(format!("{rel}: {}", describe_drift(&got, &want))),
            Err(e) => drift.push(format!("{rel}: {e}")),
        }
    }
    if drift.is_empty() {
        println!("tokens: OK — both artifacts match design/tokens.json");
        return Ok(());
    }
    for d in &drift {
        eprintln!("tokens: {d}");
    }
    anyhow::bail!("run `cargo xtask tokens` and commit the result");
}

/// Pipe Rust source through the pinned `rustfmt`. Failing loudly beats writing
/// a file that `cargo fmt --check` will reject on the contributor's next push.
fn rustfmt(src: &str, config_dir: &camino::Utf8Path) -> Result<String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("rustfmt");
    cmd.args(["--emit", "stdout", "--edition", "2021"]);
    // Only when the repository actually has one; rustfmt errors rather than
    // falling back if `--config-path` names a directory without a config.
    if config_dir.join("rustfmt.toml").is_file() {
        cmd.arg("--config-path").arg(config_dir);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning rustfmt (it ships with the pinned toolchain)")?;
    child
        .stdin
        .take()
        .context("rustfmt stdin")?
        .write_all(src.as_bytes())
        .context("writing to rustfmt")?;
    let out = child.wait_with_output().context("waiting for rustfmt")?;
    anyhow::ensure!(
        out.status.success(),
        "rustfmt rejected the generated source: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let text = String::from_utf8(out.stdout).context("rustfmt produced non-UTF-8")?;
    // `--emit stdout` prefixes a `<stdin>:` banner; the file is everything after it.
    Ok(match text.split_once('\n') {
        Some((first, rest)) if first.starts_with("<stdin>:") => rest.to_owned(),
        _ => text,
    })
}

fn describe_drift(got: &str, want: &str) -> String {
    for (i, (a, b)) in got.lines().zip(want.lines()).enumerate() {
        if a != b {
            return format!(
                "line {} differs\n      committed: {a}\n      generated: {b}",
                i + 1
            );
        }
    }
    format!(
        "committed file has {} line(s), generated has {}",
        got.lines().count(),
        want.lines().count()
    )
}

#[derive(Debug)]
pub struct Generated {
    pub rust: String,
    pub css: String,
}

// ---------------------------------------------------------------------------
// A cursor that remembers what it read
// ---------------------------------------------------------------------------

/// Reads `tokens.json` by JSON pointer and records every pointer it touched, so
/// `unconsumed()` can prove the generator covered the whole document. Keys
/// beginning with `_` are documentation for a human and are excluded from the
/// audit by the file's own `_conventions`.
struct Doc {
    root: Value,
    seen: RefCell<BTreeSet<String>>,
}

impl Doc {
    fn parse(text: &str) -> Result<Self> {
        Ok(Self {
            root: serde_json::from_str(text).context("design/tokens.json is not valid JSON")?,
            seen: RefCell::new(BTreeSet::new()),
        })
    }

    fn touch(&self, ptr: &str) {
        self.seen.borrow_mut().insert(ptr.to_owned());
    }

    fn get(&self, ptr: &str) -> Result<&Value> {
        self.touch(ptr);
        self.root
            .pointer(ptr)
            .with_context(|| format!("{ptr} is missing"))
    }

    fn opt(&self, ptr: &str) -> Option<&Value> {
        self.touch(ptr);
        self.root.pointer(ptr).filter(|v| !v.is_null())
    }

    fn s(&self, ptr: &str) -> Result<&str> {
        self.get(ptr)?
            .as_str()
            .with_context(|| format!("{ptr} is not a string"))
    }

    fn f(&self, ptr: &str) -> Result<f64> {
        self.get(ptr)?
            .as_f64()
            .with_context(|| format!("{ptr} is not a number"))
    }

    fn u(&self, ptr: &str) -> Result<u64> {
        self.get(ptr)?
            .as_u64()
            .with_context(|| format!("{ptr} is not an unsigned integer"))
    }

    fn strs(&self, ptr: &str) -> Result<Vec<String>> {
        Ok(self
            .get(ptr)?
            .as_array()
            .with_context(|| format!("{ptr} is not an array"))?
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_owned())
            .collect())
    }

    /// Child keys of an object, in document order, skipping `_`-prefixed ones.
    fn keys(&self, ptr: &str) -> Result<Vec<String>> {
        self.touch(ptr);
        Ok(self
            .root
            .pointer(ptr)
            .with_context(|| format!("{ptr} is missing"))?
            .as_object()
            .with_context(|| format!("{ptr} is not an object"))?
            .keys()
            .filter(|k| !k.starts_with('_'))
            .cloned()
            .collect())
    }

    /// Every leaf the generator never looked at. An empty result is the
    /// invariant that makes `--check` meaningful.
    fn unconsumed(&self) -> Vec<String> {
        let seen = self.seen.borrow();
        let mut out = Vec::new();
        let mut stack = vec![(String::new(), &self.root)];
        while let Some((ptr, value)) = stack.pop() {
            if !ptr.is_empty()
                && seen
                    .iter()
                    .any(|s| ptr == *s || ptr.starts_with(&format!("{s}/")))
            {
                continue;
            }
            match value {
                Value::Object(map) => {
                    for (k, v) in map {
                        if k.starts_with('_') {
                            continue;
                        }
                        stack.push((format!("{ptr}/{}", escape_ptr(k)), v));
                    }
                }
                Value::Array(items) => {
                    for (i, v) in items.iter().enumerate() {
                        stack.push((format!("{ptr}/{i}"), v));
                    }
                }
                _ => out.push(ptr),
            }
        }
        out.sort();
        out
    }
}

fn escape_ptr(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// `rustfmt_config_dir` is where `rustfmt.toml` lives — the repository root.
pub fn generate(source: &str, rustfmt_config_dir: &camino::Utf8Path) -> Result<Generated> {
    let doc = Doc::parse(source)?;
    let schema = doc.u("/schema")?;
    anyhow::ensure!(
        schema == 1,
        "design/tokens.json schema {schema} is not supported"
    );
    doc.touch("/_about");
    doc.touch("/_conventions");

    verify_semantic_refs(&doc)?;
    verify_contrast(&doc)?;

    let rust = rustfmt(&emit_rust(&doc, schema)?, rustfmt_config_dir)?;
    let css = emit_css(&doc)?;

    let missed = doc.unconsumed();
    anyhow::ensure!(
        missed.is_empty(),
        "design/tokens.json has {} token(s) this generator ignores, so the \
         committed artifacts would be silently incomplete:\n  {}",
        missed.len(),
        missed.join("\n  ")
    );

    Ok(Generated { rust, css })
}

/// `semantic.*.ref` claims a step on the neutral ramp. Checking it here is what
/// stops the two from drifting apart in a hand edit — the whole reason the
/// aliases carry a `ref` at all.
fn verify_semantic_refs(doc: &Doc) -> Result<()> {
    for theme in doc.keys("/color/themes")? {
        for key in doc.keys(&format!("/color/themes/{theme}/semantic"))? {
            let base = format!("/color/themes/{theme}/semantic/{key}");
            let Some(reference) = doc.opt(&format!("{base}/ref")) else {
                continue;
            };
            let step = reference
                .as_str()
                .context("semantic `ref` is not a string")?;
            let want = doc.s(&format!("/color/themes/{theme}/neutral/{step}/value"))?;
            let got = doc.s(&format!("{base}/value"))?;
            anyhow::ensure!(
                want == got,
                "{theme}.semantic.{key} = {got} but claims ref = {step} ({want})"
            );
        }
    }
    Ok(())
}

/// `06` §14.5's contrast policy, recomputed rather than trusted. `tokens.json`
/// records a `measured` ratio for every enforced pair; if the palette moves and
/// the recorded number does not, this is what notices.
fn verify_contrast(doc: &Doc) -> Result<()> {
    doc.touch("/a11y/_about");
    doc.touch("/a11y/_uncontrolled");
    doc.touch("/a11y/min_contrast/_source");

    let entries = doc
        .get("/a11y/enforced")?
        .as_array()
        .context("a11y.enforced")?
        .len();
    let exceptions = doc
        .get("/a11y/known_exceptions")?
        .as_array()
        .context("a11y.known_exceptions")?
        .len();

    for (list, enforce) in [("enforced", true), ("known_exceptions", false)] {
        let n = if enforce { entries } else { exceptions };
        for i in 0..n {
            let base = format!("/a11y/{list}/{i}");
            doc.touch(&format!("{base}/_note"));
            let fg = doc.s(&format!("{base}/fg"))?.to_owned();
            let bg = doc.s(&format!("{base}/bg"))?.to_owned();
            let floor = doc.f(&format!(
                "{base}/{}",
                if enforce { "min" } else { "policy_min" }
            ))?;
            for theme in ["light", "dark"] {
                let recorded = doc.f(&format!("{base}/measured/{theme}"))?;
                let actual = contrast(
                    resolve_color(doc, theme, &fg)?,
                    resolve_color(doc, theme, &bg)?,
                );
                anyhow::ensure!(
                    (round2(actual) - recorded).abs() < 0.011,
                    "a11y.{list}[{i}] {fg} on {bg} ({theme}): recorded {recorded}, \
                     computed {:.2}",
                    actual
                );
                anyhow::ensure!(
                    !enforce || actual >= floor,
                    "a11y.enforced[{i}] {fg} on {bg} ({theme}) measures {:.2}, \
                     below the {floor} floor from 06 §14.5",
                    actual
                );
            }
        }
    }
    Ok(())
}

/// `text_body` -> semantic, `state.ok` -> state, `accent` -> accent,
/// `n7` -> neutral. Ambiguity is resolved by trying the groups in that order,
/// which is the order the names are written in `06` §14.5.
fn resolve_color(doc: &Doc, theme: &str, name: &str) -> Result<&'static str> {
    let ptr = match name.split_once('.') {
        Some((group, key)) => format!("/color/themes/{theme}/{group}/{key}/value"),
        None => {
            let mut found = None;
            for group in ["semantic", "accent", "neutral", "state"] {
                let candidate = format!("/color/themes/{theme}/{group}/{name}/value");
                if doc.root.pointer(&candidate).is_some() {
                    found = Some(candidate);
                    break;
                }
            }
            found.with_context(|| format!("a11y names colour {name}, which no group defines"))?
        }
    };
    let hex = doc.s(&ptr)?;
    // Leaked deliberately: the token set is tiny, fixed, and lives for the
    // process's whole run. It buys a `&'static str` for the contrast helpers
    // without threading a lifetime through the resolver.
    Ok(Box::leak(hex.to_owned().into_boxed_str()))
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// WCAG 2.1 relative luminance contrast, `(Lmax + .05) / (Lmin + .05)`.
fn contrast(a: &str, b: &str) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

// ARCHITECTURE §8.11 bans `powf` "anywhere under `crates/`" so that a result
// never depends on which libm the host shipped. xtask is not under `crates/`
// and this number never reaches a user: it is a build-time assertion about a
// colour, compared to two decimal places. clippy.toml cannot express the path
// scope, so the exemption is stated here instead of weakening the lint.
#[allow(clippy::disallowed_methods)]
fn luminance(hex: &str) -> f64 {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    let channel = |shift: u32| {
        let c = ((v >> shift) & 0xFF) as f64 / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

// ---------------------------------------------------------------------------
// Rust artifact
// ---------------------------------------------------------------------------
//
// The emission contract is `crates/stratum-tokens/README.md`, which is that
// crate's own document: `src/lib.rs` declares the types instantiated below and
// `tests/generated.rs` asserts the emitter did not lie. Two rules from it drive
// everything here:
//
//   * Module nesting mirrors object nesting and the constant name is
//     SCREAMING_SNAKE_CASE of the JSON key; a leaf object's extra keys become
//     sibling constants named `<KEY>_<SUBKEY>`.
//   * Colours go out through `Color::new` / `Token::new` / `StateToken::new`
//     rather than as struct literals. Not cosmetic: as literals rustfmt expands
//     every nested `Rgb` over five lines and the committed file triples in size,
//     which defeats the only reason to commit generated code — that a human
//     reads its diff.
//
// Prose that `design/tokens.json` does not carry (module summaries, the doc
// comment on a token with no `use`) is the emitter's, and lives here.

const RUST_HEADER: &str = "\
//! Design tokens, generated from `design/tokens.json` by `cargo xtask tokens`.
//!
//! DO NOT EDIT. This file is a committed build artifact: `cargo xtask tokens
//! --check` regenerates it into a buffer and fails CI if the bytes differ
//! (ARCHITECTURE §8.14). Change `design/tokens.json` and regenerate.
//!
//! The emission rules are specified in `crates/stratum-tokens/README.md`.

use crate::{Accent, Color, DataPalette, Elevation, FontFamily, FontSize, Gradient, Metric, Motion, Scheme, Semantic, StatePalette, StateToken, Theme, Token, Weight};

";

fn emit_rust(doc: &Doc, schema: u64) -> Result<String> {
    let mut o = String::with_capacity(24 * 1024);
    o.push_str(RUST_HEADER);
    o.push_str("/// `schema` from `design/tokens.json`. Bumped when the file's shape changes.\n");
    writeln!(o, "pub const SCHEMA: u32 = {schema};\n")?;

    emit_typography(doc, &mut o)?;
    emit_space(doc, &mut o)?;
    emit_metrics(doc, &mut o)?;
    emit_elevation(doc, &mut o)?;
    emit_motion(doc, &mut o)?;
    emit_icon(doc, &mut o)?;
    emit_color(doc, &mut o)?;
    emit_graph(doc, &mut o)?;
    emit_a11y(doc, &mut o)?;
    Ok(o)
}

/// A token's doc comment: its `use` string when `design/tokens.json` gives one,
/// otherwise the custom property it carries. Every public item in the artifact
/// has one — `missing_docs` is a warning in `stratum-tokens` and the crate is
/// warning-clean.
fn token_doc(doc: &Doc, base: &str, indent: &str) -> Result<String> {
    let text = match doc.opt(&format!("{base}/use")) {
        Some(v) => v.as_str().unwrap_or_default().to_owned(),
        None => format!("`{}`.", doc.s(&format!("{base}/var"))?),
    };
    Ok(format!("{indent}/// {text}\n"))
}

fn emit_typography(doc: &Doc, o: &mut String) -> Result<()> {
    o.push_str(
        "/// Type families, sizes and weights.\npub mod typography {\n    use super::*;\n\n",
    );

    o.push_str(
        "    /// Bundled woff2 stacks. Nothing that must align in columns falls back to a\n\
         \x20   /// system font.\n    pub mod families {\n        use super::*;\n",
    );
    for key in doc.keys("/typography/families")? {
        let b = format!("/typography/families/{key}");
        let stack = doc.strs(&format!("{b}/stack"))?;
        o.push('\n');
        o.push_str(&token_doc(doc, &b, "        ")?);
        writeln!(
            o,
            "        pub const {}: FontFamily = FontFamily {{ var: {:?}, stack: &[{}] }};",
            key.to_uppercase(),
            doc.s(&format!("{b}/var"))?,
            stack
                .iter()
                .map(|s| format!("{s:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    o.push_str("    }\n\n");

    o.push_str(
        "    /// Six sizes, and no pane shows more than three of them.\n\
         \x20   pub mod sizes {\n        use super::*;\n",
    );
    for key in doc.keys("/typography/sizes")? {
        let b = format!("/typography/sizes/{key}");
        o.push('\n');
        o.push_str(&token_doc(doc, &b, "        ")?);
        writeln!(
            o,
            "        pub const {}: FontSize = FontSize {{ var: {:?}, px: {}, line_height_px: {} }};",
            key.to_uppercase(),
            doc.s(&format!("{b}/var"))?,
            num(doc.f(&format!("{b}/px"))?),
            num(doc.f(&format!("{b}/line_height_px"))?),
        )?;
        // A leaf object's extra keys become sibling constants, so a size that
        // carries `user_min_px` does not need a wider `FontSize` for the five
        // sizes that do not.
        for extra in doc.keys(&b)? {
            if matches!(extra.as_str(), "var" | "px" | "line_height_px" | "use") {
                continue;
            }
            let value = doc.get(&format!("{b}/{extra}"))?.clone();
            let (ty, lit) = match &value {
                Value::Bool(v) => ("bool", v.to_string()),
                Value::Number(n) => ("f32", num(n.as_f64().unwrap_or_default())),
                other => anyhow::bail!("typography.sizes.{key}.{extra} is {other}, not a scalar"),
            };
            writeln!(o, "        /// `{extra}` of the `{key}` size.")?;
            writeln!(
                o,
                "        pub const {}_{}: {ty} = {lit};",
                key.to_uppercase(),
                extra.to_uppercase()
            )?;
        }
    }
    o.push_str("    }\n\n");

    o.push_str(
        "    /// 400, 500, 600 only. No 700: bold headings fight monospace output.\n\
         \x20   pub mod weights {\n        use super::*;\n",
    );
    for key in doc.keys("/typography/weights")? {
        let b = format!("/typography/weights/{key}");
        let var = doc.s(&format!("{b}/var"))?;
        let value = doc.u(&format!("{b}/value"))?;
        o.push('\n');
        writeln!(o, "        /// `{var}` = {value}.")?;
        writeln!(
            o,
            "        pub const {}: Weight = Weight {{ var: {var:?}, value: {value} }};",
            key.to_uppercase()
        )?;
    }
    o.push_str("    }\n\n");

    writeln!(
        o,
        "    /// The custom property carrying the numeral shaping below.\n\
         \x20   pub const NUMERALS_VAR: &str = {:?};",
        doc.s("/typography/numerals/var")?
    )?;
    writeln!(
        o,
        "    /// Global `font-variant-numeric`. Decimal alignment in result tables is\n\
         \x20   /// `text-align: right` plus a per-column ch padding derived from the Stata\n\
         \x20   /// display format — never a `text-align: \".\"` hack, never per-cell\n\
         \x20   /// measurement.\n    pub const NUMERALS: &str = {:?};",
        doc.s("/typography/numerals/value")?
    )?;

    let count = doc
        .get("/typography/mono_alternates")?
        .as_array()
        .context("mono_alternates")?
        .len();
    let mut names = Vec::new();
    for i in 0..count {
        names.push(format!(
            "{:?}",
            doc.s(&format!("/typography/mono_alternates/{i}/name"))?
        ));
    }
    writeln!(
        o,
        "\n    /// Acceptable substitutes for the mono family, in preference order.\n\
         \x20   pub const MONO_ALTERNATES: &[&str] = &[{}];\n}}\n",
        names.join(", ")
    )?;
    Ok(())
}

fn emit_space(doc: &Doc, o: &mut String) -> Result<()> {
    let steps = doc
        .get("/space/steps")?
        .as_array()
        .context("space.steps")?
        .clone();
    let list: Vec<String> = steps
        .iter()
        .map(|v| {
            v.as_u64()
                .map(|n| n.to_string())
                .context("a space step is not an integer")
        })
        .collect::<Result<_>>()?;
    writeln!(
        o,
        "\n/// 4 px base with a 2 px half-step for gutters and inline gaps.\n\
         pub mod space {{\n\
         \x20   /// Custom properties are `{0}` followed by the step, e.g. `{0}12`.\n\
         \x20   pub const VAR_PREFIX: &str = {0:?};\n\
         \x20   /// The whole scale, ascending, in CSS px.\n\
         \x20   pub const STEPS: [u16; {1}] = [{2}];\n}}",
        doc.s("/space/var_prefix")?,
        list.len(),
        list.join(", ")
    )?;
    Ok(())
}

fn emit_metrics(doc: &Doc, o: &mut String) -> Result<()> {
    for (section, summary) in [
        ("metrics", "Fixed chrome heights. Density is the point."),
        ("radius", "Nothing above 3 px exists in the product."),
    ] {
        writeln!(
            o,
            "\n/// {summary}\npub mod {section} {{\n    use super::*;"
        )?;
        for key in doc.keys(&format!("/{section}"))? {
            let b = format!("/{section}/{key}");
            o.push('\n');
            o.push_str(&token_doc(doc, &b, "    ")?);
            writeln!(
                o,
                "    pub const {}: Metric = Metric {{ var: {:?}, px: {} }};",
                key.to_uppercase(),
                doc.s(&format!("{b}/var"))?,
                num(doc.f(&format!("{b}/px"))?)
            )?;
        }
        o.push_str("}\n");
    }
    Ok(())
}

fn emit_elevation(doc: &Doc, o: &mut String) -> Result<()> {
    o.push_str(
        "\n/// Exactly two levels. No result surface ever has a shadow.\n\
         pub mod elevation {\n    use super::*;\n",
    );
    for key in doc.keys("/elevation")? {
        let b = format!("/elevation/{key}");
        o.push('\n');
        o.push_str(&token_doc(doc, &b, "    ")?);
        writeln!(
            o,
            "    pub const {}: Elevation = Elevation {{ var: {:?}, light: {:?}, dark: {:?} }};",
            key.to_uppercase(),
            doc.s(&format!("{b}/var"))?,
            doc.s(&format!("{b}/light"))?,
            doc.s(&format!("{b}/dark"))?
        )?;
    }
    writeln!(
        o,
        "\n    /// Dark gets a 1 px outline instead of a soft shadow; soft shadows on dark\n\
         \x20   /// read as smudges.\n\
         \x20   pub const OVERLAY_DARK_OUTLINE_WIDTH_PX: f32 = {};\n\
         \x20   /// Which semantic token that outline is drawn in.\n\
         \x20   pub const OVERLAY_DARK_OUTLINE_TOKEN: &str = {:?};\n}}",
        num(doc.f("/elevation/overlay/dark_outline/width_px")?),
        doc.s("/elevation/overlay/dark_outline/token")?
    )?;
    Ok(())
}

fn emit_motion(doc: &Doc, o: &mut String) -> Result<()> {
    o.push_str("\n/// Near zero, and that is a feature.\npub mod motion {\n    use super::*;\n");
    for key in doc.keys("/motion")? {
        let b = format!("/motion/{key}");
        let name = key.to_uppercase();

        // `reduced_motion` is prose: a contract with the frontend, not a value.
        if doc.root.pointer(&b).is_some_and(Value::is_string) {
            writeln!(
                o,
                "\n    /// Frontend contract for `prefers-reduced-motion: reduce`.\n\
                 \x20   pub const {name}: &str = {:?};",
                doc.s(&b)?
            )?;
            continue;
        }

        let var = doc.s(&format!("{b}/var"))?.to_owned();
        o.push('\n');
        match doc.opt(&format!("{b}/ms")) {
            Some(ms) => {
                let ms = ms.as_u64().context("motion ms")?;
                o.push_str(&token_doc(doc, &b, "    ")?);
                writeln!(
                    o,
                    "    pub const {name}: Motion = Motion {{ var: {var:?}, ms: {ms}, easing: {:?} }};",
                    doc.s(&format!("{b}/easing"))?
                )?;
            }
            // A token spelled in seconds stays in seconds. Converting it here
            // would make the constant disagree with the key it is named after.
            None => {
                let secs = doc.f(&format!("{b}/s"))?;
                let use_text = doc
                    .opt(&format!("{b}/use"))
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                writeln!(o, "    /// The custom property for the shuttle below.")?;
                writeln!(o, "    pub const {name}_VAR: &str = {var:?};")?;
                writeln!(
                    o,
                    "    /// {use_text}. In SECONDS, as `design/tokens.json` spells it."
                )?;
                writeln!(o, "    pub const {name}: f32 = {};", num(secs))?;
            }
        }
    }
    o.push_str("}\n");
    Ok(())
}

fn emit_icon(doc: &Doc, o: &mut String) -> Result<()> {
    const DOCS: &[(&str, &str)] = &[
        (
            "grid_px",
            "The design grid every glyph is drawn on, in CSS px.",
        ),
        ("stroke_px", "Stroke width, in CSS px."),
        ("linecap", "SVG `stroke-linecap`."),
        ("linejoin", "SVG `stroke-linejoin`."),
        ("count", "How many glyphs the set contains."),
    ];
    o.push_str(
        "\n/// Square caps are the deliberate difference from Feather/Lucide's round caps.\n\
         pub mod icon {\n",
    );
    for key in doc.keys("/icon")? {
        let summary = DOCS
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, d)| *d)
            .with_context(|| format!("icon.{key} has no doc comment in the emitter"))?;
        let value = doc.get(&format!("/icon/{key}"))?.clone();
        let (ty, lit) = match &value {
            Value::String(s) => ("&str", format!("{s:?}")),
            Value::Number(n) if n.is_u64() && !key.ends_with("_px") => {
                ("u32", n.as_u64().unwrap().to_string())
            }
            Value::Number(n) => ("f32", num(n.as_f64().unwrap_or_default())),
            other => anyhow::bail!("icon.{key} is {other}, not a scalar"),
        };
        writeln!(o, "    /// {summary}")?;
        writeln!(o, "    pub const {}: {ty} = {lit};", key.to_uppercase())?;
    }
    o.push_str("}\n");
    Ok(())
}

/// `Color::new("#RRGGBB", r, g, b)` — the hex is the source of truth and the
/// channels are derived from it, never the other way round.
fn color_new(hex: &str) -> Result<String> {
    let (r, g, b) = rgb(hex)?;
    Ok(format!("Color::new({hex:?}, {r}, {g}, {b})"))
}

fn token_new(var: &str, hex: &str) -> Result<String> {
    let (r, g, b) = rgb(hex)?;
    Ok(format!("Token::new({var:?}, {hex:?}, {r}, {g}, {b})"))
}

fn state_new(var: &str, hex: &str, glyph: &str) -> Result<String> {
    let (r, g, b) = rgb(hex)?;
    Ok(format!(
        "StateToken::new({var:?}, {hex:?}, {r}, {g}, {b}, {glyph:?})"
    ))
}

pub fn rgb(hex: &str) -> Result<(u8, u8, u8)> {
    let body = hex
        .strip_prefix('#')
        .with_context(|| format!("{hex} is not #RRGGBB"))?;
    anyhow::ensure!(
        body.len() == 6
            && body
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c)),
        "{hex} must be uppercase #RRGGBB"
    );
    let v = u32::from_str_radix(body, 16).with_context(|| format!("{hex} is not hexadecimal"))?;
    Ok((
        ((v >> 16) & 0xFF) as u8,
        ((v >> 8) & 0xFF) as u8,
        (v & 0xFF) as u8,
    ))
}

fn emit_color(doc: &Doc, o: &mut String) -> Result<()> {
    let themes = doc.keys("/color/themes")?;
    o.push_str(
        "\n/// The two full themes. `light` and `dark` have identical key sets at every\n\
         /// depth, so a consumer can iterate one and index the other.\n\
         pub mod color {\n    use super::*;\n",
    );

    for theme in &themes {
        let t = format!("/color/themes/{theme}");
        let name = doc.s(&format!("{t}/name"))?.to_owned();

        let mut neutral = Vec::new();
        for key in doc.keys(&format!("{t}/neutral"))? {
            let b = format!("{t}/neutral/{key}");
            doc.touch(&format!("{b}/use"));
            neutral.push(token_new(
                doc.s(&format!("{b}/var"))?,
                doc.s(&format!("{b}/value"))?,
            )?);
        }

        let mut groups: Vec<String> = Vec::new();
        for group in ["semantic", "accent", "state"] {
            let mut fields = Vec::new();
            for key in doc.keys(&format!("{t}/{group}"))? {
                let b = format!("{t}/{group}/{key}");
                // `ref` is provenance for a human reading the JSON and is
                // recoverable from the hex; `use` becomes a doc comment on the
                // struct field in the hand-written lib.rs, not a value here.
                doc.touch(&format!("{b}/ref"));
                doc.touch(&format!("{b}/use"));
                let var = doc.s(&format!("{b}/var"))?;
                let value = doc.s(&format!("{b}/value"))?;
                fields.push(match doc.opt(&format!("{b}/glyph")) {
                    Some(g) => format!(
                        "{key}: {}",
                        state_new(var, value, g.as_str().unwrap_or_default())?
                    ),
                    None => format!("{key}: {}", token_new(var, value)?),
                });
            }
            let ty = match group {
                "semantic" => "Semantic",
                "accent" => "Accent",
                _ => "StatePalette",
            };
            groups.push(format!("{ty} {{ {} }}", fields.join(", ")));
        }

        let mut categorical = Vec::new();
        for hex in doc.strs(&format!("{t}/data/categorical"))? {
            categorical.push(color_new(&hex)?);
        }
        let ramp = format!(
            "Gradient {{ from: {}, to: {} }}",
            color_new(doc.s(&format!("{t}/data/sequential/from"))?)?,
            color_new(doc.s(&format!("{t}/data/sequential/to"))?)?
        );

        writeln!(
            o,
            "\n    /// {name:?}.\n\
             \x20   pub static {}: Theme = Theme {{ id: {theme:?}, name: {name:?}, \
             neutral: [{}], semantic: {}, accent: {}, state: {}, \
             data: DataPalette {{ categorical: [{}], sequential: {ramp} }} }};",
            theme.to_uppercase().replace('-', "_"),
            neutral.join(", "),
            groups[0],
            groups[1],
            groups[2],
            categorical.join(", "),
        )?;
    }

    let idents: Vec<String> = themes
        .iter()
        .map(|t| t.to_uppercase().replace('-', "_"))
        .collect();
    writeln!(
        o,
        "\n    /// Index order is stable: `[0]` is light, `[1]` is dark.\n\
         \x20   pub static THEMES: [&Theme; {}] = [{}];\n}}",
        idents.len(),
        idents
            .iter()
            .map(|i| format!("&{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    Ok(())
}

fn emit_graph(doc: &Doc, o: &mut String) -> Result<()> {
    let ids = doc.keys("/graph/schemes")?;
    writeln!(
        o,
        "\n/// Compiled into the binary and consumed by `stratum-graph`, which is L1 and\n\
         /// never reads a file at runtime (ARCHITECTURE C47 / §8.14).\n\
         pub mod graph {{\n    use super::*;\n\n\
         \x20   /// The scheme used when the user names none.\n\
         \x20   pub const DEFAULT_SCHEME: &str = {:?};\n\n\
         \x20   /// Ordered as `design/tokens.json` declares them.\n\
         \x20   pub static SCHEMES: [Scheme; {}] = [",
        doc.s("/graph/default_scheme")?,
        ids.len()
    )?;
    for id in &ids {
        let b = format!("/graph/schemes/{id}");
        let mut fields = vec![
            format!("id: {id:?}"),
            format!("theme: {:?}", doc.s(&format!("{b}/theme"))?),
        ];
        for key in [
            "background",
            "plot_background",
            "foreground",
            "axis",
            "grid",
            "rule",
            "text",
            "text_meta",
        ] {
            fields.push(format!(
                "{key}: {}",
                color_new(doc.s(&format!("{b}/{key}"))?)?
            ));
        }
        let mut series = Vec::new();
        for hex in doc.strs(&format!("{b}/series"))? {
            series.push(color_new(&hex)?);
        }
        fields.push(format!("series: [{}]", series.join(", ")));
        fields.push(format!(
            "sequential: Gradient {{ from: {}, to: {} }}",
            color_new(doc.s(&format!("{b}/sequential/from"))?)?,
            color_new(doc.s(&format!("{b}/sequential/to"))?)?
        ));
        writeln!(o, "        Scheme {{ {} }},", fields.join(", "))?;
    }
    o.push_str("    ];\n}\n");
    Ok(())
}

fn emit_a11y(doc: &Doc, o: &mut String) -> Result<()> {
    const DOCS: &[(&str, &str)] = &[
        ("body_text", "Body text against its own surface."),
        ("meta_text", "Meta text against its own surface."),
        ("glyph", "State glyphs against their own surface."),
        (
            "rule",
            "Rules that carry meaning against their own surface. Decorative\n\
             \x20   /// separators are explicitly out of scope; see `a11y.known_exceptions` in\n\
             \x20   /// `design/tokens.json`.",
        ),
    ];
    o.push_str(
        "\n/// WCAG 2.1 relative-luminance floors. The check that enforces them walks\n\
         /// `design/tokens.json` inside `cargo xtask tokens`; these constants exist so a\n\
         /// Rust consumer can state the same policy without re-reading the file.\n\
         pub mod a11y {\n",
    );
    for key in doc.keys("/a11y/min_contrast")? {
        let summary = DOCS
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, d)| *d)
            .with_context(|| {
                format!("a11y.min_contrast.{key} has no doc comment in the emitter")
            })?;
        writeln!(o, "    /// {summary}")?;
        writeln!(
            o,
            "    pub const MIN_CONTRAST_{}: f32 = {};",
            key.to_uppercase(),
            num(doc.f(&format!("/a11y/min_contrast/{key}"))?)
        )?;
    }
    o.push_str("}\n");
    Ok(())
}

/// Floats carry an explicit fractional part in the shortest form that
/// round-trips: `11` becomes `11.0`, `1.25` stays `1.25`.
fn num(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

// ---------------------------------------------------------------------------
// CSS artifact
// ---------------------------------------------------------------------------
//
// Five families of custom property have no `var` in design/tokens.json. Their
// names are fixed by `crates/stratum-tokens/README.md` — a W00 ruling, not
// something `06` states — and are the only names in this file the emitter
// invents: `--lh-*` and `--ls-*` (the size's own var with `--fs-` swapped),
// `--motion-*-easing`, `--sp-<step>`, `--icon-*`, and `--data-1`…`--data-8`
// plus `--data-seq-from` / `--data-seq-to`.

const CSS_HEADER: &str = "\
/* Design tokens, generated from design/tokens.json by `cargo xtask tokens`.
 *
 * DO NOT EDIT. This file is a committed build artifact: `cargo xtask tokens
 * --check` regenerates it into a buffer and fails CI if the bytes differ
 * (ARCHITECTURE §8.14). Change design/tokens.json and regenerate.
 *
 * Theme selection follows the three-state pattern: the light palette is the
 * unconditional :root definition, dark applies under `prefers-color-scheme`
 * unless the root element carries an explicit `data-theme=\"light\"`, and
 * `data-theme=\"dark\"` wins in both directions. `LayoutSpec.defaults.theme`
 * (CONTRACTS.md §9.1) has exactly the three states this encodes: \"light\",
 * \"dark\" and \"system\" (the attribute absent).
 */

";

/// Rules are padded to a fixed column so the sections are scannable in a diff.
const RULE_WIDTH: usize = 77;

fn section(o: &mut String, indent: &str, title: &str) {
    let head = format!("{indent}/* ---- {title} ");
    let dashes = RULE_WIDTH.saturating_sub(head.len() + 3).max(1);
    let _ = writeln!(o, "{head}{} */", "-".repeat(dashes));
}

fn emit_css(doc: &Doc) -> Result<String> {
    let mut o = String::with_capacity(12 * 1024);
    o.push_str(CSS_HEADER);
    o.push_str(":root {\n");

    section(&mut o, "  ", "typography");
    for key in doc.keys("/typography/families")? {
        let b = format!("/typography/families/{key}");
        writeln!(
            &mut o,
            "  {}: {};",
            doc.s(&format!("{b}/var"))?,
            doc.strs(&format!("{b}/stack"))?
                .iter()
                .map(|f| if f.contains(' ') {
                    format!("\"{f}\"")
                } else {
                    f.clone()
                })
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    writeln!(
        &mut o,
        "  {}: {};",
        doc.s("/typography/numerals/var")?,
        doc.s("/typography/numerals/value")?
    )?;
    o.push_str(
        "\n  /* Line height and letter spacing get no `var` in tokens.json; the names are\n\
         \x20    the size's own var with `--fs-` swapped for `--lh-` / `--ls-`. */\n",
    );
    for key in doc.keys("/typography/sizes")? {
        let b = format!("/typography/sizes/{key}");
        let var = doc.s(&format!("{b}/var"))?.to_owned();
        writeln!(
            &mut o,
            "  {var}: {}px;",
            css_num(doc.f(&format!("{b}/px"))?)
        )?;
        writeln!(
            &mut o,
            "  {}: {}px;",
            swap_prefix(&var, "--lh-")?,
            css_num(doc.f(&format!("{b}/line_height_px"))?)
        )?;
        if let Some(ls) = doc
            .opt(&format!("{b}/letter_spacing_em"))
            .and_then(Value::as_f64)
        {
            writeln!(
                &mut o,
                "  {}: {}em;",
                swap_prefix(&var, "--ls-")?,
                css_num(ls)
            )?;
        }
    }
    o.push('\n');
    for key in doc.keys("/typography/weights")? {
        let b = format!("/typography/weights/{key}");
        writeln!(
            &mut o,
            "  {}: {};",
            doc.s(&format!("{b}/var"))?,
            doc.u(&format!("{b}/value"))?
        )?;
    }

    o.push('\n');
    section(&mut o, "  ", "space");
    let prefix = doc.s("/space/var_prefix")?.to_owned();
    for step in doc
        .get("/space/steps")?
        .as_array()
        .context("space.steps")?
        .clone()
    {
        let v = step.as_u64().context("a space step is not an integer")?;
        writeln!(&mut o, "  {prefix}{v}: {v}px;")?;
    }

    for sect in ["metrics", "radius"] {
        o.push('\n');
        section(&mut o, "  ", sect);
        for key in doc.keys(&format!("/{sect}"))? {
            let b = format!("/{sect}/{key}");
            writeln!(
                &mut o,
                "  {}: {}px;",
                doc.s(&format!("{b}/var"))?,
                css_num(doc.f(&format!("{b}/px"))?)
            )?;
        }
    }

    o.push('\n');
    section(&mut o, "  ", "motion");
    for key in doc.keys("/motion")? {
        let b = format!("/motion/{key}");
        if doc.root.pointer(&b).is_some_and(Value::is_string) {
            continue; // `reduced_motion` is a contract, not a value.
        }
        let var = doc.s(&format!("{b}/var"))?.to_owned();
        match doc.opt(&format!("{b}/ms")) {
            Some(ms) => {
                writeln!(&mut o, "  {var}: {}ms;", ms.as_u64().context("motion ms")?)?;
                writeln!(
                    &mut o,
                    "  {var}-easing: {};",
                    doc.s(&format!("{b}/easing"))?
                )?;
            }
            None => writeln!(&mut o, "  {var}: {}s;", css_num(doc.f(&format!("{b}/s"))?))?,
        }
    }

    o.push('\n');
    section(&mut o, "  ", "icon");
    for key in doc.keys("/icon")? {
        // `icon.count` is a fact about the icon set, not a style value; a custom
        // property nothing can consume is dead weight in a reviewed file.
        if key == "count" {
            doc.touch("/icon/count");
            continue;
        }
        let value = doc.get(&format!("/icon/{key}"))?.clone();
        let name = key.strip_suffix("_px").unwrap_or(&key);
        match &value {
            Value::String(s) => writeln!(&mut o, "  --icon-{name}: {s};")?,
            Value::Number(n) => writeln!(
                &mut o,
                "  --icon-{name}: {}px;",
                css_num(n.as_f64().unwrap_or_default())
            )?,
            other => anyhow::bail!("icon.{key} is {other}, not a scalar"),
        }
    }

    let themes = doc.keys("/color/themes")?;
    let (light, dark) = (
        themes.first().context("color.themes is empty")?.clone(),
        themes.get(1).cloned(),
    );

    o.push('\n');
    section(
        &mut o,
        "  ",
        &format!(
            "colour: {:?}",
            doc.s(&format!("/color/themes/{light}/name"))?
        ),
    );
    theme_block(doc, &mut o, &light, "  ")?;
    o.push_str("}\n");

    if let Some(dark) = dark {
        let name = doc.s(&format!("/color/themes/{dark}/name"))?.to_owned();
        writeln!(
            &mut o,
            "\n/* {name:?}. Applies when the OS asks for dark and the document has not\n\
             \x20  pinned light. */\n\
             @media (prefers-color-scheme: {dark}) {{\n  :root:not([data-theme={light:?}]) {{"
        )?;
        theme_block(doc, &mut o, &dark, "    ")?;
        o.push_str("  }\n}\n");

        writeln!(
            &mut o,
            "\n/* An explicit choice wins over the OS in both directions. */\n\
             :root[data-theme={dark:?}] {{"
        )?;
        theme_block(doc, &mut o, &dark, "  ")?;
        o.push_str("}\n");
    }
    Ok(o)
}

fn theme_block(doc: &Doc, o: &mut String, theme: &str, i: &str) -> Result<()> {
    let t = format!("/color/themes/{theme}");
    writeln!(o, "{i}color-scheme: {theme};")?;
    for (group, label) in [
        ("neutral", "neutral ramp"),
        ("semantic", "semantic roles"),
        ("accent", "accent"),
        (
            "state",
            "block state — also the gutter and state-rail colours",
        ),
    ] {
        if group != "neutral" {
            o.push('\n');
        }
        writeln!(o, "{i}/* {label} */")?;
        for key in doc.keys(&format!("{t}/{group}"))? {
            let b = format!("{t}/{group}/{key}");
            writeln!(
                o,
                "{i}{}: {};",
                doc.s(&format!("{b}/var"))?,
                doc.s(&format!("{b}/value"))?
            )?;
        }
    }

    writeln!(o, "\n{i}/* data palette (Okabe–Ito) */")?;
    for (n, colour) in doc
        .strs(&format!("{t}/data/categorical"))?
        .iter()
        .enumerate()
    {
        writeln!(o, "{i}--data-{}: {colour};", n + 1)?;
    }
    writeln!(
        o,
        "{i}--data-seq-from: {};",
        doc.s(&format!("{t}/data/sequential/from"))?
    )?;
    writeln!(
        o,
        "{i}--data-seq-to: {};",
        doc.s(&format!("{t}/data/sequential/to"))?
    )?;

    writeln!(o, "\n{i}/* elevation */")?;
    for key in doc.keys("/elevation")? {
        let b = format!("/elevation/{key}");
        writeln!(
            o,
            "{i}{}: {};",
            doc.s(&format!("{b}/var"))?,
            doc.s(&format!("{b}/{theme}"))?
        )?;
    }
    Ok(())
}

/// `--fs-code` -> `--lh-code`. The size's `var` is authoritative and the derived
/// names hang off it, so a rename in tokens.json moves all three together.
fn swap_prefix(var: &str, to: &str) -> Result<String> {
    let stem = var
        .strip_prefix("--fs-")
        .with_context(|| format!("{var} is not a `--fs-` custom property"))?;
    Ok(format!("{to}{stem}"))
}

fn css_num(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> camino::Utf8PathBuf {
        camino::Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn tokens() -> String {
        std::fs::read_to_string(root().join("design/tokens.json"))
            .expect("design/tokens.json is readable")
    }

    fn gen(source: &str) -> Result<Generated> {
        generate(source, &root())
    }

    #[test]
    fn the_real_token_file_generates() {
        let g = gen(&tokens()).expect("generation succeeds");
        assert!(g.rust.starts_with("//! Design tokens, generated"));
        assert!(g.css.starts_with("/* Design tokens, generated"));
        // The one thing stratum-graph actually needs.
        assert!(g.rust.contains("pub static SCHEMES: [Scheme; 3]"));
        assert!(g
            .rust
            .contains("pub const DEFAULT_SCHEME: &str = \"stratum\";"));
        // Colours go out through the constructors with the channels derived
        // from the hex, never restated (README, "Shared").
        assert!(g
            .rust
            .contains(r##"Token::new("--n0", "#FFFFFF", 255, 255, 255)"##));
        // And the names 06 §14.5 pins down.
        assert!(g.css.contains("--n0: #FFFFFF;"));
        assert!(g.css.contains("--accent: #116A6A;"));
        assert!(g.css.contains(":root[data-theme=\"dark\"]"));
        assert!(g.css.contains("@media (prefers-color-scheme: dark)"));
    }

    /// The emission contract in `crates/stratum-tokens/README.md` exists because
    /// two implementations of it have to agree byte-for-byte or `--check` fails
    /// on a clean tree. This is that agreement as a test rather than a hope: the
    /// committed artifact was written against the contract by the crate's own
    /// author, and this generator reproduces it exactly.
    #[test]
    fn the_generator_reproduces_the_committed_artifact() {
        let committed = root().join("crates/stratum-tokens/src/generated.rs");
        let Ok(want) = std::fs::read_to_string(&committed) else {
            return; // the crate has not landed yet
        };
        assert_eq!(gen(&tokens()).unwrap().rust, want, "{committed}");
    }

    #[test]
    fn hex_channels_are_derived_not_guessed() {
        assert_eq!(rgb("#FFFFFF").unwrap(), (255, 255, 255));
        assert_eq!(rgb("#116A6A").unwrap(), (17, 106, 106));
        assert_eq!(rgb("#000000").unwrap(), (0, 0, 0));
        // tokens.json's own convention is uppercase 6-digit hex; anything else
        // is a typo worth stopping on rather than normalising silently.
        assert!(rgb("#116a6a").is_err());
        assert!(rgb("116A6A").is_err());
        assert!(rgb("#FFF").is_err());
    }

    #[test]
    fn generation_is_deterministic() {
        let a = gen(&tokens()).unwrap();
        let b = gen(&tokens()).unwrap();
        assert_eq!(a.rust, b.rust);
        assert_eq!(a.css, b.css);
    }

    /// The drift gate: an edit to `generated.rs` must fail `--check`. Simulated
    /// by comparing the generated text against a hand-mutated copy, which is
    /// exactly what `run(--check)` does with the file on disk.
    #[test]
    fn a_hand_edit_to_the_artifact_is_detected() {
        let g = gen(&tokens()).unwrap();
        let tampered = g.rust.replace("#116A6A", "#FF00FF");
        assert_ne!(tampered, g.rust, "the accent must appear in the artifact");
        assert!(describe_drift(&tampered, &g.rust).contains("differs"));
    }

    /// The totality guard: a token nobody consumes must fail generation rather
    /// than silently vanish from both artifacts.
    #[test]
    fn an_unconsumed_token_fails_generation() {
        let mut json: Value = serde_json::from_str(&tokens()).unwrap();
        json["metrics"]["invented"] = serde_json::json!({ "var": "--h-invented", "px": 7 });
        // A key the generator does read, so this is a real omission test.
        let ok = gen(&serde_json::to_string(&json).unwrap());
        assert!(ok.is_ok(), "a well-formed new metric is consumed: {ok:?}");

        let mut json: Value = serde_json::from_str(&tokens()).unwrap();
        json["invented_section"] = serde_json::json!({ "thing": 1 });
        let err = gen(&serde_json::to_string(&json).unwrap()).unwrap_err();
        assert!(
            format!("{err:#}").contains("/invented_section/thing"),
            "{err:#}"
        );
    }

    /// `semantic.*.ref` is a claim about the neutral ramp; breaking it must fail.
    #[test]
    fn a_broken_semantic_ref_fails() {
        let mut json: Value = serde_json::from_str(&tokens()).unwrap();
        json["color"]["themes"]["light"]["semantic"]["canvas"]["value"] = Value::from("#123456");
        let err = gen(&serde_json::to_string(&json).unwrap()).unwrap_err();
        assert!(format!("{err:#}").contains("claims ref = n1"), "{err:#}");
    }

    /// The recorded contrast ratios must be recomputed, not trusted.
    #[test]
    fn a_stale_contrast_measurement_fails() {
        let mut json: Value = serde_json::from_str(&tokens()).unwrap();
        json["a11y"]["enforced"][0]["measured"]["light"] = Value::from(99.0);
        let err = gen(&serde_json::to_string(&json).unwrap()).unwrap_err();
        assert!(format!("{err:#}").contains("recorded 99"), "{err:#}");
    }

    /// Darkening body text below the §14.5 floor must be caught even if someone
    /// updates the recorded ratio to match.
    #[test]
    fn a_palette_below_the_contrast_floor_fails() {
        let mut json: Value = serde_json::from_str(&tokens()).unwrap();
        for (key, value) in [("n10", "#9AA0A8"), ("text_body", "#9AA0A8")] {
            let group = if key.starts_with('n') {
                "neutral"
            } else {
                "semantic"
            };
            json["color"]["themes"]["light"][group][key]["value"] = Value::from(value);
        }
        let measured = round2(contrast("#9AA0A8", "#FAFAFB"));
        json["a11y"]["enforced"][0]["measured"]["light"] = Value::from(measured);
        json["a11y"]["enforced"][1]["measured"]["light"] =
            Value::from(round2(contrast("#9AA0A8", "#F4F5F6")));
        let err = gen(&serde_json::to_string(&json).unwrap()).unwrap_err();
        assert!(format!("{err:#}").contains("below the 7 floor"), "{err:#}");
    }

    /// WCAG 2.1 has published reference values; these are two of them.
    #[test]
    fn contrast_matches_the_wcag_reference() {
        assert!((contrast("#000000", "#FFFFFF") - 21.0).abs() < 1e-9);
        assert!((contrast("#FFFFFF", "#FFFFFF") - 1.0).abs() < 1e-9);
        assert!((contrast("#777777", "#FFFFFF") - 4.478).abs() < 0.001);
    }
}
