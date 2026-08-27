//! The two section writers and their gates — ARCHITECTURE §6.3 / A15, plan
//! W26's fourth acceptance bullet.
//!
//! Two halves:
//!
//! 1. The happy path — `section_rename` produces edits that pass
//!    `assert_comment_only`, `section_move` produces edits that pass
//!    `assert_statement_partition_preserved` **and** returns `restaled`.
//! 2. The **mutation tests** — a doctored edit list must be rejected. This is
//!    the half that matters. A gate that is called but whose verdict does not
//!    stop the write is decoration, and the audit found exactly that failure
//!    (A15) in the pre-audit design.

use stratum_proto::{
    BlockId, BlockMap, Delimiter, DocumentId, Edit, LineRange, RegionKind, RegionSummary,
    SectionId, Span,
};
use stratum_workspace::document::Document;
use stratum_workspace::entry::RefusingGate;
use stratum_workspace::sections;
use stratum_workspace::write::{Check, EditGate, GatedEdits, StandaloneGate, WriteError};

mod common;
use common::{project_at, tmp};

const SRC: &str = "\
// %% Load
sysuse auto, clear

// %% Clean
drop if price > 15000
gen lprice = log(price)

// %% Model
regress lprice mpg
";

fn on_disk() -> (
    tempfile::TempDir,
    camino::Utf8PathBuf,
    stratum_workspace::Workspace,
) {
    let (t, root) = tmp();
    let path = root.join("analysis.do");
    std::fs::write(&path, SRC).unwrap();
    let ws = project_at(&root);
    (t, path, ws)
}

// ---------------------------------------------------------------------------
// section_rename
// ---------------------------------------------------------------------------

#[test]
fn rename_edits_only_the_comment_and_lands_on_disk() {
    let (_t, path, mut ws) = on_disk();
    let opened = ws.doc_open(&path).unwrap();
    let before = std::fs::read(&path).unwrap();

    let r = ws
        .section_rename(opened.doc, SectionId(1), "Clean and transform")
        .unwrap();
    assert_eq!(r.edits.len(), 1);
    assert_eq!(r.version, opened.version + 1);

    let after = std::fs::read(&path).unwrap();
    assert_eq!(common::lines_differing(&before, &after), 1);
    assert!(String::from_utf8(after)
        .unwrap()
        .contains("// %% Clean and transform\n"));

    // And the code the runtime sees is provably unchanged.
    assert!(StandaloneGate
        .assert_comment_only(SRC, &ws.document(opened.doc).unwrap().text)
        .is_ok());
}

#[test]
fn rename_is_reached_through_its_gate_not_merely_accompanied_by_one() {
    // The mutation: swap in a gate that refuses everything. If the writer really
    // is gated, nothing reaches disk.
    let (_t, root) = tmp();
    let path = root.join("analysis.do");
    std::fs::write(&path, SRC).unwrap();
    let project = stratum_workspace::project::Project::load(&root).unwrap();
    let mut ws = stratum_workspace::Workspace::with_gate(
        project,
        stratum_workspace::layout::LayoutStore::new(root.join("r"), root.join("c")),
        stratum_workspace::keymap::KeymapStore::new(root.join("r"), root.join("c")),
        Box::new(RefusingGate),
    );
    let opened = ws.doc_open(&path).unwrap();

    assert!(ws.section_rename(opened.doc, SectionId(0), "Nope").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), SRC.as_bytes());
    assert!(ws
        .section_move(opened.doc, SectionId(0), None, None)
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), SRC.as_bytes());
}

/// A doctored edit list: the span claims to cover a section title but reaches
/// into the statement below it.
#[test]
fn the_comment_gate_rejects_an_edit_list_that_touches_code() {
    let doctored = vec![Edit {
        span: Span {
            start: SRC.find("Load").unwrap() as u32,
            end: SRC.find(", clear").unwrap() as u32,
        },
        text: "Load\nsysuse nlsw88".to_owned(),
    }];
    let err = GatedEdits::section_rename(SRC, doctored, &StandaloneGate).unwrap_err();
    assert!(matches!(err, WriteError::Gate(_)), "{err:?}");
}

/// The classic §23 failure: a comment inserted into a `///` continuation chain
/// terminates the statement early, so the runtime sees a different program.
#[test]
fn the_comment_gate_rejects_a_comment_that_splits_a_continuation_chain() {
    let src = "// %% M\ngen x = a + ///\n    b + ///\n    c\nlist\n";
    let at = src.find("    b").unwrap() as u32;
    let doctored = vec![Edit {
        span: Span { start: at, end: at },
        text: "// halfway\n".to_owned(),
    }];
    match GatedEdits::ai_comment_patch(src, doctored, &StandaloneGate).unwrap_err() {
        WriteError::Gate(r) => assert_eq!(r.check, Check::StatementPartition),
        other => panic!("expected a gate rejection, got {other:?}"),
    }
}

/// Gate 1's `Star` rule, stated as an edit: `*` after code is multiplication,
/// not a comment.
#[test]
fn the_comment_gate_rejects_a_star_appended_after_code() {
    let src = "gen z = a\nlist\n";
    let at = src.find('\n').unwrap() as u32;
    let doctored = vec![Edit {
        span: Span { start: at, end: at },
        text: " * scale it".to_owned(),
    }];
    assert!(GatedEdits::ai_comment_patch(src, doctored, &StandaloneGate).is_err());
}

#[test]
fn a_title_that_would_re_enter_the_grammar_is_refused() {
    let doc = Document::untitled(DocumentId(1), SRC);
    for bad in ["a\nsysuse auto", "a\r\nlist"] {
        assert!(
            sections::rename(&doc, SectionId(0), bad, &StandaloneGate).is_err(),
            "accepted {bad:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// section_move
// ---------------------------------------------------------------------------

#[test]
fn move_reorders_text_passes_its_gate_and_restales() {
    let (_t, path, mut ws) = on_disk();
    let opened = ws.doc_open(&path).unwrap();
    let map = block_map_for(&opened.text);

    // Move "Model" (2) above "Clean" (1).
    let m = ws
        .section_move(opened.doc, SectionId(2), Some(SectionId(1)), Some(&map))
        .unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.find("// %% Model").unwrap() < after.find("// %% Clean").unwrap());
    assert!(after.find("// %% Load").unwrap() < after.find("// %% Model").unwrap());

    // Every statement survived, byte for byte.
    for stmt in [
        "sysuse auto, clear",
        "drop if price > 15000",
        "gen lprice = log(price)",
        "regress lprice mpg",
    ] {
        assert_eq!(after.matches(stmt).count(), 1, "{stmt} did not survive");
    }
    assert!(StandaloneGate
        .assert_statement_partition_preserved(SRC, &after)
        .is_ok());

    // **A15.** Reordering executable statements changes execution order, so the
    // blocks at and after the earlier of the two positions must be swept. A move
    // that left statuses untouched would be a direct INV-1 violation.
    assert!(
        !m.restaled.is_empty(),
        "section_move must report the blocks whose execution order changed"
    );
    assert!(m.restaled.contains(&BlockId(2)), "{:?}", m.restaled);
    assert!(m.restaled.contains(&BlockId(4)), "{:?}", m.restaled);
    // The `sysuse` above both positions is untouched.
    assert!(!m.restaled.contains(&BlockId(1)), "{:?}", m.restaled);
}

/// The mutation: an edit list that *claims* to be a move but changes one of the
/// statements it carries.
#[test]
fn the_partition_gate_rejects_an_edit_list_that_alters_a_statement() {
    let doctored = vec![Edit {
        span: Span {
            start: 0,
            end: SRC.len() as u32,
        },
        text: SRC.replace("regress lprice mpg", "regress lprice weight"),
    }];
    match GatedEdits::section_move(SRC, doctored, &StandaloneGate).unwrap_err() {
        WriteError::Gate(r) => assert_eq!(r.check, Check::StatementMultiset),
        other => panic!("expected a gate rejection, got {other:?}"),
    }
}

/// The subtler mutation: a move that silently drops a statement. The multiset
/// check is what catches it — a naive "the text is a permutation of the lines"
/// check would not.
#[test]
fn the_partition_gate_rejects_a_move_that_drops_a_statement() {
    let doctored = vec![Edit {
        span: Span {
            start: 0,
            end: SRC.len() as u32,
        },
        text: SRC.replace("drop if price > 15000\n", ""),
    }];
    assert!(GatedEdits::section_move(SRC, doctored, &StandaloneGate).is_err());
}

/// And the mutation nothing else in the gate would see: a move that loses a
/// heading. Comments are stripped before every other comparison, so without the
/// comment-multiset check this passes.
#[test]
fn the_partition_gate_rejects_a_move_that_drops_a_comment() {
    let doctored = vec![Edit {
        span: Span {
            start: 0,
            end: SRC.len() as u32,
        },
        text: SRC.replace("// %% Clean\n", ""),
    }];
    match GatedEdits::section_move(SRC, doctored, &StandaloneGate).unwrap_err() {
        WriteError::Gate(r) => assert_eq!(r.check, Check::CommentMultiset),
        other => panic!("expected a gate rejection, got {other:?}"),
    }
}

#[test]
fn a_move_to_the_end_works_and_a_no_op_move_writes_nothing() {
    let (_t, path, mut ws) = on_disk();
    let opened = ws.doc_open(&path).unwrap();

    let noop = ws
        .section_move(opened.doc, SectionId(1), Some(SectionId(2)), None)
        .unwrap();
    assert!(noop.edits.is_empty());
    assert!(noop.restaled.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), SRC.as_bytes());

    let m = ws
        .section_move(opened.doc, SectionId(0), None, None)
        .unwrap();
    assert_eq!(m.edits.len(), 1);
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.find("// %% Load").unwrap() > after.find("// %% Model").unwrap());
}

/// A `BlockMap` for `text` in which each non-blank, non-comment line is a block.
///
/// Enough to exercise `restaled`; the engine's real map is authoritative in
/// production and this crate never builds one.
fn block_map_for(text: &str) -> BlockMap {
    let mut blocks = Vec::new();
    let mut regions = Vec::new();
    let mut offset = 0u32;
    let mut next = 1u64;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        let is_code =
            !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with('*');
        let span = Span {
            start: offset,
            end: offset + line.len() as u32,
        };
        blocks.push(if is_code {
            let id = BlockId(next);
            next += 1;
            id
        } else {
            BlockId::NONE
        });
        regions.push(RegionSummary {
            index: i as u32,
            span,
            outer_span: span,
            lines: LineRange {
                start: i as u32,
                end: i as u32 + 1,
            },
            code_lines: LineRange {
                start: i as u32,
                end: i as u32 + 1,
            },
            kind: if is_code {
                RegionKind::Simple
            } else {
                RegionKind::Trivia { has_marker: false }
            },
            entry_delimiter: Delimiter::Cr,
            exit_delimiter: Delimiter::Cr,
            code_hash: stratum_proto::CodeHash([i as u8; 16]),
            hash_ordinal: 0,
            canonical: None,
            is_estimation: false,
            has_macro_in_head: false,
            section: None,
        });
        offset += line.len() as u32;
    }
    BlockMap {
        doc: DocumentId(1),
        generation: 1,
        doc_version: 0,
        blocks,
        regions,
        markers: Vec::new(),
        sections: Vec::new(),
        retired: Vec::new(),
        diagnostics: Vec::new(),
        end_delimiter: Delimiter::Cr,
    }
}
