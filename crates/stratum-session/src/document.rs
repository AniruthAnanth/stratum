//! The document model: `stratum_parse::segment`, then
//! `stratum_runtime::doc::reconcile`.
//!
//! This module holds the *state* — text, version, and the [`BlockId`] currently
//! attached to each region — and assembles the [`BlockMap`] the frontend reads.
//! It holds **no diff algorithm**. CONTRACTS §2 makes reconcile normative and
//! places it in `stratum_runtime::doc`; a Myers diff here would be a second
//! implementation of a normative algorithm, and the two would disagree on the
//! first `Replace` hunk with unequal arms. `similar` is deliberately absent from
//! this crate's manifest so that "just do the diff here" is not reachable.
//!
//! # What this crate does own
//!
//! Everything identity-adjacent that is *not* the diff:
//!
//! * **Which regions are eligible.** `RegionKind::Trivia` gets `BlockId::NONE`
//!   (A3), and `reconcile_regions` holds it out of the sequence being matched —
//!   which is what makes spec §23's promise literal rather than approximate.
//! * **Where fresh ids come from.** CONTRACTS §2 rule 3 says "the session
//!   counter", and [`Session::alloc_block_id`] is it. Ids are never reused, so a
//!   retired block's `ExecutionRecord`s in the append-only ledger cannot be
//!   silently re-attached to code the user never ran.
//! * **The generation counter**, which is how the frontend drops out-of-order
//!   maps, and the `doc_version` it was computed against.
//! * **Retirement**, which is a reconcile *outcome*, not a close-tab event.
//!
//! # Cost
//!
//! One `segment` pass over the buffer, one Myers diff over the executable
//! hashes, one `BlockMap`. Nothing here is O(rows) and nothing here touches the
//! dataset. [`Applied::allocated`] is the counter the keystroke path is asserted
//! on, per ADR-017 — a steady-state edit allocates zero ids.

use indexmap::IndexMap;
use stratum_parse::segment;
use stratum_proto::{
    Block, BlockId, BlockMap, CellMarker, Delimiter, Diagnostic, DocumentId, RegionSummary,
    SectionSpan,
};
use stratum_runtime::doc::{is_executable, reconcile_regions, BlockIdAlloc, DocumentModel};

use crate::session::Session;

/// One open document.
#[derive(Clone, PartialEq, Debug)]
struct DocumentState {
    text: String,
    /// The version the frontend stamped on the `doc_change` this state was
    /// built from.
    version: u64,
    /// Bumps on every reconcile. Monotone per document and never restarted:
    /// the frontend drops maps that arrive out of order.
    generation: u64,
    /// Parallel to `regions`.
    blocks: Vec<BlockId>,
    regions: Vec<RegionSummary>,
    /// `DocumentModel::blocks` returns `&[Block]`, so the projection is
    /// materialised once per reconcile rather than rebuilt per call —
    /// `stratum-exec` asks for it on every plan resolution.
    projected: Vec<Block>,
    markers: Vec<CellMarker>,
    sections: Vec<SectionSpan>,
    diagnostics: Vec<Diagnostic>,
    end_delimiter: Delimiter,
}

/// Every document this session knows about.
///
/// Insertion-ordered so that a `SessionSnapshot`'s `docs` vector arrives in a
/// stable order — a late-joining window that received them in hash order would
/// paint its tabs differently on every reconnect.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Documents {
    docs: IndexMap<DocumentId, DocumentState>,
}

impl Documents {
    /// No documents.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many documents are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// True when nothing is open — which is what a fresh session looks like.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// The document ids, in open order.
    pub fn ids(&self) -> impl Iterator<Item = DocumentId> + '_ {
        self.docs.keys().copied()
    }

    /// Forget a document. Its blocks are **not** retired: retirement is a
    /// reconcile outcome that the ledger reads, and closing a tab is not an
    /// edit.
    pub fn close(&mut self, doc: DocumentId) -> bool {
        self.docs.shift_remove(&doc).is_some()
    }
}

/// The result of applying a text change: the map, plus the counters ADR-017
/// wants asserted instead of a duration.
#[derive(Clone, PartialEq, Debug)]
pub struct Applied {
    /// What the frontend receives.
    pub map: BlockMap,
    /// `BlockId`s minted by this reconcile. **Zero for an edit inside an
    /// existing block** — that is the property that makes identity survive
    /// typing, and it is a count, not a time.
    pub allocated: u32,
    /// Executable regions in the new segmentation. The diff is O(this), not
    /// O(bytes) and certainly not O(rows).
    pub executable: u32,
}

impl Session {
    /// The documents this session has open.
    #[must_use]
    pub fn documents(&self) -> &Documents {
        &self.docs
    }

    /// The generation of the last [`BlockMap`] emitted for `doc`.
    #[must_use]
    pub fn document_generation(&self, doc: DocumentId) -> Option<u64> {
        self.docs.docs.get(&doc).map(|d| d.generation)
    }

    /// The editor's version counter for `doc`, `0` when it is not open.
    ///
    /// Named `document_version` rather than `version` because
    /// [`Session::version`] is checklist item 10 — the *Stata language* version
    /// — and an inherent method shadows a trait method of the same name at
    /// every call site. Two different meanings of "version" resolving by
    /// argument count is exactly the ambiguity a reader should not have to
    /// resolve, so the document one is spelled out.
    #[must_use]
    pub fn document_version(&self, doc: DocumentId) -> u64 {
        self.docs.docs.get(&doc).map_or(0, |d| d.version)
    }

    /// Segment `text` and reconcile it against what this session holds for
    /// `doc`.
    ///
    /// This is the full path, and it is the plan's sentence compiled: *segment*
    /// with `stratum_parse::segment`, *reconcile* with
    /// `stratum_runtime::doc::reconcile_regions`. Opening a document is the same
    /// call against an empty previous state, so there is one code path rather
    /// than an `open` that drifts from a `change`.
    pub fn apply_document_change(
        &mut self,
        doc: DocumentId,
        text: String,
        version: u64,
    ) -> Applied {
        let (regions, markers, sections, diagnostics, end_delimiter) = {
            let seg = segment(&text);
            (
                seg.summaries(),
                seg.markers.clone(),
                seg.sections.clone(),
                seg.diags.clone(),
                seg.end_delimiter,
            )
        };
        self.identify(doc, regions, version, |st| {
            st.markers = markers;
            st.sections = sections;
            st.diagnostics = diagnostics;
            st.end_delimiter = end_delimiter;
            st.text = text;
        })
    }

    /// Forget a document.
    ///
    /// Its blocks are **not** retired: retirement is a reconcile outcome the
    /// ledger reads, and closing a tab is not an edit. Reopening the same file
    /// therefore starts from no previous state and mints fresh ids, which is
    /// honest — we cannot claim the text on disk is the text we last identified.
    pub fn close_document(&mut self, doc: DocumentId) -> bool {
        self.docs.close(doc)
    }

    /// The identity half, shared by [`Session::apply_document_change`] and by
    /// the `DocumentModel::reconcile` impl.
    ///
    /// `finish` installs whatever the caller knows that the region vector does
    /// not carry — text, markers, sections, diagnostics — onto the state that is
    /// about to be stored.
    fn identify(
        &mut self,
        doc: DocumentId,
        regions: Vec<RegionSummary>,
        version: u64,
        finish: impl FnOnce(&mut DocumentState),
    ) -> Applied {
        // The allocator is a *cursor over the session counter*, not a second
        // counter: it resumes after the highest id this session has issued and
        // the session counter is advanced by exactly what it consumed. Two
        // documents in one session therefore never collide, which a per-document
        // allocator would not guarantee.
        let mut alloc = BlockIdAlloc::resuming_after(self.high_block_id());
        // The previous state is borrowed for exactly as long as the diff needs
        // it and not one statement longer: carrying the borrow past this block
        // would force a clone of the previous text on every keystroke, which is
        // O(bytes) work on the interaction path for a string we are about to
        // overwrite.
        let out = {
            let prev = self.docs.docs.get(&doc);
            let (prev_regions, prev_ids): (&[RegionSummary], &[BlockId]) = prev
                .map_or((&[][..], &[][..]), |d| {
                    (d.regions.as_slice(), d.blocks.as_slice())
                });
            reconcile_regions(prev_regions, prev_ids, &regions, &mut alloc)
        };
        self.set_high_block_id(alloc.high_water());

        // Counted here rather than at the one call site that has the text, so
        // that the region-only path (`DocumentModel::reconcile`, which is what
        // the wasm segmenter's keystroke feeds) reports the same counter.
        let executable = u32::try_from(regions.iter().filter(|r| is_executable(&r.kind)).count())
            .unwrap_or(u32::MAX);
        let projected: Vec<Block> = regions
            .iter()
            .zip(&out.blocks)
            .filter(|(_, id)| id.is_real())
            .map(|(region, id)| Block {
                id: *id,
                region: region.clone(),
                doc,
            })
            .collect();

        // Updated in place rather than cloned-and-reinserted: a document that is
        // being typed into is the hot path, and the markers and sections a
        // region-only `reconcile` carries forward are already here.
        let state = self.docs.docs.entry(doc).or_insert_with(|| DocumentState {
            text: String::new(),
            version,
            // 0, so the increment below makes the first map generation 1 — the
            // frontend treats 0 as "no map seen yet".
            generation: 0,
            blocks: Vec::new(),
            regions: Vec::new(),
            projected: Vec::new(),
            markers: Vec::new(),
            sections: Vec::new(),
            diagnostics: Vec::new(),
            end_delimiter: Delimiter::Cr,
        });
        state.version = version;
        state.generation = state.generation.saturating_add(1);
        state.blocks = out.blocks.clone();
        state.regions = regions.clone();
        state.projected = projected;
        finish(state);

        let map = BlockMap {
            doc,
            generation: state.generation,
            doc_version: version,
            blocks: out.blocks,
            regions,
            markers: state.markers.clone(),
            sections: state.sections.clone(),
            retired: out.retired,
            diagnostics: state.diagnostics.clone(),
            end_delimiter: state.end_delimiter,
        };
        Applied {
            map,
            allocated: out.allocated,
            executable,
        }
    }
}

/// CONTRACTS §13 / C25: declared in `stratum_runtime::doc`, implemented here,
/// consumed by `stratum-exec`.
impl DocumentModel for Session {
    fn blocks(&self, doc: DocumentId) -> &[Block] {
        self.docs
            .docs
            .get(&doc)
            .map_or(&[][..], |d| d.projected.as_slice())
    }

    fn text(&self, doc: DocumentId) -> &str {
        self.docs.docs.get(&doc).map_or("", |d| d.text.as_str())
    }

    fn version(&self, doc: DocumentId) -> u64 {
        self.document_version(doc)
    }

    /// Re-identify against a segmentation the caller already has.
    ///
    /// The editor segments in wasm on every keystroke (CONTRACTS §14), so the
    /// engine is regularly handed regions rather than text. Markers, sections
    /// and diagnostics are carried forward from the stored state, because a
    /// region vector does not carry them; a caller that has new text should use
    /// [`Session::apply_document_change`], which does.
    fn reconcile(&mut self, doc: DocumentId, new_regions: Vec<RegionSummary>) -> BlockMap {
        let version = self.document_version(doc);
        self.identify(doc, new_regions, version, |_| {}).map
    }
}
