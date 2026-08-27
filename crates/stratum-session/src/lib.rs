//! The engine-side Stratum session.
//!
//! Spec §15 asks for the distinction between *interactive* execution — against
//! whatever the live session currently holds — and *clean* execution — a fresh,
//! deterministic environment — to be **clear**. ARCHITECTURE §7.7 makes that
//! demand structural rather than aspirational:
//!
//! > Clean execution does not *reset* a session — it **constructs a new one**
//! > (`Session::fresh(SessionConfig)`), so there is no cleanup path that can
//! > forget an item.
//!
//! This crate is that sentence, compiled. [`Session`] is a product of sixteen
//! namespaces and nothing else; [`Session::fresh`] is the only constructor;
//! there is no `reset`, no `clear_all` and no `Session::reuse` anywhere in the
//! public surface, so "the clean path forgot to drop the estimates" is not a bug
//! that can be written here.
//!
//! # The checklist, and how it is enforced
//!
//! | # | item | lives in | audited by |
//! |---|---|---|---|
//! | 1 | frames | [`frames::Frames`] | [`fresh::CleanItem::Frames`] |
//! | 2 | dataset | [`frames::Frames`] | [`fresh::CleanItem::Dataset`] |
//! | 3 | macros | `stratum_parse::MacroEnv` | [`fresh::CleanItem::Macros`] |
//! | 4 | scalars, matrices | [`session::ScalarStore`], [`session::MatrixStore`] | [`fresh::CleanItem::ScalarsMatrices`] |
//! | 5 | estimates, `r()`/`e()`/`s()` | [`session::EstimateStore`] | [`fresh::CleanItem::Estimates`] |
//! | 6 | RNG | [`session::RngState`] | [`fresh::CleanItem::Rng`] |
//! | 7 | working directory | [`Session::cwd`] | [`fresh::CleanItem::Cwd`] |
//! | 8 | settings | [`config::SettingsSnapshot`] | [`fresh::CleanItem::Settings`] |
//! | 9 | programs and ado | [`ado::AdoState`] | [`fresh::CleanItem::Ado`] |
//! | 10 | version | [`config::StataVersion`] | [`fresh::CleanItem::Version`] |
//! | 11 | control state | [`session::ControlState`] | [`fresh::CleanItem::Control`] |
//! | 12 | graphs | [`session::GraphStore`] | [`fresh::CleanItem::Graphs`] |
//! | 13 | file handles | [`session::FileHandles`] | [`fresh::CleanItem::FileHandles`] |
//! | 14 | temp names | `MacroEnv`'s one `__000000` counter | [`fresh::CleanItem::Tempnames`] |
//! | 15 | environment reads | [`session::EnvTaint`] | [`fresh::CleanItem::Environment`] |
//! | 16 | collation | [`frames::collate`] | [`fresh::CleanItem::Collation`] |
//!
//! Three mechanisms, not one, because a checklist with a single guard is a
//! checklist with a single point of failure:
//!
//! * **The struct literal.** `Session::fresh_at` names every field of
//!   [`Session`] in one expression. A new namespace does not compile until it
//!   has a clean-state answer.
//! * **The derive.** `Session` is `#[derive(PartialEq)]`, so the
//!   construct-don't-reset test in `tests/fresh_checklist.rs` compares *all* of
//!   it. A hand-written `eq` listing fifteen of sixteen is the failure mode
//!   §7.7 was written to remove, so there is no hand-written `eq`.
//! * **The list as data.** [`fresh::CleanItem::ALL`] has sixteen entries and
//!   [`fresh::audit`] answers every one, so a running engine can *prove* its own
//!   starting state rather than assume it.
//!
//! # Layering
//!
//! ARCHITECTURE §5 puts this crate above `stratum-runtime` and below
//! `stratum-exec` (A7/C49 inverted that edge: `session → runtime`, `exec →
//! session`). There is one `Session` type, one `Session::fresh`, one owner, and
//! `stratum-exec` simply calls it — no `SessionFactory` trait, no injection.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ado;
pub mod config;
pub mod document;
pub mod frames;
pub mod fresh;
pub mod isolate;
pub mod session;

pub use ado::{AdoDir, AdoEntry, AdoPath, AdoState, ProgramClass, ProgramDef};
pub use config::{
    ConfigError, LocaleMode, RngKind, SessionConfig, SettingId, SettingValue, SettingsSnapshot,
    StataVersion, DEFAULT_LEVEL, DEFAULT_SEED, LINESIZE,
};
pub use document::{Applied, Documents};
pub use frames::{collate, DatasetBindings, FrameLink, Frames, MiStyle, SurveySet, TimeSeriesSet};
pub use fresh::{audit, CleanItem};
pub use isolate::{
    clean_state_tick, CleanRun, CleanRunOutcome, IsolateError, Isolation, Sandboxed, WriteSandbox,
    WriteVerb,
};
pub use session::{
    ControlState, EnvRead, EnvSource, EnvTaint, EstimateStore, FileHandles, GraphHandle,
    GraphStore, Matrix, MatrixStore, OpenFile, PreserveEntry, RngState, ScalarStore, Session,
    StoredEstimate, DEFAULT_SCHEME,
};

/// CONTRACTS §13's document surface, declared in `stratum_runtime::doc` (C25)
/// and implemented here for [`Session`].
///
/// Re-exported so `stratum-exec` — which depends on this crate, not necessarily
/// on the runtime by name — can name the trait it calls through. A re-export and
/// never a second declaration: two structurally identical traits with no
/// conversion is the twin A10 bans.
pub use stratum_runtime::doc::DocumentModel;
