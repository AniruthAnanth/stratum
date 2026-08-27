//! `L003` — a `//////` separator line silently joins with the next line.
//!
//! Design 02 §2.1, verified: THREE OR MORE slashes is a continuation, not a
//! comment, and it splices with no inserted separator. A decorative
//! `// ─────` is fine; a decorative `//////////` swallows the line under it
//! and the command that disappears is invisible in the editor.
//!
//! This one is a SOURCE scan and not an AST walk, and it is exported separately
//! from [`crate::lints::lint`] for that reason: by the time there is an AST the
//! two lines have already been joined into one and the evidence is gone.

use stratum_proto::{Diagnostic, Edit, Span, SuggestionKind};

use crate::lints::{code, warn, with_fix};

/// Scan raw source for separator lines that continue.
pub fn check(src: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut at = 0u32;
    for line in src.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let body = trimmed.trim_start();
        let lead = (trimmed.len() - body.len()) as u32;
        let slashes = body.bytes().take_while(|c| *c == b'/').count();
        // Three or more slashes AND nothing but slashes: that is a decoration,
        // not a deliberate continuation of a real statement.
        if slashes >= 3 && body.len() == slashes && slashes > 0 {
            let span = Span {
                start: at + lead,
                end: at + lead + body.len() as u32,
            };
            let d = warn(
                code::L003,
                "three or more slashes is a CONTINUATION, not a comment: this \
                 separator line joins with the line below it",
                span,
            );
            out.push(with_fix(
                d,
                "make it a comment",
                SuggestionKind::Rewrite,
                vec![Edit {
                    span,
                    text: format!("// {}", "\u{2500}".repeat(slashes.saturating_sub(2))),
                }],
            ));
        }
        at += line.len() as u32;
    }
    out
}
