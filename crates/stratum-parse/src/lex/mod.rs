//! The lexer — design 02 §3, step 3 of the pipeline.
//!
//! # It runs AFTER macro expansion, and that is the whole design
//!
//! Design 02 §1.1 proves the ordering with two measured cases. `local q =
//! `"embedded "quote""'` then `di "B13: `q'"` is an ERROR in Stata [V], because
//! substituting the macro into the string literal produces
//! `di "B13: embedded "quote""` and *that* text is what gets tokenized. And
//! `di `="ab"+"cd"'` produces `abcd not found  r(111)` [V], because the `=exp`
//! result is inserted as a bare, unquoted literal and `di abcd` then reads
//! `abcd` as a variable name.
//!
//! Both would be impossible if the lexer knew about macros. So [`Lexer`] has two
//! modes and the difference between them is exactly one thing: in
//! [`LexMode::Expanded`] a backtick is an ordinary byte that cannot appear
//! (expansion consumed them all), and in [`LexMode::Speculative`] — the editor's
//! path, where nothing has been expanded — `` `x' `` and `$x` lex to one
//! [`TokKind::MacroRef`] token that the parser accepts wherever a name, a
//! number, a string or a whole expression is expected.
//!
//! # Compound double quotes
//!
//! `` `"…"' `` is a single string token whose body may contain bare `"`
//! characters, and it NESTS. `di `"nested "quoted" text"'` prints
//! `nested "quoted" text` [V] — `tests/golden/stata18/semantics.log`. The
//! matching close is found by counting `` `" `` / `"' `` pairs, which is why a
//! plain "scan to the next `"'`" is wrong on `` `"a `"b"' c"' ``.

use stratum_proto::{Span, Token, TokenKind};

/// How much the lexer is allowed to assume has already happened.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum LexMode {
    /// Input is macro-EXPANDED text. This is the execution path.
    #[default]
    Expanded,
    /// Input is raw source with macro references still in it. This is the
    /// editor's path: highlighting, completion, and the "Used by" walk.
    Speculative,
}

/// A lexed token. `text` is recovered from the source with [`Tok::text`] rather
/// than stored: a `&str` in every token doubles the struct and an expression is
/// re-lexed on every keystroke in the editor.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Tok {
    /// What it is.
    pub kind: TokKind,
    /// Extent in the text that was lexed.
    pub span: Span,
    /// True when no whitespace separates this token from the previous one.
    ///
    /// Adjacency is meaningful in Stata and cannot be recovered later without
    /// the source: `L.x` is a time-series operator but `L . x` is not, and
    /// `pri~e` is one varlist atom while `pri ~ e` is three tokens.
    pub glued: bool,
}

impl Tok {
    /// The token's source text.
    pub fn text<'s>(&self, src: &'s str) -> &'s str {
        &src[self.span.start as usize..self.span.end as usize]
    }
}

/// The token kinds the parser branches on.
///
/// Richer than [`TokenKind`], which is the wire projection: the parser needs to
/// know *which* operator, and the editor's highlighter does not.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum TokKind {
    /// A name: letters, digits and `_`, not starting with a digit.
    Ident,
    /// A numeric literal.
    Number,
    /// `.`, `.a` .. `.z` — a missing literal. The payload is the tag, `0` for
    /// `.` and `1..=26` for `.a`..=`.z`.
    MissingLit(u8),
    /// `"…"`. [`Tok::span`] covers the quotes; [`unquote`] strips them.
    Str,
    /// `` `"…"' ``, nesting-aware.
    CompoundStr,
    /// `` `x' ``, `$x`, `${x}` — [`LexMode::Speculative`] only.
    MacroRef,
    /// One of [`Op`].
    Op(Op),
    /// `,`.
    Comma,
    /// `:`.
    Colon,
    /// `.` not forming a missing literal — the member/operator dot of `L.x`,
    /// `i.rep78`, `foo.bar`.
    Dot,
    /// `(`.
    LParen,
    /// `)`.
    RParen,
    /// `{`.
    LBrace,
    /// `}`.
    RBrace,
    /// `[`.
    LBracket,
    /// `]`.
    RBracket,
    /// `=` used as assignment, not `==`.
    Assign,
    /// `;` — a statement break in `#delimit ;` mode.
    Semi,
    /// A byte that is not part of any Stata token.
    Unknown,
    /// End of input. Emitted once, with an empty span at the end, so the
    /// parser never has to bounds-check the cursor.
    Eof,
}

/// The operators, split out so [`crate::ast::BinOp`] can be derived from one.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Op {
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `*`.
    Star,
    /// `/`.
    Slash,
    /// `^`.
    Caret,
    /// `==`.
    EqEq,
    /// `!=` or `~=`.
    Ne,
    /// `<`.
    Lt,
    /// `>`.
    Gt,
    /// `<=`.
    Le,
    /// `>=`.
    Ge,
    /// `&`.
    And,
    /// `|`.
    Or,
    /// `!` or `~` in prefix position.
    Not,
    /// `~` — the varlist wildcard, when it is not `~=`.
    Tilde,
    /// `\` — the matrix row separator.
    Backslash,
    /// `#` — a factor-variable interaction.
    Hash,
    /// `##`.
    HashHash,
}

/// Tokenize `src`.
///
/// Always terminates with exactly one [`TokKind::Eof`].
pub fn tokens(src: &str, mode: LexMode) -> Vec<Tok> {
    let mut lx = Lexer::new(src, mode);
    let mut out = Vec::with_capacity(src.len() / 4 + 2);
    loop {
        let t = lx.next_token();
        let done = t.kind == TokKind::Eof;
        out.push(t);
        if done {
            return out;
        }
    }
}

/// `ProgramIndex::lex` (CONTRACTS §13): the same tokenizer the runtime executes
/// with, projected onto the wire [`TokenKind`].
///
/// CONTRACTS §13 is emphatic that this must be the exact code the runtime uses;
/// a "close enough" second implementation voids the §23 auto-comment guarantee.
/// It is therefore a projection of [`tokens`] and not a reimplementation.
/// Speculative mode, because it is called on raw editor text.
pub fn lex(src: &str) -> Vec<Token> {
    tokens(src, LexMode::Speculative)
        .into_iter()
        .filter(|t| t.kind != TokKind::Eof)
        .map(|t| Token {
            kind: t.kind.wire(),
            span: t.span,
        })
        .collect()
}

impl TokKind {
    /// The wire projection (CONTRACTS §1.2 / proto `TokenKind`).
    pub const fn wire(self) -> TokenKind {
        match self {
            TokKind::Ident => TokenKind::Ident,
            TokKind::Number | TokKind::MissingLit(_) => TokenKind::Number,
            TokKind::Str => TokenKind::StrLit,
            TokKind::CompoundStr => TokenKind::CompoundQuote,
            TokKind::MacroRef => TokenKind::MacroRef,
            TokKind::Comma => TokenKind::Comma,
            TokKind::Colon => TokenKind::Colon,
            TokKind::LParen => TokenKind::LParen,
            TokKind::RParen => TokenKind::RParen,
            TokKind::LBrace => TokenKind::LBrace,
            TokKind::RBrace => TokenKind::RBrace,
            TokKind::LBracket => TokenKind::LBracket,
            TokKind::RBracket => TokenKind::RBracket,
            TokKind::Semi => TokenKind::StatementBreak,
            TokKind::Op(_) | TokKind::Assign | TokKind::Dot => TokenKind::Op,
            TokKind::Unknown | TokKind::Eof => TokenKind::Unknown,
        }
    }
}

/// A hand-written lexer over already-expanded text.
pub struct Lexer<'s> {
    src: &'s str,
    b: &'s [u8],
    i: usize,
    mode: LexMode,
}

impl<'s> Lexer<'s> {
    /// Start at byte 0 of `src`.
    pub fn new(src: &'s str, mode: LexMode) -> Self {
        Lexer {
            src,
            b: src.as_bytes(),
            i: 0,
            mode,
        }
    }

    /// Byte offset of the cursor.
    pub fn offset(&self) -> usize {
        self.i
    }

    fn tok(&self, kind: TokKind, start: usize, glued: bool) -> Tok {
        Tok {
            kind,
            span: Span {
                start: start as u32,
                end: self.i as u32,
            },
            glued,
        }
    }

    /// The next token, whitespace skipped.
    pub fn next_token(&mut self) -> Tok {
        let before = self.i;
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
        let glued = self.i == before && before != 0;
        let start = self.i;
        if self.i >= self.b.len() {
            return self.tok(TokKind::Eof, start, glued);
        }
        let c = self.b[self.i];
        match c {
            b'"' => {
                self.scan_dquote();
                self.tok(TokKind::Str, start, glued)
            }
            b'`' if self.mode == LexMode::Speculative || self.starts_compound() => {
                if self.starts_compound() {
                    self.scan_compound();
                    self.tok(TokKind::CompoundStr, start, glued)
                } else {
                    self.scan_macro_ref();
                    self.tok(TokKind::MacroRef, start, glued)
                }
            }
            b'$' if self.mode == LexMode::Speculative => {
                self.scan_global_ref();
                self.tok(TokKind::MacroRef, start, glued)
            }
            b'0'..=b'9' => {
                self.scan_number();
                self.tok(TokKind::Number, start, glued)
            }
            b'.' => self.scan_dot(start, glued),
            b',' => self.one(TokKind::Comma, start, glued),
            b':' => self.one(TokKind::Colon, start, glued),
            b'(' => self.one(TokKind::LParen, start, glued),
            b')' => self.one(TokKind::RParen, start, glued),
            b'{' => self.one(TokKind::LBrace, start, glued),
            b'}' => self.one(TokKind::RBrace, start, glued),
            b'[' => self.one(TokKind::LBracket, start, glued),
            b']' => self.one(TokKind::RBracket, start, glued),
            b';' => self.one(TokKind::Semi, start, glued),
            b'+' => self.one(TokKind::Op(Op::Plus), start, glued),
            b'-' => self.one(TokKind::Op(Op::Minus), start, glued),
            b'*' => self.one(TokKind::Op(Op::Star), start, glued),
            b'/' => self.one(TokKind::Op(Op::Slash), start, glued),
            b'^' => self.one(TokKind::Op(Op::Caret), start, glued),
            b'&' => self.one(TokKind::Op(Op::And), start, glued),
            b'|' => self.one(TokKind::Op(Op::Or), start, glued),
            b'\\' => self.one(TokKind::Op(Op::Backslash), start, glued),
            b'#' => {
                self.i += 1;
                if self.peek() == Some(b'#') {
                    self.i += 1;
                    self.tok(TokKind::Op(Op::HashHash), start, glued)
                } else {
                    self.tok(TokKind::Op(Op::Hash), start, glued)
                }
            }
            b'=' => {
                self.i += 1;
                if self.peek() == Some(b'=') {
                    self.i += 1;
                    self.tok(TokKind::Op(Op::EqEq), start, glued)
                } else {
                    self.tok(TokKind::Assign, start, glued)
                }
            }
            b'<' => {
                self.i += 1;
                match self.peek() {
                    Some(b'=') => {
                        self.i += 1;
                        self.tok(TokKind::Op(Op::Le), start, glued)
                    }
                    // `<>` is not Stata, but accepting it as `!=` in the
                    // speculative pass would silently highlight bad code as
                    // good. It falls through as `<` then `>`.
                    _ => self.tok(TokKind::Op(Op::Lt), start, glued),
                }
            }
            b'>' => {
                self.i += 1;
                if self.peek() == Some(b'=') {
                    self.i += 1;
                    self.tok(TokKind::Op(Op::Ge), start, glued)
                } else {
                    self.tok(TokKind::Op(Op::Gt), start, glued)
                }
            }
            b'!' => {
                self.i += 1;
                if self.peek() == Some(b'=') {
                    self.i += 1;
                    self.tok(TokKind::Op(Op::Ne), start, glued)
                } else {
                    self.tok(TokKind::Op(Op::Not), start, glued)
                }
            }
            // `~` is `!=`'s partner in `~=`, prefix `not` on its own in an
            // expression, and the "must match exactly one" wildcard in a
            // varlist. The varlist reader works on text (02 §7 is a word
            // grammar), so the ambiguity never reaches it: here `~` alone is
            // `Tilde` and the expression parser reads it as `Not` in prefix
            // position.
            b'~' => {
                self.i += 1;
                if self.peek() == Some(b'=') {
                    self.i += 1;
                    self.tok(TokKind::Op(Op::Ne), start, glued)
                } else {
                    self.tok(TokKind::Op(Op::Tilde), start, glued)
                }
            }
            _ if is_ident_start(self.src, self.i) => {
                self.scan_ident();
                self.tok(TokKind::Ident, start, glued)
            }
            _ => {
                // Advance a whole UTF-8 scalar: `self.i + 1` inside a multibyte
                // sequence produces a span that panics when sliced.
                self.i += utf8_len(c);
                self.tok(TokKind::Unknown, start, glued)
            }
        }
    }

    fn one(&mut self, kind: TokKind, start: usize, glued: bool) -> Tok {
        self.i += 1;
        self.tok(kind, start, glued)
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn starts_compound(&self) -> bool {
        self.b[self.i] == b'`' && self.b.get(self.i + 1) == Some(&b'"')
    }

    /// `"…"`. An unterminated literal closes at end of input rather than
    /// erroring — 02 §2.2, and it is what the logical-line reader already did.
    fn scan_dquote(&mut self) {
        self.i += 1;
        while self.i < self.b.len() {
            if self.b[self.i] == b'"' {
                self.i += 1;
                return;
            }
            if self.b[self.i] == b'\n' {
                return;
            }
            self.i += 1;
        }
    }

    /// `` `"…"' ``, counting nested `` `" `` / `"' `` pairs.
    fn scan_compound(&mut self) {
        let mut depth = 0usize;
        while self.i < self.b.len() {
            if self.b[self.i] == b'`' && self.b.get(self.i + 1) == Some(&b'"') {
                depth += 1;
                self.i += 2;
                continue;
            }
            if self.b[self.i] == b'"' && self.b.get(self.i + 1) == Some(&b'\'') {
                depth -= 1;
                self.i += 2;
                if depth == 0 {
                    return;
                }
                continue;
            }
            self.i += 1;
        }
    }

    /// `` `x' `` with `` ` ``/`'` nesting — speculative mode only.
    fn scan_macro_ref(&mut self) {
        let mut depth = 0usize;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'`' => {
                    depth += 1;
                    self.i += 1;
                }
                b'\'' => {
                    depth -= 1;
                    self.i += 1;
                    if depth == 0 {
                        return;
                    }
                }
                b'\n' => return,
                c => self.i += utf8_len(c),
            }
        }
    }

    /// `$x`, `${x}` — speculative mode only. Maximal munch (02 §4.2).
    fn scan_global_ref(&mut self) {
        self.i += 1;
        if self.peek() == Some(b'{') {
            while self.i < self.b.len() {
                let c = self.b[self.i];
                self.i += utf8_len(c);
                if c == b'}' {
                    return;
                }
            }
            return;
        }
        while self.i < self.b.len() && is_ident_continue(self.src, self.i) {
            self.i += utf8_len(self.b[self.i]);
        }
    }

    fn scan_ident(&mut self) {
        while self.i < self.b.len() && is_ident_continue(self.src, self.i) {
            self.i += utf8_len(self.b[self.i]);
        }
    }

    fn scan_number(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        // `1e5`, `1E-5`, `1e+5`. `1e` with no digits is NOT an exponent: the
        // cursor is rolled back so `2exp` stays `2` then `exp`.
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let save = self.i;
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                    self.i += 1;
                }
            } else {
                self.i = save;
            }
        }
    }

    /// A leading `.` is one of three things and the byte BEFORE it settles which.
    ///
    /// `.5` is a number, `.a` is extended missing `a`, and the `.` of `L.gnp` or
    /// `i.rep78` is an operator. The discriminator is that a missing literal
    /// never follows a name or a closing bracket: `L.a` has `L` before the dot,
    /// `di .a` has a space. One byte of lookback, no parser feedback.
    fn scan_dot(&mut self, start: usize, glued: bool) -> Tok {
        let after_name = start > 0
            && (is_ident_continue(self.src, prev_char_start(self.src, start))
                || matches!(self.b[start - 1], b')' | b']'));
        if !after_name {
            if matches!(self.b.get(self.i + 1), Some(c) if c.is_ascii_digit()) {
                self.scan_number();
                return self.tok(TokKind::Number, start, glued);
            }
            if let Some(&c @ b'a'..=b'z') = self.b.get(self.i + 1) {
                // `.a` is missing-a only when the letter ENDS the word: `.abc`
                // is not a missing value and must not silently become one.
                let ends = !self
                    .b
                    .get(self.i + 2)
                    .is_some_and(|&n| n == b'_' || n.is_ascii_alphanumeric());
                if ends {
                    self.i += 2;
                    return self.tok(TokKind::MissingLit(c - b'a' + 1), start, glued);
                }
            }
            let bare = !self
                .b
                .get(self.i + 1)
                .is_some_and(|&n| n == b'_' || n.is_ascii_alphanumeric());
            if bare {
                self.i += 1;
                return self.tok(TokKind::MissingLit(0), start, glued);
            }
        }
        self.i += 1;
        self.tok(TokKind::Dot, start, glued)
    }
}

/// Strip the quoting from a [`TokKind::Str`] or [`TokKind::CompoundStr`] token.
///
/// Stata has no escape sequences inside `"…"` — that is what `` `"…"' `` is for
/// — so this is a slice, never an unescaping pass.
pub fn unquote(text: &str) -> &str {
    if let Some(inner) = text.strip_prefix("`\"") {
        return inner.strip_suffix("\"'").unwrap_or(inner);
    }
    if let Some(inner) = text.strip_prefix('"') {
        return inner.strip_suffix('"').unwrap_or(inner);
    }
    text
}

/// Length in bytes of the UTF-8 scalar whose lead byte is `c`.
#[inline]
pub(crate) fn utf8_len(c: u8) -> usize {
    match c {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn prev_char_start(src: &str, mut i: usize) -> usize {
    i -= 1;
    while i > 0 && !src.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// [U] 11.3: a name begins with a letter or `_`.
///
/// "Any Unicode letter" is design 02's OPEN QUESTION Q6 — the manual does not
/// say whether it means `XID_Start`, Unicode `Alphabetic`, or something
/// narrower. `char::is_alphabetic` is Unicode `Alphabetic`, which is the widest
/// of the three: it accepts everything Stata accepts plus possibly a little
/// more. Being generous is the safe direction for a front end whose job is to
/// hand the name to Stata's own resolver, and it keeps a non-English dataset
/// working without pulling `unicode-ident` into a crate whose dependency table
/// is W04's file.
#[inline]
pub fn is_ident_start(src: &str, i: usize) -> bool {
    let b = src.as_bytes()[i];
    if b < 0x80 {
        return b == b'_' || b.is_ascii_alphabetic();
    }
    src[i..].chars().next().is_some_and(char::is_alphabetic)
}

/// [U] 11.3: a name continues with letters, digits or `_`.
#[inline]
pub fn is_ident_continue(src: &str, i: usize) -> bool {
    let b = src.as_bytes()[i];
    if b < 0x80 {
        return b == b'_' || b.is_ascii_alphanumeric();
    }
    src[i..].chars().next().is_some_and(char::is_alphanumeric)
}

/// True when `word` is a legal Stata name ([U] 11.3): 1..=32 characters,
/// leading letter or `_`.
pub fn is_name(word: &str) -> bool {
    if word.is_empty() || word.chars().count() > 32 {
        return false;
    }
    if !is_ident_start(word, 0) {
        return false;
    }
    let mut i = 0;
    while i < word.len() {
        if !is_ident_continue(word, i) {
            return false;
        }
        i += utf8_len(word.as_bytes()[i]);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str, mode: LexMode) -> Vec<TokKind> {
        tokens(src, mode)
            .into_iter()
            .filter(|t| t.kind != TokKind::Eof)
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn dot_disambiguation_is_one_byte_of_lookback() {
        assert_eq!(kinds(".5", LexMode::Expanded), [TokKind::Number]);
        assert_eq!(kinds(".", LexMode::Expanded), [TokKind::MissingLit(0)]);
        assert_eq!(kinds(".a", LexMode::Expanded), [TokKind::MissingLit(1)]);
        assert_eq!(kinds(".z", LexMode::Expanded), [TokKind::MissingLit(26)]);
        // `L.gnp` is a time-series operator, not lag-of-missing.
        assert_eq!(
            kinds("L.gnp", LexMode::Expanded),
            [TokKind::Ident, TokKind::Dot, TokKind::Ident]
        );
        // `.abc` is not a missing value.
        assert_eq!(
            kinds(".abc", LexMode::Expanded),
            [TokKind::Dot, TokKind::Ident]
        );
        assert_eq!(kinds("2.5", LexMode::Expanded), [TokKind::Number]);
    }

    #[test]
    fn compound_quotes_nest() {
        let src = r#"`"a `"b"' c"'"#;
        let t = tokens(src, LexMode::Expanded);
        assert_eq!(t[0].kind, TokKind::CompoundStr);
        assert_eq!(
            t[0].span.end as usize,
            src.len(),
            "must span the whole thing"
        );
        assert_eq!(unquote(t[0].text(src)), r#"a `"b"' c"#);
    }

    #[test]
    fn exponent_needs_digits() {
        // `2exp` is `2` then the function name, not a broken float.
        assert_eq!(
            kinds("2exp", LexMode::Expanded),
            [TokKind::Number, TokKind::Ident]
        );
        assert_eq!(kinds("1e-20", LexMode::Expanded), [TokKind::Number]);
    }

    #[test]
    fn macro_refs_only_lex_in_speculative_mode() {
        assert_eq!(kinds("`x'", LexMode::Speculative), [TokKind::MacroRef]);
        assert_eq!(kinds("$x", LexMode::Speculative), [TokKind::MacroRef]);
        // Expanded text cannot contain them; if it does they are junk, not names.
        assert!(kinds("`x'", LexMode::Expanded).contains(&TokKind::Unknown));
    }

    #[test]
    fn glued_records_adjacency() {
        let t = tokens("a b", LexMode::Expanded);
        assert!(!t[1].glued);
        let t = tokens("a+b", LexMode::Expanded);
        assert!(t[1].glued && t[2].glued);
    }

    #[test]
    fn non_ascii_names_are_accepted_and_never_split_a_scalar() {
        assert!(is_name("año"));
        assert_eq!(kinds("año", LexMode::Expanded), [TokKind::Ident]);
        // A lone continuation byte must not produce a span that panics on slice.
        let src = "é";
        for t in tokens(src, LexMode::Expanded) {
            let _ = &src[t.span.start as usize..t.span.end as usize];
        }
    }
}
