//! Gate 2 — context legality (design 07 §8.2).
//!
//! An insertion is **refused, counted and reported** — never silently dropped
//! and never partially applied — when the target position is somewhere a
//! comment either would not be a comment, or would move a statement boundary.
//!
//! | Position | Why it is refused |
//! |---|---|
//! | inside a `/* … */` block | the text is already inside a comment; a `//` there does nothing, and a `*/` in it would close the block early |
//! | inside a string literal or a `` `" … "' `` compound quote | the bytes are data, not program text |
//! | inside `input … end`, `mata … end`, `python:`/`java:` | those bodies are not Stata commands, and `//` may not be a comment there at all |
//! | between the physical lines of a `///` chain | the comment terminates the statement early — the single nastiest failure in this design |
//! | any line ending in `///`, for a trailing comment | same, one keystroke smaller |
//! | inside a `#delimit ;` region | a `//` line inside an unterminated `;`-statement is technically legal and is exactly where a subtle break would hide. **Decision: refuse.** Comments for such a statement go above the whole statement |
//!
//! Refusing is cheap and recoverable; the user is told "3 of 18 comments could
//! not be safely placed" and nothing about their file changed.

use core::fmt;

use stratum_parse::Segmentation;
use stratum_proto::block::Delimiter;

use super::edit::CommentEdit;
use crate::{spans_contain, ParseIndex};

/// Why one comment could not be placed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The anchor line does not exist in the buffer.
    LineOutOfRange,
    /// Inside a block comment, a string literal, or an `input`/`mata` body.
    InsideVerbatim,
    /// Between the physical lines of a `///` continuation chain.
    InsideContinuationChain,
    /// A trailing comment on a line that ends with `///`.
    TrailingOnContinuation,
    /// Inside a `#delimit ;` region.
    InsideDelimitSemi,
    /// A trailing comment on a line that carries no code.
    NotACodeLine,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Refusal::LineOutOfRange => "that line does not exist",
            Refusal::InsideVerbatim => {
                "that position is inside a comment, a string, or a verbatim block"
            }
            Refusal::InsideContinuationChain => {
                "that position is inside a `///` continuation, where a comment would end the \
                 statement early"
            }
            Refusal::TrailingOnContinuation => "that line ends with `///`",
            Refusal::InsideDelimitSemi => "that position is inside a `#delimit ;` region",
            Refusal::NotACodeLine => "that line carries no code",
        };
        f.write_str(s)
    }
}

impl core::error::Error for Refusal {}

/// Everything Gate 2 needs, computed once for a buffer.
///
/// Built once per patch rather than once per comment: a file-scope auto-comment
/// is thirty anchors in one request (design 07 §8.6), and recomputing the
/// verbatim cover thirty times would be thirty segmentations of the file.
pub struct Legality {
    verbatim: Vec<stratum_proto::Span>,
    chains: Vec<stratum_proto::Span>,
    semi: Vec<stratum_proto::Span>,
    line_count: usize,
}

impl Legality {
    /// Compute the cover for `idx`'s buffer.
    #[must_use]
    pub fn new(idx: &ParseIndex<'_>) -> Self {
        use crate::ProgramIndex;
        let src = idx.source();
        let seg = idx.segmentation();
        Legality {
            verbatim: idx.verbatim_regions(src),
            chains: idx.continuation_chains(src),
            semi: semi_regions(seg),
            line_count: seg.line_index.line_count() as usize,
        }
    }

    /// Where this edit would write, or why it cannot.
    ///
    /// The returned offset is the byte the comment is inserted at:
    /// the start of the anchor line for `InsertLineAbove`, the end of its code
    /// for `AppendTrailing`.
    pub fn resolve(&self, idx: &ParseIndex<'_>, edit: &CommentEdit) -> Result<u32, Refusal> {
        let line = edit.line();
        if line >= self.line_count {
            return Err(Refusal::LineOutOfRange);
        }
        let li = &idx.segmentation().line_index;
        let start = li.line_start(line as u32);

        match edit {
            CommentEdit::InsertLineAbove { .. } => {
                self.check_position(start)?;
                // A chain that *starts* at this line may be commented above; one
                // that merely covers it may not.
                if let Some(c) = self.chain_covering(start) {
                    if c.start != start {
                        return Err(Refusal::InsideContinuationChain);
                    }
                }
                Ok(start)
            }
            CommentEdit::AppendTrailing { .. } => {
                // The `///` test comes first because it is the only one of these
                // that is true of the line as the user sees it. `code_end_of_line`
                // answers "does a logical line's code END here", and a middle
                // line of a chain ends no logical line — so asking it first
                // reported `NotACodeLine` for `regress price ///`, a line that is
                // nothing but code.
                let text = line_text(idx, line as u32);
                if text.trim_end().ends_with("///") {
                    return Err(Refusal::TrailingOnContinuation);
                }
                let (code_end, is_code) = code_end_of_line(idx, line as u32);
                if !is_code {
                    return Err(Refusal::NotACodeLine);
                }
                self.check_position(start)?;
                self.check_position(code_end.saturating_sub(1))?;
                if let Some(c) = self.chain_covering(start) {
                    // Only the last physical line of a chain may take a trailing
                    // comment, and only because that line ends the statement. A
                    // chain's span runs to the end of the last line it covers, so
                    // the question is whether it reaches past THIS line; testing
                    // `code_end < c.end` instead refused the final line too, over
                    // nothing but the newline between the two.
                    if c.end > line_end_of(idx, line as u32) {
                        return Err(Refusal::InsideContinuationChain);
                    }
                }
                Ok(code_end)
            }
        }
    }

    fn check_position(&self, pos: u32) -> Result<(), Refusal> {
        if spans_contain(&self.verbatim, pos) {
            return Err(Refusal::InsideVerbatim);
        }
        if spans_contain(&self.semi, pos) {
            return Err(Refusal::InsideDelimitSemi);
        }
        Ok(())
    }

    fn chain_covering(&self, pos: u32) -> Option<stratum_proto::Span> {
        self.chains
            .iter()
            .copied()
            .find(|c| pos >= c.start && pos < c.end)
    }
}

/// Regions scanned in `#delimit ;` mode.
fn semi_regions(seg: &Segmentation<'_>) -> Vec<stratum_proto::Span> {
    seg.regions
        .iter()
        .filter(|r| r.entry_delimiter == Delimiter::Semi)
        .map(|r| r.outer_span)
        .collect()
}

/// `(end of the last code byte on this physical line, the line has code)`.
fn code_end_of_line(idx: &ParseIndex<'_>, line: u32) -> (u32, bool) {
    let seg = idx.segmentation();
    for l in &seg.lines {
        if l.is_trivia {
            continue;
        }
        if l.code_last_line == line && l.code_span.end > l.code_span.start {
            return (l.code_span.end, true);
        }
    }
    (seg.line_index.line_start(line), false)
}

/// One past the physical line's last byte, terminator INCLUDED.
fn line_end_of(idx: &ParseIndex<'_>, line: u32) -> u32 {
    let li = &idx.segmentation().line_index;
    if line + 1 < li.line_count() {
        li.line_start(line + 1)
    } else {
        idx.source().len() as u32
    }
}

/// The physical line's text, terminator excluded.
fn line_text<'a>(idx: &ParseIndex<'a>, line: u32) -> &'a str {
    let start = idx.segmentation().line_index.line_start(line) as usize;
    let end = line_end_of(idx, line) as usize;
    idx.source()
        .get(start..end)
        .unwrap_or("")
        .trim_end_matches(['\n', '\r'])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::comment_safety::edit::{CommentLine, CommentStyle};

    fn above(line: usize) -> CommentEdit {
        CommentEdit::InsertLineAbove {
            line,
            text: CommentLine::new("note", CommentStyle::Slash, "").unwrap(),
        }
    }

    fn trailing(line: usize) -> CommentEdit {
        CommentEdit::AppendTrailing {
            line,
            text: CommentLine::trailing("note", CommentStyle::Slash).unwrap(),
        }
    }

    fn resolve(src: &str, edit: &CommentEdit) -> Result<u32, Refusal> {
        let idx = ParseIndex::new(src);
        Legality::new(&idx).resolve(&idx, edit)
    }

    #[test]
    fn a_plain_line_accepts_both_forms() {
        let src = "sysuse auto, clear\nregress price mpg\n";
        assert!(resolve(src, &above(1)).is_ok());
        assert!(resolve(src, &trailing(1)).is_ok());
    }

    #[test]
    fn a_line_past_the_end_is_refused() {
        assert_eq!(resolve("di 1\n", &above(99)), Err(Refusal::LineOutOfRange));
    }

    #[test]
    fn the_middle_of_a_continuation_chain_is_refused_both_ways() {
        let src = "regress price ///\n    mpg ///\n    weight\n";
        assert!(resolve(src, &above(0)).is_ok(), "above the chain is fine");
        assert_eq!(
            resolve(src, &above(1)),
            Err(Refusal::InsideContinuationChain)
        );
        assert_eq!(
            resolve(src, &trailing(0)),
            Err(Refusal::TrailingOnContinuation)
        );
        assert_eq!(
            resolve(src, &trailing(1)),
            Err(Refusal::TrailingOnContinuation)
        );
        assert!(resolve(src, &trailing(2)).is_ok(), "the last line ends it");
    }

    #[test]
    fn inside_a_block_comment_is_refused() {
        let src = "/* a\n   b\n*/\ndi 1\n";
        assert_eq!(resolve(src, &above(1)), Err(Refusal::InsideVerbatim));
        assert!(resolve(src, &above(3)).is_ok());
    }

    #[test]
    fn inside_an_input_block_is_refused() {
        let src = "input a b\n1 2\n3 4\nend\ndi 1\n";
        assert_eq!(resolve(src, &above(1)), Err(Refusal::InsideVerbatim));
    }

    #[test]
    fn inside_a_delimit_semi_region_is_refused() {
        let src = "#delimit ;\nregress price\n    mpg weight;\n#delimit cr\ndi 1\n";
        assert_eq!(resolve(src, &above(2)), Err(Refusal::InsideDelimitSemi));
        assert!(resolve(src, &above(4)).is_ok());
    }

    #[test]
    fn a_blank_line_takes_no_trailing_comment() {
        assert_eq!(
            resolve("di 1\n\ndi 2\n", &trailing(1)),
            Err(Refusal::NotACodeLine)
        );
    }
}
