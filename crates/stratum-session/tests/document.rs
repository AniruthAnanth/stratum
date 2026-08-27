//! The document model: `stratum_parse::segment`, then
//! `stratum_runtime::doc::reconcile` — and no third thing.
//!
//! The plan's bullet for this crate is short and has two halves, and only the
//! second one is usually tested:
//!
//! > `DocumentModel` is implemented here by calling `stratum_parse::segment`
//! > then `stratum_runtime::doc::reconcile`; **the reconcile algorithm itself
//! > stays in W06's `doc.rs` and is not duplicated.**
//!
//! So the file below asserts the behaviour (identity survives typing; trivia
//! carries none; ids are never re-issued) *and* asserts the non-duplication
//! directly: [`identity_is_the_runtimes_answer_not_a_second_diff`] recomputes
//! the same reconcile with `stratum_runtime::doc::reconcile_regions` and a
//! fresh allocator and demands the same three answers — blocks, retired and
//! the allocation count. A private Myers diff that agreed on every one of these
//! cases would be indistinguishable from calling the real one, and any that did
//! not agree fails here.
//!
//! # Counters, not clocks (ADR-017)
//!
//! Every performance claim in this file is a count. `Applied::allocated` is the
//! keystroke counter: **an edit inside an existing block allocates zero ids**,
//! which is the property that makes a block's history and its result card
//! survive typing. No test here measures a duration.

use pretty_assertions::assert_eq;
use stratum_proto::{BlockId, DocumentId, RegionKind};
use stratum_runtime::doc::{reconcile_regions, BlockIdAlloc, DocumentModel};
use stratum_session::{Session, SessionConfig};

const DOC: DocumentId = DocumentId(1);
const OTHER: DocumentId = DocumentId(2);

/// Design `03` §6.4's worked example, which is also W08's staleness fixture.
/// Using the same text here means the identity assertions and the staleness
/// assertions are about the same five blocks.
const V1: &str = "\
* Wage study
use wages.dta, clear
gen age2 = age^2
gen ln_income = ln(income)
regress ln_income age age2
";

fn session() -> Session {
    Session::fresh(SessionConfig::new("/projects/wage-study").expect("a plain cwd is accepted"))
}

/// The real ids of a map, in document order, with trivia dropped.
fn ids(map: &stratum_proto::BlockMap) -> Vec<BlockId> {
    map.blocks.iter().copied().filter(|b| b.is_real()).collect()
}

// ── opening ────────────────────────────────────────────────────────────────

#[test]
fn opening_gives_every_executable_region_an_id_and_trivia_none() {
    let mut s = session();
    let applied = s.apply_document_change(DOC, V1.to_owned(), 1);

    assert_eq!(applied.map.blocks.len(), applied.map.regions.len());
    for (region, id) in applied.map.regions.iter().zip(&applied.map.blocks) {
        if matches!(region.kind, RegionKind::Trivia { .. }) {
            // A3: trivia gets `NONE`, never `EPHEMERAL`. Conflating them made a
            // command-bar run repaint every comment in the document.
            assert_eq!(*id, BlockId::NONE, "trivia must carry no identity");
        } else {
            assert!(id.is_real(), "an executable region has no id: {region:?}");
            assert_ne!(
                *id,
                BlockId::EPHEMERAL,
                "B0 is the command bar, not a block"
            );
        }
    }

    let real = ids(&applied.map);
    assert_eq!(real.len(), 4, "use, gen, gen, regress");
    assert_eq!(applied.executable, 4);
    assert_eq!(
        applied.allocated, 4,
        "opening a document mints one id per executable region and no more"
    );
    assert_eq!(real, [BlockId(1), BlockId(2), BlockId(3), BlockId(4)]);
    assert!(applied.map.retired.is_empty());
    assert_eq!(applied.map.generation, 1, "the first map a window sees");
    assert_eq!(applied.map.doc_version, 1);
    assert_eq!(s.document_version(DOC), 1);
    assert_eq!(s.document_generation(DOC), Some(1));
}

// ── the keystroke path ─────────────────────────────────────────────────────

#[test]
fn typing_inside_a_block_keeps_its_id_and_allocates_nothing() {
    let mut s = session();
    let before = ids(&s.apply_document_change(DOC, V1.to_owned(), 1).map);

    // The user adds an option to the regression. Its `CodeHash` changes, so the
    // diff sees a `Replace` hunk — CONTRACTS §2 rule 2 maps it positionally and
    // the id survives. If it did not, the block's ExecutionRecords and its
    // result card would vanish on a keystroke.
    let v2 = V1.replace(
        "regress ln_income age age2",
        "regress ln_income age age2, robust",
    );
    let after = s.apply_document_change(DOC, v2, 2);

    assert_eq!(
        after.allocated, 0,
        "an edit inside an existing block must mint nothing — this is the \
         ADR-017 counter for the interaction path"
    );
    assert_eq!(ids(&after.map), before, "every id survived the edit");
    assert!(after.map.retired.is_empty());
    assert_eq!(
        after.map.generation, 2,
        "generations are monotone per document"
    );
    assert_eq!(after.map.doc_version, 2);
}

#[test]
fn a_comment_above_a_block_moves_no_identity() {
    // Spec §23's promise, literally: trivia is held out of the sequence being
    // matched (A3), so a comment cannot displace anything. Two shapes of it —
    // a comment line, which the segmenter attaches to the following region, and
    // a blank line, which becomes a `Trivia` region of its own.
    let mut s = session();
    let before = ids(&s.apply_document_change(DOC, V1.to_owned(), 1).map);
    let regions_before = s
        .apply_document_change(DOC, V1.to_owned(), 2)
        .map
        .regions
        .len();

    let commented = V1.replace(
        "regress ln_income",
        "* the model the referee asked for\nregress ln_income",
    );
    let after = s.apply_document_change(DOC, commented.clone(), 3);
    assert_eq!(after.allocated, 0, "a comment allocated an id");
    assert_eq!(ids(&after.map), before);

    let blank = commented.replace("gen ln_income", "\ngen ln_income");
    let after_blank = s.apply_document_change(DOC, blank, 4);
    assert_eq!(after_blank.allocated, 0, "a blank line allocated an id");
    assert_eq!(ids(&after_blank.map), before);
    assert!(
        after_blank.map.regions.len() > regions_before,
        "the blank line really did add a region; the test would be vacuous \
         otherwise"
    );
    assert_eq!(
        after_blank
            .map
            .blocks
            .iter()
            .filter(|b| **b == BlockId::NONE)
            .count(),
        after_blank
            .map
            .regions
            .iter()
            .filter(|r| matches!(r.kind, RegionKind::Trivia { .. }))
            .count(),
        "one NONE per trivia region, no more and no fewer"
    );
}

#[test]
fn inserting_a_command_allocates_exactly_one_id() {
    let mut s = session();
    let before = ids(&s.apply_document_change(DOC, V1.to_owned(), 1).map);

    let v2 = V1.replace("regress", "gen lwage = ln(wage)\nregress");
    let after = s.apply_document_change(DOC, v2, 2);

    assert_eq!(after.allocated, 1, "one new block, one new id");
    let now = ids(&after.map);
    assert_eq!(now.len(), before.len() + 1);
    assert_eq!(now[..3], before[..3], "the blocks above it did not move");
    assert_eq!(
        now[4], before[3],
        "the regress block kept its id even though its index moved"
    );
    assert!(after.map.retired.is_empty());
}

#[test]
fn deleting_a_block_retires_it_and_its_id_is_never_reissued() {
    let mut s = session();
    let before = ids(&s.apply_document_change(DOC, V1.to_owned(), 1).map);
    let doomed = before[1];

    let v2 = V1.replace("gen age2 = age^2\n", "");
    let after = s.apply_document_change(DOC, v2.clone(), 2);
    assert_eq!(after.map.retired, [doomed], "the deleted block is retired");
    assert!(!ids(&after.map).contains(&doomed));

    // Retyping the same text is a NEW block. Reusing `doomed` would silently
    // re-attach the deleted block's ExecutionRecords — which stay in the
    // append-only ledger — to code the user has never run.
    let again = s.apply_document_change(DOC, V1.to_owned(), 3);
    assert_eq!(again.allocated, 1);
    let now = ids(&again.map);
    assert!(
        !now.contains(&doomed),
        "a retired id came back: {doomed:?} in {now:?}"
    );
    assert!(now.iter().all(|id| id.0 <= s.blocks_issued()));
}

// ── one session, one counter ───────────────────────────────────────────────

#[test]
fn two_documents_in_one_session_never_share_an_id() {
    // CONTRACTS §2 rule 3 says ids come from "the session counter". A
    // per-document allocator would hand `B1` to the first block of every tab,
    // and a `StatusChanged` for one document would repaint the other.
    let mut s = session();
    let a = ids(&s.apply_document_change(DOC, V1.to_owned(), 1).map);
    let b = ids(&s
        .apply_document_change(OTHER, "summarize price\ntabulate foreign\n".to_owned(), 1)
        .map);

    assert!(!b.is_empty());
    for id in &b {
        assert!(!a.contains(id), "{id:?} was issued to both documents");
    }
    assert_eq!(
        s.blocks_issued(),
        (a.len() + b.len()) as u64,
        "the counter issued exactly as many ids as there are blocks"
    );
    assert_eq!(s.documents().len(), 2);
    assert_eq!(s.documents().ids().collect::<Vec<_>>(), [DOC, OTHER]);
}

#[test]
fn closing_a_document_forgets_it_without_retiring_its_blocks() {
    let mut s = session();
    let opened = s.apply_document_change(DOC, V1.to_owned(), 1);
    let high = s.blocks_issued();

    assert!(s.close_document(DOC));
    assert!(s.documents().is_empty());
    assert!(
        !s.close_document(DOC),
        "closing twice is not an edit either"
    );

    // Reopening starts from no previous state — we cannot claim the bytes on
    // disk are the bytes we last identified — so it mints fresh ids, and they
    // are above the high-water mark rather than the old ones again.
    let reopened = s.apply_document_change(DOC, V1.to_owned(), 1);
    assert_eq!(reopened.allocated, opened.allocated);
    assert!(ids(&reopened.map).iter().all(|id| id.0 > high));
}

// ── the trait, and the algorithm's single home ─────────────────────────────

#[test]
fn the_trait_reconcile_is_the_same_code_path_as_a_text_change() {
    // The editor segments in wasm on every keystroke (CONTRACTS §14), so the
    // engine is regularly handed regions rather than text. Both entry points
    // must produce the same identity, or the two halves of the split editor
    // disagree about which block is which.
    let regions = stratum_parse::segment(V1).summaries();

    let mut from_text = session();
    let via_text = from_text.apply_document_change(DOC, V1.to_owned(), 1);

    let mut from_regions = session();
    let via_regions = DocumentModel::reconcile(&mut from_regions, DOC, regions);

    assert_eq!(via_text.map.blocks, via_regions.blocks);
    assert_eq!(via_text.map.retired, via_regions.retired);
    assert_eq!(via_text.map.generation, via_regions.generation);

    // And the read side of the trait answers from the same state.
    assert_eq!(DocumentModel::text(&from_text, DOC), V1);
    assert_eq!(DocumentModel::version(&from_text, DOC), 1);
    let blocks = DocumentModel::blocks(&from_text, DOC);
    assert_eq!(blocks.len(), via_text.executable as usize);
    assert!(blocks.iter().all(|b| b.doc == DOC && b.id.is_real()));
    assert_eq!(
        blocks.iter().map(|b| b.id).collect::<Vec<_>>(),
        ids(&via_text.map),
        "`blocks()` is the same identity assignment, with trivia dropped"
    );

    // A document nobody opened is empty rather than a panic: `stratum-exec`
    // resolves plans against documents that a window may have closed.
    assert_eq!(DocumentModel::blocks(&from_text, OTHER), &[]);
    assert_eq!(DocumentModel::text(&from_text, OTHER), "");
    assert_eq!(DocumentModel::version(&from_text, OTHER), 0);
}

#[test]
fn identity_is_the_runtimes_answer_not_a_second_diff() {
    // THE NON-DUPLICATION ASSERTION. Run the session's path and W06's algorithm
    // over the same inputs and demand all three outputs agree, on the edit shape
    // that separates a real Myers diff from a positional zip: a block deleted in
    // the middle, another edited, and one appended.
    let v2 = V1
        .replace("gen age2 = age^2\n", "")
        .replace("age age2", "age")
        + "estimates store m1\n";

    let mut s = session();
    let first = s.apply_document_change(DOC, V1.to_owned(), 1);
    let second = s.apply_document_change(DOC, v2.clone(), 2);

    let prev_regions = stratum_parse::segment(V1).summaries();
    let next_regions = stratum_parse::segment(&v2).summaries();
    let high = first
        .map
        .blocks
        .iter()
        .copied()
        .filter(|b| b.is_real())
        .max()
        .expect("the first pass identified something");
    let mut alloc = BlockIdAlloc::resuming_after(high);
    let oracle = reconcile_regions(&prev_regions, &first.map.blocks, &next_regions, &mut alloc);

    assert_eq!(second.map.blocks, oracle.blocks, "identity diverged");
    assert_eq!(second.map.retired, oracle.retired, "retirement diverged");
    assert_eq!(second.allocated, oracle.allocated, "allocation diverged");
}

#[test]
fn this_crate_has_no_diff_dependency_to_duplicate_it_with() {
    // The mechanical half of "not duplicated": `similar` is deliberately absent
    // from this crate's manifest, so writing a second Myers diff here is
    // inconvenient enough to notice. The word appears in the manifest's prose —
    // it says exactly this — so only real dependency lines are examined.
    let manifest = include_str!("../Cargo.toml");
    let dep = manifest
        .lines()
        .map(str::trim_start)
        .filter(|l| !l.starts_with('#'))
        .find(|l| l.starts_with("similar"));
    assert_eq!(
        dep, None,
        "stratum-session took a diff dependency; CONTRACTS §2's reconcile is \
         normative and lives in stratum_runtime::doc"
    );
}
