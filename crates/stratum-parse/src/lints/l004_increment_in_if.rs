//! `L004` — `` `i++' `` on a single-line `if` increments unconditionally.
//!
//! [U] 18.3.7's technical note, and a direct consequence of the ordering this
//! crate exists to model: macro expansion happens BEFORE the line is
//! interpreted, so `` `i++' `` has already fired by the time the `if` decides
//! whether to run anything. Wrapping the body in braces makes the increment part
//! of a separate line, which IS re-expanded per execution.
//!
//! The check runs on unexpanded text: after expansion the `` `i++' `` is a
//! number and there is nothing to see.

use stratum_proto::{Diagnostic, Edit, Span, SuggestionKind};

use crate::ast::{BlockCommand, Command, CommandAst};
use crate::lints::{code, warn, with_fix, LintCtx};

/// Report an increment inside a braceless `if`.
pub fn check(cmd: &CommandAst, cx: &LintCtx<'_>, out: &mut Vec<Diagnostic>) {
    let Command::Block(b) = &cmd.cmd else { return };
    let BlockCommand::IfElse { arms } = b.as_ref() else {
        return;
    };
    for (_, body) in arms {
        let start = (body.start as usize).min(cx.text.len());
        let text = &cx.text[start..(body.end as usize).min(cx.text.len())];
        // A braced arm is fine: the body is its own logical line and is expanded
        // per execution. `BlockCommand::IfElse` stores the body span INSIDE the
        // braces, so the evidence is the byte in front of it.
        if cx.text[..start].trim_end().ends_with('{') {
            continue;
        }
        for (off, _) in step_refs(text) {
            let span = Span {
                start: body.start + off as u32,
                end: body.start + off as u32,
            };
            let d = warn(
                code::L004,
                "macro expansion runs before the `if` is interpreted, so this \
                 increment fires even when the branch is not taken ([U] 18.3.7)",
                span,
            );
            out.push(with_fix(
                d,
                "wrap the body in braces",
                SuggestionKind::Rewrite,
                vec![
                    Edit {
                        span: Span {
                            start: body.start,
                            end: body.start,
                        },
                        text: "{\n    ".to_owned(),
                    },
                    Edit {
                        span: Span {
                            start: body.end,
                            end: body.end,
                        },
                        text: "\n}".to_owned(),
                    },
                ],
            ));
        }
    }
}

/// Byte offsets of `` `x++' ``, `` `x--' ``, `` `++x' `` and `` `--x' ``.
fn step_refs(text: &str) -> Vec<(usize, &str)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let Some(j) = crate::macros::expand::match_backtick(b, i) else {
            break;
        };
        let inner = &text[i + 1..j];
        if inner.starts_with("++")
            || inner.starts_with("--")
            || inner.ends_with("++")
            || inner.ends_with("--")
        {
            out.push((i, inner));
        }
        i = j + 1;
    }
    out
}
