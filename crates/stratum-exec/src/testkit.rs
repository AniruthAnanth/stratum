//! Fixtures shared by this crate's tests and by downstream units' tests.
//!
//! Behind the `testkit` feature and `cfg(test)`; never compiled into a shipped
//! engine. The point is that W08b, W09 and the e2e scenarios build a document
//! the same way this crate's own tests do, so a fixture that drifts breaks one
//! suite loudly rather than every suite subtly.
//!
//! Nothing here fabricates a *staleness* answer. These builders produce inputs —
//! a `BlockMap`, an `EffectSet`, a code hash — and the sweep is left to say what
//! it says about them.

use std::sync::Arc;

use stratum_effects::EffectSet;
use stratum_proto::{
    BlockId, BlockMap, CodeHash, Delimiter, DocumentId, LineRange, RegionKind, RegionSummary, Span,
};

use crate::staleness::AnalysedDoc;

/// A distinct [`CodeHash`] per `tag`. Distinctness is the only property any test
/// here depends on; the real hash is over the canonical token stream and is
/// `stratum-parse`'s to compute.
#[must_use]
pub fn code_hash(tag: u64) -> CodeHash {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&tag.to_le_bytes());
    CodeHash(bytes)
}

/// One executable `Simple` region carrying `hash`.
#[must_use]
pub fn region(index: u32, hash: CodeHash) -> RegionSummary {
    let start = index * 16;
    RegionSummary {
        index,
        span: Span {
            start,
            end: start + 16,
        },
        outer_span: Span {
            start,
            end: start + 16,
        },
        lines: LineRange {
            start: index,
            end: index + 1,
        },
        code_lines: LineRange {
            start: index,
            end: index + 1,
        },
        kind: RegionKind::Simple,
        entry_delimiter: Delimiter::Cr,
        exit_delimiter: Delimiter::Cr,
        code_hash: hash,
        hash_ordinal: 0,
        canonical: None,
        is_estimation: false,
        has_macro_in_head: false,
        section: None,
    }
}

/// A document of `hashes.len()` executable blocks, ids `BlockId(1..=n)`.
///
/// Block ids are handed out here rather than by [`crate::IdAllocator`] so a test
/// can name the block it is asserting about. Real ids come from the allocator
/// and only from it (CONTRACTS §2).
#[must_use]
pub fn doc(id: DocumentId, generation: u64, hashes: &[CodeHash]) -> AnalysedDoc {
    let regions: Vec<RegionSummary> = hashes
        .iter()
        .enumerate()
        .map(|(i, h)| region(u32::try_from(i).unwrap_or(u32::MAX), *h))
        .collect();
    let blocks: Vec<BlockId> = (1..=hashes.len() as u64).map(BlockId).collect();
    let effects: Vec<Arc<EffectSet>> = hashes
        .iter()
        .map(|_| Arc::new(EffectSet::default()))
        .collect();
    AnalysedDoc::new(
        BlockMap {
            doc: id,
            generation,
            doc_version: generation,
            blocks,
            regions,
            markers: Vec::new(),
            sections: Vec::new(),
            retired: Vec::new(),
            diagnostics: Vec::new(),
            end_delimiter: Delimiter::Cr,
        },
        effects,
    )
}
