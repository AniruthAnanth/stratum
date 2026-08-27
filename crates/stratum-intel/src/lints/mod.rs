//! The `L001`–`L012` registry, and the five dataflow lints this crate owns.
//!
//! # ONE registry, and where each rule actually lives
//!
//! ARCHITECTURE C14 merged three overlapping lint schemes into one namespace:
//! "`L###` — editor/parse lints. `02`'s `L001`–`L007`, plus `07`'s dataflow
//! lints as `L008`–`L012`." So the twelve codes are one list, but they are
//! implemented in two crates, because seven of them are questions about a single
//! parsed command and five are questions about the whole file:
//!
//! * `L001`–`L007` are `stratum-parse`'s. They run per-command, at keystroke
//!   latency, inside the wasm segmenter, and this module **delegates** to
//!   [`stratum_parse::lint`] rather than reimplementing them. A second copy is
//!   how a code seen in the problems pane, in `--json` and in a
//!   `*! nolint(L006)` suppression stops being the same string.
//! * `L008`–`L012` are here. Each needs the [`dataflow::Doc`] model — what was
//!   created earlier in the file, what a `capture` swallowed, whether a macro
//!   can be empty — which is a file-scope fact and not a command-scope one.
//!
//! [`registry`] is the union, and `tests/lints.rs` asserts it is complete,
//! unique, contiguous and consistent with what the two implementations emit.
//!
//! # The unobtrusiveness contract (07 §6.3)
//!
//! A lint renders as a 2 px dotted underline plus one gutter dot: no inline
//! text, no lightbulb, no popup, at most one decoration per line and
//! [`MAX_DECORATIONS_PER_FILE`] per file, with the overflow reported as a single
//! count in the status bar. That is a presentation rule, but the cap belongs
//! here so every consumer gets the same number — [`lint_document`] returns
//! everything and [`decorations`] applies the rule.

pub mod dataflow;
pub mod facts;
mod l008_replace_undeclared;
mod l009_predict_stale;
mod l010_capture_unchecked;
mod l011_loop_empty_macro;
mod l012_missing_setup;

use stratum_proto::diagnostic::{Confidence, Diagnostic, Severity, Suggestion};
use stratum_proto::Span;

use crate::{Env, ParseIndex};

pub use dataflow::{Doc, Stmt};

/// Design 07 §6.3: at most one decoration per line and 25 per file.
pub const MAX_DECORATIONS_PER_FILE: usize = 25;

/// One rule in the registry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LintMeta {
    /// The wire code, `"L001"`.
    pub code: &'static str,
    /// Default severity.
    pub severity: Severity,
    /// One line, sentence case, no trailing period. What the gutter card titles.
    pub title: &'static str,
    /// The rule, in the words design 02 §11 / design 07 §6.3 wrote it.
    pub rule: &'static str,
    /// Which crate implements it.
    pub owner: Owner,
    /// Whether the rule can offer a deterministic `[Fix]`.
    pub has_fix: bool,
}

/// Where a rule is implemented.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Owner {
    /// `stratum-parse`, per parsed command (design 02 §11).
    Parse,
    /// `stratum-intel`, over the whole file (design 07 §6.3).
    Intel,
}

/// The twelve lints, in code order.
///
/// Transcribed from design 02 §11's table (`L001`–`L007`) and design 07 §6.3's
/// table (`L008`–`L012`) under ARCHITECTURE C14's renumbering. C14 allots five
/// slots to `07`'s ten dataflow lints because the other five had already been
/// given `R###` codes by design 03: `07`'s merge pair is `R015`, its seed rule
/// is `R002`, its absolute-path rule is `R001`, and its uninstalled-ado rule is
/// `R025`. Putting them in both namespaces would have produced two codes for one
/// finding, which is exactly what C14 exists to prevent.
pub const REGISTRY: &[LintMeta] = &[
    LintMeta {
        code: "L001",
        severity: Severity::Warning,
        title: "Macro interpolated into a plain-quoted string",
        rule: "A macro whose value may contain `\"` is interpolated into a \
               `\"…\"` string. Rewrite the string as a compound quote `` `\"…\"' ``.",
        owner: Owner::Parse,
        has_fix: true,
    },
    LintMeta {
        code: "L002",
        severity: Severity::Warning,
        title: "Comparison treats missing as true",
        rule: "`x > k` is true when `x` is missing, because numeric missing \
               sorts above every number. Append `& !missing(x)` if that is not \
               what you meant.",
        owner: Owner::Parse,
        has_fix: true,
    },
    LintMeta {
        code: "L003",
        severity: Severity::Warning,
        title: "Separator line silently joins with the next line",
        rule: "A `//////` rule line is a `///` continuation followed by a \
               comment, so it swallows the line beneath it. Use `// ─────`.",
        owner: Owner::Parse,
        has_fix: true,
    },
    LintMeta {
        code: "L004",
        severity: Severity::Warning,
        title: "Increment inside a single-line `if` runs unconditionally",
        rule: "`` `i++' `` is expanded before the `if` is evaluated ([U] \
               18.3.7 technical note), so the counter advances even when the \
               branch is not taken. Wrap the body in braces.",
        owner: Owner::Parse,
        has_fix: true,
    },
    LintMeta {
        code: "L005",
        severity: Severity::Warning,
        title: "Absolute file path",
        rule: "An absolute path in `use`/`using` only works on the machine it \
               was written on. Rewrite it relative to the project root.",
        owner: Owner::Parse,
        has_fix: true,
    },
    LintMeta {
        code: "L006",
        severity: Severity::Error,
        title: "Unknown option on a known command",
        rule: "The command does not take this option. The nearest option name \
               in its grammar is offered.",
        owner: Owner::Parse,
        has_fix: true,
    },
    LintMeta {
        code: "L007",
        severity: Severity::Error,
        title: "Unresolved variable name",
        rule: "No variable of this name exists in the loaded dataset. The \
               nearest name is offered (spec §21's \"Did you mean 'income'?\").",
        owner: Owner::Parse,
        has_fix: true,
    },
    LintMeta {
        code: "L008",
        severity: Severity::Warning,
        title: "`replace` targets a variable this file never creates",
        rule: "The target of this `replace` is neither generated earlier in the \
               file nor present in the loaded dataset, so the file only runs \
               when something else created the variable first.",
        owner: Owner::Intel,
        has_fix: false,
    },
    LintMeta {
        code: "L009",
        severity: Severity::Warning,
        title: "`predict` after the estimation sample changed",
        rule: "The dataset was modified between the estimation that defined \
               `e(sample)` and this `predict`, so the prediction is computed \
               over a different set of observations than the model was fitted \
               on.",
        owner: Owner::Intel,
        has_fix: false,
    },
    LintMeta {
        code: "L010",
        severity: Severity::Warning,
        title: "`capture` with no `_rc` inspection",
        rule: "`capture` swallows the error and nothing afterwards reads `_rc`, \
               so a failure here is invisible and the run silently diverges.",
        owner: Owner::Intel,
        has_fix: true,
    },
    LintMeta {
        code: "L011",
        severity: Severity::Warning,
        title: "Loop over a macro that can be empty",
        rule: "The list this loop iterates comes from a macro that is not \
               assigned on every path reaching the loop. An empty macro makes \
               the body run zero times, silently.",
        owner: Owner::Intel,
        has_fix: false,
    },
    LintMeta {
        code: "L012",
        severity: Severity::Error,
        title: "Command requires `tsset`/`xtset`/`svyset`",
        rule: "This command — or a time-series operator in its varlist — \
               requires the data to be declared first, and no such declaration \
               appears earlier in the file.",
        owner: Owner::Intel,
        has_fix: true,
    },
];

/// The registry, in code order.
#[must_use]
pub fn registry() -> &'static [LintMeta] {
    REGISTRY
}

/// Look one rule up by code.
#[must_use]
pub fn meta(code: &str) -> Option<&'static LintMeta> {
    REGISTRY.iter().find(|m| m.code == code)
}

/// Run every lint over a whole buffer.
///
/// Findings come back in a total order — severity, then position, then code —
/// so the problems pane can be diffed and does not repaint on every keystroke.
/// Suppressed findings are dropped here and reported by [`suppressed_codes`],
/// because design 03 §10 requires suppressions to be listed rather than silent.
#[must_use]
pub fn lint_document(idx: &ParseIndex<'_>, env: &Env) -> Vec<Diagnostic> {
    let doc = Doc::build(idx);
    lint_with(idx, env, &doc)
}

/// [`lint_document`] against an already-built [`Doc`], so a caller that also
/// runs the reproducibility audit parses the file once rather than twice.
#[must_use]
pub fn lint_with(idx: &ParseIndex<'_>, env: &Env, doc: &Doc<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    // L001–L007: stratum-parse's, per command, delegated.
    for st in &doc.stmts {
        let cx = stratum_parse::LintCtx {
            text: st.code,
            vars: None,
        };
        for mut d in stratum_parse::lint(&st.ast, &cx) {
            d.span = d.span.map(|s| idx.span_to_source(st.ix, s));
            out.push(d);
        }
    }
    // L003 is a scan-level finding, so it arrives on the segmentation rather
    // than from `lint`.
    for d in &idx.segmentation().diags {
        if d.code.starts_with('L') {
            out.push(d.clone());
        }
    }

    // L008–L012: ours, over the whole file.
    l008_replace_undeclared::check(idx, env, doc, &mut out);
    l009_predict_stale::check(idx, doc, &mut out);
    l010_capture_unchecked::check(idx, doc, &mut out);
    l011_loop_empty_macro::check(idx, env, doc, &mut out);
    l012_missing_setup::check(idx, doc, &mut out);

    out.retain(|d| {
        d.span
            .map(|s| idx.segmentation().line_index.line_of(s.start))
            .is_none_or(|line| !doc.suppresses(idx, line, &d.code))
    });
    sort_findings(&mut out);
    out
}

/// The codes a buffer suppresses, with the extent of each marker.
#[must_use]
pub fn suppressed_codes(idx: &ParseIndex<'_>) -> Vec<(String, Span)> {
    Doc::build(idx).suppressions
}

/// Apply design 07 §6.3's presentation cap: at most one decoration per line and
/// [`MAX_DECORATIONS_PER_FILE`] per file. Returns the kept findings and the
/// number dropped, which the status bar renders as a single count.
#[must_use]
pub fn decorations(idx: &ParseIndex<'_>, findings: &[Diagnostic]) -> (Vec<Diagnostic>, usize) {
    let li = &idx.segmentation().line_index;
    let mut kept: Vec<Diagnostic> = Vec::with_capacity(MAX_DECORATIONS_PER_FILE);
    let mut seen_lines: Vec<u32> = Vec::with_capacity(MAX_DECORATIONS_PER_FILE);
    let mut dropped = 0usize;
    for d in findings {
        let line = d.span.map_or(u32::MAX, |s| li.line_of(s.start));
        if seen_lines.contains(&line) || kept.len() == MAX_DECORATIONS_PER_FILE {
            dropped += 1;
            continue;
        }
        seen_lines.push(line);
        kept.push(d.clone());
    }
    (kept, dropped)
}

/// Severity, then position, then code.
pub(crate) fn sort_findings(v: &mut [Diagnostic]) {
    v.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| span_key(a.span).cmp(&span_key(b.span)))
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.message.cmp(&b.message))
    });
}

fn span_key(s: Option<Span>) -> (u32, u32) {
    s.map_or((u32::MAX, u32::MAX), |x| (x.start, x.end))
}

/// Build a lint diagnostic with the registry's own severity and no fix.
pub(crate) fn finding(code: &str, message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic {
        severity: meta(code).map_or(Severity::Warning, |m| m.severity),
        code: code.to_owned(),
        stata_rc: None,
        message: message.into(),
        file: None,
        span: Some(span),
        offending_token: None,
        block: None,
        related: Vec::new(),
        suggestions: Vec::new(),
        notes: Vec::new(),
        confidence: Confidence::Exact,
    }
}

/// Attach a deterministic fix.
pub(crate) fn with_fix(mut d: Diagnostic, s: Suggestion) -> Diagnostic {
    d.suggestions.push(s);
    d
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn the_registry_is_contiguous_unique_and_complete() {
        assert_eq!(REGISTRY.len(), 12);
        for (i, m) in REGISTRY.iter().enumerate() {
            assert_eq!(m.code, format!("L{:03}", i + 1), "out of order at {i}");
            assert!(!m.title.is_empty() && !m.rule.is_empty(), "{}", m.code);
            assert!(!m.title.ends_with('.'), "{} title is a title", m.code);
        }
        let intel = REGISTRY.iter().filter(|m| m.owner == Owner::Intel).count();
        assert_eq!(intel, 5, "C14 gives five slots to 07's dataflow lints");
    }

    #[test]
    fn decorations_cap_at_one_per_line_and_25_per_file() {
        let src = (0..40)
            .map(|i| format!("replace v{i} = 1\n"))
            .collect::<String>();
        let idx = ParseIndex::new(&src);
        let env = Env {
            varnames: Some(vec!["price".to_owned()]),
            ..Env::default()
        };
        let all = lint_document(&idx, &env);
        assert!(all.len() > MAX_DECORATIONS_PER_FILE, "{}", all.len());
        let (kept, dropped) = decorations(&idx, &all);
        assert_eq!(kept.len(), MAX_DECORATIONS_PER_FILE);
        assert_eq!(dropped, all.len() - MAX_DECORATIONS_PER_FILE);
    }
}
