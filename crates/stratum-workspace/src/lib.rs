//! Project model, document buffers, both sidecars, and **the four gated `.do`
//! writers**.
//!
//! This is the crate that makes spec §5 true. A researcher's analysis lives in
//! an ordinary `.do` file that Stata can run, and the only reason that promise
//! survives contact with an IDE is that there is exactly one function in the
//! product which can put bytes into such a file, and four callers of it, each
//! holding a proof that its edit is safe:
//!
//! | Writer | Gate |
//! |---|---|
//! | [`entry::Workspace::doc_save`] | Byte fidelity — reproduce the recorded EOL and BOM exactly (A24) |
//! | [`entry::Workspace::section_rename`] | `assert_comment_only` |
//! | [`entry::Workspace::section_move`] | `assert_statement_partition_preserved`, plus a forced restale (A15) |
//! | [`entry::Workspace::ai_apply_patch`] | Explicit user acceptance, plus `assert_comment_only` for a comment-scoped task |
//!
//! ADR-010, ARCHITECTURE §6.3. The mechanism is [`write::GatedEdits`]: it has no
//! public constructor other than one per writer, each of which runs that
//! writer's gate, and [`write::write_document`] takes nothing else. A fifth
//! writer cannot be added by forgetting to call something.
//!
//! # Layering
//!
//! ARCHITECTURE C24: `stratum-desktop` links this crate and links **none** of
//! core/data/dta/parse/stats/runtime/exec. So nothing here parses Stata, runs
//! Stata, or reads a `.dta`. What the crate needs from the language — "is this
//! edit comment-only?" — arrives through the [`write::EditGate`] trait, which
//! `stratum-intel` (W20) implements. Until it lands,
//! [`write::StandaloneGate`] is a deliberately conservative stand-in; read the
//! header of [`write`] before touching it.
//!
//! # The two sidecars
//!
//! ARCHITECTURE C19 splits them, and this crate tolerates either being absent or
//! stale:
//!
//! * [`sidecar_durable`] — `.<name>.do.workspace`, committed, human-meaningful,
//!   deterministic bytes, **no timestamps and no output**.
//! * [`sidecar_cache`] — `.stratum/cache/<hash>/`, gitignored and
//!   self-ignoring, entirely derived. Deleting it loses nothing.

pub mod bytes;
pub mod document;
pub mod entry;
pub mod keymap;
pub mod layout;
pub mod project;
pub mod sections;
pub mod sidecar_cache;
pub mod sidecar_durable;
pub mod write;

pub use bytes::{DocBytes, Eol};
pub use document::Document;
pub use entry::{DocumentOpened, Workspace, WorkspaceError};
pub use project::{Project, WorkspaceState};
pub use sidecar_durable::DurableSidecar;
pub use write::{write_document, EditGate, GateRejection, GatedEdits, SavedAck, Writer};
