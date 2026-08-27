//! CONTRACTS.md §1.2 — the canonical token stream.
//!
//! `CodeHash` is computed over `stratum_parse::canonical_tokens(&Region)`, whose
//! normative definition is in §1.2: comments removed, `///` continuations
//! resolved, `#delimit ;` regions normalised with a per-statement discriminant
//! byte, insignificant whitespace collapsed, and string / compound-quote /
//! macro-reference spans hashed byte-exact.
//!
//! **AMENDED (A29).** `Token` and `CanonToken` were named in §13 but declared
//! nowhere, blocking W20 and W11b. They are declared here, in proto, because
//! `ProgramIndex::lex` returns them across a crate boundary and they are plain
//! data. `CommandAst`, `CommandTable` and `CommandSig` stay in `stratum-parse`:
//! they are the parser's product and have lifetimes.

use serde::{Deserialize, Serialize};

use crate::ids::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    Ident,
    Number,
    StrLit,
    CompoundQuote,
    MacroRef,
    Op,
    Comma,
    Colon,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comment,
    Whitespace,
    StatementBreak,
    Continuation,
    Directive,
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// The comment-free, delimiter-normalised token used for `CodeHash`. `text` is
/// borrowed from the source in the engine; the owned form crosses the wasm
/// boundary only in test fixtures.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CanonToken {
    pub kind: TokenKind,
    pub text: String,
}
