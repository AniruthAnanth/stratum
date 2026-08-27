//! Cell markers and sections — spec §3.
//!
//! `// %% Title` and `* %% Title` are ordinary Stata comments, so a file that
//! uses them still runs anywhere else. That is the whole design: markers are
//! editor metadata and the runtime must not require them.
//!
//! A marker always begins a new group. It is never swallowed as a doc comment
//! attached to the command below it, because "run this section" and "run this
//! command with its explanatory comment" are different operations and the user
//! chose the first by typing the marker.

use stratum_proto::{CellMarker, LineRange, SectionId, SectionSpan, Span};

use crate::lineindex::LineIndex;
use crate::scan::logical::LogicalLine;

/// True when the raw source text of a trivia line is a cell marker.
///
/// Accepts `//`-form (any slash run, so a `/// %%` continuation marker is one
/// too) and `*`-form. The title is whatever follows `%%` on the line.
pub fn is_cell_marker(raw: &str) -> bool {
    marker_title(raw).is_some()
}

/// The marker's title, trimmed. `None` when this line is not a marker.
pub fn marker_title(raw: &str) -> Option<&str> {
    let t = raw.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let body = if let Some(rest) = t.strip_prefix("//") {
        rest.trim_start_matches('/')
    } else {
        t.strip_prefix('*')?
    };
    let body = body.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let title = body.strip_prefix("%%")?;
    Some(title.trim_matches(|c: char| c.is_ascii_whitespace()))
}

/// Collect the markers of a document and the section each one opens.
///
/// Sections are numbered from 0 in document order and run from their marker to
/// the byte before the next marker (or to end of source). Text above the first
/// marker belongs to no section, which is why `RegionSummary::section` is an
/// `Option`: a file with one marker halfway down has a genuinely unsectioned
/// head, and inventing a "section 0" for it would make "move section" offer to
/// move the preamble.
pub(crate) fn collect(
    src: &str,
    lines: &[LogicalLine],
    li: &LineIndex,
) -> (Vec<CellMarker>, Vec<SectionSpan>) {
    let mut raw = Vec::new();
    scan_range(src, lines, &mut raw);
    finish(raw, src.len() as u32, li.line_count())
}

/// One marker before section ids are assigned. Ids are positional, so they
/// cannot be given out until every marker in the document is known.
pub(crate) type RawMarker = (Span, u32, String);

/// Append the markers found in `lines`.
pub(crate) fn scan_range(src: &str, lines: &[LogicalLine], out: &mut Vec<RawMarker>) {
    for line in lines {
        if !line.is_cell_marker {
            continue;
        }
        let raw = &src[line.span.start as usize..line.span.end as usize];
        if let Some(title) = marker_title(raw) {
            out.push((line.span, line.first_line, title.to_owned()));
        }
    }
}

/// Number the markers and derive the section each one opens.
pub(crate) fn finish(
    raw: Vec<RawMarker>,
    src_len: u32,
    line_count: u32,
) -> (Vec<CellMarker>, Vec<SectionSpan>) {
    let mut markers = Vec::with_capacity(raw.len());
    let mut sections: Vec<SectionSpan> = Vec::with_capacity(raw.len());
    for (span, line, title) in raw {
        let id = SectionId(markers.len() as u32);
        if let Some(prev) = sections.last_mut() {
            prev.span.end = span.start;
            prev.lines.end = line;
        }
        sections.push(SectionSpan {
            id,
            span: Span {
                start: span.start,
                end: src_len,
            },
            title: title.clone(),
            lines: LineRange {
                start: line,
                end: line_count,
            },
        });
        markers.push(CellMarker {
            span,
            line,
            title,
            section: id,
        });
    }
    (markers, sections)
}
