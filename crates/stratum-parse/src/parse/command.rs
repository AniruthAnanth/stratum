//! The universal command grammar — design 02 §6.1.
//!
//! ```text
//! [prefix [args] :] command [varlist] [=exp] [if exp] [in range] [weight] [using file] [, options]
//! ```
//!
//! Three qualifications from [U] 11.1 that a naive splitter gets wrong, all of
//! them structural rather than cosmetic:
//!
//! * **`if` and `in` may appear in either order** [V]. The qualifier reader is
//!   therefore a loop over whatever comes next, not a fixed sequence.
//! * **Options need not be contiguous and may be re-entered.**
//!   `summarize a b, detail, if x==1, noformat` is legal — a SECOND comma
//!   returns to the command line ([U] 11.1.7 technical note). So the line is
//!   split into top-level comma segments and each later segment may re-enter the
//!   qualifier grammar. A single split on the first comma cannot express this.
//! * **An option may not appear in the middle of a varlist**, which is what
//!   makes "everything up to the first qualifier or comma" a sound definition of
//!   the varlist slot.
//!
//! # Loop bodies are spans, never ASTs
//!
//! 02 §6.2: Stata re-expands a loop body on every iteration — that is how
//! `` `x' `` picks up the new loop value, and it is what makes `foreach` cheap
//! [V]. Every [`BlockCommand`] body is therefore a `Span` into the text this
//! parse ran over, and the executor re-runs expansion and parsing per iteration.

use smallvec::SmallVec;
use stratum_proto::{DirectiveKind, Span};

use crate::ast::command::{
    BlockCommand, ByPrefix, Command, CommandAst, FileSpec, ForeachSource, InRange, KnownCommand,
    NumRange, ObsRef, Prefix, RawArgs, Slots, Weight, WeightKind,
};
use crate::cmdsig::{CmdFlags, CommandLookup, CommandSig, SlotMask, WeightMask};
use crate::cmdtable::table;
use crate::lex::{Op, TokKind};
use crate::parse::expr::{join, parse_expr, parse_numlist};
use crate::parse::options::parse_options;
use crate::parse::{Cursor, ParseMode};
use crate::varlist::parse_varlist;

/// Parse one command out of a cursor positioned at its first token.
pub fn parse_command_tokens(cur: &mut Cursor<'_>) -> CommandAst {
    let span = cur.range_span(0..cur.len());
    let prefixes = parse_prefixes(cur);
    let cmd = parse_body(cur);
    CommandAst {
        span,
        src: span,
        prefixes,
        cmd,
    }
}

// ───────────────────────────── the prefix chain ─────────────────────────────

fn parse_prefixes(cur: &mut Cursor<'_>) -> SmallVec<[Prefix; 2]> {
    let mut out: SmallVec<[Prefix; 2]> = SmallVec::new();
    loop {
        if cur.peek_kind() != TokKind::Ident {
            return out;
        }
        let head = cur.peek();
        let word = cur.text(head.span);
        let Some(sig) = table().canonical(word) else {
            return out;
        };
        if !sig.flags.contains(CmdFlags::PREFIX) {
            return out;
        }
        match sig.canonical {
            "by" | "bysort" => {
                // `by` without a colon is not a prefix — it is the `by` command
                // (or a typo), and consuming the rest of the line as its
                // grouping varlist would swallow the real command.
                let Some(colon) = top_colon(cur, cur.pos() + 1) else {
                    return out;
                };
                cur.bump();
                let args = cur.range_span(cur.pos()..colon);
                out.push(Prefix::By(by_prefix(
                    cur,
                    args,
                    sig.canonical == "bysort",
                    join(head.span, cur.at(colon).span),
                )));
                cur.seek(colon + 1);
            }
            "quietly" | "noisily" | "capture" => {
                // `quietly {` is a BLOCK, not a prefix on an empty command.
                if cur.at(cur.pos() + 1).kind == TokKind::LBrace {
                    return out;
                }
                let s = cur.bump().span;
                if cur.peek_kind() == TokKind::Colon {
                    cur.bump();
                }
                out.push(match sig.canonical {
                    "quietly" => Prefix::Quietly { span: s },
                    "noisily" => Prefix::Noisily { span: s },
                    _ => Prefix::Capture { span: s },
                });
            }
            "version" => {
                let s = cur.bump().span;
                let mut ver = String::new();
                let mut end = s;
                if cur.peek_kind() == TokKind::Number {
                    let t = cur.bump();
                    ver = cur.text(t.span).to_owned();
                    end = t.span;
                }
                if cur.peek_kind() == TokKind::Colon {
                    end = cur.bump().span;
                } else if ver.is_empty() {
                    // `version` on its own is the COMMAND, which reports the
                    // current version. Only `version #:` is a prefix.
                    cur.seek(cur.pos() - 1);
                    return out;
                }
                out.push(Prefix::Version {
                    ver,
                    span: join(s, end),
                });
            }
            _ => {
                // Every other prefix REQUIRES the colon ([U] 11.1.10).
                let Some(colon) = top_colon(cur, cur.pos() + 1) else {
                    return out;
                };
                let s = cur.bump().span;
                let args = cur.range_span(cur.pos()..colon);
                let name = sig.canonical.to_owned();
                let end = cur.at(colon).span;
                if name == "frame" {
                    out.push(Prefix::Frame {
                        name: cur.text(args).trim().to_owned(),
                        span: join(s, end),
                    });
                } else {
                    out.push(Prefix::Generic {
                        name,
                        args,
                        span: join(s, end),
                    });
                }
                cur.seek(colon + 1);
            }
        }
    }
}

/// `by a b:` → group `[a, b]`. `bysort a (b):` → group `[a]`, sort-only `[b]`.
fn by_prefix(cur: &Cursor<'_>, args: Span, bysort: bool, span: Span) -> ByPrefix {
    let text = cur.text(args);
    let base = args.start;
    // `, sort` after the varlist. Found on the TEXT because the paren group has
    // already been located there; splitting on a token index first would need
    // the same scan twice.
    let (body, opts) = match top_comma_in_text(text) {
        Some(k) => (&text[..k], &text[k + 1..]),
        None => (text, ""),
    };
    let (group_txt, extra_txt) = match (body.find('('), body.rfind(')')) {
        (Some(a), Some(b)) if b > a => (&body[..a], Some((a + 1, b))),
        _ => (body, None),
    };
    let group = parse_varlist(
        cur.src,
        Span {
            start: base,
            end: base + group_txt.len() as u32,
        },
    );
    let extra_sort = match extra_txt {
        Some((a, b)) => parse_varlist(
            cur.src,
            Span {
                start: base + a as u32,
                end: base + b as u32,
            },
        ),
        None => crate::ast::VarList::default(),
    };
    ByPrefix {
        group,
        extra_sort,
        sort: bysort || opts.split_whitespace().any(|w| "sort".starts_with(w)),
        span,
    }
}

// ──────────────────────────── the command itself ────────────────────────────

fn parse_body(cur: &mut Cursor<'_>) -> Command {
    match cur.peek_kind() {
        TokKind::Eof => Command::Unknown {
            name: String::new(),
            name_span: cur.end_span(),
            rest: RawArgs::default(),
        },
        TokKind::LBrace => {
            let (body, _) = brace_body(cur);
            Command::Block(Box::new(BlockCommand::Anonymous { body }))
        }
        // `#delimit cr|;` — the only directive the front end acts on.
        TokKind::Op(Op::Hash) => {
            cur.bump();
            let kind = if cur.peek_kind() == TokKind::Ident
                && cur.text(cur.peek().span).starts_with("delimit")
            {
                cur.bump();
                match cur.peek_kind() {
                    TokKind::Semi => DirectiveKind::DelimitSemi,
                    _ => DirectiveKind::DelimitCr,
                }
            } else {
                DirectiveKind::Other
            };
            cur.seek(cur.len());
            Command::Directive(kind)
        }
        TokKind::Ident => {
            let head = cur.peek();
            let word = cur.text(head.span);
            // `if` and `else` are reserved words, not table rows, so they are
            // matched before the lookup that would call them unknown commands.
            if word == "if" || word == "else" {
                return Command::Block(Box::new(if_else(cur)));
            }
            match table().resolve(word) {
                CommandLookup::Exact(id) | CommandLookup::Abbrev(id) => {
                    let sig = table().get(id);
                    known_or_block(cur, id, sig, head.span)
                }
                CommandLookup::Ambiguous(ids) => {
                    let names: Vec<&str> = ids.iter().map(|i| table().get(*i).canonical).collect();
                    cur.error(
                        199,
                        format!(
                            "ambiguous abbreviation; did you mean {}?",
                            names.join(" or ")
                        ),
                        head.span,
                    );
                    unknown(cur, head.span)
                }
                CommandLookup::Unknown => unknown(cur, head.span),
            }
        }
        _ => {
            let t = cur.peek();
            cur.error(198, "invalid syntax", t.span);
            unknown(cur, t.span)
        }
    }
}

fn unknown(cur: &mut Cursor<'_>, name_span: Span) -> Command {
    cur.bump();
    let rest_span = cur.range_span(cur.pos()..cur.len());
    cur.seek(cur.len());
    Command::Unknown {
        name: cur.text(name_span).to_owned(),
        name_span,
        rest: RawArgs {
            text: cur.text(rest_span).to_owned(),
            span: rest_span,
        },
    }
}

fn known_or_block(
    cur: &mut Cursor<'_>,
    id: crate::cmdsig::CmdId,
    sig: &'static CommandSig,
    name_span: Span,
) -> Command {
    match sig.canonical {
        "foreach" => return Command::Block(Box::new(foreach(cur))),
        "forvalues" => return Command::Block(Box::new(forvalues(cur))),
        "while" => return Command::Block(Box::new(while_(cur))),
        "input" => return Command::Block(Box::new(input(cur))),
        "mata" | "python" | "java" => {
            cur.bump();
            let body = tail_span(cur);
            cur.seek(cur.len());
            return Command::Block(Box::new(match sig.canonical {
                "mata" => BlockCommand::Mata { body },
                // `java` has no variant of its own in 02 §6.2's enum; it is a
                // v2 opaque block exactly like Python's, and inventing a variant
                // would change a type `stratum-effects` already codes against.
                _ => BlockCommand::Python { body },
            }));
        }
        "capture" | "quietly" | "noisily" if cur.at(cur.pos() + 1).kind == TokKind::LBrace => {
            cur.bump();
            let (body, _) = brace_body(cur);
            return Command::Block(Box::new(match sig.canonical {
                "capture" => BlockCommand::Capture { body },
                "quietly" => BlockCommand::Quietly { body },
                _ => BlockCommand::Noisily { body },
            }));
        }
        "program" => {
            if let Some(b) = program(cur) {
                return Command::Block(Box::new(b));
            }
        }
        _ => {}
    }
    cur.bump();
    let slots = parse_slots(cur, sig);
    Command::Known(Box::new(KnownCommand {
        id,
        name_span,
        slots,
    }))
}

/// The span from the cursor to the end of the token slice.
fn tail_span(cur: &Cursor<'_>) -> Span {
    cur.range_span(cur.pos()..cur.len())
}

/// The body of the `{` at the cursor: the span between it and its match, and
/// the token index just past the closing `}`.
///
/// An unmatched `{` yields everything to the end. The segmenter has already
/// classified that region as `Unterminated`, so raising a second error here
/// would double-report a condition the gutter is already showing.
fn brace_body(cur: &mut Cursor<'_>) -> (Span, usize) {
    let open = cur.bump().span;
    let mut depth = 1i32;
    let start = cur.pos();
    let mut i = start;
    while i < cur.len() {
        match cur.at(i).kind {
            TokKind::LBrace => depth += 1,
            TokKind::RBrace => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let end = if i > start {
        cur.at(i - 1).span.end
    } else {
        open.end
    };
    cur.seek((i + 1).min(cur.len()));
    (
        Span {
            start: open.end,
            end: end.max(open.end),
        },
        i + 1,
    )
}

fn foreach(cur: &mut Cursor<'_>) -> BlockCommand {
    cur.bump();
    let loopvar = ident_text(cur);
    let kw = ident_text(cur);
    let brace = find_top_brace(cur);
    let args = cur.range_span(cur.pos()..brace.unwrap_or(cur.len()));
    let source = if kw == "in" {
        ForeachSource::In(RawArgs {
            text: cur.text(args).trim().to_owned(),
            span: args,
        })
    } else {
        // `of local|global|varlist|newlist|numlist`.
        let kind = ident_text(cur);
        let rest = cur.range_span(cur.pos()..brace.unwrap_or(cur.len()));
        let text = cur.text(rest).trim().to_owned();
        match kind.as_str() {
            k if "local".starts_with(k) && !k.is_empty() => ForeachSource::OfLocal(text),
            k if "global".starts_with(k) && !k.is_empty() => ForeachSource::OfGlobal(text),
            "newlist" => ForeachSource::OfNewlist(parse_varlist(cur.src, rest)),
            "numlist" => {
                ForeachSource::OfNumlist(parse_numlist(cur.text(rest), rest).unwrap_or_default())
            }
            _ => ForeachSource::OfVarlist(parse_varlist(cur.src, rest)),
        }
    };
    let body = match brace {
        Some(i) => {
            cur.seek(i);
            brace_body(cur).0
        }
        None => {
            cur.seek(cur.len());
            cur.end_span()
        }
    };
    BlockCommand::Foreach {
        loopvar,
        source,
        body,
    }
}

fn forvalues(cur: &mut Cursor<'_>) -> BlockCommand {
    cur.bump();
    let loopvar = ident_text(cur);
    if cur.peek_kind() == TokKind::Assign {
        cur.bump();
    }
    let brace = find_top_brace(cur);
    let spec = cur.range_span(cur.pos()..brace.unwrap_or(cur.len()));
    let nl = parse_numlist(cur.text(spec), spec);
    let range = match nl.as_ref().and_then(|n| n.items.first()) {
        Some(crate::ast::NumListItem::Range { from, step, to }) => NumRange {
            from: *from,
            step: Some(*step),
            to: *to,
        },
        Some(crate::ast::NumListItem::Single(v)) => NumRange {
            from: *v,
            step: None,
            to: *v,
        },
        None => {
            cur.error(198, "invalid syntax", spec);
            NumRange {
                from: 0.0,
                step: None,
                to: -1.0,
            }
        }
    };
    let body = match brace {
        Some(i) => {
            cur.seek(i);
            brace_body(cur).0
        }
        None => {
            cur.seek(cur.len());
            cur.end_span()
        }
    };
    BlockCommand::Forvalues {
        loopvar,
        range,
        body,
    }
}

fn while_(cur: &mut Cursor<'_>) -> BlockCommand {
    cur.bump();
    let brace = find_top_brace(cur);
    let end = brace.unwrap_or(cur.len());
    let mut sub = cur.slice(cur.pos()..end);
    let cond = parse_expr(&mut sub);
    cur.absorb(sub);
    let body = match brace {
        Some(i) => {
            cur.seek(i);
            brace_body(cur).0
        }
        None => {
            cur.seek(cur.len());
            cur.end_span()
        }
    };
    BlockCommand::While { cond, body }
}

fn if_else(cur: &mut Cursor<'_>) -> BlockCommand {
    let mut arms: Vec<(Option<crate::ast::Expr>, Span)> = Vec::new();
    loop {
        if cur.peek_kind() != TokKind::Ident {
            break;
        }
        let word = cur.text(cur.peek().span);
        let is_else = word == "else";
        if !is_else && word != "if" {
            break;
        }
        cur.bump();
        // `else if` is one arm with a condition; a bare `else` has none.
        let has_cond = if is_else {
            if cur.peek_kind() == TokKind::Ident && cur.text(cur.peek().span) == "if" {
                cur.bump();
                true
            } else {
                false
            }
        } else {
            true
        };
        let cond = if has_cond {
            let brace = find_top_brace(cur);
            let end = brace.unwrap_or_else(|| first_non_expr(cur));
            let mut sub = cur.slice(cur.pos()..end);
            let e = parse_expr(&mut sub);
            cur.absorb(sub);
            cur.seek(end);
            Some(e)
        } else {
            None
        };
        let body = if cur.peek_kind() == TokKind::LBrace {
            brace_body(cur).0
        } else {
            // A single-line `if exp cmd`: the body is the rest of the text. This
            // is the shape lint `L004` is about — expansion has already fired
            // any `` `i++' `` in it, whether or not the branch is taken.
            let s = tail_span(cur);
            cur.seek(cur.len());
            s
        };
        arms.push((cond, body));
        if cur.peek_kind() != TokKind::Ident || cur.text(cur.peek().span) != "else" {
            break;
        }
    }
    BlockCommand::IfElse { arms }
}

/// `program [define] name [, options]`. `program drop|dir|list` do NOT define
/// (02 §9), so they fall through to the ordinary command path.
fn program(cur: &mut Cursor<'_>) -> Option<BlockCommand> {
    let save = cur.pos();
    cur.bump();
    let mut word = ident_text(cur);
    if word == "define" {
        word = ident_text(cur);
    } else if matches!(word.as_str(), "drop" | "dir" | "list") {
        cur.seek(save);
        return None;
    }
    if word.is_empty() {
        cur.seek(save);
        return None;
    }
    let opts_span = tail_span(cur);
    cur.seek(cur.len());
    Some(BlockCommand::Program {
        name: word,
        opts: RawArgs {
            text: cur.text(opts_span).trim().to_owned(),
            span: opts_span,
        },
        // The body is captured by the SEGMENTER, which knows where the matching
        // `end` is (02 §5.3). It is deliberately not re-derived here: a program
        // is compiled lazily, so re-scanning for `end` in the parser would be a
        // second definition of the same boundary.
        body: Span {
            start: opts_span.end,
            end: opts_span.end,
        },
    })
}

fn input(cur: &mut Cursor<'_>) -> BlockCommand {
    cur.bump();
    let spec_span = tail_span(cur);
    cur.seek(cur.len());
    BlockCommand::Input {
        spec: parse_varlist(cur.src, spec_span),
        data: Span {
            start: spec_span.end,
            end: spec_span.end,
        },
    }
}

fn ident_text(cur: &mut Cursor<'_>) -> String {
    if cur.peek_kind() == TokKind::Ident {
        let t = cur.bump();
        cur.text(t.span).to_owned()
    } else {
        String::new()
    }
}

// ────────────────────────────── the slot parser ─────────────────────────────

/// Split the tail into the universal-syntax slots.
fn parse_slots(cur: &mut Cursor<'_>, sig: &'static CommandSig) -> Slots {
    let mut slots = Slots::default();
    let end = cur.len();
    let segments = comma_segments(cur, cur.pos(), end);
    let mut first = true;
    for seg in segments {
        if first {
            parse_command_line(cur, sig, seg.clone(), &mut slots);
            first = false;
            continue;
        }
        // [U] 11.1.7: a SECOND comma returns to the command line, so a later
        // segment may be a qualifier rather than an option list.
        if seg.start < seg.end && is_qualifier_start(cur, sig, seg.start) {
            parse_qualifiers(cur, sig, seg, &mut slots);
        } else {
            let mut sub = cur.slice(seg);
            parse_options(&mut sub, sig, &mut slots.options);
            cur.absorb(sub);
        }
    }
    cur.seek(end);
    slots
}

fn parse_command_line(
    cur: &mut Cursor<'_>,
    sig: &'static CommandSig,
    seg: core::ops::Range<usize>,
    slots: &mut Slots,
) {
    let head_end = (seg.start..seg.end)
        .find(|i| is_qualifier_start(cur, sig, *i))
        .unwrap_or(seg.end);
    if head_end > seg.start {
        let span = cur.range_span(seg.start..head_end);
        let takes_varlist = sig
            .slots
            .intersects(SlotMask::VARLIST.union(SlotMask::NEWVARLIST));
        if takes_varlist {
            slots.varlist = Some(parse_varlist(cur.src, span));
        }
        // A command with BOTH slots — `use`, `format`, `merge`, `rename` — needs
        // the raw head as well: its tail is not a pure varlist and only the
        // command's own mini-parser knows what the extra words mean.
        if sig.slots.contains(SlotMask::REST) || !takes_varlist {
            if !sig.slots.contains(SlotMask::REST) {
                cur.error(101, "varlist not allowed", span);
            } else {
                slots.rest = Some(RawArgs {
                    text: cur.text(span).trim().to_owned(),
                    span,
                });
            }
        }
    }
    parse_qualifiers(cur, sig, head_end..seg.end, slots);
}

fn parse_qualifiers(
    cur: &mut Cursor<'_>,
    sig: &'static CommandSig,
    seg: core::ops::Range<usize>,
    slots: &mut Slots,
) {
    let mut i = seg.start;
    while i < seg.end {
        let next = ((i + 1)..seg.end)
            .find(|j| is_qualifier_start(cur, sig, *j))
            .unwrap_or(seg.end);
        match qualifier_at(cur, sig, i) {
            Some(Qual::Assign) => {
                let span = cur.range_span(i + 1..next);
                let mut sub = cur.slice(i + 1..next);
                let e = parse_expr(&mut sub);
                cur.absorb(sub);
                let _ = span;
                slots.assign = Some(e);
                i = next;
            }
            Some(Qual::If) => {
                let mut sub = cur.slice(i + 1..next);
                let e = parse_expr(&mut sub);
                cur.absorb(sub);
                slots.if_ = Some(e);
                i = next;
            }
            Some(Qual::In) => {
                let span = cur.range_span(i + 1..next);
                slots.in_ = Some(in_range(cur, span));
                i = next;
            }
            Some(Qual::Using) => {
                let span = cur.range_span(i + 1..next);
                slots.using = Some(FileSpec {
                    raw: cur.text(span).trim().to_owned(),
                    span,
                });
                i = next;
            }
            Some(Qual::Weight) => {
                let close = matching(cur, i, TokKind::LBracket, TokKind::RBracket, seg.end);
                let span = cur.range_span(i..close.min(seg.end) + 1);
                slots.weight = weight(cur, sig, i + 1, close, span);
                i = (close + 1).min(seg.end);
            }
            None => {
                // Unclassified tail: `label define lbl 1 "a"`, `matrix M = …`.
                let span = cur.range_span(i..seg.end);
                if sig.slots.contains(SlotMask::REST) {
                    match &mut slots.rest {
                        Some(r) => {
                            r.text = cur.text(join(r.span, span)).trim().to_owned();
                            r.span = join(r.span, span);
                        }
                        None => {
                            slots.rest = Some(RawArgs {
                                text: cur.text(span).trim().to_owned(),
                                span,
                            })
                        }
                    }
                } else if cur.mode == ParseMode::Execute {
                    cur.error(198, "invalid syntax", span);
                }
                return;
            }
        }
    }
}

enum Qual {
    Assign,
    If,
    In,
    Using,
    Weight,
}

/// A token is a qualifier only if the COMMAND accepts that slot.
///
/// Without the signature check the universal grammar reaches into commands whose
/// tail is deliberately raw: `local x = 1` would have its `=` read as an
/// assignment slot, be rejected because `local` has no `ASSIGN`, and lose the
/// value — a legal line failing with r(198). `local`, `display`, `scalar`,
/// `matrix`, `macro`, `label` and `set` all take a tail only their own
/// mini-parser understands, which is what `SlotMask::REST` means.
fn qualifier_at(cur: &Cursor<'_>, sig: &'static CommandSig, i: usize) -> Option<Qual> {
    let t = cur.at(i);
    let has = |m: SlotMask| sig.slots.contains(m);
    match t.kind {
        TokKind::Assign if has(SlotMask::ASSIGN) => Some(Qual::Assign),
        // A weight clause is its own WORD: `summarize price [fweight = n]`. A
        // GLUED `[` is an observation subscript — `gen lag = price[_n-1]` — and
        // reading that as a weight cuts the expression in half and loses the
        // subscript entirely.
        TokKind::LBracket if has(SlotMask::WEIGHT) && !t.glued => Some(Qual::Weight),
        TokKind::Ident => match cur.text(t.span) {
            "if" if has(SlotMask::IF) => Some(Qual::If),
            "in" if has(SlotMask::IN) => Some(Qual::In),
            "using" if has(SlotMask::USING) => Some(Qual::Using),
            _ => None,
        },
        _ => None,
    }
}

fn is_qualifier_start(cur: &Cursor<'_>, sig: &'static CommandSig, i: usize) -> bool {
    qualifier_at(cur, sig, i).is_some()
}

/// `in 1/10`, `in -5/l`, `in f/l`. Negative numbers count from the end
/// ([U] 11.1.4).
fn in_range(cur: &mut Cursor<'_>, span: Span) -> InRange {
    let text = cur.text(span).trim();
    let (a, b) = match text.split_once('/') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (text, text),
    };
    InRange {
        from: obs_ref(a),
        to: obs_ref(b),
        span,
    }
}

fn obs_ref(s: &str) -> ObsRef {
    match s {
        "f" | "F" => ObsRef::First,
        "l" | "L" => ObsRef::Last,
        _ => s.parse().map(ObsRef::Num).unwrap_or(ObsRef::Num(0)),
    }
}

fn weight(
    cur: &mut Cursor<'_>,
    sig: &'static CommandSig,
    start: usize,
    close: usize,
    span: Span,
) -> Option<Weight> {
    let (kind, expr_start) = match cur.at(start).kind {
        TokKind::Ident => {
            let w = cur.text(cur.at(start).span);
            let k = match w {
                "fweight" | "freq" => WeightKind::FWeight,
                "pweight" => WeightKind::PWeight,
                "aweight" | "weight" => WeightKind::AWeight,
                "iweight" => WeightKind::IWeight,
                _ => WeightKind::Default,
            };
            (
                k,
                if k == WeightKind::Default {
                    start
                } else {
                    start + 1
                },
            )
        }
        _ => (WeightKind::Default, start),
    };
    let mask = match kind {
        WeightKind::FWeight => WeightMask::FWEIGHT,
        WeightKind::PWeight => WeightMask::PWEIGHT,
        WeightKind::AWeight => WeightMask::AWEIGHT,
        WeightKind::IWeight => WeightMask::IWEIGHT,
        WeightKind::Default => WeightMask::empty(),
    };
    if !mask.is_empty() && !sig.weights.contains(mask) {
        cur.error(101, "weights not allowed", span);
    }
    let mut i = expr_start;
    if cur.at(i).kind == TokKind::Assign {
        i += 1;
    }
    let mut sub = cur.slice(i..close.min(cur.len()));
    let e = parse_expr(&mut sub);
    cur.absorb(sub);
    Some(Weight {
        kind,
        expr: e,
        span,
    })
}

// ─────────────────────────────── token scanning ─────────────────────────────

/// Top-level comma segments of `[start, end)`.
fn comma_segments(cur: &Cursor<'_>, start: usize, end: usize) -> Vec<core::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut seg = start;
    for i in start..end {
        match cur.at(i).kind {
            TokKind::LParen | TokKind::LBracket | TokKind::LBrace => depth += 1,
            TokKind::RParen | TokKind::RBracket | TokKind::RBrace => depth -= 1,
            TokKind::Comma if depth == 0 => {
                out.push(seg..i);
                seg = i + 1;
            }
            _ => {}
        }
    }
    out.push(seg..end);
    out
}

/// Index of the first `:` at depth 0 from `from`, or `None`.
fn top_colon(cur: &Cursor<'_>, from: usize) -> Option<usize> {
    let mut depth = 0i32;
    for i in from..cur.len() {
        match cur.at(i).kind {
            TokKind::LParen | TokKind::LBracket | TokKind::LBrace => depth += 1,
            TokKind::RParen | TokKind::RBracket | TokKind::RBrace => depth -= 1,
            TokKind::Colon if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Index of the first `{` at depth 0 from the cursor.
fn find_top_brace(cur: &Cursor<'_>) -> Option<usize> {
    let mut depth = 0i32;
    for i in cur.pos()..cur.len() {
        match cur.at(i).kind {
            TokKind::LParen | TokKind::LBracket => depth += 1,
            TokKind::RParen | TokKind::RBracket => depth -= 1,
            TokKind::LBrace if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Where a bare `if exp cmd` condition stops: the first token that cannot
/// continue an expression, which is the command word of the guarded command.
fn first_non_expr(cur: &Cursor<'_>) -> usize {
    let mut depth = 0i32;
    let mut i = cur.pos();
    let mut prev_was_value = false;
    while i < cur.len() {
        let k = cur.at(i).kind;
        match k {
            TokKind::LParen | TokKind::LBracket => depth += 1,
            TokKind::RParen | TokKind::RBracket => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            let is_value = matches!(
                k,
                TokKind::Ident
                    | TokKind::Number
                    | TokKind::MissingLit(_)
                    | TokKind::Str
                    | TokKind::CompoundStr
                    | TokKind::MacroRef
            );
            // Two values in a row at depth 0 mean the expression ended and the
            // guarded command began: `if x>0 summarize price`.
            if is_value && prev_was_value {
                return i;
            }
            prev_was_value = is_value || matches!(k, TokKind::RParen | TokKind::RBracket);
        }
        i += 1;
    }
    cur.len()
}

fn matching(cur: &Cursor<'_>, from: usize, open: TokKind, close: TokKind, end: usize) -> usize {
    let mut depth = 0i32;
    for i in from..end {
        let k = cur.at(i).kind;
        if k == open {
            depth += 1;
        } else if k == close {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
    }
    end.saturating_sub(1)
}

/// The first `,` at depth 0 of a raw text, as a byte offset.
fn top_comma_in_text(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in text.bytes().enumerate() {
        match c {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}
