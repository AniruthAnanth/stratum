//! Stratum's execution engine — design 03, ADR-008, ARCHITECTURE §5.
//!
//! The run queue, the append-only ledger, the event stream, and the staleness
//! sweep that makes spec §12's promise checkable:
//!
//! > **INV-1.** A block is displayed as `Current` **only if** re-executing it
//! > right now, from the current session state, would produce a byte-identical
//! > result to the result currently displayed under it.
//!
//! INV-1 is one-directional and that asymmetry runs through every decision
//! here. Marking a block `Stale` when a re-run would in fact produce the same
//! bytes costs the user a re-run. Marking it `Current` when it would not is a
//! research-integrity bug, and it is the exact Jupyter failure this product
//! exists to fix. Every "may" answer in this crate is therefore biased toward
//! stale, and the places where it is allowed to be exact are argued for in
//! [`staleness`].
//!
//! # Ports — what this crate needs from below, and why it is a trait
//!
//! ARCHITECTURE §5 gives `stratum-exec` `session, runtime` as its dependencies.
//! Neither crate exists in the tree yet (W06 is a 20-day unit, W08b lands
//! beside this one), so what this crate needs from them is expressed as ports
//! over **frozen `stratum-proto` types** rather than as Cargo edges:
//!
//! | Port | Implemented later by | Carries |
//! |---|---|---|
//! | [`SessionHost`] | `stratum-session` (W08b) | run one block, publish state |
//! | [`Versions`] | `stratum_runtime::snapshot::VersionTable` | current version of every [`DepKey`] |
//! | [`RecordedReads`] / [`RecordedWrites`] | `stratum_runtime::footprint` | what an execution ACTUALLY read and wrote |
//! | [`AnalysedDoc`] | `stratum_session::DocumentModel` + `stratum_runtime::extract` | the reconciled [`BlockMap`] plus one [`EffectSet`] per block |
//!
//! These are **projections, not twins** (A10). `DepFootprint` keeps `VarId`s,
//! generation counters, file stamps and an RNG fingerprint; the sweep compares
//! them for equality and nothing else, so the projection it consumes is
//! `(DepKey, u64)` pairs and the adapter that folds a `FileStamp` or an
//! `RngFingerprint` into its `u64` belongs beside the rich type. No type
//! declared in `stratum-runtime` is redeclared here.
//!
//! # Threading (C50 / A17)
//!
//! Two threads, and the split is the whole point:
//!
//! * the **session worker** owns the `!Sync` session and runs user code;
//! * the **control thread** ([`control`]) owns documents, the status map and
//!   the sweep.
//!
//! The control thread never takes a lock the worker can hold across a command.
//! State reaches it as an immutable `Arc` snapshot published by a swap-only
//! critical section ([`Snapshot`]). Without that split nothing services a
//! `DocChange` during a 30-second `regress`, so every block downstream of the
//! edit keeps showing ✓ Current for the duration — the §12 failure this design
//! exists to prevent, reintroduced by a scheduling omission.
//!
//! # Performance is asserted as counters (ADR-017)
//!
//! Every gate in this crate counts work — blocks re-evaluated, keys compared,
//! events coalesced — and never asserts a duration. Durations are recorded in
//! `benches/staleness.rs`, where a moving line is the signal and a red build is
//! not.

#![forbid(unsafe_code)]

pub mod cancel;
pub mod control;
pub mod depindex;
pub mod engine;
pub mod introspect;
pub mod ledger;
pub mod plan;
pub mod queue;
pub mod resultstore;
pub mod staleness;
pub mod store;
pub mod stream;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

pub use cancel::{
    CancelToken, Escalation, Interrupt, ABORT_AFTER_MS, ACK_BUDGET_MS, KILL_AFTER_MS,
};
pub use control::{ControlLoop, ControlMsg, Resolved, Snapshot};
pub use depindex::{DepIndex, SweepStats};
pub use engine::{
    CleanRunConfig, EngineHandle, ExecContext, ExecOutcome, ExecutionEngine, SessionHost,
};
pub use introspect::{FrameView, IntrospectSnapshot, AI_CONTEXT_COMMANDS, AI_CONTEXT_ERRORS};
pub use ledger::{Committed, ExecutionLedger, IdAllocator, LedgerView};
pub use plan::{resolve, resolve_advance, PlanCtx, PlanError};
pub use queue::{QueuedItem, QueuedRun, RunQueue};
pub use resultstore::{ResultStore, TextBuf};
pub use staleness::{
    slot_into, slot_of, sweep, sweep_counted, AnalysedDoc, Dep, RecordedReads, RecordedWrites,
    RunState, StatusMap, SweepInput, Version, Versions, Witness,
};
pub use store::{SessionStore, StoreError, ENGINE_DB_RELATIVE, UI_DB_RELATIVE};
pub use stream::{EventBus, EventSink, OutputCoalescer};

// Re-exported for doc links above; these are the proto types the ports speak.
#[doc(no_inline)]
pub use stratum_effects::EffectSet;
#[doc(no_inline)]
pub use stratum_proto::{BlockMap, DepKey};
