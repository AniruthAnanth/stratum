//! `L001` — a macro interpolated into a `"…"` string whose value may contain `"`.
//!
//! Design 02 §1.1's first measured case: `local q = `"embedded "quote""'` then
//! `di "B13: `q'"` ERRORS [V], because expansion is quote-blind and the
//! substituted text re-tokenizes. The fix is a compound double quote, which
//! protects the interior.
//!
//! The check runs on UNEXPANDED text, so it is a scan of the raw string
//! literals rather than a walk of the tree: after expansion the macro is gone
//! and there is nothing left to warn about.

use stratum_proto::{Edit, Span, SuggestionKind};

use crate::ast::CommandAst;
use crate::lex::{tokens, LexMode, TokKind};
use crate::lints::{code, warn, with_fix, LintCtx};

/// Report every `"…"` literal that interpolates a macro.
pub fn check(_cmd: &CommandAst, cx: &LintCtx<'_>, out: &mut Vec<stratum_proto::Diagnostic>) {
    for t in tokens(cx.text, LexMode::Speculative) {
        if t.kind != TokKind::Str {
            continue;
        }
        let text = &cx.text[t.span.start as usize..t.span.end as usize];
        if !text.contains('`') && !text.contains('$') {
            continue;
        }
        let inner = &text[1..text.len().saturating_sub(1).max(1)];
        let d = warn(
            code::L001,
            "macro interpolated into a plain string; if its value contains a \
             double quote the line re-tokenizes and fails",
            t.span,
        );
        out.push(with_fix(
            d,
            "use a compound double quote",
            SuggestionKind::Rewrite,
            vec![Edit {
                span: Span {
                    start: t.span.start,
                    end: t.span.end,
                },
                text: format!("`\"{inner}\"'"),
            }],
        ));
    }
}
