//! Diagnostics, return codes and the deterministic `L###` lints — design 02 §11.
//!
//! # There is one `Diagnostic` type and it lives in proto
//!
//! Design 02 §11 sketches a `Diagnostic` local to the language module. It is not
//! declared here: ARCHITECTURE C14 requires ONE code registry so that a code
//! seen in the problems pane, in `--json` output and in a `*! nolint(...)`
//! suppression is the same string, and CONTRACTS §4 already declares the wire
//! type. A parser-local twin would be the `Span`-duplication mistake A10 was
//! raised about, one layer up.
//!
//! What IS declared here is [`StataError`] — a return code plus a message, the
//! thing every fallible operation in this crate returns — and the mapping from
//! it to a wire [`Diagnostic`].
//!
//! # Why the lints live next to the return codes
//!
//! 02 §11 puts them in one section because they are the same mechanism: a
//! deterministic, offline, instant finding with a machine code and an optional
//! edit. Spec §16 and §21 both require the deterministic check to run first and
//! the AI second, so `L002`'s "this comparison treats missing as true" has to be
//! available with no model, no network and no engine — which is why every lint
//! in this module is a function of the AST and nothing else.

mod l001_quote_in_string;
mod l002_missing_comparison;
mod l003_separator_joins;
mod l004_increment_in_if;
mod l005_absolute_path;
mod l006_unknown_option;
mod l007_unresolved_name;

use stratum_proto::{Confidence, Diagnostic, Edit, Severity, Span, Suggestion, SuggestionKind};

use crate::ast::CommandAst;
use crate::varlist::VarlistCtx;

pub use l001_quote_in_string::check as check_l001;
pub use l002_missing_comparison::check as check_l002;
pub use l003_separator_joins::check as check_l003;
pub use l004_increment_in_if::check as check_l004;
pub use l005_absolute_path::check as check_l005;
pub use l006_unknown_option::check as check_l006;
pub use l007_unresolved_name::check as check_l007;

/// A Stata return code with the message Stata prints for it.
///
/// The return codes this crate emits, all verified against StataMP 18.5 in
/// `tests/golden/stata18/errors.log`: `100` varlist required, `101` varlist not
/// allowed, `102`/`103` too few / too many variables, `109` type mismatch,
/// `111` variable or element not found (also an ambiguous `~`), `130`
/// expression too long, `132` too many `)` or `"`, `133` unknown function,
/// `198` invalid syntax or invalid name, `199` unrecognized command, and `920`
/// — ours — for the macro nesting and length limits.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StataError {
    /// The return code. `r(198)` is `rc == 198`.
    pub rc: u32,
    /// The message Stata prints, lower case and without the `r(nnn);` line.
    pub message: String,
    /// Where, in the text the failing operation was reading.
    pub span: Option<Span>,
    /// THE field spec §21's "Did you mean 'income'?" needs. CONTRACTS §4 calls
    /// out that every r(111)/r(199)/r(198)-class error must populate it.
    pub offending_token: Option<String>,
}

impl StataError {
    /// A return code and its message.
    pub fn new(rc: u32, message: impl Into<String>) -> Self {
        StataError {
            rc,
            message: message.into(),
            span: None,
            offending_token: None,
        }
    }

    /// Attach the extent of the offending text.
    pub fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// Attach the offending word.
    pub fn token(mut self, tok: impl Into<String>) -> Self {
        self.offending_token = Some(tok.into());
        self
    }

    /// The wire form. The code is `STATA<rc>` zero-padded to four digits, which
    /// is CONTRACTS §4's `"STATA0111"` shape.
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            code: format!("STATA{:04}", self.rc),
            stata_rc: Some(self.rc),
            message: self.message.clone(),
            file: None,
            span: self.span,
            offending_token: self.offending_token.clone(),
            block: None,
            related: Vec::new(),
            suggestions: Vec::new(),
            notes: Vec::new(),
            confidence: Confidence::Exact,
        }
    }
}

impl core::fmt::Display for StataError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}\nr({});", self.message, self.rc)
    }
}

impl core::error::Error for StataError {}

/// The deterministic lint codes of 02 §11. `&'static str` and not an enum
/// because C14's registry is a string registry and a suppression comment carries
/// the string.
pub mod code {
    /// A macro interpolated into a `"…"` string whose value may contain `"`.
    pub const L001: &str = "L001";
    /// A comparison that will treat a missing value as true.
    pub const L002: &str = "L002";
    /// A `//////` separator line that silently joins with the next line.
    pub const L003: &str = "L003";
    /// `` `i++' `` on a single-line `if`, which increments unconditionally.
    pub const L004: &str = "L004";
    /// An absolute file path in `using` / `use`.
    pub const L005: &str = "L005";
    /// An unknown option on a known command.
    pub const L006: &str = "L006";
    /// An unresolved variable name, with an edit-distance suggestion.
    pub const L007: &str = "L007";
}

/// Everything a lint may look at. Deliberately small: a lint that needed the
/// engine would not be able to run at keystroke latency in the editor's wasm
/// build, which is the whole point of them existing (spec §16, §21).
pub struct LintCtx<'a> {
    /// The macro-expanded text the AST was parsed from.
    pub text: &'a str,
    /// The live variable layout, when the editor has one.
    pub vars: Option<&'a VarlistCtx<'a>>,
}

/// Run every lint that applies to one parsed command.
///
/// Findings come back in `code` order so that two runs over the same input
/// produce the same list — the problems pane is diffed, and an unstable order
/// would repaint it on every keystroke.
pub fn lint(cmd: &CommandAst, cx: &LintCtx<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    check_l001(cmd, cx, &mut out);
    check_l002(cmd, cx, &mut out);
    check_l004(cmd, cx, &mut out);
    check_l005(cmd, cx, &mut out);
    check_l006(cmd, cx, &mut out);
    check_l007(cmd, cx, &mut out);
    out.sort_by(|a, b| a.code.cmp(&b.code).then(cmp_span(a.span, b.span)));
    out
}

fn cmp_span(a: Option<Span>, b: Option<Span>) -> core::cmp::Ordering {
    let key = |s: Option<Span>| s.map_or((u32::MAX, u32::MAX), |x| (x.start, x.end));
    key(a).cmp(&key(b))
}

/// Build a warning-severity lint diagnostic.
pub(crate) fn warn(code: &str, message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
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

/// Attach a deterministic quick fix.
pub(crate) fn with_fix(
    mut d: Diagnostic,
    label: impl Into<String>,
    kind: SuggestionKind,
    edits: Vec<Edit>,
) -> Diagnostic {
    d.suggestions.push(Suggestion {
        label: label.into(),
        kind,
        edits,
    });
    d
}

/// Damerau–Levenshtein distance, capped at `max`.
///
/// `L007`'s "Did you mean 'income'?" (spec §21) is this and nothing else: no
/// model, no network, works offline and instantly. The cap turns the usual
/// O(nm) table into an early exit, which matters because it runs against every
/// variable in a dataset that may have thousands.
pub fn edit_distance(a: &str, b: &str, max: usize) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let mut prev2: Vec<usize> = Vec::new();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(prev2[j - 2] + 1);
            }
            cur[j] = v;
            row_min = row_min.min(v);
        }
        if row_min > max {
            return max + 1;
        }
        prev2 = std::mem::replace(&mut prev, std::mem::replace(&mut cur, vec![0; b.len() + 1]));
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_take_the_contracts_shape() {
        let d = StataError::new(111, "variable nosuchvar not found")
            .token("nosuchvar")
            .to_diagnostic();
        assert_eq!(d.code, "STATA0111");
        assert_eq!(d.stata_rc, Some(111));
        assert_eq!(d.offending_token.as_deref(), Some("nosuchvar"));
    }

    #[test]
    fn edit_distance_is_damerau() {
        assert_eq!(edit_distance("incom", "income", 2), 1);
        assert_eq!(
            edit_distance("detial", "detail", 2),
            1,
            "a transposition is one edit"
        );
        assert!(edit_distance("price", "foreign", 2) > 2);
    }
}
