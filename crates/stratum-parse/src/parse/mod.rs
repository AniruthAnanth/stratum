//! The parser — design 02 §6 (universal syntax), §8 (expressions), §9.1
//! (`syntax`), §10 (speculative parsing).
//!
//! # One grammar, two modes
//!
//! 02 §10 is emphatic that the editor's tolerant parse and the executor's strict
//! parse must be the SAME code path with a mode flag, not two grammars. Two
//! grammars diverge silently, and the divergence shows up as "the editor said
//! this was fine and the engine refused it" — the single worst class of bug in
//! an IDE for a language its users already know.
//!
//! [`ParseMode::Speculative`] therefore changes three things and nothing else:
//! an unexpanded macro reference is accepted wherever a name, number, string or
//! whole expression is expected (producing [`crate::ast::Expr::Hole`]), an
//! unexpected token is recorded and skipped rather than aborting, and no
//! diagnostic is raised for a macro that was never expanded.

pub mod command;
pub mod expr;
pub mod options;
pub mod speculative;
pub mod syntax;

use stratum_proto::{Confidence, Diagnostic, Severity, Span};

use crate::ast::CommandAst;
use crate::lex::{tokens, LexMode, Tok, TokKind};

pub use command::parse_command_tokens;

pub use expr::{parse_expr, parse_numlist};
pub use speculative::{parse_speculative, SpecStmt};
pub use syntax::{parse_syntax, SyntaxOpt, SyntaxSpec, SyntaxTy};

/// Strict for the executor, tolerant for the editor.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum ParseMode {
    /// The text has been macro-expanded and is about to run.
    #[default]
    Execute,
    /// The text is raw source with macros still in it.
    Speculative,
}

/// Design 02 §13.1's entry point: parse one macro-EXPANDED logical line.
///
/// Never returns `Err`. An unrecognised command word becomes
/// [`crate::ast::Command::Unknown`] with the raw tail preserved (decision D7),
/// because the user has ado-files and the "command foo is unrecognized  r(199)"
/// error is the RUNTIME's to raise, not the parser's.
pub fn parse_command(expanded: &str, mode: ParseMode) -> (CommandAst, Vec<Diagnostic>) {
    let (stmt, diags, _) = parse_command_counted(expanded, mode);
    (stmt, diags)
}

/// [`parse_command`] plus the ADR-017 token-read counter.
///
/// Separate entry point rather than a third tuple element on the main one: the
/// counter is a test instrument, and every caller in the engine wants the pair.
pub fn parse_command_counted(
    expanded: &str,
    mode: ParseMode,
) -> (CommandAst, Vec<Diagnostic>, u32) {
    let lex_mode = match mode {
        ParseMode::Execute => LexMode::Expanded,
        ParseMode::Speculative => LexMode::Speculative,
    };
    let toks = tokens(expanded, lex_mode);
    let mut cur = Cursor::new(expanded, &toks, mode);
    let stmt = parse_command_tokens(&mut cur);
    let reads = cur.token_reads();
    (stmt, cur.into_diagnostics(), reads)
}

/// A cursor over a token slice, carrying the diagnostics collected so far.
///
/// The whole line is lexed ONCE and slots are token index ranges into that one
/// buffer, so every span in the resulting AST is an offset in the text that was
/// handed to [`parse_command`] — never into a temporary substring that would
/// have to be rebased before it could underline anything.
pub struct Cursor<'s> {
    /// The text being parsed.
    pub src: &'s str,
    toks: &'s [Tok],
    pos: usize,
    /// Strict or tolerant.
    pub mode: ParseMode,
    diags: Vec<Diagnostic>,
    /// ADR-017 counter: how many times a token has been looked at.
    ///
    /// The slot splitter locates qualifiers by scanning forward, and the danger
    /// with that shape is a rescan per qualifier turning into O(n²) on a long
    /// option list. This counts the reads so the property can be ASSERTED
    /// instead of assumed — `tests/parse.rs` parses the same command with 8 and
    /// with 128 options and requires the reads-per-token ratio not to grow.
    reads: core::cell::Cell<u32>,
}

impl<'s> Cursor<'s> {
    /// Wrap a token slice.
    pub fn new(src: &'s str, toks: &'s [Tok], mode: ParseMode) -> Self {
        // The `Eof` the lexer appends is dropped: a sub-cursor over a slot has
        // no `Eof` of its own, so `peek` synthesises one and the two behave the
        // same way at the end.
        let toks = match toks.last() {
            Some(t) if t.kind == TokKind::Eof => &toks[..toks.len() - 1],
            _ => toks,
        };
        Cursor {
            src,
            toks,
            pos: 0,
            mode,
            diags: Vec::new(),
            reads: core::cell::Cell::new(0),
        }
    }

    /// A cursor over a token index range of the same buffer, inheriting nothing
    /// but the mode. Diagnostics are merged back with [`Cursor::absorb`].
    pub fn slice(&self, range: core::ops::Range<usize>) -> Cursor<'s> {
        Cursor {
            src: self.src,
            toks: &self.toks[range],
            pos: 0,
            mode: self.mode,
            diags: Vec::new(),
            reads: core::cell::Cell::new(0),
        }
    }

    /// Take a sub-cursor's diagnostics and its read count.
    pub fn absorb(&mut self, other: Cursor<'s>) {
        self.diags.extend(other.diags);
        self.reads.set(self.reads.get() + other.reads.get());
    }

    /// ADR-017 counter: total token reads so far. See [`Cursor::reads`].
    pub fn token_reads(&self) -> u32 {
        self.reads.get()
    }

    /// Number of tokens.
    pub fn len(&self) -> usize {
        self.toks.len()
    }

    /// True when there are no tokens at all.
    pub fn is_empty(&self) -> bool {
        self.toks.is_empty()
    }

    /// Current index.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Jump to an index.
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.toks.len());
    }

    /// The whole token slice.
    pub fn toks(&self) -> &'s [Tok] {
        self.toks
    }

    /// The token at the cursor, or a synthetic `Eof` at the end.
    pub fn peek(&self) -> Tok {
        self.at(self.pos)
    }

    /// The token `n` positions ahead.
    pub fn at(&self, i: usize) -> Tok {
        self.reads.set(self.reads.get() + 1);
        self.toks.get(i).copied().unwrap_or(Tok {
            kind: TokKind::Eof,
            span: self.end_span(),
            glued: false,
        })
    }

    /// [`Cursor::peek`]'s kind.
    pub fn peek_kind(&self) -> TokKind {
        self.peek().kind
    }

    /// Advance one token and return the one stepped over.
    pub fn bump(&mut self) -> Tok {
        let t = self.peek();
        if self.pos < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    /// True when the cursor is past the last token.
    pub fn done(&self) -> bool {
        self.pos >= self.toks.len()
    }

    /// The source text of a span.
    pub fn text(&self, s: Span) -> &'s str {
        &self.src[s.start as usize..s.end as usize]
    }

    /// An empty span at the end of the token slice.
    pub fn end_span(&self) -> Span {
        match self.toks.last() {
            Some(t) => Span {
                start: t.span.end,
                end: t.span.end,
            },
            None => Span { start: 0, end: 0 },
        }
    }

    /// The span from the first to the last token of a range.
    pub fn range_span(&self, range: core::ops::Range<usize>) -> Span {
        match (self.toks.get(range.start), self.toks[..range.end].last()) {
            (Some(a), Some(b)) if range.start < range.end => Span {
                start: a.span.start,
                end: b.span.end,
            },
            _ => self.end_span(),
        }
    }

    /// Consume a token of the given kind, or record r(198) and consume nothing.
    pub fn expect(&mut self, kind: TokKind, what: &str) -> Option<Span> {
        if self.peek_kind() == kind {
            return Some(self.bump().span);
        }
        let at = self.peek().span;
        // r(132) is Stata's "too many )" — the one unbalanced-delimiter code it
        // has. Everything else here is r(198) invalid syntax.
        let rc = if matches!(kind, TokKind::RParen) {
            132
        } else {
            198
        };
        self.error(rc, format!("expected `{what}`"), at);
        None
    }

    /// Record a Stata-coded error.
    pub fn error(&mut self, rc: u32, message: impl Into<String>, span: Span) {
        let token = self.text(span).to_owned();
        self.diags.push(Diagnostic {
            severity: Severity::Error,
            code: format!("STATA{rc:04}"),
            stata_rc: Some(rc),
            message: message.into(),
            file: None,
            span: Some(span),
            offending_token: (!token.is_empty()).then_some(token),
            block: None,
            related: Vec::new(),
            suggestions: Vec::new(),
            notes: Vec::new(),
            confidence: Confidence::Exact,
        });
    }

    /// Push a fully built diagnostic.
    pub fn push(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    /// Everything recorded so far.
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diags
    }

    /// Diagnostics recorded so far, without consuming the cursor.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diags
    }

    /// True when the `[` at the cursor encloses a top-level comma — the one
    /// thing that tells `M[i,j]` from `x[exp]`.
    pub fn bracket_has_top_comma(&self) -> bool {
        let mut depth = 0i32;
        for i in self.pos..self.toks.len() {
            match self.toks[i].kind {
                TokKind::LBracket | TokKind::LParen => depth += 1,
                TokKind::RBracket | TokKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                TokKind::Comma if depth == 1 => return true,
                _ => {}
            }
        }
        false
    }

    /// Consume every token glued to the previous one, returning the last span.
    ///
    /// `L.gnp` is three tokens with no whitespace between them; the varlist
    /// grammar is a WORD grammar, so the whole run has to be handed to it as one
    /// slice of source.
    pub fn consume_glued_word(&mut self) -> Span {
        let mut last = self.peek().span;
        while self.peek().glued && !self.done() {
            last = self.bump().span;
        }
        last
    }
}
