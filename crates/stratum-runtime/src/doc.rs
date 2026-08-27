//! `DocumentModel` and `reconcile` — CONTRACTS §2, ARCHITECTURE C25.
//!
//! Block identity has to survive editing, because every `ExecutionRecord`, every
//! inline result card and every status glyph is keyed by `BlockId`. If typing a
//! character inside a block turned it into a delete plus an insert, the block's
//! history and its output card would vanish on every keystroke.
//!
//! The algorithm and the trait live here rather than in `stratum-parse` or
//! `stratum-session` because they need **hash sequences and nothing else** —
//! no parsing, no session (C25). `stratum-session` owns the concrete `Document`
//! and implements [`DocumentModel`] by calling `stratum_parse`'s segmenter and
//! then [`reconcile`]; the algorithm is not duplicated there.
//!
//! # Trivia is not in the diff
//!
//! Comments and blank lines are `RegionKind::Trivia` and get `BlockId::NONE`
//! (A3) — *not* `EPHEMERAL`, which means "a command-bar run". They are removed
//! before diffing and spliced back after. That is what makes spec §23's promise
//! literal: adding a comment above a block cannot move any identity, because the
//! comment was never in the sequence being matched. `CodeHash` is already
//! computed over the canonical token stream rather than the bytes, so
//! reindentation and `///` reflow are staleness-neutral for the same reason one
//! level down.

use similar::{capture_diff_slices, Algorithm, DiffOp};
use stratum_proto::{Block, BlockId, BlockMap, CodeHash, DocumentId, RegionKind, RegionSummary};

/// Allocates `BlockId`s for one session. Ids are never reused (CONTRACTS §2
/// rule 3): a retired block's `ExecutionRecord`s stay in the append-only ledger,
/// and re-issuing its id would silently re-attach that history to new code.
#[derive(Clone, Debug)]
pub struct BlockIdAlloc {
    next: u64,
}

impl Default for BlockIdAlloc {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockIdAlloc {
    /// A fresh allocator. The first id issued is `B1`: `BlockId(0)` is
    /// `EPHEMERAL` and `BlockId(u64::MAX)` is `NONE`.
    #[must_use]
    pub fn new() -> Self {
        Self { next: 0 }
    }

    /// An allocator that will not re-issue anything at or below `high`.
    #[must_use]
    pub fn resuming_after(high: BlockId) -> Self {
        Self {
            next: if high.is_real() { high.0 } else { 0 },
        }
    }

    /// The next unused id.
    pub fn alloc(&mut self) -> BlockId {
        self.next += 1;
        debug_assert!(self.next < u64::MAX, "BlockId::NONE reached");
        BlockId(self.next)
    }

    /// The highest id issued so far.
    #[must_use]
    pub fn high_water(&self) -> BlockId {
        BlockId(self.next)
    }
}

/// The outcome of one reconcile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reconciled {
    /// Parallel to the new sequence: `blocks[i]` is the identity of item `i`.
    pub blocks: Vec<BlockId>,
    /// Blocks that were in the document and are not any more. Their records
    /// stay in the ledger; the UI removes their widgets.
    pub retired: Vec<BlockId>,
    /// Ids allocated by this reconcile. A counter, per ADR-017 — the keystroke
    /// path is asserted on this rather than on a duration.
    pub allocated: u32,
}

/// CONTRACTS §2's reconcile contract, over `CodeHash` sequences.
///
/// 1. Myers diff. Equal runs map 1:1 and keep their `BlockId`.
/// 2. Inside each `Replace` hunk, map positionally `old[i] → new[i]` for
///    `i < min(len_old, len_new)`, keeping the id. **This is what makes "I
///    edited this block" keep identity rather than becoming delete+insert.**
/// 3. Surplus new items get fresh ids; surplus old items are retired.
///
/// A split `{A B}` → `{A}{B}` keeps the id on the first fragment; a merge keeps
/// the first's. The second fragment becomes `NeverRun`, which is honest — we
/// have never run *that* code.
///
/// # Panics
///
/// Debug-asserts that `prev` and `prev_ids` are the same length. In release the
/// shorter of the two bounds the mapping, so a caller mismatch degrades to
/// "these blocks are new" rather than to a panic in the engine's control thread.
#[must_use]
pub fn reconcile(
    prev: &[CodeHash],
    prev_ids: &[BlockId],
    next: &[CodeHash],
    alloc: &mut BlockIdAlloc,
) -> Reconciled {
    debug_assert_eq!(
        prev.len(),
        prev_ids.len(),
        "hash and id sequences must align"
    );

    let mut blocks = vec![BlockId::NONE; next.len()];
    let mut kept = vec![false; prev.len()];
    let mut allocated = 0u32;

    let id_at = |i: usize| prev_ids.get(i).copied().filter(|b| b.is_real());

    for op in capture_diff_slices(Algorithm::Myers, prev, next) {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for k in 0..len {
                    if let Some(id) = id_at(old_index + k) {
                        blocks[new_index + k] = id;
                        kept[old_index + k] = true;
                    }
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                // Rule 2. Lowest ordinal first, which is also how ties break.
                for k in 0..old_len.min(new_len) {
                    if let Some(id) = id_at(old_index + k) {
                        blocks[new_index + k] = id;
                        kept[old_index + k] = true;
                    }
                }
            }
            DiffOp::Delete { .. } | DiffOp::Insert { .. } => {}
        }
    }

    for slot in &mut blocks {
        if !slot.is_real() {
            *slot = alloc.alloc();
            allocated += 1;
        }
    }

    let retired = prev_ids
        .iter()
        .enumerate()
        .filter(|(i, id)| id.is_real() && !kept.get(*i).copied().unwrap_or(false))
        .map(|(_, id)| *id)
        .collect();

    Reconciled {
        blocks,
        retired,
        allocated,
    }
}

/// [`reconcile`] over regions, with `RegionKind::Trivia` held out of the diff
/// and given `BlockId::NONE` (A3).
///
/// `prev_ids` is parallel to `prev`, exactly as `BlockMap::blocks` is parallel to
/// `BlockMap::regions`, so a caller can feed the previous `BlockMap` straight
/// back in.
#[must_use]
pub fn reconcile_regions(
    prev: &[RegionSummary],
    prev_ids: &[BlockId],
    next: &[RegionSummary],
    alloc: &mut BlockIdAlloc,
) -> Reconciled {
    let (prev_hashes, prev_exec_ids): (Vec<CodeHash>, Vec<BlockId>) = prev
        .iter()
        .enumerate()
        .filter(|(_, r)| is_executable(&r.kind))
        .map(|(i, r)| {
            (
                r.code_hash,
                prev_ids.get(i).copied().unwrap_or(BlockId::NONE),
            )
        })
        .unzip();
    let next_hashes: Vec<CodeHash> = next
        .iter()
        .filter(|r| is_executable(&r.kind))
        .map(|r| r.code_hash)
        .collect();

    let inner = reconcile(&prev_hashes, &prev_exec_ids, &next_hashes, alloc);

    let mut blocks = Vec::with_capacity(next.len());
    let mut it = inner.blocks.iter();
    for r in next {
        if is_executable(&r.kind) {
            blocks.push(it.next().copied().unwrap_or(BlockId::NONE));
        } else {
            blocks.push(BlockId::NONE);
        }
    }
    Reconciled { blocks, ..inner }
}

/// Does this region carry block identity?
///
/// Everything but `Trivia`. A `Directive` (`#delimit`) is executable — it mutates
/// scanner state — and `Unterminated` is executable behind an explicit override,
/// so both keep identity.
#[must_use]
pub fn is_executable(kind: &RegionKind) -> bool {
    !matches!(kind, RegionKind::Trivia { .. })
}

/// The document surface `stratum-exec` runs against — CONTRACTS §13.
///
/// Declared here, implemented by `stratum-session` (C25/C49).
pub trait DocumentModel {
    /// The document's blocks, in document order.
    fn blocks(&self, doc: DocumentId) -> &[Block];
    /// The document's text.
    fn text(&self, doc: DocumentId) -> &str;
    /// The editor's version counter for this document.
    fn version(&self, doc: DocumentId) -> u64;
    /// Re-segment and re-identify. MUST preserve ids per CONTRACTS §2 — i.e.
    /// MUST go through [`reconcile`] rather than reimplementing it.
    fn reconcile(&mut self, doc: DocumentId, new_regions: Vec<RegionSummary>) -> BlockMap;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> CodeHash {
        CodeHash([n; 16])
    }

    fn run(prev: &[u8], next: &[u8], alloc: &mut BlockIdAlloc) -> (Vec<BlockId>, Vec<BlockId>) {
        let ph: Vec<CodeHash> = prev.iter().map(|n| h(*n)).collect();
        let ids: Vec<BlockId> = (1..=prev.len() as u64).map(BlockId).collect();
        let nh: Vec<CodeHash> = next.iter().map(|n| h(*n)).collect();
        let r = reconcile(&ph, &ids, &nh, alloc);
        (r.blocks, r.retired)
    }

    #[test]
    fn an_edit_in_place_keeps_the_id() {
        let mut a = BlockIdAlloc::resuming_after(BlockId(3));
        let (blocks, retired) = run(&[1, 2, 3], &[1, 9, 3], &mut a);
        assert_eq!(blocks, vec![BlockId(1), BlockId(2), BlockId(3)]);
        assert!(retired.is_empty());
    }

    #[test]
    fn identical_input_allocates_nothing() {
        let mut a = BlockIdAlloc::resuming_after(BlockId(3));
        let r = reconcile(
            &[h(1), h(2), h(3)],
            &[BlockId(1), BlockId(2), BlockId(3)],
            &[h(1), h(2), h(3)],
            &mut a,
        );
        assert_eq!(r.allocated, 0);
        assert_eq!(a.high_water(), BlockId(3));
    }
}
