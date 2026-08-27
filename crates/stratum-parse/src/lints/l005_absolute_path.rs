//! `L005` — an absolute file path in `using` / `use`.
//!
//! Spec §16: a project that only runs on the machine it was written on is not
//! reproducible. The check is deterministic and offline, which is what §16
//! asks for; the AI's opinion about the path is a separate, later thing.

use stratum_proto::{Diagnostic, SuggestionKind};

use crate::ast::{Command, CommandAst};
use crate::lints::{code, warn, with_fix, LintCtx};

/// Report an absolute path in a file slot.
pub fn check(cmd: &CommandAst, _cx: &LintCtx<'_>, out: &mut Vec<Diagnostic>) {
    let Command::Known(k) = &cmd.cmd else { return };
    let candidates = [
        k.slots.using.as_ref().map(|f| (f.raw.as_str(), f.span)),
        k.slots.rest.as_ref().map(|r| (r.text.as_str(), r.span)),
    ];
    for (raw, span) in candidates.into_iter().flatten() {
        let path = crate::lex::unquote(raw.trim());
        if !is_absolute(path) {
            continue;
        }
        let d = warn(
            code::L005,
            format!("absolute path `{path}` will not resolve on another machine"),
            span,
        );
        out.push(with_fix(
            d,
            "make it project-relative",
            SuggestionKind::ChangePath,
            Vec::new(),
        ));
    }
}

/// POSIX absolute, a Windows drive letter, a UNC share, or `~`.
fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    match b.first() {
        Some(b'/') | Some(b'~') => true,
        Some(b'\\') => b.get(1) == Some(&b'\\'),
        Some(c) if c.is_ascii_alphabetic() => {
            b.get(1) == Some(&b':') && matches!(b.get(2), Some(b'\\') | Some(b'/'))
        }
        _ => false,
    }
}
