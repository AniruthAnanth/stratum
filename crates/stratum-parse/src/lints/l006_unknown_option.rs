//! `L006` — an unknown option on a known command, with the nearest neighbour.
//!
//! `summarize price, detial` is r(198) `option detial not allowed` [V]
//! (`tests/golden/stata18/errors.log`). The RETURN CODE is the parser's; this
//! lint adds the thing Stata does not: `detail` is one transposition away, and
//! saying so is free, deterministic and offline (spec §21).

use stratum_proto::{Diagnostic, Edit, SuggestionKind};

use crate::ast::{Command, CommandAst};
use crate::cmdtable::command;
use crate::lints::{code, edit_distance, warn, with_fix, LintCtx};

/// Report an unresolved option and suggest the closest spelling.
pub fn check(cmd: &CommandAst, _cx: &LintCtx<'_>, out: &mut Vec<Diagnostic>) {
    let Command::Known(k) = &cmd.cmd else { return };
    let sig = command(k.id);
    for opt in &k.slots.options.items {
        if opt.canonical.is_some() {
            continue;
        }
        let mut best: Option<(usize, &'static str)> = None;
        for spec in sig.options {
            // Cap 2: past that the "suggestion" is a guess, and a wrong guess
            // costs more trust than no guess.
            let d = edit_distance(&opt.name, spec.canonical, 2);
            if d <= 2 && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, spec.canonical));
            }
        }
        let msg = format!("option {} not allowed on {}", opt.name, sig.canonical);
        let diag = warn(code::L006, msg, opt.span);
        out.push(match best {
            Some((_, name)) => with_fix(
                diag,
                format!("did you mean `{name}`?"),
                SuggestionKind::Rename,
                vec![Edit {
                    span: stratum_proto::Span {
                        start: opt.span.start,
                        end: opt.span.start + opt.name.len() as u32,
                    },
                    text: name.to_owned(),
                }],
            ),
            None => diag,
        });
    }
}
