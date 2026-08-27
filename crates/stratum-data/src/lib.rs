//! Stratum's columnar data engine.
//!
//! This crate holds the dataset a researcher keeps open for hours, so its
//! design is dominated by two rules that are in tension everywhere else:
//!
//! 1. **Latency is the product.** Never do O(rows) work on an interaction path;
//!    prefer a bigger, faster structure to a compact, slow one (spec §0a).
//! 2. **Every write is undoable.** A command with `Atomicity::Rollbackable`
//!    either completes or leaves the dataset exactly as at entry (INV-2).
//!
//! Chunked copy-on-write columns are what let both hold at once. A column is
//! `Vec<Arc<chunk>>` at [`CHUNK_ROWS`] rows per chunk — the *same* granule as
//! `stratum_core::reduce::map_reduce_blocks`, so a fold boundary is a memory
//! boundary — and [`Frame::col_mut`] journals the chunk it is about to dirty.
//! `replace x = 1 in 1` on 10 M doubles therefore retains 512 KiB, not 80 MB
//! (A18), while a snapshot of the whole frame is still five pointer clones.
//!
//! # Where the numbers come from
//!
//! Nothing about Stata's semantics is remembered here. The missing-value
//! encoding, the ordering rules, the promotion ladder and the `markout` string
//! rules are `stratum_core`'s or are quoted from `docs/design/04` §§2, 5, 6 with
//! the measurement that produced them. Where a golden log pins a case
//! (`tests/golden/stata18/semantics.log`, `errors.log`) the test cites it.
//!
//! # Module map
//!
//! | module | what it owns |
//! |---|---|
//! | [`chunk`] | the granule, chunk index arithmetic, `StrLChunk` |
//! | [`column`] | `Column`, `NumCol`, `FixedStrCol`, `StrLCol`, digests, bulk ingest |
//! | [`journal`] | the chunk-granular undo log |
//! | [`frame`] | `Frame`, `FrameSnapshot`, and the write barrier |
//! | [`frames`] | `FrameSet` — Stata 16 frames |
//! | [`variable`], [`labels`], [`chars`] | metadata, value labels, characteristics and notes |
//! | [`sample`], [`bitset`] | `if`/`in`/`markout` and run extraction |
//! | [`sortkey`], [`sort`] | order-preserving keys, radix and comparator sorters |
//! | [`version`] | `DataVersion`, `FrameEpoch` |
//! | [`perf`] | thresholds, counters, and the Q9 memory policy |
//! | [`weights`] | `WeightSpec`/`EvaluatedWeights` — the four weight kinds |
//! | [`bygroup`] | `BySpec`/`GroupIndex`/`ByCursor` — `_n` and `_N` within `by` |
//! | [`order`] | `ViewOrder`/`OrderRegistry` — the engine-side view order (A13) |
//! | [`view`], [`page`] | the windowed UI feed and its `SDP1` wire encoding |
//! | [`strl`] | GSO `(v,o)` packing per release and the write-side dedup pass |
//!
//! # The write barrier is not reachable around
//!
//! `Frame::col_mut` is the only way to get a mutable column, and the buffers
//! underneath are private. The three examples below are **compile-fail
//! doctests**: rustdoc builds each as a separate crate against this one, so
//! they prove the property from outside rather than asserting it from inside,
//! and they are what a reader of these docs sees.
//!
//! They are not the whole gate. `compile_fail` passes when a snippet fails to
//! compile for *any* reason, and its `,E0616` error-code annotation is not
//! enforced on stable — measured on rustc 1.96.0, where a block tagged
//! `compile_fail,E0616` whose real error was E0308 passed. So the binding check
//! lives in `tests/cow.rs`
//! (`the_write_barrier_is_the_only_route_to_a_mutable_column`), which spawns
//! `rustc` on the same snippets and asserts the *specific* diagnostic — E0616
//! private field, E0624 private method, E0599 no such method — alongside a
//! positive control that must compile. That is mechanically what a `trybuild`
//! case does; `trybuild` itself is not used because it is absent from the
//! workspace dependency table (adding it edits `Cargo.toml` and `Cargo.lock`,
//! both W00's under R0) and its fixtures want `tests/ui/**`, which no unit owns.
//!
//! A column's chunk vector is private:
//!
//! ```compile_fail
//! use stratum_data::column::{Column, NumCol};
//! let c = NumCol::<f64>::missing(10);
//! let _ = c.chunks;            // private field
//! ```
//!
//! There is no public mutable accessor for a chunk:
//!
//! ```compile_fail
//! use stratum_data::column::NumCol;
//! let mut c = NumCol::<f64>::missing(10);
//! let _ = c.chunk_mut(0);      // pub(crate)
//! ```
//!
//! And a `FrameSnapshot` — what every command is handed — cannot be written to
//! at all:
//!
//! ```compile_fail
//! use stratum_data::{Frame, StorageType};
//! use stratum_proto::VarIdx;
//! let mut f = Frame::new("default");
//! f.set_n_obs(4);
//! f.add_var("x", StorageType::Double).unwrap();
//! let snap = f.snapshot();
//! snap.col_mut(VarIdx(0));     // no such method on a snapshot
//! ```
//!
//! What *is* reachable is the barrier, and it compiles:
//!
//! ```
//! use stratum_data::{Frame, StorageType};
//! use stratum_proto::VarIdx;
//! let mut f = Frame::new("default");
//! f.set_n_obs(4);
//! f.add_var("x", StorageType::Double).unwrap();
//! f.begin_command();
//! f.col_mut(VarIdx(0)).unwrap().set_f64(0, 1.5).unwrap();
//! assert_eq!(f.col(VarIdx(0)).unwrap().get_f64(0), Some(1.5));
//! f.rollback();
//! assert!(f.col(VarIdx(0)).unwrap().get_f64(0).map(stratum_core::is_missing) == Some(true));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bitset;
pub mod bygroup;
pub mod chars;
pub mod chunk;
pub mod column;
pub mod frame;
pub mod frames;
pub mod journal;
pub mod labels;
pub mod order;
pub mod page;
pub mod perf;
pub mod sample;
pub mod sort;
pub mod sortkey;
pub mod strl;
pub mod variable;
pub mod version;
pub mod view;
pub mod weights;

pub use bitset::BitSet;
pub use bygroup::{ByCursor, ByError, BySpec, GroupIndex};
pub use chars::CharTable;
pub use chunk::{StrLChunk, CHUNK_ROWS};
pub use column::{Column, ColumnRef, FixedStrCol, NumCol, StrLCol, WriteError};
pub use frame::{ColMut, Frame, FrameError, FrameSnapshot};
pub use frames::{FrameSet, FrameSetError};
pub use journal::Journal;
pub use labels::{ValueLabel, ValueLabelSet};
pub use order::{OrderError, OrderRegistry, ViewOrder};
pub use page::{decode, encode, page, PageError};
pub use perf::{counters, memory_policy, CapacityError, Counters, MemoryPolicy, Snapshot};
pub use sample::{Bound, InRange, Run, Sample, SampleBuilder, SampleError, SampleKind};
pub use sort::{SortError, SortState, Strategy};
pub use strl::{GsoPlan, GsoRecord, StrLPacking};
pub use variable::{Provenance, Variable};
pub use version::{DataVersion, FrameEpoch};
pub use view::{ColumnBlock, ColumnSpec, DataPage, PageView, ViewError};
pub use weights::{EvaluatedWeights, WeightError, WeightKind, WeightSpec};

/// Re-exported so a consumer never needs a direct `stratum-proto` dependency to
/// name a storage type (the same reason `stratum-core` re-exports it — A10).
pub use stratum_proto::StorageType;
