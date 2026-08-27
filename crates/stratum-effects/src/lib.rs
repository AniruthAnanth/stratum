//! The static-effect vocabulary and the two capability traits — ARCHITECTURE §5,
//! design 03 §5, CONTRACTS §13 (A1).
//!
//! **Trait and vocabulary only. Zero rows.** Rows come from `stratum-runtime`
//! (built-in commands) and `stratum-stats` (its own). That split is not
//! bookkeeping: [`EffectTable`] has no default method, so a command cannot be
//! added anywhere in the workspace without someone declaring its effects.
//!
//! # The one rule
//!
//! Every set here is a MAY-set and every answer is biased toward "yes". An
//! effect table that returns too small a read or write set is a soundness bug
//! against INV-1 — the staleness sweep would leave a block marked `Current`
//! after its input changed, which is the single failure this whole subsystem
//! exists to prevent. Over-approximating costs a spurious re-run; under-
//! approximating costs a wrong number in a paper.

pub mod ctx;
pub mod effectset;
pub mod registry;
pub mod varset;

pub use ctx::StaticCtx;
pub use effectset::{
    Atomicity, CwdEffect, EffectSet, EffectTable, FrameEffect, RngEffect, RwEffect,
};
pub use registry::CommandRegistry;
pub use varset::{FileSet, Name, NameSet, VarPattern, VarSet};
