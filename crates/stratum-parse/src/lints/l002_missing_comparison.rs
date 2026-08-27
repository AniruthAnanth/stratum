//! `L002` — a comparison that silently treats missing as true.
//!
//! Design 02 §8.3: every missing value is a real IEEE double at or above
//! 2^1023, so `. > 1e300` is 1 [V] and the classic `gen d = income > 10000`
//! marks every missing income as 1 ([U] 13.2.3). That is not a quirk of our
//! encoding — it IS Stata — which is exactly why a deterministic, offline
//! warning about it is worth more than any amount of documentation.
//!
//! The check is deliberately narrow: only `>` and `>=` against a numeric
//! LITERAL, and only when the other side is a bare variable name. `x > y` where
//! both are variables is usually intentional, and warning about it would train
//! people to ignore the lint.

use stratum_proto::{Edit, SuggestionKind};

use crate::ast::{BinOp, Command, CommandAst, Expr};
use crate::lints::{code, warn, with_fix, LintCtx};

/// Report `var > literal` and `var >= literal` comparisons.
pub fn check(cmd: &CommandAst, cx: &LintCtx<'_>, out: &mut Vec<stratum_proto::Diagnostic>) {
    let Command::Known(k) = &cmd.cmd else { return };
    for e in [&k.slots.assign, &k.slots.if_].into_iter().flatten() {
        e.walk(&mut |node| {
            let Expr::Binary { op, lhs, rhs, span } = node else {
                return;
            };
            if !matches!(op, BinOp::Gt | BinOp::Ge) {
                return;
            }
            let (Expr::Name(var, _), Expr::Num(..)) = (lhs.as_ref(), rhs.as_ref()) else {
                return;
            };
            let d = warn(
                code::L002,
                format!(
                    "`{var}` is missing for some observations and every missing value \
                     compares GREATER than any number, so this is true where `{var}` is missing"
                ),
                *span,
            );
            out.push(with_fix(
                d,
                format!("exclude missing: & !missing({var})"),
                SuggestionKind::Rewrite,
                vec![Edit {
                    span: stratum_proto::Span {
                        start: span.end,
                        end: span.end,
                    },
                    text: format!(" & !missing({var})"),
                }],
            ));
        });
    }
    let _ = cx;
}
