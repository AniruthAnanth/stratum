//! The command surface — one function per CONTRACTS §11 row this crate owns.
//!
//! The Tauri layer (W07) is a thin `async` wrapper over these; everything here
//! is synchronous, because none of it does anything an `await` would help with.
//! Keeping the logic on this side of that boundary is what lets the whole
//! surface be exercised by the tests in `crates/stratum-workspace/tests/`
//! without a webview, a Tauri runtime, or the engine process.
//!
//! # The invariant this file is responsible for
//!
//! Every path that ends in bytes on disk goes through
//! [`crate::write::write_document`] with a [`crate::write::GatedEdits`]. There
//! are exactly four such paths here — [`Workspace::doc_save`],
//! [`Workspace::section_rename`], [`Workspace::section_move`] and
//! [`Workspace::ai_apply_patch`] — and each obtains its `GatedEdits` from the
//! constructor that runs its gate. Adding a fifth means writing a
//! `GatedEdits` constructor, which means choosing a gate.

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use stratum_proto::{BlockMap, DocumentId, Edit, SectionId, SessionId};

use crate::bytes::DocBytes;
use crate::document::{Document, RefusedOpen};
use crate::keymap::{KeyBinding, KeymapError, KeymapPreset, KeymapStore, Platform};
use crate::layout::{LayoutError, LayoutSpec, LayoutStore};
use crate::project::{Claim, Claims, Project, ProjectError, WorkspaceState};
use crate::sections::{self, MovedSection, RenamedSection, Section};
use crate::sidecar_cache::{CachePaths, CacheSidecar};
use crate::sidecar_durable::{DurableSidecar, DurableSidecarPatch, SidecarError};
use crate::write::{
    write_document, Check, EditGate, GateRejection, GatedEdits, SavedAck, StandaloneGate,
    WriteError,
};

/// `doc_open`'s reply — CONTRACTS §11's `DocumentOpened`, minus the `blockMap`,
/// which the engine supplies.
#[derive(Clone, PartialEq, Debug)]
pub struct DocumentOpened {
    /// The id assigned to this buffer.
    pub doc: DocumentId,
    /// Where it came from. `None` for an untitled buffer.
    pub path: Option<Utf8PathBuf>,
    /// The text, LF-normalised.
    pub text: String,
    /// Starts at 0.
    pub version: u64,
    /// The durable sidecar, reconciled against the text.
    pub sidecar: DurableSidecar,
    /// **A24.** The byte policy, echoed so the frontend can show it in the
    /// status bar. `eol` and `bom` are separate fields on the wire.
    pub bytes: DocBytes,
    /// `L013` when the file mixed line endings; `STRATUM0601` when it was opened
    /// read-only.
    pub diagnostics: Vec<stratum_proto::Diagnostic>,
    /// True when the file could not be decoded and is displayed only.
    pub read_only: bool,
    /// Sections found in the text.
    pub sections: Vec<Section>,
}

/// Anything a command here can fail with.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// No such open document.
    #[error("no open document {0}")]
    NoSuchDocument(DocumentId),
    /// An untitled buffer cannot be saved without a path.
    #[error("document {0} has no path; save-as must supply one")]
    NoPath(DocumentId),
    /// `doc_change` arrived against a version that is no longer current.
    #[error("document {doc} is at version {have}, edit was computed against {want}")]
    StaleVersion {
        /// The document.
        doc: DocumentId,
        /// The version the caller expected.
        want: u64,
        /// The version the buffer is actually at.
        have: u64,
    },
    /// A write or a gate failed.
    #[error(transparent)]
    Write(#[from] WriteError),
    /// The durable sidecar could not be read or written.
    #[error(transparent)]
    Sidecar(#[from] SidecarError),
    /// The layout store failed.
    #[error(transparent)]
    Layout(#[from] LayoutError),
    /// The keymap store failed.
    #[error(transparent)]
    Keymap(#[from] KeymapError),
    /// The project state failed.
    #[error(transparent)]
    Project(#[from] ProjectError),
    /// The filesystem said no.
    #[error("{path}: {source}")]
    Io {
        /// Path involved.
        path: Utf8PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

struct Open {
    doc: Document,
    sidecar: DurableSidecar,
    cache: CacheSidecar,
    cache_paths: CachePaths,
}

/// One open project's worth of desktop-side state.
pub struct Workspace {
    /// The project.
    pub project: Project,
    /// Layout persistence.
    pub layouts: LayoutStore,
    /// Keymap persistence.
    pub keymaps: KeymapStore,
    /// Which window owns which `.do`.
    pub claims: Claims,
    /// The platform accelerators are rendered for.
    pub platform: Platform,
    gate: Box<dyn EditGate>,
    docs: BTreeMap<DocumentId, Open>,
    next_doc: u32,
}

impl Workspace {
    /// Open a project with the default gate.
    ///
    /// The default is [`StandaloneGate`]. When `stratum-intel` (W20) lands, the
    /// desktop passes its implementation to [`Workspace::with_gate`] and nothing
    /// else changes — that is the whole point of the seam.
    pub fn new(project: Project, layouts: LayoutStore, keymaps: KeymapStore) -> Self {
        Self::with_gate(project, layouts, keymaps, Box::new(StandaloneGate))
    }

    /// Open a project with an explicit equivalence gate.
    pub fn with_gate(
        project: Project,
        layouts: LayoutStore,
        keymaps: KeymapStore,
        gate: Box<dyn EditGate>,
    ) -> Self {
        Workspace {
            project,
            layouts,
            keymaps,
            claims: Claims::new(),
            platform: Platform::host(),
            gate,
            docs: BTreeMap::new(),
            next_doc: 1,
        }
    }

    fn get(&self, doc: DocumentId) -> Result<&Open, WorkspaceError> {
        self.docs
            .get(&doc)
            .ok_or(WorkspaceError::NoSuchDocument(doc))
    }

    fn get_mut(&mut self, doc: DocumentId) -> Result<&mut Open, WorkspaceError> {
        self.docs
            .get_mut(&doc)
            .ok_or(WorkspaceError::NoSuchDocument(doc))
    }

    /// The buffer, for callers that need the text (segmentation, completion).
    pub fn document(&self, doc: DocumentId) -> Result<&Document, WorkspaceError> {
        Ok(&self.get(doc)?.doc)
    }

    // -- documents ----------------------------------------------------------

    /// `doc_open { session, path }`.
    ///
    /// `Err(RefusedOpen)` means the bytes are not UTF-8. **The file is not
    /// touched**; offer [`Workspace::doc_open_read_only`].
    pub fn doc_open(&mut self, path: &Utf8Path) -> Result<DocumentOpened, Box<OpenFailure>> {
        let raw = std::fs::read(path).map_err(|source| {
            Box::new(OpenFailure::Io {
                path: path.to_owned(),
                source,
            })
        })?;
        let id = self.alloc();
        let doc = Document::open(id, path, &raw).map_err(|r| Box::new(OpenFailure::Refused(*r)))?;
        Ok(self.admit(doc))
    }

    /// Open a file we refused to decode, for display only.
    pub fn doc_open_read_only(
        &mut self,
        path: &Utf8Path,
    ) -> Result<DocumentOpened, Box<OpenFailure>> {
        let raw = std::fs::read(path).map_err(|source| {
            Box::new(OpenFailure::Io {
                path: path.to_owned(),
                source,
            })
        })?;
        let id = self.alloc();
        Ok(self.admit(Document::open_read_only(id, path, &raw)))
    }

    /// `doc_open { session, text }` — an untitled buffer.
    pub fn doc_new(&mut self, text: impl Into<String>) -> DocumentOpened {
        let id = self.alloc();
        self.admit(Document::untitled(id, text))
    }

    fn alloc(&mut self) -> DocumentId {
        let id = DocumentId(self.next_doc);
        self.next_doc += 1;
        id
    }

    fn admit(&mut self, doc: Document) -> DocumentOpened {
        let path = doc.path.clone();
        let mut sidecar = path
            .as_deref()
            .map(DurableSidecar::load)
            .transpose()
            // A malformed sidecar must not stop the document opening: C19 says
            // this crate tolerates it being absent OR STALE, and "unparseable"
            // is the limiting case of stale. The `.do` is the truth.
            .unwrap_or(None)
            .unwrap_or_default();
        sidecar.reconcile(&doc.text);
        if !doc.read_only {
            sidecar.eol = doc.bytes.eol;
            sidecar.bom = doc.bytes.bom;
        }

        let cache_paths = CachePaths::for_document(
            &self.project.root,
            path.as_deref().unwrap_or(Utf8Path::new("untitled.do")),
        );
        let cache = CacheSidecar::load(&cache_paths);

        let opened = DocumentOpened {
            doc: doc.id,
            path: path.clone(),
            text: doc.text.clone(),
            version: doc.version,
            sidecar: sidecar.clone(),
            bytes: doc.bytes,
            diagnostics: doc.diagnostics.clone(),
            read_only: doc.read_only,
            sections: sections::index(&doc.text),
        };
        self.docs.insert(
            doc.id,
            Open {
                doc,
                sidecar,
                cache,
                cache_paths,
            },
        );
        opened
    }

    /// `doc_change { session, doc, version, edits }`.
    pub fn doc_change(
        &mut self,
        doc: DocumentId,
        version: u64,
        edits: &[Edit],
    ) -> Result<u64, WorkspaceError> {
        let open = self.get_mut(doc)?;
        if open.doc.version != version {
            return Err(WorkspaceError::StaleVersion {
                doc,
                want: version,
                have: open.doc.version,
            });
        }
        open.doc.apply(edits).map_err(WriteError::from)?;
        open.sidecar.reconcile(&open.doc.text);
        Ok(open.doc.version)
    }

    /// **Writer 1 of 4** — `doc_save { session, doc }`.
    ///
    /// Reproduces the recorded EOL and BOM byte for byte and transforms nothing
    /// else. Saving a file you have not edited rewrites the same bytes.
    pub fn doc_save(&mut self, doc: DocumentId) -> Result<SavedAck, WorkspaceError> {
        let open = self.get(doc)?;
        let path = open.doc.path.clone().ok_or(WorkspaceError::NoPath(doc))?;
        if open.doc.read_only {
            return Err(WriteError::ReadOnly { path }.into());
        }
        let gated = GatedEdits::byte_fidelity(open.doc.text.clone());
        let ack = write_document(&path, open.doc.bytes, &gated)?;

        // The sidecar is written beside the document on every save so that the
        // two never disagree about the byte policy — but only when it carries
        // something. A project with no sections and no collapse intent should not
        // acquire a sidecar just for existing.
        let open = self.get_mut(doc)?;
        if !open.sidecar.sections.is_empty()
            || !open.sidecar.collapsed.is_empty()
            || !open.sidecar.pinned_comparisons.is_empty()
            || !open.sidecar.auto_comment_anchors.is_empty()
            || !open.sidecar.ai_conversations.is_empty()
        {
            open.sidecar.save(&path)?;
        }
        Ok(ack)
    }

    /// `doc_close { session, doc }`. Persists the volatile cache and releases the
    /// claim; does **not** save the document.
    pub fn doc_close(&mut self, doc: DocumentId) -> Result<(), WorkspaceError> {
        if let Some(open) = self.docs.remove(&doc) {
            let _ = open.cache.save(&open.cache_paths);
            if let Some(p) = open.doc.path.as_deref() {
                self.claims.release(p);
            }
        }
        Ok(())
    }

    /// **Writer 2 of 4** — `section_rename { session, doc, section, title }`.
    ///
    /// Gated by `assert_comment_only`.
    pub fn section_rename(
        &mut self,
        doc: DocumentId,
        section: SectionId,
        title: &str,
    ) -> Result<RenamedSection, WorkspaceError> {
        let open = self.get(doc)?;
        let (gated, result) = sections::rename(&open.doc, section, title, self.gate.as_ref())?;
        self.commit(doc, gated)?;
        Ok(result)
    }

    /// **Writer 3 of 4** — `section_move { session, doc, section, before }`.
    ///
    /// Gated by `assert_statement_partition_preserved`, and returns `restaled`
    /// (A15): reordering executable statements changes execution order, so the
    /// blocks at and after the earlier of the two positions have to be swept.
    pub fn section_move(
        &mut self,
        doc: DocumentId,
        section: SectionId,
        before: Option<SectionId>,
        block_map: Option<&BlockMap>,
    ) -> Result<MovedSection, WorkspaceError> {
        let open = self.get(doc)?;
        let (gated, result) =
            sections::move_section(&open.doc, section, before, block_map, self.gate.as_ref())?;
        if !result.edits.is_empty() {
            self.commit(doc, gated)?;
        }
        Ok(result)
    }

    /// **Writer 4 of 4** — `ai_apply_patch { session, doc, patch }`.
    ///
    /// `comment_scoped` mirrors the task's declared scope (spec §23): when it is
    /// set the patch must additionally pass `assert_comment_only`, and when it is
    /// not, `accepted` must record that a human read the diff and pressed Accept.
    pub fn ai_apply_patch(
        &mut self,
        doc: DocumentId,
        edits: Vec<Edit>,
        comment_scoped: bool,
        accepted: bool,
    ) -> Result<Vec<Edit>, WorkspaceError> {
        let open = self.get(doc)?;
        let gated = if comment_scoped {
            GatedEdits::ai_comment_patch(&open.doc.text, edits, self.gate.as_ref())?
        } else {
            GatedEdits::ai_accepted_patch(&open.doc.text, edits, accepted)?
        };
        let out = gated.edits().to_vec();
        self.commit(doc, gated)?;
        Ok(out)
    }

    /// Apply a gated edit set to the buffer and, if the document is on disk,
    /// write it.
    fn commit(&mut self, doc: DocumentId, gated: GatedEdits) -> Result<(), WorkspaceError> {
        let open = self.get(doc)?;
        if open.doc.read_only {
            return Err(WriteError::ReadOnly {
                path: open.doc.path.clone().unwrap_or_default(),
            }
            .into());
        }
        if let Some(path) = open.doc.path.clone() {
            write_document(&path, open.doc.bytes, &gated)?;
        }
        let open = self.get_mut(doc)?;
        open.doc.set_text(gated.text());
        open.sidecar.reconcile(&open.doc.text);
        Ok(())
    }

    // -- sidecars -----------------------------------------------------------

    /// `sidecar_get { doc }`.
    pub fn sidecar_get(&self, doc: DocumentId) -> Result<&DurableSidecar, WorkspaceError> {
        Ok(&self.get(doc)?.sidecar)
    }

    /// `sidecar_patch { doc, patch }`. Persisted immediately when the document
    /// has a path, because collapse intent that is lost on crash is intent the
    /// user has to re-express.
    pub fn sidecar_patch(
        &mut self,
        doc: DocumentId,
        patch: DurableSidecarPatch,
    ) -> Result<(), WorkspaceError> {
        let open = self.get_mut(doc)?;
        open.sidecar.patch(patch);
        if let Some(path) = open.doc.path.clone() {
            open.sidecar.save(&path)?;
        }
        Ok(())
    }

    /// The volatile cache for a document.
    pub fn cache(&self, doc: DocumentId) -> Result<&CacheSidecar, WorkspaceError> {
        Ok(&self.get(doc)?.cache)
    }

    /// Mutate and persist the volatile cache.
    pub fn cache_update(
        &mut self,
        doc: DocumentId,
        f: impl FnOnce(&mut CacheSidecar),
    ) -> Result<(), WorkspaceError> {
        let open = self.get_mut(doc)?;
        f(&mut open.cache);
        open.cache
            .save(&open.cache_paths)
            .map_err(|source| WorkspaceError::Io {
                path: open.cache_paths.scratch(),
                source,
            })
    }

    // -- layout, keymap, project -------------------------------------------

    /// `layout_load { id }`.
    pub fn layout_load(&self, id: &str) -> Result<LayoutSpec, WorkspaceError> {
        Ok(self.layouts.load(id)?)
    }

    /// `layout_save { spec }`.
    pub fn layout_save(&self, spec: &LayoutSpec) -> Result<(), WorkspaceError> {
        self.layouts.save(spec)?;
        Ok(())
    }

    /// `layout_reset { id }` — deletes only the user overlay.
    pub fn layout_reset(&self, id: &str) -> Result<bool, WorkspaceError> {
        Ok(self.layouts.reset(id)?)
    }

    /// `keymap_load { preset }`.
    pub fn keymap_load(&self, preset: KeymapPreset) -> Result<Vec<KeyBinding>, WorkspaceError> {
        Ok(self.keymaps.load(preset)?)
    }

    /// `keymap_save { bindings }`.
    pub fn keymap_save(&self, bindings: &[KeyBinding]) -> Result<(), WorkspaceError> {
        self.keymaps.save(bindings)?;
        Ok(())
    }

    /// `menu_accelerator { action, preset }` — **the frontend never hardcodes
    /// ⌘/Ctrl**.
    pub fn menu_accelerator(
        &self,
        action: &str,
        preset: KeymapPreset,
    ) -> Result<Option<String>, WorkspaceError> {
        Ok(crate::keymap::menu_accelerator(
            &self.keymaps,
            action,
            preset,
            self.platform,
        )?)
    }

    /// `workspace_load { projectRoot }`.
    pub fn workspace_load(&self) -> &WorkspaceState {
        &self.project.state
    }

    /// `workspace_save { projectRoot, state }`.
    pub fn workspace_save(&mut self, state: WorkspaceState) -> Result<(), WorkspaceError> {
        self.project.state = state;
        self.project.save()?;
        Ok(())
    }

    /// `doc_claim { session, path }`.
    pub fn doc_claim(
        &mut self,
        path: &Utf8Path,
        window_label: &str,
        session: SessionId,
        doc: DocumentId,
    ) -> Claim {
        self.claims.claim(path, window_label, session, doc)
    }
}

/// Why `doc_open` could not produce a buffer.
#[derive(Debug, thiserror::Error)]
pub enum OpenFailure {
    /// The bytes are not UTF-8 (`STRATUM0601`). Offer read-only.
    #[error("{}", .0.diagnostic.message)]
    Refused(RefusedOpen),
    /// The filesystem said no.
    #[error("{path}: {source}")]
    Io {
        /// Path involved.
        path: Utf8PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl OpenFailure {
    /// The `STRATUM0601` diagnostic, when this was an encoding refusal.
    pub fn diagnostic(&self) -> Option<&stratum_proto::Diagnostic> {
        match self {
            OpenFailure::Refused(r) => Some(&r.diagnostic),
            OpenFailure::Io { .. } => None,
        }
    }
}

/// A gate that refuses everything.
///
/// Used by the mutation tests to prove that a writer really is *reached through*
/// its gate rather than merely accompanied by one: swap this in, and every write
/// path that claims to be gated must stop writing.
#[derive(Clone, Copy, Debug, Default)]
pub struct RefusingGate;

impl EditGate for RefusingGate {
    fn assert_comment_only(&self, _: &str, _: &str) -> Result<(), GateRejection> {
        Err(GateRejection {
            writer: "RefusingGate",
            check: Check::TokenStream,
            detail: "this gate refuses everything".to_owned(),
        })
    }

    fn assert_statement_partition_preserved(&self, _: &str, _: &str) -> Result<(), GateRejection> {
        Err(GateRejection {
            writer: "RefusingGate",
            check: Check::StatementPartition,
            detail: "this gate refuses everything".to_owned(),
        })
    }
}
