//! `Engine::tokens` — the `[from, to, tag]` triples CodeMirror paints with.
//!
//! Scoped to the requested byte range because a 10 k-line file has ~8 k tokens
//! per screen and materialising the whole document's stream would cost more than
//! the parse (design 06 §3.4).
//!
//! # The one design decision here
//!
//! `stratum-parse`'s lexer runs over a logical line's *code* — comments already
//! removed, `///` continuations already spliced — because that is what the
//! runtime executes and CONTRACTS §13 requires the editor to use the exact same
//! tokenizer. So the code half of every line comes from `stratum_parse::lex`,
//! mapped back through the line's piece table, and the comment half is the
//! COMPLEMENT: whatever bytes of the line the code did not claim.
//!
//! That complement is not a second comment scanner. The logical-line reader
//! already decided which bytes are comment, continuation and whitespace — every
//! trap in design 02 §2.1, `/* */` nesting included — and this module only
//! labels what it left behind. A `//` inside a string never reaches here as a
//! gap, because it never left the code.

use std::ops::Range;

use stratum_parse::{lex, LogicalLine, Segmentation as ParseSegmentation};
use stratum_proto::{Span, TokenKind};

use crate::encode_token_kind;

/// Append `[from, to, tag]` triples for every token overlapping `range`.
///
/// Emitted in ascending `from` order with no overlaps, which is what CM6's
/// decoration builder requires; whitespace produces no token at all.
pub fn project(seg: &ParseSegmentation<'_>, range: Range<usize>, out: &mut Vec<i32>) {
    let src = seg.src;
    // Both ends clamped into the document BEFORE they are compared. `clamp`
    // panics when its own bounds are inverted, which is exactly what a request
    // starting past EOF produces — and CM6 issues one on every transaction that
    // deletes the tail of a viewport, so this is the ordinary case, not an
    // exotic one.
    let from = range.start.min(src.len()) as u32;
    let to = range.end.clamp(from as usize, src.len()) as u32;
    if from >= to || seg.lines.is_empty() {
        return;
    }

    // The first line whose span can reach `from`: `partition_point` gives the
    // first line starting strictly after it, and the one before it is the line
    // `from` falls in.
    let first = seg
        .lines
        .partition_point(|l| l.span.start <= from)
        .saturating_sub(1);

    // Reused across lines so a viewport-sized request is one allocation, not one
    // per line. The per-line sort is what merges the code tokens and the comment
    // gaps back into document order.
    let mut line_out: Vec<(u32, u32, i32)> = Vec::new();

    for (i, line) in seg.lines.iter().enumerate().skip(first) {
        if line.span.start >= to {
            break;
        }
        line_out.clear();
        one_line(src, line, seg.derived[i].as_deref(), &mut line_out);
        line_out.sort_unstable();
        for (s, e, tag) in line_out.drain(..) {
            if e <= from || s >= to {
                continue;
            }
            out.extend_from_slice(&[s as i32, e as i32, tag]);
        }
    }
}

/// Every token of one logical line, in no particular order.
fn one_line(
    src: &str,
    line: &LogicalLine,
    derived: Option<&stratum_parse::Derived>,
    out: &mut Vec<(u32, u32, i32)>,
) {
    let map = line.map(derived);
    let code = line.code(src, derived);

    // The source runs the code was assembled from. Everything in the line's
    // span that is NOT one of these is comment, continuation or whitespace.
    let mut pieces = map.span_to_source(Span {
        start: 0,
        end: map.dst_len(),
    });
    pieces.retain(|p| p.end > p.start);

    for tok in lex(code) {
        let tag = encode_token_kind(tok.kind);
        for s in line.span_to_source(derived, tok.span) {
            if s.end > s.start {
                out.push((s.start, s.end, tag));
            }
        }
    }

    let mut cursor = line.span.start;
    for p in &pieces {
        push_gap(src, cursor, p.start, out);
        cursor = p.end;
    }
    push_gap(src, cursor, line.span.end, out);
}

/// Label a run of bytes the code did not claim.
///
/// Whitespace is trimmed off both ends and produces no token — an editor that
/// decorates whitespace pays for a decoration per space. What is left is a
/// comment or a `///` continuation, and which one is decided by its first byte,
/// not by re-parsing: the reader has already ruled that these bytes are not
/// code, so this is a labelling choice and cannot disagree with segmentation.
fn push_gap(src: &str, from: u32, to: u32, out: &mut Vec<(u32, u32, i32)>) {
    if to <= from {
        return;
    }
    let raw = &src[from as usize..to as usize];
    let lead = raw.len()
        - raw
            .trim_start_matches(|c: char| c.is_ascii_whitespace())
            .len();
    let body = raw.trim_matches(|c: char| c.is_ascii_whitespace());
    if body.is_empty() {
        return;
    }
    let start = from + lead as u32;
    let end = start + body.len() as u32;
    let kind = if body.starts_with("///") {
        // Three or more slashes is a CONTINUATION, not a comment (02 §2.1), and
        // the editor dims it differently: it is punctuation, not prose.
        TokenKind::Continuation
    } else {
        TokenKind::Comment
    };
    out.push((start, end, encode_token_kind(kind)));
}

#[cfg(test)]
mod tests {
    use stratum_parse::segment;

    use super::*;
    use crate::{decode_token_kind, TOKEN_STRIDE};

    fn toks(doc: &str) -> Vec<(u32, u32, TokenKind)> {
        let seg = segment(doc);
        let mut out = Vec::new();
        project(&seg, 0..doc.len(), &mut out);
        assert_eq!(out.len() % TOKEN_STRIDE, 0);
        out.chunks_exact(TOKEN_STRIDE)
            .map(|t| {
                (
                    t[0] as u32,
                    t[1] as u32,
                    decode_token_kind(t[2]).expect("every emitted tag decodes"),
                )
            })
            .collect()
    }

    fn assert_well_formed(doc: &str, got: &[(u32, u32, TokenKind)]) {
        let mut cursor = 0;
        for (s, e, _) in got {
            assert!(s >= &cursor, "tokens are not ascending in {doc:?}");
            assert!(e > s, "empty token in {doc:?}");
            assert!(*e as usize <= doc.len(), "token past EOF in {doc:?}");
            cursor = *e;
        }
    }

    #[test]
    fn code_and_comment_both_appear_and_do_not_overlap() {
        let doc = "regress price mpg // fit\n";
        let got = toks(doc);
        assert_well_formed(doc, &got);
        assert_eq!(got[0].2, TokenKind::Ident);
        assert_eq!(&doc[got[0].0 as usize..got[0].1 as usize], "regress");
        let last = got.last().unwrap();
        assert_eq!(last.2, TokenKind::Comment);
        assert_eq!(&doc[last.0 as usize..last.1 as usize], "// fit");
    }

    #[test]
    fn a_comment_inside_a_string_is_not_a_comment() {
        let doc = "display \"a // b\"\n";
        let got = toks(doc);
        assert_well_formed(doc, &got);
        assert!(
            got.iter().all(|t| t.2 != TokenKind::Comment),
            "the string was split at the slashes: {got:?}"
        );
        assert!(got.iter().any(|t| t.2 == TokenKind::StrLit));
    }

    #[test]
    fn a_continuation_is_labelled_as_one() {
        let doc = "local t 1 ///\n    2\n";
        let got = toks(doc);
        assert_well_formed(doc, &got);
        let cont = got
            .iter()
            .find(|t| t.2 == TokenKind::Continuation)
            .expect("no continuation token");
        assert_eq!(&doc[cont.0 as usize..cont.1 as usize], "///");
    }

    #[test]
    fn an_interior_block_comment_splits_the_code_around_it() {
        let doc = "display 1 /* mid */ + 2\n";
        let got = toks(doc);
        assert_well_formed(doc, &got);
        let c = got
            .iter()
            .find(|t| t.2 == TokenKind::Comment)
            .expect("no comment token");
        assert_eq!(&doc[c.0 as usize..c.1 as usize], "/* mid */");
    }

    #[test]
    fn a_range_past_the_end_invents_nothing() {
        let doc = "list\n";
        let seg = segment(doc);
        let mut out = Vec::new();
        project(&seg, doc.len() + 500..doc.len() + 900, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_narrow_range_returns_only_what_it_overlaps() {
        let doc = "list\nsummarize price\nregress price mpg\n";
        let seg = segment(doc);
        let from = doc.find("summarize").unwrap();
        let to = from + "summarize".len();
        let mut out = Vec::new();
        project(&seg, from..to, &mut out);
        assert!(!out.is_empty());
        for t in out.chunks_exact(TOKEN_STRIDE) {
            assert!(
                (t[1] as usize) > from && (t[0] as usize) < to,
                "token {}..{} does not overlap {from}..{to}",
                t[0],
                t[1]
            );
        }
    }

    #[test]
    fn a_pure_comment_line_is_one_token() {
        let doc = "* a whole line of prose\nlist\n";
        let got = toks(doc);
        assert_well_formed(doc, &got);
        assert_eq!(got[0].2, TokenKind::Comment);
        assert_eq!(
            &doc[got[0].0 as usize..got[0].1 as usize],
            "* a whole line of prose"
        );
    }

    #[test]
    fn semicolon_mode_still_tokenizes() {
        let doc = "#delimit ;\nsummarize price\n   mpg;\n#delimit cr\n";
        let got = toks(doc);
        assert_well_formed(doc, &got);
        assert!(got.iter().any(|t| t.2 == TokenKind::StatementBreak));
    }
}
