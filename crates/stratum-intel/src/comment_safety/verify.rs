//! Gate 3 — token-stream equivalence (design 07 §8.3). **The actual guarantee.**
//!
//! Three checks. **All three must pass**, and they are independent code paths on
//! purpose: a bug in one must not be able to let a semantic change through.
//!
//! * **Check A — significant token stream.** Lex both buffers with the lexer the
//!   runtime executes with ([`crate::ProgramIndex::lex`]), drop comments and
//!   whitespace, and map a newline to a `StatementBreak` **iff the runtime's own
//!   statement splitter treats it as a terminator**. That last clause is the
//!   subtle one: newlines are semantically load-bearing in Stata, so erasing
//!   them all would let us "prove" that joining two statements is safe. We do
//!   not decide when a newline terminates — the scanner's logical-line
//!   segmentation already did, and a `///` chain or a `#delimit ;` body is one
//!   logical line, so it contributes one break.
//! * **Check B — statement partition.** Independently of the lexer projection,
//!   run the splitter on both buffers and require the same statement count and
//!   the same normalised text per index. This is the check that catches the
//!   nastiest real failure: a `// note` inserted into the middle of a `///`
//!   chain terminates the statement early, the statement count changes, and B
//!   fires even if A's newline handling had a bug.
//! * **Check C — byte histogram.** A 256-entry count over the non-whitespace
//!   bytes of both buffers with comments stripped. One pass, no allocation
//!   beyond the arrays, and it catches any case where A and B were both fooled
//!   by a compensating pair of changes.
//!
//! # Why the independent stripper is safe
//!
//! Checks B and C need "the same text with comments removed", and they use
//! [`strip_comments`] — written here rather than taken from the scanner,
//! because sharing the scanner's answer would make B and C dependent on A.
//! A bug in [`strip_comments`] can only ever produce a **false rejection**: it
//! is applied identically to both buffers, and the edit under test only *adds*
//! comment text, so anything the stripper fails to recognise as a comment shows
//! up as surplus bytes on the `after` side and the patch is refused. There is no
//! stripper bug that turns a semantic change into a pass.
//!
//! On any failure the caller must **reject the entire patch**, apply nothing,
//! and tell the user. Design 07 §8.3: "Silence on a failed safety check is not
//! an option; this is the one place the product should be loud."

// The scanners below are flat byte loops whose every index is guarded by the
// enclosing `i < n`. This module is the INDEPENDENT second implementation Gate 3
// weighs against the runtime's own scanner, so it is written as one obvious pass
// with the bound in the loop header; threading `.get()` through it would obscure
// the only thing a reader of a safety gate needs to be able to check.
#![allow(clippy::indexing_slicing)]

use core::fmt;

use stratum_proto::token::TokenKind;

use crate::ProgramIndex;

/// A refusal from Gate 3, naming which check fired and on what.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SemanticDrift {
    /// Check A: the significant token streams differ.
    TokenStream {
        /// Index of the first differing token.
        at: usize,
        /// What the original had there, or `None` past the end.
        before: Option<String>,
        /// What the candidate has there.
        after: Option<String>,
    },
    /// Check B: a different number of statements.
    StatementCount {
        /// Statements in the original.
        before: usize,
        /// Statements in the candidate.
        after: usize,
    },
    /// Check B: statement `index` reads differently.
    StatementText {
        /// Which statement.
        index: usize,
        /// Normalised original.
        before: String,
        /// Normalised candidate.
        after: String,
    },
    /// Check C: the code-byte histograms differ.
    ByteHistogram {
        /// The byte whose count moved.
        byte: u8,
        /// Occurrences in the original.
        before: u32,
        /// Occurrences in the candidate.
        after: u32,
    },
}

impl fmt::Display for SemanticDrift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SemanticDrift::TokenStream { at, before, after } => write!(
                f,
                "the executed token stream changed at token {at}: {} became {}",
                before.as_deref().unwrap_or("<end of file>"),
                after.as_deref().unwrap_or("<end of file>")
            ),
            SemanticDrift::StatementCount { before, after } => write!(
                f,
                "the file went from {before} statement(s) to {after} — a comment cannot do that"
            ),
            SemanticDrift::StatementText {
                index,
                before,
                after,
            } => write!(f, "statement {index} changed: {before:?} became {after:?}"),
            SemanticDrift::ByteHistogram {
                byte,
                before,
                after,
            } => write!(
                f,
                "the code-byte histogram changed: {:?} appears {before} time(s) before and \
                 {after} time(s) after",
                char::from(*byte)
            ),
        }
    }
}

impl core::error::Error for SemanticDrift {}

/// Prove that `after` differs from `before` only in comments and whitespace.
///
/// `Ok(())` means the runtime executes exactly the same program.
pub fn assert_comment_only(
    before: &str,
    after: &str,
    idx: &dyn ProgramIndex,
) -> Result<(), SemanticDrift> {
    check_a(before, after, idx)?;
    check_b(before, after, idx)?;
    check_c(before, after)
}

// ---------------------------------------------------------------------------
// Check A
// ---------------------------------------------------------------------------

/// One entry of the significance signature: `(kind, text)`.
type Sig = Vec<(TokenKind, String)>;

/// Project a buffer onto its significant token stream.
///
/// Public because `tests/comment_safety.rs` asserts the projection's own
/// properties — that it drops comments, that it keeps statement breaks, and
/// that it distinguishes the two delimiter modes.
#[must_use]
pub fn signature(src: &str, idx: &dyn ProgramIndex) -> Sig {
    let mut out: Sig = Vec::new();
    let mut first = true;
    for stmt in idx.split_statements(src) {
        // The scanner already decided where a statement ends, so a break here is
        // the runtime's own answer and not ours.
        if !first {
            out.push((TokenKind::StatementBreak, String::new()));
        }
        first = false;
        let code = src
            .get(stmt.start as usize..stmt.end as usize)
            .unwrap_or("");
        // `split_statements` returns first-code-byte..last-code-byte, which can
        // still contain an INTERIOR comment (`gen x = 1 /* why */ + 2`). Those
        // bytes are not code and must not reach the lexer.
        let code = strip_comments(code);
        for tok in idx.lex(&code) {
            if matches!(tok.kind, TokenKind::Comment | TokenKind::Whitespace) {
                continue;
            }
            let text = code
                .get(tok.span.start as usize..tok.span.end as usize)
                .unwrap_or("")
                .to_owned();
            out.push((tok.kind, text));
        }
    }
    out
}

fn check_a(before: &str, after: &str, idx: &dyn ProgramIndex) -> Result<(), SemanticDrift> {
    let a = signature(before, idx);
    let b = signature(after, idx);
    if a == b {
        return Ok(());
    }
    let at = a
        .iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.len().min(b.len()));
    Err(SemanticDrift::TokenStream {
        at,
        before: a.get(at).map(|t| format!("{:?} {:?}", t.0, t.1)),
        after: b.get(at).map(|t| format!("{:?} {:?}", t.0, t.1)),
    })
}

// ---------------------------------------------------------------------------
// Check B
// ---------------------------------------------------------------------------

fn check_b(before: &str, after: &str, idx: &dyn ProgramIndex) -> Result<(), SemanticDrift> {
    let a = idx.split_statements(before);
    let b = idx.split_statements(after);
    if a.len() != b.len() {
        return Err(SemanticDrift::StatementCount {
            before: a.len(),
            after: b.len(),
        });
    }
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        let sx = normalize(before.get(x.start as usize..x.end as usize).unwrap_or(""));
        let sy = normalize(after.get(y.start as usize..y.end as usize).unwrap_or(""));
        if sx != sy {
            return Err(SemanticDrift::StatementText {
                index: i,
                before: sx,
                after: sy,
            });
        }
    }
    Ok(())
}

/// Comments stripped, every run of whitespace collapsed to one space, trimmed.
#[must_use]
pub fn normalize(s: &str) -> String {
    let stripped = strip_comments(s);
    let mut out = String::with_capacity(stripped.len());
    let mut in_ws = false;
    for c in stripped.chars() {
        if c.is_whitespace() {
            in_ws = true;
            continue;
        }
        if in_ws && !out.is_empty() {
            out.push(' ');
        }
        in_ws = false;
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// Check C
// ---------------------------------------------------------------------------

fn check_c(before: &str, after: &str) -> Result<(), SemanticDrift> {
    let a = histogram(before);
    let b = histogram(after);
    for i in 0..256 {
        // `a` and `b` are fixed 256-element arrays, so both indices are in
        // range; the pattern match keeps that a fact rather than an assertion.
        let (Some(x), Some(y)) = (a.get(i), b.get(i)) else {
            continue;
        };
        if x != y {
            return Err(SemanticDrift::ByteHistogram {
                byte: i as u8,
                before: *x,
                after: *y,
            });
        }
    }
    Ok(())
}

/// Counts of every non-whitespace byte, comments removed.
#[must_use]
pub fn histogram(src: &str) -> [u32; 256] {
    let mut counts = [0u32; 256];
    for b in strip_comments(src).bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if let Some(slot) = counts.get_mut(b as usize) {
            *slot += 1;
        }
    }
    counts
}

// ---------------------------------------------------------------------------
// The independent comment stripper
// ---------------------------------------------------------------------------

/// Remove Stata comments, preserving every other byte and every line break.
///
/// The four forms of design 02 §2:
/// * `* …` — only when it is the first non-blank text of a statement;
/// * `// …` — only when preceded by whitespace or at the start of a line
///   ([U] 16.1.2: `x//y` is not a comment);
/// * `/* … */` — nestable, spanning lines;
/// * `/// …` — a continuation: the `///`, the rest of the line and the newline
///   all go, which is what makes the following line part of this statement.
///
/// String literals and compound quotes are skipped whole, so `di "http://x"`
/// keeps its URL.
///
/// A newline is emitted wherever one was consumed, so byte offsets shift but
/// line structure does not — which is what makes [`normalize`]'s output
/// comparable between two buffers whose comments differ.
#[must_use]
pub fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0usize;
    let mut at_stmt_start = true;
    let mut block_depth = 0u32;

    while i < n {
        let c = b[i];

        if block_depth > 0 {
            if c == b'*' && b.get(i + 1) == Some(&b'/') {
                block_depth -= 1;
                i += 2;
                continue;
            }
            if c == b'/' && b.get(i + 1) == Some(&b'*') {
                block_depth += 1;
                i += 2;
                continue;
            }
            if c == b'\n' {
                out.push('\n');
            }
            i += 1;
            continue;
        }

        if c == b'\n' {
            out.push('\n');
            at_stmt_start = true;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            out.push(char::from(c));
            i += 1;
            continue;
        }

        // `*` comment: only at the start of a statement.
        if c == b'*' && at_stmt_start {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // `/*`, `///`, `//`.
        if c == b'/' {
            if b.get(i + 1) == Some(&b'*') {
                block_depth = 1;
                i += 2;
                at_stmt_start = false;
                continue;
            }
            if b.get(i + 1) == Some(&b'/') {
                let triple = b.get(i + 2) == Some(&b'/');
                // `//` needs whitespace before it, or the start of a line.
                let preceded = i == 0
                    || b.get(i - 1)
                        .is_some_and(|p| p.is_ascii_whitespace() || *p == b'\n');
                if triple || preceded {
                    while i < n && b[i] != b'\n' {
                        i += 1;
                    }
                    if triple {
                        // A continuation swallows the newline: the next physical
                        // line is part of this statement. Nothing is emitted in
                        // its place — 02 §2.1, verified against StataMP 18.5,
                        // splices with NO inserted separator, so `local t 1 ///`
                        // over `   2` is `1` plus four spaces plus `2`. An
                        // invented space here would make this stripper disagree
                        // with the scanner about the language, which is the one
                        // thing a second implementation must never do.
                        if i < n {
                            i += 1;
                        }
                        at_stmt_start = false;
                        continue;
                    }
                    continue;
                }
            }
        }

        // Strings and compound quotes pass through untouched.
        if c == b'"' {
            out.push('"');
            i += 1;
            while i < n && b[i] != b'"' && b[i] != b'\n' {
                out.push(char::from(b[i]));
                i += 1;
            }
            if i < n && b[i] == b'"' {
                out.push('"');
                i += 1;
            }
            at_stmt_start = false;
            continue;
        }
        if c == b'`' && b.get(i + 1) == Some(&b'"') {
            let mut depth = 1u32;
            out.push_str("`\"");
            i += 2;
            while i < n && depth > 0 {
                if b[i] == b'`' && b.get(i + 1) == Some(&b'"') {
                    depth += 1;
                    out.push_str("`\"");
                    i += 2;
                } else if b[i] == b'"' && b.get(i + 1) == Some(&b'\'') {
                    depth -= 1;
                    out.push_str("\"'");
                    i += 2;
                } else {
                    out.push(char::from(b[i]));
                    i += 1;
                }
            }
            at_stmt_start = false;
            continue;
        }

        if c == b';' {
            // A `;` ends a statement in `#delimit ;` mode, so a `*` after it is
            // a comment again. Treating it as a statement start in `cr` mode
            // too is harmless: a bare `;` is not legal Stata there.
            out.push(';');
            at_stmt_start = true;
            i += 1;
            continue;
        }

        out.push(char::from(c));
        at_stmt_start = false;
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::ParseIndex;

    fn gate(before: &str, after: &str) -> Result<(), SemanticDrift> {
        let idx = ParseIndex::new(before);
        assert_comment_only(before, after, &idx)
    }

    #[test]
    fn adding_a_comment_line_passes() {
        let before = "sysuse auto, clear\nregress price mpg\n";
        let after = "sysuse auto, clear\n// fit the model\nregress price mpg\n";
        assert_eq!(gate(before, after), Ok(()));
    }

    #[test]
    fn adding_a_trailing_comment_passes() {
        let before = "regress price mpg\n";
        let after = "regress price mpg  // fit the model\n";
        assert_eq!(gate(before, after), Ok(()));
    }

    #[test]
    fn changing_one_identifier_is_caught_by_check_a() {
        let before = "regress price mpg\n";
        let after = "regress price weight\n";
        assert!(matches!(
            gate(before, after),
            Err(SemanticDrift::TokenStream { .. })
        ));
    }

    #[test]
    fn splitting_a_continuation_chain_is_caught() {
        let before = "regress price ///\n    mpg weight\n";
        // A comment inserted between the `///` and its continuation terminates
        // the statement early. Design 07 §8.4's third row.
        let after = "regress price ///\n// note\n    mpg weight\n";
        assert!(gate(before, after).is_err(), "must be refused");
    }

    #[test]
    fn a_deleted_statement_is_caught_by_check_b() {
        let before = "sysuse auto, clear\ndrop if mpg < 15\nregress price mpg\n";
        let after = "sysuse auto, clear\n// drop if mpg < 15\nregress price mpg\n";
        assert!(matches!(
            gate(before, after),
            Err(SemanticDrift::StatementCount { .. } | SemanticDrift::TokenStream { .. })
        ));
    }

    #[test]
    fn a_new_statement_is_caught() {
        let before = "regress price mpg\n";
        let after = "regress price mpg\ndrop _all\n";
        assert!(gate(before, after).is_err());
    }

    #[test]
    fn a_star_appended_mid_line_changes_the_tokens() {
        let before = "gen x = a\n";
        // What Gate 1 makes unrepresentable, spelled out at the buffer level:
        // `*` after code is multiplication.
        let after = "gen x = a * note\n";
        assert!(gate(before, after).is_err());
    }

    #[test]
    fn the_stripper_leaves_a_url_inside_a_string_alone() {
        assert_eq!(
            strip_comments("di \"see http://x/y\"\n"),
            "di \"see http://x/y\"\n"
        );
        assert_eq!(strip_comments("di 1 // note\n"), "di 1 \n");
        assert_eq!(strip_comments("* whole line\ndi 1\n"), "\ndi 1\n");
        assert_eq!(strip_comments("di /* mid */ 1\n"), "di  1\n");
        assert_eq!(strip_comments("di 1 ///\n + 2\n"), "di 1  + 2\n");
    }

    #[test]
    fn the_stripper_does_not_treat_a_division_as_a_comment() {
        // [U] 16.1.2: `//` is a comment only after whitespace or at line start.
        assert_eq!(strip_comments("gen r = a//b\n"), "gen r = a//b\n");
    }

    #[test]
    fn nested_block_comments_close_at_the_right_depth() {
        assert_eq!(strip_comments("a /* x /* y */ z */ b\n"), "a  b\n");
    }

    #[test]
    fn normalize_collapses_whitespace_and_drops_comments() {
        assert_eq!(
            normalize("regress   price    mpg // fit"),
            "regress price mpg"
        );
    }

    #[test]
    fn the_histogram_ignores_whitespace_and_comments() {
        assert_eq!(histogram("a b\n"), histogram("a    b   // note\n"));
        assert_ne!(histogram("a b\n"), histogram("a c\n"));
    }
}
