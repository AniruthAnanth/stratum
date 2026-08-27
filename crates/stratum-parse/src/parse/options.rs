//! Option parsing — design 02 §6.2's two-stage scheme.
//!
//! Stage one is generic and knows nothing: an option is a word, optionally
//! followed by a paren-balanced argument, and the argument comes back as
//! [`OptionArg::Raw`]. Stage two consults the command's [`OptionSpec`] and
//! re-parses the raw text into a typed variant.
//!
//! # Why two stages rather than one table-driven pass
//!
//! Unknown options on a KNOWN command are r(198) `option … not allowed` [V]
//! (`tests/golden/stata18/errors.log`: both `summarize price, nosuchoption` and
//! the misspelling `summarize price, detial` give exactly that). Unknown options
//! on `Command::Unknown` must pass through untouched, because the user's
//! ado-file has options no table will ever list. One pass that needed a spec for
//! every option could not do the second thing, and decision D7 — parse
//! everything, evaluate a subset — depends on it.

use stratum_proto::Span;

use crate::ast::command::{OptionArg, OptionItem, Options, RawArgs};
use crate::ast::expr::Format;
use crate::cmdsig::{CommandSig, OptionArgKind};
use crate::cmdtable::resolve_option;
use crate::lex::{unquote, TokKind};
use crate::parse::expr::{join, parse_expr, parse_numlist};
use crate::parse::{Cursor, ParseMode};
use crate::varlist::parse_varlist;

/// Parse one comma segment as a whitespace-separated option list.
pub fn parse_options(cur: &mut Cursor<'_>, sig: &'static CommandSig, out: &mut Options) {
    while !cur.done() {
        let t = cur.peek();
        if t.kind != TokKind::Ident {
            // `, %9.2f` on `format`, `, 3` on `separator` — not an option word.
            // Skipping one token keeps the loop from spinning; a hard error here
            // would reject legal commands whose tail we have not tabulated.
            if cur.mode == ParseMode::Execute && !matches!(t.kind, TokKind::Eof) {
                cur.error(198, "invalid syntax", t.span);
            }
            cur.bump();
            continue;
        }
        cur.bump();
        let name = cur.text(t.span).to_owned();
        let mut span = t.span;
        // The argument must be GLUED: `by(rep78)` is an option with an argument,
        // `robust (x)` is the option `robust` followed by something else.
        let arg_raw = if cur.peek_kind() == TokKind::LParen && cur.peek().glued {
            let open = cur.bump().span;
            let start = cur.pos();
            let mut depth = 1i32;
            while !cur.done() && depth > 0 {
                match cur.peek_kind() {
                    TokKind::LParen => depth += 1,
                    TokKind::RParen => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    break;
                }
                cur.bump();
            }
            let inner = cur.range_span(start..cur.pos());
            let close = cur.expect(TokKind::RParen, ")").unwrap_or(open);
            span = join(span, close);
            Some(RawArgs {
                text: cur.text(inner).to_owned(),
                span: inner,
            })
        } else {
            None
        };

        let (canonical, negated, kind) = match resolve_option(sig, &name) {
            Some((spec, neg)) => (Some(spec.canonical), neg, Some(spec.arg)),
            None => {
                // Exactly the golden's wording and code.
                if cur.mode == ParseMode::Execute {
                    cur.error(198, format!("option {name} not allowed"), t.span);
                }
                (None, name.starts_with("no"), None)
            }
        };
        let arg = typed_arg(cur, kind, arg_raw, &name, span);
        out.items.push(OptionItem {
            name,
            canonical,
            negated,
            arg,
            span,
        });
    }
}

fn typed_arg(
    cur: &mut Cursor<'_>,
    kind: Option<OptionArgKind>,
    raw: Option<RawArgs>,
    name: &str,
    span: Span,
) -> Option<OptionArg> {
    let raw = raw?;
    let Some(kind) = kind else {
        // Unknown option on a known command, or a pass-through on an unknown
        // command: keep the text so the ado-file still receives it.
        return Some(OptionArg::Raw(raw));
    };
    let text = raw.text.trim();
    Some(match kind {
        OptionArgKind::None => {
            if cur.mode == ParseMode::Execute {
                cur.error(
                    198,
                    format!("option {name} does not take an argument"),
                    span,
                );
            }
            OptionArg::Raw(raw)
        }
        OptionArgKind::Int => match text.parse::<i64>() {
            Ok(v) => OptionArg::Int(v),
            Err(_) => {
                cur.error(
                    198,
                    format!("option {name}() requires an integer"),
                    raw.span,
                );
                OptionArg::Raw(raw)
            }
        },
        OptionArgKind::Real => match text.parse::<f64>() {
            Ok(v) => OptionArg::Real(v),
            Err(_) => {
                cur.error(198, format!("option {name}() requires a number"), raw.span);
                OptionArg::Raw(raw)
            }
        },
        OptionArgKind::Str => OptionArg::Str(unquote(text).to_owned()),
        OptionArgKind::Numlist => match parse_numlist(text, raw.span) {
            Some(n) => OptionArg::Numlist(n),
            None => {
                cur.error(198, format!("option {name}() requires a numlist"), raw.span);
                OptionArg::Raw(raw)
            }
        },
        OptionArgKind::Varlist => OptionArg::VarList(parse_varlist(cur.src, raw.span)),
        OptionArgKind::Fmt => OptionArg::Fmt(Format {
            text: text.to_owned(),
            span: raw.span,
        }),
        OptionArgKind::Exprs => {
            let mut sub = cur.slice(token_range(cur, raw.span));
            let mut exprs = Vec::new();
            while !sub.done() {
                exprs.push(parse_expr(&mut sub));
                if sub.peek_kind() == TokKind::Comma {
                    sub.bump();
                } else {
                    break;
                }
            }
            cur.absorb(sub);
            OptionArg::Exprs(exprs)
        }
        OptionArgKind::Raw => OptionArg::Raw(raw),
    })
}

/// The token index range covering a span of the buffer the cursor is over.
fn token_range(cur: &Cursor<'_>, span: Span) -> core::ops::Range<usize> {
    let toks = cur.toks();
    let start = toks.partition_point(|t| t.span.start < span.start);
    let end = toks.partition_point(|t| t.span.end <= span.end);
    start..end.max(start)
}
