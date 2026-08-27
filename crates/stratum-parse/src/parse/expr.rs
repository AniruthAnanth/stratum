//! The expression parser — design 02 §8, a Pratt parser over the precedence
//! table of §8.1.
//!
//! Left-associativity for EVERY binary operator, `^` included, is implemented by
//! recursing at `prec + 1` on the right-hand side. There is no associativity
//! table because there are no right-associative operators in Stata: `2^3^2` is
//! `64` [V], which is `(2^3)^2`, and getting that one wrong silently changes the
//! answer of any expression with two exponentiations in it.

use stratum_proto::Span;

use crate::ast::expr::{BinOp, CoefKind, Expr, NumList, NumListItem, StoredClass, SysVar, UnOp};
use crate::ast::expr::{NOT_PREC, SIGN_PREC};
use crate::cmdtable;
use crate::lex::{unquote, Op, TokKind};
use crate::parse::{Cursor, ParseMode};

/// Parse one expression from the cursor.
pub fn parse_expr(cur: &mut Cursor<'_>) -> Expr {
    expr_bp(cur, 0)
}

fn expr_bp(cur: &mut Cursor<'_>, min_bp: u8) -> Expr {
    let mut lhs = prefix(cur);
    while let Some(op) = infix_op(cur.peek_kind()) {
        if op.prec() < min_bp {
            break;
        }
        cur.bump();
        // `prec + 1`: LEFT-associative. `prec` here would make `^` right-assoc
        // and `2^3^2` would answer 512.
        let rhs = expr_bp(cur, op.prec() + 1);
        let span = join(lhs.span(), rhs.span());
        lhs = Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span,
        };
    }
    lhs
}

fn prefix(cur: &mut Cursor<'_>) -> Expr {
    let t = cur.peek();
    match t.kind {
        TokKind::Op(Op::Minus) | TokKind::Op(Op::Plus) => {
            let op = if t.kind == TokKind::Op(Op::Minus) {
                UnOp::Neg
            } else {
                UnOp::Pos
            };
            cur.bump();
            // `-2^2` is `-4` [V]: the sign binds LOOSER than `^`, so the operand
            // is parsed at 80 and `^` (90) is swallowed into it.
            let rhs = expr_bp(cur, SIGN_PREC);
            let span = join(t.span, rhs.span());
            Expr::Unary {
                op,
                rhs: Box::new(rhs),
                span,
            }
        }
        TokKind::Op(Op::Not) | TokKind::Op(Op::Tilde) => {
            cur.bump();
            // `!0/2` is `.5` [V] — `!` binds tighter than `/` — and `!2^0` is
            // `0` [V], so `^` still wins. NOT_PREC sits between them, which is
            // where the manual is wrong and the machine is right.
            let rhs = expr_bp(cur, NOT_PREC);
            let span = join(t.span, rhs.span());
            Expr::Unary {
                op: UnOp::Not,
                rhs: Box::new(rhs),
                span,
            }
        }
        _ => postfix(cur),
    }
}

fn postfix(cur: &mut Cursor<'_>) -> Expr {
    let mut e = primary(cur);
    // `x[_n-1]` — an observation subscript may follow any primary.
    while cur.peek_kind() == TokKind::LBracket {
        let open = cur.peek().span;
        cur.bump();
        let idx = expr_bp(cur, 0);
        let close = cur.expect(TokKind::RBracket, "]");
        let span = join(e.span(), close.unwrap_or(open));
        e = Expr::Index {
            base: Box::new(e),
            idx: Box::new(idx),
            span,
        };
    }
    e
}

fn primary(cur: &mut Cursor<'_>) -> Expr {
    let t = cur.peek();
    match t.kind {
        TokKind::Number => {
            cur.bump();
            let text = cur.text(t.span);
            match text.parse::<f64>() {
                Ok(v) => Expr::Num(v, t.span),
                Err(_) => {
                    cur.error(198, format!("invalid number {text}"), t.span);
                    Expr::Num(0.0, t.span)
                }
            }
        }
        TokKind::MissingLit(k) => {
            cur.bump();
            Expr::Missing(k, t.span)
        }
        TokKind::Str | TokKind::CompoundStr => {
            cur.bump();
            Expr::Str(unquote(cur.text(t.span)).to_owned(), t.span)
        }
        TokKind::MacroRef => {
            cur.bump();
            if cur.mode == ParseMode::Execute {
                // Expanded text cannot contain one. If it does, expansion is
                // broken and saying so beats parsing a hole into an executable
                // AST.
                cur.error(198, "unexpanded macro reference", t.span);
            }
            Expr::Hole { src: t.span }
        }
        TokKind::LParen => {
            cur.bump();
            let inner = expr_bp(cur, 0);
            let close = cur.expect(TokKind::RParen, ")");
            Expr::Paren(Box::new(inner), join(t.span, close.unwrap_or(t.span)))
        }
        TokKind::Ident => {
            cur.bump();
            ident_primary(cur, t.span)
        }
        TokKind::Dot => {
            // A leading `.` the lexer could not read as a missing literal. It is
            // still `.` in Stata (`di .` prints `.`), so accept it rather than
            // producing a parse error the user cannot act on.
            cur.bump();
            Expr::Missing(0, t.span)
        }
        _ => {
            cur.error(198, "invalid syntax", t.span);
            Expr::Hole { src: t.span }
        }
    }
}

fn ident_primary(cur: &mut Cursor<'_>, name_span: Span) -> Expr {
    let name = cur.text(name_span);
    // `_b[price]`, `_se[_cons]`, `_coef[price]`.
    if let Some(kind) = coef_kind(name) {
        if cur.peek_kind() == TokKind::LBracket {
            cur.bump();
            let key = expr_bp(cur, 0);
            let close = cur.expect(TokKind::RBracket, "]");
            return Expr::Coef {
                kind,
                key: Box::new(key),
                span: join(name_span, close.unwrap_or(name_span)),
            };
        }
    }
    if let Some(sv) = sys_var(name) {
        return Expr::Sys(sv, name_span);
    }
    if cur.peek_kind() == TokKind::LParen {
        let owned = name.to_owned();
        cur.bump();
        let mut args = Vec::new();
        if cur.peek_kind() != TokKind::RParen {
            loop {
                args.push(expr_bp(cur, 0));
                if cur.peek_kind() == TokKind::Comma {
                    cur.bump();
                    continue;
                }
                break;
            }
        }
        let close = cur.expect(TokKind::RParen, ")");
        let span = join(name_span, close.unwrap_or(name_span));
        // `r()`, `e()`, `c()`, `s()` are stored-result lookups, not calls.
        if let Some(class) = stored_class(&owned) {
            let key = args
                .into_iter()
                .next()
                .unwrap_or(Expr::Str(String::new(), span));
            return Expr::Stored {
                class,
                key: Box::new(key),
                span,
            };
        }
        // An UNKNOWN function is not a parse error: 02 §8.5 and decision D7 put
        // that failure at eval time as r(133), so an ado-file using a v2
        // function still highlights and folds.
        if let Some(sig) = cmdtable::function(&owned) {
            if !sig.accepts(args.len()) {
                cur.error(
                    198,
                    format!("{owned}() takes a different number of arguments"),
                    span,
                );
            }
        }
        return Expr::Call {
            name: owned,
            args,
            span,
        };
    }
    // `M[i,j]` — a matrix element. Told from `x[exp]` by the comma, which is
    // exactly how Stata tells them apart: a subscript expression has no
    // top-level comma.
    if cur.peek_kind() == TokKind::LBracket && cur.bracket_has_top_comma() {
        let owned = name.to_owned();
        cur.bump();
        let i = expr_bp(cur, 0);
        cur.expect(TokKind::Comma, ",");
        let j = expr_bp(cur, 0);
        let close = cur.expect(TokKind::RBracket, "]");
        return Expr::MatElem {
            name: owned,
            i: Box::new(i),
            j: Box::new(j),
            span: join(name_span, close.unwrap_or(name_span)),
        };
    }
    // `L.gnp`, `i.rep78` inside an expression: a glued `.` followed by a glued
    // name. The varlist reader owns the grammar, so the whole run of glued
    // tokens is handed to it verbatim.
    if cur.peek_kind() == TokKind::Dot && cur.peek().glued {
        let end = cur.consume_glued_word();
        let span = join(name_span, end);
        let atom = crate::varlist::parse_varlist(cur.src, span);
        if let Some(item) = atom.items.into_iter().next() {
            if let crate::ast::VarItemKind::Single(a) = item.kind {
                return Expr::Term(Box::new(a), span);
            }
        }
        return Expr::Name(cur.text(span).to_owned(), span);
    }
    Expr::Name(name.to_owned(), name_span)
}

fn coef_kind(name: &str) -> Option<CoefKind> {
    match name {
        "_b" => Some(CoefKind::B),
        "_se" => Some(CoefKind::Se),
        "_coef" => Some(CoefKind::Coef),
        _ => None,
    }
}

fn sys_var(name: &str) -> Option<SysVar> {
    match name {
        "_n" => Some(SysVar::NLower),
        "_N" => Some(SysVar::NUpper),
        "_pi" => Some(SysVar::Pi),
        "_rc" => Some(SysVar::Rc),
        _ => None,
    }
}

fn stored_class(name: &str) -> Option<StoredClass> {
    match name {
        "r" => Some(StoredClass::R),
        "e" => Some(StoredClass::E),
        "c" => Some(StoredClass::C),
        "s" => Some(StoredClass::S),
        _ => None,
    }
}

fn infix_op(k: TokKind) -> Option<BinOp> {
    Some(match k {
        TokKind::Op(Op::Caret) => BinOp::Pow,
        TokKind::Op(Op::Slash) => BinOp::Div,
        TokKind::Op(Op::Star) => BinOp::Mul,
        TokKind::Op(Op::Minus) => BinOp::Sub,
        TokKind::Op(Op::Plus) => BinOp::Add,
        TokKind::Op(Op::Ne) => BinOp::Ne,
        TokKind::Op(Op::Gt) => BinOp::Gt,
        TokKind::Op(Op::Lt) => BinOp::Lt,
        TokKind::Op(Op::Le) => BinOp::Le,
        TokKind::Op(Op::Ge) => BinOp::Ge,
        TokKind::Op(Op::EqEq) => BinOp::Eq,
        TokKind::Op(Op::And) => BinOp::And,
        TokKind::Op(Op::Or) => BinOp::Or,
        _ => return None,
    })
}

pub(crate) fn join(a: Span, b: Span) -> Span {
    Span {
        start: a.start.min(b.start),
        end: a.end.max(b.end),
    }
}

// ──────────────────────────────── numlists ──────────────────────────────────

/// Parse a numlist: `1 2 3`, `1/10`, `1(2)9`, `1 3 to 9`, and mixtures.
///
/// Returns `None` on anything that is not a numlist, which is how `foreach x of
/// numlist …` tells a bad range from a good one. Ranges are kept as ranges —
/// see [`NumList::count`] for why `forvalues i = 1/10000000` must not
/// materialise.
pub fn parse_numlist(text: &str, span: Span) -> Option<NumList> {
    let mut items = Vec::new();
    let words: Vec<&str> = text
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    let mut i = 0usize;
    while i < words.len() {
        let w = words[i];
        // `a/b` and `a(s)b` are one word.
        if let Some(item) = range_word(w) {
            items.push(item);
            i += 1;
            continue;
        }
        let first: f64 = w.parse().ok()?;
        // `first to last` and `first second to last`.
        if words.get(i + 1).is_some_and(|n| eq_ignore_case(n, "to")) {
            let last: f64 = words.get(i + 2)?.parse().ok()?;
            items.push(NumListItem::Range {
                from: first,
                step: 1.0,
                to: last,
            });
            i += 3;
            continue;
        }
        if words.get(i + 2).is_some_and(|n| eq_ignore_case(n, "to")) {
            if let Ok(second) = words[i + 1].parse::<f64>() {
                let last: f64 = words.get(i + 3)?.parse().ok()?;
                let step = second - first;
                if step == 0.0 {
                    return None;
                }
                items.push(NumListItem::Range {
                    from: first,
                    step,
                    to: last,
                });
                i += 4;
                continue;
            }
        }
        items.push(NumListItem::Single(first));
        i += 1;
    }
    if items.is_empty() {
        return None;
    }
    Some(NumList { items, span })
}

fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn range_word(w: &str) -> Option<NumListItem> {
    if let Some((a, b)) = w.split_once('/') {
        return Some(NumListItem::Range {
            from: a.parse().ok()?,
            step: 1.0,
            to: b.parse().ok()?,
        });
    }
    if let Some(open) = w.find('(') {
        let close = w.find(')')?;
        if close < open {
            return None;
        }
        let from: f64 = w[..open].parse().ok()?;
        let step: f64 = w[open + 1..close].parse().ok()?;
        let to: f64 = w[close + 1..].parse().ok()?;
        if step == 0.0 {
            return None;
        }
        return Some(NumListItem::Range { from, step, to });
    }
    None
}
