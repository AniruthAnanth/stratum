//! Block identity across an edit — CONTRACTS §2, W06's `reconcile` acceptance
//! bullet.
//!
//! > `reconcile`: the LCS+positional algorithm of CONTRACTS §2, tested on split,
//! > merge, reorder, insert-above, delete-middle, and edit-in-place.
//!
//! Each of the six is a named test below. The one that matters most is
//! edit-in-place: without rule 2's positional mapping inside a `Replace` hunk,
//! typing one character inside a block would retire it and mint a new id, and
//! every `ExecutionRecord`, result card and status glyph keyed on that id would
//! vanish on every keystroke.

use stratum_proto::{
    BlockId, BraceOpener, CodeHash, Delimiter, LineRange, RegionKind, RegionSummary, Span,
};
use stratum_runtime::doc::{reconcile, reconcile_regions, BlockIdAlloc, Reconciled};

fn h(n: u8) -> CodeHash {
    CodeHash([n; 16])
}

/// Reconcile `prev` (ids `B1..Bn` in order) against `next`.
fn go(prev: &[u8], next: &[u8]) -> Reconciled {
    let mut alloc = BlockIdAlloc::resuming_after(BlockId(prev.len() as u64));
    let ph: Vec<CodeHash> = prev.iter().map(|n| h(*n)).collect();
    let ids: Vec<BlockId> = (1..=prev.len() as u64).map(BlockId).collect();
    let nh: Vec<CodeHash> = next.iter().map(|n| h(*n)).collect();
    reconcile(&ph, &ids, &nh, &mut alloc)
}

fn ids(r: &Reconciled) -> Vec<u64> {
    r.blocks.iter().map(|b| b.0).collect()
}

#[test]
fn edit_in_place_keeps_the_id() {
    // `{A}{B}{C}` -> `{A}{B'}{C}`. Rule 2: inside the Replace hunk, old[0] maps
    // to new[0] positionally, so B keeps its id.
    let r = go(&[1, 2, 3], &[1, 9, 3]);
    assert_eq!(ids(&r), vec![1, 2, 3]);
    assert!(r.retired.is_empty());
    assert_eq!(r.allocated, 0, "an edit allocates nothing");
}

#[test]
fn insert_above_keeps_every_existing_id() {
    let r = go(&[1, 2, 3], &[9, 1, 2, 3]);
    assert_eq!(ids(&r), vec![4, 1, 2, 3]);
    assert!(r.retired.is_empty());
    assert_eq!(r.allocated, 1);
}

#[test]
fn delete_middle_retires_exactly_one_block() {
    let r = go(&[1, 2, 3], &[1, 3]);
    assert_eq!(ids(&r), vec![1, 3]);
    assert_eq!(r.retired, vec![BlockId(2)]);
    assert_eq!(r.allocated, 0);
}

#[test]
fn a_split_keeps_the_id_on_the_first_fragment() {
    // CONTRACTS §2: "A split `{A B}` -> `{A}{B}` keeps the id on the first
    // fragment … The second fragment becomes `NeverRun`, which is honest" — we
    // have never run *that* code.
    let r = go(&[7], &[1, 2]);
    assert_eq!(r.blocks[0], BlockId(1), "the first fragment keeps the id");
    assert_ne!(r.blocks[1], BlockId(1));
    assert!(r.retired.is_empty());
    assert_eq!(r.allocated, 1);
}

#[test]
fn a_merge_keeps_the_first_blocks_id() {
    let r = go(&[1, 2], &[7]);
    assert_eq!(r.blocks, vec![BlockId(1)]);
    assert_eq!(
        r.retired,
        vec![BlockId(2)],
        "the second is retired, not reused"
    );
    assert_eq!(r.allocated, 0);
}

#[test]
fn a_reorder_is_deterministic_and_never_duplicates_an_id() {
    // Swapping two blocks has no identity-preserving answer — Myers sees a
    // delete and an insert. What must hold is that the result is deterministic
    // and that no id appears twice, because a duplicated id would attach one
    // block's history to another's code.
    let first = go(&[1, 2], &[2, 1]);
    let again = go(&[1, 2], &[2, 1]);
    assert_eq!(first, again, "the same edit must reconcile the same way");

    let mut seen = first.blocks.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), first.blocks.len(), "no id appears twice");
    for r in &first.retired {
        assert!(!first.blocks.contains(r), "a retired id is not also live");
    }
}

#[test]
fn a_retired_id_is_never_reissued() {
    // Rule 3. A retired block's ExecutionRecords stay in the append-only ledger;
    // reissuing its id would silently re-attach that history to new code.
    let mut alloc = BlockIdAlloc::resuming_after(BlockId(3));
    let ids0 = [BlockId(1), BlockId(2), BlockId(3)];
    let a = reconcile(&[h(1), h(2), h(3)], &ids0, &[h(1)], &mut alloc);
    assert_eq!(a.retired, vec![BlockId(2), BlockId(3)]);

    let b = reconcile(&[h(1)], &a.blocks, &[h(1), h(5), h(6)], &mut alloc);
    for fresh in &b.blocks[1..] {
        assert!(
            !a.retired.contains(fresh),
            "{fresh:?} was retired and must not come back"
        );
    }
    assert_eq!(b.blocks, vec![BlockId(1), BlockId(4), BlockId(5)]);
}

#[test]
fn an_empty_document_retires_everything_and_a_first_edit_allocates_from_one() {
    let mut alloc = BlockIdAlloc::new();
    let r = reconcile(&[], &[], &[h(1), h(2)], &mut alloc);
    assert_eq!(r.blocks, vec![BlockId(1), BlockId(2)]);
    assert!(BlockId(1).is_real() && BlockId(2).is_real());

    let empty = reconcile(&[h(1), h(2)], &r.blocks, &[], &mut alloc);
    assert!(empty.blocks.is_empty());
    assert_eq!(empty.retired, vec![BlockId(1), BlockId(2)]);
}

// ---------------------------------------------------------------------------
// Trivia (A3)
// ---------------------------------------------------------------------------

fn region(index: u32, hash: u8, kind: RegionKind) -> RegionSummary {
    let start = index * 10;
    RegionSummary {
        index,
        span: Span {
            start,
            end: start + 9,
        },
        outer_span: Span {
            start,
            end: start + 9,
        },
        lines: LineRange {
            start: index,
            end: index,
        },
        code_lines: LineRange {
            start: index,
            end: index,
        },
        kind,
        entry_delimiter: Delimiter::Cr,
        exit_delimiter: Delimiter::Cr,
        code_hash: h(hash),
        hash_ordinal: 0,
        canonical: None,
        is_estimation: false,
        has_macro_in_head: false,
        section: None,
    }
}

fn code(index: u32, hash: u8) -> RegionSummary {
    region(index, hash, RegionKind::Simple)
}

fn comment(index: u32) -> RegionSummary {
    region(index, 0, RegionKind::Trivia { has_marker: false })
}

#[test]
fn a_comment_carries_no_identity_and_inserting_one_moves_nothing() {
    // Spec §23's promise, made literal: comments are held out of the diff, so
    // adding one above a block cannot move any identity. `CodeHash` is already
    // over the canonical token stream rather than the bytes, which is the same
    // property one level down.
    let mut alloc = BlockIdAlloc::new();
    let before = vec![code(0, 1), code(1, 2)];
    let first = reconcile_regions(&before, &[BlockId::NONE; 2], &before, &mut alloc);
    assert_eq!(first.blocks, vec![BlockId(1), BlockId(2)]);

    let after = vec![comment(0), code(1, 1), comment(2), code(3, 2)];
    let second = reconcile_regions(&before, &first.blocks, &after, &mut alloc);
    assert_eq!(
        second.blocks,
        vec![BlockId::NONE, BlockId(1), BlockId::NONE, BlockId(2)],
        "trivia gets NONE (A3), and the two commands keep their ids"
    );
    assert_eq!(second.allocated, 0);
    assert!(second.retired.is_empty());
}

#[test]
fn trivia_is_none_and_not_ephemeral() {
    // A3: conflating them made a command-bar `StatusChanged { [(BlockId(0), …)] }`
    // repaint every comment region in the document with that status.
    let mut alloc = BlockIdAlloc::new();
    let regions = vec![comment(0), code(1, 1)];
    let r = reconcile_regions(&[], &[], &regions, &mut alloc);
    assert_eq!(r.blocks[0], BlockId::NONE);
    assert_ne!(r.blocks[0], BlockId::EPHEMERAL);
    assert!(!r.blocks[0].is_real());
    assert!(r.blocks[1].is_real());
}

#[test]
fn a_brace_block_keeps_identity_the_same_way_a_simple_one_does() {
    let mut alloc = BlockIdAlloc::new();
    let before = vec![
        code(0, 1),
        region(
            1,
            2,
            RegionKind::Brace {
                opener: BraceOpener::Foreach,
            },
        ),
    ];
    let a = reconcile_regions(&[], &[], &before, &mut alloc);
    let after = vec![
        code(0, 1),
        region(
            1,
            9,
            RegionKind::Brace {
                opener: BraceOpener::Foreach,
            },
        ),
    ];
    let b = reconcile_regions(&before, &a.blocks, &after, &mut alloc);
    assert_eq!(
        b.blocks, a.blocks,
        "editing the loop body keeps the loop's id"
    );
}
