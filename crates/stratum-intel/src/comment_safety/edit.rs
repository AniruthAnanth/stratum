//! Gate 1 — the edit shape cannot express a code change (design 07 §8.1).
//!
//! The guarantee spec §23 needs is not "we prompt the model carefully". It is
//! that **the applier's type system cannot represent a non-comment edit**. There
//! is deliberately no `Replace`, no `Delete`, and no insert-arbitrary-text: the
//! two variants of [`CommentEdit`] each carry a [`CommentLine`], and
//! [`CommentLine::new`] is its only constructor.
//!
//! Every rejection below is a way a comment body could re-enter the grammar:
//!
//! | Body contains | What it would become | Rejection |
//! |---|---|---|
//! | a newline | a second line, which is code | [`Rejected::Multiline`] |
//! | `/*` or `*/` | a block-comment open/close, moving the comment boundary | [`Rejected::ContainsCommentDelimiter`] |
//! | `//` or `///` | a nested comment, or a line continuation that swallows the next statement | [`Rejected::ContainsContinuation`] |
//! | a control character | an unprintable that the editor and the runtime may disagree about | [`Rejected::ControlChar`] |
//! | nothing | a bare `//` that reads as noise | [`Rejected::Empty`] |
//! | more than 240 bytes | a line that wraps, hiding the rest | [`Rejected::TooLong`] |
//!
//! And one rejection that is about *placement* rather than content:
//! [`CommentStyle::Star`] can never be appended to a line, because a `*` in the
//! middle of a Stata statement is **multiplication**, not a comment. That is the
//! exact bug class this gate exists to make unrepresentable, so it is a variant
//! ([`Rejected::TrailingStar`]) and not a comment in the prose.

use core::fmt;

/// Longest comment body accepted. Design 07 §8.1.
pub const MAX_COMMENT_BYTES: usize = 240;

/// Which comment syntax to render.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommentStyle {
    /// `// text`. Legal at the start of a line and after code.
    Slash,
    /// `* text`. Legal ONLY at the start of a statement.
    Star,
}

/// Why a comment body was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rejected {
    /// The body contains a line break (including U+2028 / U+2029).
    Multiline,
    /// The body contains `/*` or `*/`.
    ContainsCommentDelimiter,
    /// The body contains `//` or `///`.
    ContainsContinuation,
    /// The body is empty, or whitespace only.
    Empty,
    /// The body exceeds [`MAX_COMMENT_BYTES`].
    TooLong,
    /// The body contains a control character other than tab.
    ControlChar,
    /// [`CommentStyle::Star`] was asked for as a trailing comment.
    TrailingStar,
}

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Rejected::Multiline => "the comment body contains a line break",
            Rejected::ContainsCommentDelimiter => "the comment body contains `/*` or `*/`",
            Rejected::ContainsContinuation => "the comment body contains `//`",
            Rejected::Empty => "the comment body is empty",
            Rejected::TooLong => "the comment body is longer than 240 bytes",
            Rejected::ControlChar => "the comment body contains a control character",
            Rejected::TrailingStar => {
                "a `*` comment cannot follow code — mid-line, `*` is multiplication"
            }
        };
        f.write_str(s)
    }
}

impl core::error::Error for Rejected {}

/// A validated, fully rendered single-line comment.
///
/// The inner string is the exact text that will be written, indent included.
/// There is no way to construct one except through [`CommentLine::new`] or
/// [`CommentLine::trailing`], and no way to mutate one afterwards.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CommentLine(String);

impl CommentLine {
    /// A comment on its own line, at `indent`.
    ///
    /// `indent` is taken verbatim and must be whitespace; anything else is a
    /// caller bug and is normalised away rather than trusted.
    pub fn new(body: &str, style: CommentStyle, indent: &str) -> Result<CommentLine, Rejected> {
        let body = validate(body)?;
        let indent: String = indent
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        Ok(CommentLine(match style {
            CommentStyle::Slash => format!("{indent}// {body}"),
            CommentStyle::Star => format!("{indent}* {body}"),
        }))
    }

    /// A comment appended after code on an existing line.
    ///
    /// Always two spaces then `// `, and never [`CommentStyle::Star`].
    pub fn trailing(body: &str, style: CommentStyle) -> Result<CommentLine, Rejected> {
        if style == CommentStyle::Star {
            return Err(Rejected::TrailingStar);
        }
        let body = validate(body)?;
        Ok(CommentLine(format!("  // {body}")))
    }

    /// The rendered line.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.0
    }
}

fn validate(body: &str) -> Result<&str, Rejected> {
    if body.len() > MAX_COMMENT_BYTES {
        return Err(Rejected::TooLong);
    }
    if body.contains(['\n', '\r', '\u{2028}', '\u{2029}', '\u{0085}']) {
        return Err(Rejected::Multiline);
    }
    if body.bytes().any(|b| b < 0x20 && b != b'\t') || body.contains('\u{7f}') {
        return Err(Rejected::ControlChar);
    }
    if body.contains("/*") || body.contains("*/") {
        return Err(Rejected::ContainsCommentDelimiter);
    }
    if body.contains("//") {
        return Err(Rejected::ContainsContinuation);
    }
    let body = body.trim();
    if body.is_empty() {
        return Err(Rejected::Empty);
    }
    Ok(body)
}

/// The ONLY operations the auto-comment applier can perform.
///
/// `line` is a 0-based **physical** line index in the buffer being edited.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommentEdit {
    /// Insert `text` as a new line immediately above `line`.
    InsertLineAbove {
        /// Target line.
        line: usize,
        /// The comment.
        text: CommentLine,
    },
    /// Append `text` to the end of `line`.
    AppendTrailing {
        /// Target line.
        line: usize,
        /// The comment.
        text: CommentLine,
    },
}

impl CommentEdit {
    /// The line this edit targets.
    #[must_use]
    pub const fn line(&self) -> usize {
        match self {
            CommentEdit::InsertLineAbove { line, .. }
            | CommentEdit::AppendTrailing { line, .. } => *line,
        }
    }

    /// The rendered comment.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            CommentEdit::InsertLineAbove { text, .. }
            | CommentEdit::AppendTrailing { text, .. } => text.text(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn err(body: &str) -> Rejected {
        CommentLine::new(body, CommentStyle::Slash, "").expect_err("must reject")
    }

    #[test]
    fn a_body_that_could_re_enter_the_grammar_is_refused() {
        assert_eq!(err("two\nlines"), Rejected::Multiline);
        assert_eq!(err("two\u{2028}lines"), Rejected::Multiline);
        assert_eq!(err("open /* here"), Rejected::ContainsCommentDelimiter);
        assert_eq!(err("close */ here"), Rejected::ContainsCommentDelimiter);
        assert_eq!(err("a // b"), Rejected::ContainsContinuation);
        assert_eq!(err("continue ///"), Rejected::ContainsContinuation);
        assert_eq!(err("bell \u{7}"), Rejected::ControlChar);
        assert_eq!(err(""), Rejected::Empty);
        assert_eq!(err("   "), Rejected::Empty);
        assert_eq!(err(&"x".repeat(MAX_COMMENT_BYTES + 1)), Rejected::TooLong);
    }

    #[test]
    fn a_star_comment_can_never_follow_code() {
        assert_eq!(
            CommentLine::trailing("note", CommentStyle::Star),
            Err(Rejected::TrailingStar)
        );
        assert_eq!(
            CommentLine::trailing("note", CommentStyle::Slash)
                .unwrap()
                .text(),
            "  // note"
        );
    }

    #[test]
    fn rendering_keeps_the_indent_and_trims_the_body() {
        let c = CommentLine::new("  padded  ", CommentStyle::Slash, "    ").unwrap();
        assert_eq!(c.text(), "    // padded");
        let c = CommentLine::new("padded", CommentStyle::Star, "\t").unwrap();
        assert_eq!(c.text(), "\t* padded");
    }

    #[test]
    fn a_non_whitespace_indent_is_normalised_away() {
        let c = CommentLine::new("x", CommentStyle::Slash, "  drop _all").unwrap();
        assert_eq!(c.text(), "  // x");
    }

    /// The prompt-injection row of design 07 §8.4's table, at the shape level:
    /// a hostile body naming a destructive command is inert, because the only
    /// thing the type can produce is a comment.
    #[test]
    fn a_hostile_body_becomes_an_inert_comment() {
        let c = CommentLine::new("drop _all", CommentStyle::Slash, "").unwrap();
        assert_eq!(c.text(), "// drop _all");
    }
}
