//! `L010` — `capture` with no `_rc` inspection.
//!
//! Design 07 §6.3: "`capture` with no subsequent `_rc` inspection". `capture`
//! turns a failure into silence. That is a legitimate idiom — `capture drop x`
//! before creating `x` is idiomatic Stata — but it is also the single easiest
//! way to make a do-file *appear* to run cleanly while producing nothing.
//!
//! # The window
//!
//! `_rc` holds the return code of the most recent `capture`, so the inspection
//! must come before the next `capture` overwrites it. The check therefore looks
//! forward from each `capture` only as far as the next one at the same or lower
//! brace depth, and treats any read of `_rc` in that window as an inspection.
//!
//! # The two idioms that are not findings
//!
//! `capture drop`, `capture confirm`, `capture erase` and friends exist
//! *because* their failure is the expected case; flagging them would put a
//! decoration on every well-written do-file in the world, which is precisely the
//! clutter design 07 §6 forbids. They are excluded by name.

use stratum_proto::diagnostic::{Diagnostic, Suggestion, SuggestionKind};
use stratum_proto::{Edit, Span};

use super::dataflow::{reads_rc, Doc};
use crate::ParseIndex;

/// Commands whose failure under `capture` is the point of writing it.
const IDIOMATIC: &[&str] = &["assert", "confirm", "drop", "erase", "log", "rmdir"];

pub(super) fn check(idx: &ParseIndex<'_>, doc: &Doc<'_>, out: &mut Vec<Diagnostic>) {
    for (i, st) in doc.stmts.iter().enumerate() {
        if !st.has_prefix("capture") && st.name() != "capture" {
            continue;
        }
        if IDIOMATIC.contains(&st.name()) {
            continue;
        }
        if inspected_before_the_next_capture(doc, i, st.depth) {
            continue;
        }
        let span = st.span;
        let d = super::finding(
            "L010",
            format!(
                "`capture` swallows any error from `{}` and nothing afterwards reads `_rc`",
                st.name()
            ),
            span,
        );
        // The fix inserts the inspection rather than removing the `capture`:
        // deleting the `capture` changes what the file does, and a deterministic
        // fix must not.
        let line_start = idx.segmentation().line_index.line_start(
            idx.segmentation()
                .line_index
                .line_of(span.start)
                .saturating_add(1),
        );
        let indent = leading_indent(idx.source(), span.start);
        out.push(super::with_fix(
            d,
            Suggestion {
                label: "Insert an `_rc` check".to_owned(),
                kind: SuggestionKind::InsertLine,
                edits: vec![Edit {
                    span: Span {
                        start: line_start,
                        end: line_start,
                    },
                    text: format!(
                        "{indent}if _rc {{\n{indent}    // handle the failure\n{indent}}}\n"
                    ),
                }],
            },
        ));
    }
}

/// Whether some statement after `from` reads `_rc` before the next `capture` at
/// the same or an enclosing depth.
fn inspected_before_the_next_capture(doc: &Doc<'_>, from: usize, depth: u32) -> bool {
    for st in doc.stmts.iter().skip(from + 1) {
        if (st.has_prefix("capture") || st.name() == "capture") && st.depth <= depth {
            return false;
        }
        if st.exprs().iter().any(|e| reads_rc(e)) {
            return true;
        }
        // `display _rc`, `local rc = _rc`, `return scalar rc = _rc`: the tail is
        // raw text for these, so the token is looked for there too. `_rc` cannot
        // appear as a substring of a Stata name — names may not contain a
        // leading underscore followed by `rc` without being that system value —
        // so a plain search is exact enough, and it is bounded by the statement.
        if st.rest().is_some_and(mentions_rc) {
            return true;
        }
    }
    false
}

fn mentions_rc(text: &str) -> bool {
    let b = text.as_bytes();
    let mut i = 0usize;
    while let Some(rel) = text.get(i..).and_then(|t| t.find("_rc")) {
        let at = i + rel;
        let before_ok = at == 0
            || b.get(at - 1)
                .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_');
        let after_ok = b
            .get(at + 3)
            .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_');
        if before_ok && after_ok {
            return true;
        }
        i = at + 3;
    }
    false
}

/// The whitespace at the start of the physical line containing `pos`.
fn leading_indent(src: &str, pos: u32) -> String {
    let head = src.get(..pos as usize).unwrap_or("");
    let line_start = head.rfind('\n').map_or(0, |i| i + 1);
    src.get(line_start..pos as usize)
        .unwrap_or("")
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::lints::lint_document;
    use crate::Env;

    fn fires(src: &str) -> bool {
        let idx = ParseIndex::new(src);
        lint_document(&idx, &Env::default())
            .iter()
            .any(|d| d.code == "L010")
    }

    #[test]
    fn fires_on_an_unchecked_capture() {
        assert!(fires(
            "capture merge 1:1 pid using wave2.dta\nsummarize price\n"
        ));
    }

    #[test]
    fn silent_when_rc_is_inspected() {
        assert!(fires("capture merge 1:1 pid using w.dta\n"));
        assert!(!fires(
            "capture merge 1:1 pid using w.dta\nif _rc {\n    di \"merge failed\"\n}\n"
        ));
    }

    #[test]
    fn silent_on_the_idiomatic_forms() {
        assert!(!fires("capture drop tmp\ngenerate tmp = 1\n"));
        assert!(!fires("capture confirm variable price\n"));
    }

    #[test]
    fn a_second_capture_closes_the_window() {
        // The `if _rc` reads the SECOND capture's code, not the first's.
        assert!(fires(
            "capture regress price mpg\ncapture summarize price\nif _rc {\n    di 1\n}\n"
        ));
    }

    #[test]
    fn the_fix_inserts_rather_than_deletes() {
        let src = "capture regress price mpg\n";
        let idx = ParseIndex::new(src);
        let found = lint_document(&idx, &Env::default());
        let d = found.iter().find(|d| d.code == "L010").expect("L010");
        let s = d.suggestions.first().expect("a fix");
        assert_eq!(s.edits.len(), 1);
        assert_eq!(s.edits[0].span.start, s.edits[0].span.end, "insert only");
        assert!(s.edits[0].text.contains("if _rc"));
    }

    #[test]
    fn rc_is_matched_as_a_whole_token() {
        assert!(mentions_rc("di _rc"));
        assert!(mentions_rc("local r = _rc"));
        assert!(!mentions_rc("di my_rc2"));
        assert!(!mentions_rc("di _rcode"));
    }
}
