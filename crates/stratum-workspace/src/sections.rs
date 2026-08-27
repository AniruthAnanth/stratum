//! Sections (spec §3 explicit cells) and the two writers that edit them —
//! `section_rename` and `section_move`, ARCHITECTURE §6.3 / A15.
//!
//! # Why these two are writers at all
//!
//! Because a section's name lives *in the source*, as an ordinary Stata comment.
//! That is the whole design: the `.do` file stays the truth and a collaborator
//! without Stratum reads the same headings we do. Renaming a section therefore
//! has to edit the document — and moving one has to move executable statements.
//!
//! The audit (A15) found both listed as permitted writers in prose with no
//! command, no owner and no gate. They are gated here:
//!
//! * `section_rename` → `assert_comment_only`. The title is inside a comment, so
//!   the edit is provably comment-only or it is not a rename.
//! * `section_move` → `assert_statement_partition_preserved`, **and** it returns
//!   the `restaled` block list. Reordering executable statements changes
//!   execution order, so a move that left statuses untouched would be a direct
//!   INV-1 violation.
//!
//! # Marker syntax
//!
//! Design 06 §4.8: `// %% Label`, `//%% Label`, `* %% Label`. All three are
//! ordinary Stata comments, which is the point — the sigil is visible in the
//! source and dimmed in the editor, never hidden.
//!
//! The scan here is deliberately line-shaped and deliberately *not* the
//! segmenter: `stratum-parse` (W04) owns block segmentation and the engine's
//! [`stratum_proto::BlockMap`] is authoritative for regions. What this module
//! needs is narrower — where the marker lines are — and a marker line is
//! recognisable without parsing Stata.

use stratum_proto::{BlockId, BlockMap, Edit, LineRange, SectionId, SectionSpan, Span};

use crate::document::Document;
use crate::write::{Check, EditGate, GateRejection, GatedEdits, WriteError};

/// A section as this crate sees it: the marker line, the title inside it, and
/// the extent it owns.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Section {
    /// Position-derived id, stable for as long as the marker order is.
    pub id: SectionId,
    /// The title text after `%%`, trimmed.
    pub title: String,
    /// Byte span of the title text *inside* the marker line. This is what
    /// `section_rename` replaces, and nothing else.
    pub title_span: Span,
    /// The whole section: its marker line through the byte before the next
    /// marker (or EOF).
    pub span: Span,
    /// Lines of [`Section::span`].
    pub lines: LineRange,
}

impl Section {
    /// The wire projection the engine also produces.
    pub fn to_span(&self) -> SectionSpan {
        SectionSpan {
            id: self.id,
            span: self.span,
            title: self.title.clone(),
            lines: self.lines,
        }
    }
}

/// Find every section marker in a buffer, in document order.
///
/// A marker inside a `/* … */` block comment is still a marker: it is on its own
/// line, it reads as a heading to a human, and treating it otherwise would make
/// the fold arrows in the editor disagree with what the file looks like.
pub fn index(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut line_no = 0u32;
    let mut offset = 0usize;

    for line in text.split_inclusive('\n') {
        if let Some((title, title_span)) = marker_title(line, offset) {
            if let Some(prev) = out.last_mut() {
                prev.span.end = offset as u32;
                prev.lines.end = line_no;
            }
            out.push(Section {
                id: SectionId(out.len() as u32),
                title,
                title_span,
                span: Span {
                    start: offset as u32,
                    end: text.len() as u32,
                },
                lines: LineRange {
                    start: line_no,
                    end: 0,
                },
            });
        }
        offset += line.len();
        line_no += 1;
    }
    if let Some(last) = out.last_mut() {
        last.span.end = text.len() as u32;
        last.lines.end = line_no;
    }
    out
}

/// Recognise `// %% Title`, `//%% Title` and `* %% Title`, returning the title
/// and its span in the buffer.
fn marker_title(line: &str, offset: usize) -> Option<(String, Span)> {
    let indent = line.len() - line.trim_start().len();
    let body = line.trim_start();
    let after_sigil = body
        .strip_prefix("//")
        .or_else(|| body.strip_prefix('*'))?
        .trim_start();
    let rest = after_sigil.strip_prefix("%%")?;

    // The title starts after `%%` plus whatever spacing follows it, and ends
    // before the line terminator. Both ends matter: `section_rename` replaces
    // exactly this range, so a sloppy span here is how a rename eats a `//`.
    // Spaces and tabs only. `trim_start` would swallow the line terminator on a
    // marker with no title, putting the (empty) title span on the next line.
    let lead = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    let title = rest.trim();
    let start = offset + indent + (body.len() - rest.len()) + lead;
    Some((
        title.to_owned(),
        Span {
            start: start as u32,
            end: (start + title.len()) as u32,
        },
    ))
}

/// `section_rename` — CONTRACTS §11, `{ edits, version }`.
#[derive(Clone, PartialEq, Debug)]
pub struct RenamedSection {
    /// The edits applied, echoed to the frontend.
    pub edits: Vec<Edit>,
    /// The document version after the rename.
    pub version: u64,
}

/// `section_move` — CONTRACTS §11, `{ edits, version, restaled }`.
#[derive(Clone, PartialEq, Debug)]
pub struct MovedSection {
    /// The edits applied.
    pub edits: Vec<Edit>,
    /// The document version after the move.
    pub version: u64,
    /// **A15.** Every block whose execution order changed, and every block after
    /// the earlier of the two positions. Empty only when no `BlockMap` was
    /// supplied, i.e. before the engine has ever segmented this document.
    pub restaled: Vec<BlockId>,
}

/// Rename a section's title in place.
///
/// Produces exactly one edit, over the title text inside the marker comment, and
/// runs it past `assert_comment_only` before the caller is allowed to write it.
/// If the gate objects — because the computed span was wrong, or because the new
/// title itself re-enters the grammar — the rename is refused whole.
pub fn rename(
    doc: &Document,
    section: SectionId,
    title: &str,
    gate: &dyn EditGate,
) -> Result<(GatedEdits, RenamedSection), WriteError> {
    let sections = index(&doc.text);
    let s = sections
        .iter()
        .find(|s| s.id == section)
        .ok_or_else(|| no_such_section(section))?;

    // A title containing a newline would end the comment and turn the remainder
    // into code. `assert_comment_only` catches it, but rejecting it here gives
    // the user a message about their title rather than about a token stream.
    if title.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return Err(WriteError::Gate(GateRejection {
            writer: "section_rename",
            check: Check::StatementPartition,
            detail: "a section title is a single-line comment and cannot contain a \
                     line break"
                .to_owned(),
        }));
    }

    let edits = vec![Edit {
        span: s.title_span,
        text: title.trim().to_owned(),
    }];
    let gated = GatedEdits::section_rename(&doc.text, edits.clone(), gate)?;
    Ok((
        gated,
        RenamedSection {
            edits,
            version: doc.version + 1,
        },
    ))
}

/// Move a section so that it sits immediately before `before`, or to the end of
/// the document when `before` is `None`.
///
/// The edit is expressed as a single replacement over the union of the two
/// affected extents rather than as a delete plus an insert, because the two
/// forms are not equivalent under [`crate::document::apply_edits`]: a delete and
/// an insert at overlapping offsets is exactly the ordering ambiguity that
/// function refuses.
pub fn move_section(
    doc: &Document,
    section: SectionId,
    before: Option<SectionId>,
    block_map: Option<&BlockMap>,
    gate: &dyn EditGate,
) -> Result<(GatedEdits, MovedSection), WriteError> {
    let sections = index(&doc.text);
    let from = sections
        .iter()
        .position(|s| s.id == section)
        .ok_or_else(|| no_such_section(section))?;
    let to = match before {
        None => sections.len(),
        Some(b) => sections
            .iter()
            .position(|s| s.id == b)
            .ok_or_else(|| no_such_section(b))?,
    };
    if to == from || to == from + 1 {
        // A no-op move still has to be a legal call — the frontend's drag can end
        // where it started — but it must not produce a write or a restale.
        return Ok((
            GatedEdits::section_move(&doc.text, Vec::new(), gate)?,
            MovedSection {
                edits: Vec::new(),
                version: doc.version,
                restaled: Vec::new(),
            },
        ));
    }

    let mut order: Vec<usize> = (0..sections.len()).collect();
    let moved = order.remove(from);
    order.insert(if to > from { to - 1 } else { to }, moved);

    // Everything before the first marker is preamble and never moves.
    let region_start = sections[0].span.start as usize;
    let rewritten: String = order
        .iter()
        .map(|&i| &doc.text[sections[i].span.start as usize..sections[i].span.end as usize])
        .collect();

    let edits = vec![Edit {
        span: Span {
            start: region_start as u32,
            end: doc.text.len() as u32,
        },
        text: rewritten,
    }];

    let gated = GatedEdits::section_move(&doc.text, edits.clone(), gate)?;

    // "the earlier of the two positions" — `to` may be `sections.len()` when the
    // section is moved to the end, so it is clamped rather than indexed raw.
    let earliest = sections[from.min(to).min(sections.len() - 1)].span.start;
    Ok((
        gated,
        MovedSection {
            edits,
            version: doc.version + 1,
            restaled: restaled_blocks(block_map, earliest),
        },
    ))
}

/// **A15.** Every real block at or after `earliest` in the pre-move document.
///
/// ARCHITECTURE §6.3: "every moved block and every block after the earlier of
/// the two positions is recomputed by the C0–C9 sweep". This function names the
/// candidates; the engine's sweep decides what each one actually becomes (they
/// will typically land on `Stale{UpstreamPending}`). Trivia regions are skipped
/// because `BlockId::NONE` is not a node in the staleness graph (A3).
fn restaled_blocks(map: Option<&BlockMap>, earliest: u32) -> Vec<BlockId> {
    let Some(map) = map else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, region) in map.blocks.iter().zip(&map.regions) {
        if id.is_real() && region.outer_span.end > earliest {
            out.push(*id);
        }
    }
    out.dedup();
    out
}

fn no_such_section(id: SectionId) -> WriteError {
    WriteError::Gate(GateRejection {
        writer: "section",
        check: Check::StatementPartition,
        detail: format!("no section {id} in this document"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::StandaloneGate;
    use stratum_proto::DocumentId;

    const SRC: &str = "\
// %% Setup
sysuse auto

//%% Clean
drop if price > 15000

* %% Model
regress price mpg
";

    fn doc() -> Document {
        Document::untitled(DocumentId(1), SRC)
    }

    #[test]
    fn all_three_marker_spellings_are_recognised() {
        let s = index(SRC);
        assert_eq!(
            s.iter().map(|x| x.title.as_str()).collect::<Vec<_>>(),
            vec!["Setup", "Clean", "Model"]
        );
    }

    #[test]
    fn the_title_span_covers_the_title_and_nothing_else() {
        let s = index(SRC);
        assert_eq!(
            &SRC[s[0].title_span.start as usize..s[0].title_span.end as usize],
            "Setup"
        );
        assert_eq!(
            &SRC[s[1].title_span.start as usize..s[1].title_span.end as usize],
            "Clean"
        );
        assert_eq!(
            &SRC[s[2].title_span.start as usize..s[2].title_span.end as usize],
            "Model"
        );
    }

    #[test]
    fn sections_tile_from_the_first_marker_to_eof() {
        let s = index(SRC);
        assert_eq!(s[0].span.end, s[1].span.start);
        assert_eq!(s[1].span.end, s[2].span.start);
        assert_eq!(s[2].span.end as usize, SRC.len());
    }

    #[test]
    fn an_empty_title_is_a_section() {
        let s = index("// %%\nlist\n");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].title, "");
        // The span is empty but positioned, so a rename inserts in the right place.
        assert_eq!(s[0].title_span, Span { start: 5, end: 5 });
    }

    #[test]
    fn rename_is_one_edit_and_passes_the_comment_gate() {
        let d = doc();
        let (gated, r) = rename(&d, SectionId(1), "Cleaning", &StandaloneGate).unwrap();
        assert_eq!(r.edits.len(), 1);
        assert!(gated.text().contains("//%% Cleaning\n"));
        assert!(gated.text().contains("drop if price > 15000"));
    }

    #[test]
    fn rename_refuses_a_multiline_title() {
        let d = doc();
        assert!(rename(&d, SectionId(0), "a\nb", &StandaloneGate).is_err());
    }

    #[test]
    fn move_reorders_and_passes_the_partition_gate() {
        let d = doc();
        let (gated, m) =
            move_section(&d, SectionId(2), Some(SectionId(1)), None, &StandaloneGate).unwrap();
        let t = gated.text();
        assert!(t.find("* %% Model").unwrap() < t.find("//%% Clean").unwrap());
        assert!(t.find("// %% Setup").unwrap() < t.find("* %% Model").unwrap());
        assert_eq!(m.edits.len(), 1);
    }

    #[test]
    fn a_move_onto_itself_is_a_no_op_with_no_restale() {
        let d = doc();
        let (gated, m) =
            move_section(&d, SectionId(0), Some(SectionId(1)), None, &StandaloneGate).unwrap();
        assert!(m.edits.is_empty());
        assert!(m.restaled.is_empty());
        assert_eq!(gated.text(), SRC);
    }
}
