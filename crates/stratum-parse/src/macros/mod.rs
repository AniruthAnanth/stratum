//! Macro expansion — design 02 §4, step 2 of the pipeline.
//!
//! Expansion runs **between** segmentation and lexing, once per logical line, at
//! execution time. That ordering is the single most load-bearing fact in this
//! crate: see [`expand`]'s module docs for the two measured cases that prove it
//! is observable rather than an implementation detail.
//!
//! ```text
//!   Region ──▶ LogicalLine::code ──▶ expand() ──▶ lex() ──▶ parse_command()
//!                                      ▲
//!                              MacroEnv + ExpandHost
//! ```

pub mod env;
pub mod expand;
pub mod xmf;

pub use env::{split_args, LocalScope, MacroEnv, MacroLimits, MacroValue, ScopeKind, TempAlloc};
pub use expand::{expand, ExpandStats};

use crate::lints::StataError;
use crate::spanmap::SpanMap;

/// What the runtime must provide for expansion to be able to finish.
///
/// CONTRACTS §13: implemented by `stratum-runtime`, consumed here. Both methods
/// re-enter the interpreter, which is why [`ExpandStats::host_calls`] counts
/// them separately from ordinary substitutions.
pub trait ExpandHost {
    /// Evaluate `` `=exp' `` and format the result the way Stata does.
    ///
    /// The formatting rule is 02 §4.4 and it is **not** a display choice:
    /// `%18.0g` then trim, which loses precision on large magnitudes exactly the
    /// way Stata loses it. [`stringify_number`] is that rule; an implementation
    /// that reaches for `f64::to_string` diverges on the first loop that
    /// accumulates a large counter.
    fn eval_expr_to_macro_text(&mut self, exp: &str) -> Result<String, StataError>;

    /// Evaluate an extended macro function body — the text after the `:` — that
    /// [`xmf::eval`] could not answer without live state.
    fn eval_xmf(&mut self, body: &str) -> Result<String, StataError>;
}

/// The result of expanding one logical line.
#[derive(Clone, PartialEq, Debug)]
pub struct Expansion {
    /// The expanded text. Lexing runs over THIS, never over the source.
    pub text: String,
    /// Expanded offset → offset in the text that was expanded. Compose with
    /// [`crate::scan::LogicalLine::map`] to reach the original source.
    pub map: SpanMap,
    /// ADR-017 counters. Design 02 §4.2 does not name this field; it is here
    /// because a performance claim about the expansion path has to be assertable
    /// as a count rather than as a duration.
    pub stats: ExpandStats,
}

/// 02 §4.4: a numeric value becomes macro text via `%18.0g`, then trimmed.
///
/// This delegates to `stratum_core::fmt::fmt_macro` rather than reimplementing
/// `%g`. There is exactly one `%g` in the workspace and it was calibrated
/// against `tests/golden/stata18/gformat.log`; a second one here would be a
/// second set of rounding boundaries for the same job.
pub fn stringify_number(v: f64) -> String {
    stratum_core::fmt::fmt_macro(v)
}

/// A host that refuses every callback.
///
/// `` `=exp' `` and the state-dependent extended macro functions genuinely need
/// the runtime; a parser test that wants to exercise the substitution algorithm
/// does not. This makes the difference explicit — a test that trips one of these
/// gets `r(198)` with a clear message instead of a silently empty macro.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoHost;

impl ExpandHost for NoHost {
    fn eval_expr_to_macro_text(&mut self, _exp: &str) -> Result<String, StataError> {
        Err(StataError::new(
            198,
            "`=exp' needs a running engine; no ExpandHost was supplied",
        ))
    }

    fn eval_xmf(&mut self, body: &str) -> Result<String, StataError> {
        Err(StataError::new(
            198,
            format!("`:{body}' needs a running engine; no ExpandHost was supplied"),
        ))
    }
}
