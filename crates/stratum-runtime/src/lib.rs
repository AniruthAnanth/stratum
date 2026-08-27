//! Stratum's Stata interpreter.
//!
//! This crate is where a parsed command becomes a change to a dataset. It owns
//! [`ExecCtx`] — ARCHITECTURE §5's "*only* ambient access to env/clock/fs, all
//! recorded" — the dispatcher that runs one command under `catch_unwind`, the
//! expression evaluator, the program and scope machinery, the `ExpandHost` the
//! macro expander re-enters, and the static effect rows for the built-in command
//! surface.
//!
//! # The two invariants everything here serves
//!
//! * **INV-1.** A block marked `Current` would re-produce identical bytes. That
//!   is only true if the set of things a command can observe is exactly what the
//!   footprint records, which is why [`ctx::ExecCtx`] is the only door to the
//!   world and why every door records (design 03 §6.3).
//! * **INV-2.** A `Rollbackable` command either completes or leaves the dataset
//!   exactly as at entry. [`dispatch::exec_command`] opens the frame journal
//!   once, and every exit path — error, interrupt, or panic — rolls it back.
//!
//! # Module map
//!
//! | module | what it owns |
//! |---|---|
//! | [`ctx`] | `ExecCtx`, the host traits, the access log, the counters |
//! | [`dispatch`] | one command: prefixes, `catch_unwind`, commit/rollback |
//! | [`eval`] | `Expr` → `Compiled` → values, per observation |
//! | [`scope`] | call frames, `tempvar`/`tempname` lifetimes, the depth cap |
//! | [`program`] | `program define` bodies |
//! | [`syntax_cmd`] | the `syntax` command inside a program |
//! | [`expand_host`] | `` `=exp' `` and the state-dependent `` `:…' `` |
//! | [`effects_rows`] | this crate's `EffectTable` rows (A1) |
//! | [`extract`] | the static effect-extraction driver, design 03 §5.3 |
//!
//! # PARTITION NOTE — the modules W06b and W06c declare here
//!
//! `docs/ownership.toml` splits this crate three ways and gives **this file** to
//! W06a. A module cannot be declared anywhere but in its parent, so the `pub
//! mod` lines for W06b's `state`, `footprint`, `smcl`, `doc`, `snapshot`,
//! `results` and for W06c's `cmd` are edits to a W06a file that only those units
//! can make correct — exactly the situation `stratum-parse/src/lib.rs` records
//! for W04b, and it is resolved the same way: the declaration is sanctioned
//! here, and nothing else in this file is touched.
//!
//! A declaration is added when the module's files exist and compile. A `pub mod`
//! naming a directory that is half-written does not fail politely — it fails the
//! whole crate, including the parts that were green — which is why the list
//! below grows as those units land rather than being written out in advance.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ctx;
pub mod dispatch;
pub mod effects_rows;
pub mod eval;
pub mod expand_host;
pub mod extract;
pub mod program;
pub mod scope;
pub mod syntax_cmd;

// W06c — the built-in command surface. Declared here under the PARTITION NOTE
// above; nothing else in this file is touched.
pub mod cmd;

// The engine edge (W06a): the shipping `RuntimeHost` over the real filesystem
// and `stratum-dta`, and `CmdHost::run_stat`'s back half into `stratum-stats`.
// Both are reachable only through `ExecCtx`'s recorded wrappers.
pub mod host;
pub mod stat_glue;

// W06b — state, footprints, SMCL, document model. Declared here under the
// PARTITION NOTE above; nothing else in this file is touched.
pub mod doc;
pub mod footprint;
pub mod results;
pub mod smcl;
pub mod snapshot;
pub mod state;

pub use ctx::{
    AccessLog, CancelToken, Counters, ExecCtx, NeverCancel, NoHost, Ns, Output, RuntimeHost, Sink,
    StoredResults, Transcript,
};
pub use dispatch::{exec_command, exec_source, Outcome};
pub use effects_rows::BuiltinEffects;
pub use eval::{Compiled, Ty};
pub use expand_host::RuntimeExpandHost;
pub use extract::{extract_block, extract_command};
pub use program::{Program, ProgramClass, ProgramTable, MAX_CALL_DEPTH};
pub use scope::CallStack;
