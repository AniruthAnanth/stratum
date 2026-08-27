//! The canonical token stream and `CodeHash` — CONTRACTS §1.2, normative.
//!
//! `CodeHash` identifies a region for STALENESS. It is deliberately not a hash
//! of the source text: spec §23 promises that reindenting a block, reflowing a
//! `///` chain, or writing a comment above a command does not invalidate a
//! cached result. So the hash input is the region's token stream after:
//!
//! 1. all comments removed (`*`, `//`, `///`, `/* */`, nested) — done already by
//!    the logical-line reader, which is why this module never sees one;
//! 2. `///` continuations resolved, the joined logical line being what is hashed;
//! 3. `#delimit ;` statements normalised, with the delimiter mode in force
//!    folded in as a leading discriminant per statement, so `;`-mode code cannot
//!    collide with the same tokens in `cr` mode;
//! 4. insignificant whitespace runs collapsed to one separator token;
//! 5. string literals, compound `` `"…"' `` literals and macro-reference spans
//!    kept BYTE-EXACT — they carry meaning that whitespace does not.
//!
//! **A known consequence, recorded rather than hidden.** Rule 4 makes
//! `local t 1 ///⏎   2` and `local t 1 2` hash equal, although Stata assigns
//! `"1    2"` and `"1 2"` respectively. Telling those apart needs the parser to
//! know that `local`'s tail is verbatim text, which is W04b's command table, not
//! the scanner's. The contract chose staleness-neutral reflow over that case;
//! the cost is a cached `local` result surviving a reflow that changed its value.

use stratum_proto::{CanonToken, CodeHash, Delimiter, TextHash, TokenKind};

use crate::scan::logical::{DerivedText, LogicalLine};

/// Statement discriminant for `cr` mode (CONTRACTS §1.2 rule 3).
const DISCRIMINANT_CR: &[u8] = b"\x01";
/// Statement discriminant for `;` mode.
const DISCRIMINANT_SEMI: &[u8] = b"\x02";
/// The one separator every insignificant whitespace run collapses to.
const SEPARATOR: &[u8] = b" ";

/// The canonical token stream of a run of logical lines.
///
/// Trivia lines contribute nothing, which is what makes "insert a comment above
/// this command" staleness-neutral.
pub fn canonical_tokens(
    src: &str,
    lines: &[LogicalLine],
    derived: &[DerivedText],
) -> Vec<CanonToken> {
    let mut out = Vec::new();
    for_each_canon_token(src, lines, derived, |kind, text| {
        out.push(CanonToken {
            kind,
            // Every token boundary is a char boundary by construction: the
            // scanner only ever splits on ASCII punctuation and on the ends of
            // whole identifiers.
            text: String::from_utf8_lossy(text).into_owned(),
        });
    });
    out
}

/// `CodeHash` of a run of logical lines: blake3, first 16 bytes, over the
/// canonical token stream encoded exactly as CONTRACTS §1.2 rule 6 specifies.
pub fn code_hash(src: &str, lines: &[LogicalLine], derived: &[DerivedText]) -> CodeHash {
    let mut buf = Vec::with_capacity(256);
    code_hash_into(src, lines, derived, &mut buf)
}

/// [`code_hash`] with a caller-owned scratch buffer.
///
/// This exists for a measured reason. `blake3::Hasher::update` costs on the
/// order of 15 ns per call regardless of how few bytes it is given, and rule 6
/// specifies three calls PER TOKEN; at ~30 tokens a region that is ~1.4 µs of
/// pure call overhead, which over the ~19 000 regions of a 1 MB do-file was two
/// thirds of the whole segmentation pass. Encoding into a buffer and hashing it
/// once is the same bytes in the same order — the hash is unchanged — for about
/// a tenth of the time.
pub fn code_hash_into(
    src: &str,
    lines: &[LogicalLine],
    derived: &[DerivedText],
    buf: &mut Vec<u8>,
) -> CodeHash {
    buf.clear();
    for_each_canon_token(src, lines, derived, |kind, text| {
        // CONTRACTS §1.2 rule 6, verbatim. The length prefix is what stops
        // `["ab", "c"]` and `["a", "bc"]` from hashing alike.
        let n = text.len() as u32;
        let header = [
            kind as u8,
            n as u8,
            (n >> 8) as u8,
            (n >> 16) as u8,
            (n >> 24) as u8,
        ];
        buf.extend_from_slice(&header);
        buf.extend_from_slice(text);
    });
    let mut out = [0u8; 16];
    out.copy_from_slice(&blake3::hash(buf).as_bytes()[..16]);
    CodeHash(out)
}

/// blake3-128 over the raw UTF-8 bytes INCLUDING comments (CONTRACTS §1.1).
///
/// UI only — "the file changed on disk". Never used for staleness, and
/// deliberately NOT a field of `Segmentation`: it costs a full pass over the
/// buffer, and re-hashing 2 MB on every keystroke to answer a question the
/// keystroke path never asks is exactly the kind of cost that has to be paid on
/// purpose.
pub fn text_hash(src: &str) -> TextHash {
    let mut out = [0u8; 16];
    out.copy_from_slice(&blake3::hash(src.as_bytes()).as_bytes()[..16]);
    TextHash(out)
}

/// Drive `f` over the canonical token stream without allocating a `Vec`.
///
/// Tokens are handed over as BYTES, not `&str`. Slicing a `&str` validates two
/// char boundaries per slice, and at fifteen tokens a region over ~19 000
/// regions that showed up as a measurable fraction of the pass for a check that
/// can never fail here — the tokenizer only ever splits on ASCII punctuation and
/// at the end of a whole identifier.
pub fn for_each_canon_token<F: FnMut(TokenKind, &[u8])>(
    src: &str,
    lines: &[LogicalLine],
    derived: &[DerivedText],
    mut f: F,
) {
    for (line, d) in lines.iter().zip(derived) {
        if line.is_trivia {
            continue;
        }
        f(
            TokenKind::StatementBreak,
            match line.entry_delimiter {
                Delimiter::Cr => DISCRIMINANT_CR,
                Delimiter::Semi => DISCRIMINANT_SEMI,
            },
        );
        tokenize(line.code(src, d.as_deref()), &mut f);
    }
}

/// Split one logical line's code into canonical tokens.
///
/// The input is already comment-free and continuation-spliced, so this is a flat
/// scan with no state beyond the current token. It is NOT the execution lexer:
/// W04b's `lex/` produces `Token` with spans for the parser and the editor's
/// syntax highlighting, and should be built on this scan rather than beside it —
/// CONTRACTS §13 requires `ProgramIndex::lex` to be the exact code the runtime
/// executes with, and two token splitters is how that promise quietly breaks.
fn tokenize<F: FnMut(TokenKind, &[u8])>(code: &str, f: &mut F) {
    let b = code.as_bytes();
    let n = b.len();
    let mut i = 0usize;
    let mut at_stmt_start = true;

    while i < n {
        let c = b[i];

        if c.is_ascii_whitespace() {
            while i < n && b[i].is_ascii_whitespace() {
                i += 1;
            }
            f(TokenKind::Whitespace, SEPARATOR);
            continue;
        }

        // `#delimit` and friends. Only at statement start, so `a#b` in a factor
        // variable is never mistaken for a directive.
        if c == b'#' && at_stmt_start {
            let s = i;
            i += 1;
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            f(TokenKind::Directive, &b[s..i]);
            at_stmt_start = false;
            continue;
        }
        at_stmt_start = false;

        if c == b'"' {
            let s = i;
            i += 1;
            while i < n && b[i] != b'"' {
                i += 1;
            }
            if i < n {
                i += 1;
            }
            f(TokenKind::StrLit, &b[s..i]);
            continue;
        }

        if c == b'`' && i + 1 < n && b[i + 1] == b'"' {
            let s = i;
            let mut depth = 1u32;
            i += 2;
            while i < n && depth > 0 {
                if b[i] == b'`' && i + 1 < n && b[i + 1] == b'"' {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'"' && i + 1 < n && b[i + 1] == b'\'' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            f(TokenKind::CompoundQuote, &b[s..i]);
            continue;
        }

        if c == b'`' {
            let s = i;
            let mut depth = 1u32;
            i += 1;
            while i < n && depth > 0 {
                match b[i] {
                    b'`' => depth += 1,
                    b'\'' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            f(TokenKind::MacroRef, &b[s..i]);
            continue;
        }

        if c == b'$' {
            let s = i;
            i += 1;
            if i < n && b[i] == b'{' {
                while i < n && b[i] != b'}' {
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
            } else {
                // Maximal munch: `$G1x` reads the macro `G1x` (02 §4.2).
                while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
            }
            f(TokenKind::MacroRef, &b[s..i]);
            continue;
        }

        if c.is_ascii_digit() || (c == b'.' && i + 1 < n && b[i + 1].is_ascii_digit()) {
            let s = i;
            while i < n && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            if i < n && (b[i] == b'e' || b[i] == b'E') {
                let save = i;
                i += 1;
                if i < n && (b[i] == b'+' || b[i] == b'-') {
                    i += 1;
                }
                if i < n && b[i].is_ascii_digit() {
                    while i < n && b[i].is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    i = save;
                }
            }
            f(TokenKind::Number, &b[s..i]);
            continue;
        }

        if c == b'_' || c.is_ascii_alphabetic() || c >= 0x80 {
            let s = i;
            i += ident_len(b, i, code);
            if i == s {
                // A non-ASCII byte that is not alphanumeric: emit the whole char.
                i += char_len(&code[s..]);
                f(TokenKind::Unknown, &b[s..i]);
                continue;
            }
            f(TokenKind::Ident, &b[s..i]);
            continue;
        }

        let (kind, len) = match c {
            b',' => (TokenKind::Comma, 1),
            b':' => (TokenKind::Colon, 1),
            b'(' => (TokenKind::LParen, 1),
            b')' => (TokenKind::RParen, 1),
            b'{' => (TokenKind::LBrace, 1),
            b'}' => (TokenKind::RBrace, 1),
            b'[' => (TokenKind::LBracket, 1),
            b']' => (TokenKind::RBracket, 1),
            _ => (TokenKind::Op, op_len(&b[i..])),
        };
        if len == 0 {
            let l = char_len(&code[i..]);
            f(TokenKind::Unknown, &b[i..i + l]);
            i += l;
            continue;
        }
        f(kind, &b[i..i + len]);
        i += len;
    }
}

/// Length of the identifier starting at the front of `s`, or 0.
///
/// The ASCII loop is not premature: every identifier in a do-file goes through
/// here, and `char_indices` decoding each one cost about a third of the whole
/// canonical pass. Non-ASCII falls back to the general rule, because Stata 14+
/// allows Unicode variable names.
fn ident_len(b: &[u8], at: usize, code: &str) -> usize {
    if at >= b.len() {
        return 0;
    }
    if !(b[at] == b'_' || b[at].is_ascii_alphabetic()) {
        if b[at] >= 0x80 {
            return ident_len_unicode(&code[at..]);
        }
        return 0;
    }
    let mut i = at + 1;
    while i < b.len() && (b[i] == b'_' || b[i].is_ascii_alphanumeric()) {
        i += 1;
    }
    if i < b.len() && b[i] >= 0x80 {
        return (i - at) + ident_len_unicode_tail(&code[i..]);
    }
    i - at
}

fn ident_len_unicode(s: &str) -> usize {
    let Some(ch) = s.chars().next() else { return 0 };
    if !(ch == '_' || ch.is_alphabetic()) {
        return 0;
    }
    ch.len_utf8() + ident_len_unicode_tail(&s[ch.len_utf8()..])
}

fn ident_len_unicode_tail(s: &str) -> usize {
    let mut end = 0;
    for (off, ch) in s.char_indices() {
        if !(ch == '_' || ch.is_alphanumeric()) {
            break;
        }
        end = off + ch.len_utf8();
    }
    end
}

/// Byte length of the first character of `s`. `s` is never empty at a call site.
fn char_len(s: &str) -> usize {
    s.chars().next().map_or(1, char::len_utf8)
}

/// Length of the operator at the front of `b`, longest match first, or 0 when
/// the byte is not an operator character at all.
fn op_len(b: &[u8]) -> usize {
    if b.len() >= 2 {
        let two = [b[0], b[1]];
        if matches!(
            &two,
            b"==" | b"!=" | b"~=" | b"<=" | b">=" | b"++" | b"--" | b"->" | b"//"
        ) {
            return 2;
        }
    }
    match b[0] {
        b'+' | b'-' | b'*' | b'/' | b'^' | b'=' | b'<' | b'>' | b'!' | b'~' | b'&' | b'|'
        | b'.' | b'\'' | b'\\' | b'#' | b'$' | b'@' | b'?' | b';' | b'%' => 1,
        _ => 0,
    }
}
