//! State identity: the write barrier, version vectors, and interning.
//!
//! This is the module ADR-008 is about, and staleness is the product's
//! differentiator rather than bookkeeping — spec §§12 and 35 name the exact
//! Jupyter failure it exists to fix, which is *showing old output as if it
//! reflected current code*.
//!
//! The one invariant everything here serves:
//!
//! > **INV-1.** A block displayed ✓ Current was produced by exactly this code
//! > against exactly this state, and re-running it now would produce identical
//! > bytes.
//!
//! INV-1 is **one-directional**. Over-marking is a UX cost; under-marking is a
//! research-integrity hazard. Every judgement call in this module resolves
//! toward *more* stale, and where a property cannot be proved at all
//! (`shell`, `python`, plugins) the answer is `Taint::EXTERNAL` and
//! `CurrentUnverifiable` — never a silent ✓.
//!
//! | module | what it owns |
//! |---|---|
//! | [`versions`] | `VarVersion` and the structurally shared map that holds one per column |
//! | [`dataset`] | `DatasetFingerprint`, the `03` §4.3 bump table, `DatasetStateId` interning |
//! | [`fingerprint`] | the 128-bit accumulator and the whole-session `StateFingerprint` |
//! | [`digest`] | content convergence: the reason a verbatim re-run does not grey out the file |
//! | [`barrier`] | the only route from a column mutation to a version bump |
//!
//! # Counters, not clocks
//!
//! ADR-017 is binding: a performance claim in this crate is asserted with a
//! counter. [`counters`] is compiled into the shipping build for that reason —
//! a counter that exists only under `cfg(test)` cannot be asserted about the
//! code that ships. Nothing here is incremented per row; the busiest counter
//! moves once per column per command.

pub mod barrier;
pub mod dataset;
pub mod digest;
pub mod fingerprint;
pub mod versions;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use rustc_hash::FxHashMap;
use stratum_data::Frame;
use stratum_proto::{ExecutionId, FrameId, SessionEpoch, StateId, VarId};

pub use dataset::{Carry, DatasetFingerprint, DatasetInterner, DatasetKey, Eff, TsSpec};
pub use digest::{Convergence, ConvergencePolicy, DigestCache};
pub use fingerprint::{
    FileStamp, FingerprintAcc, Ns, PathKey, RngFingerprint, RngKind, StateFingerprint,
};
pub use versions::{VarVersion, VarVersions, VERSION_CHUNK};

use crate::footprint::{DepFootprint, FootprintBuilder, WriteFootprint};

/// Instrumentation counters for the state subsystem.
///
/// All monotonic, all relaxed, all incremented at most once per *column per
/// command*. Read them with [`StateCounters::snapshot`] and subtract two
/// snapshots with [`CounterSnapshot::since`] — the shape every acceptance
/// assertion in `tests/barrier.rs` and `tests/fingerprint.rs` uses.
#[derive(Debug, Default)]
pub struct StateCounters {
    /// Column version bumps. **This is the ADR-008 number**: `replace x = x+1`
    /// over 10 M rows must move it by exactly 1.
    pub gen_bumps: AtomicU64,
    /// Columns whose digest matched the previous generation, so no bump.
    pub converged_columns: AtomicU64,
    /// Columns blake3'd at commit.
    pub columns_digested: AtomicU64,
    /// Bytes those digests covered.
    pub digest_bytes: AtomicU64,
    /// Command commits.
    pub commits: AtomicU64,
    /// Command rollbacks (INV-2).
    pub rollbacks: AtomicU64,
    /// Version-map chunks deep-copied. The O(changed) number.
    pub version_chunks_cloned: AtomicU64,
    /// Version-map chunks allocated for the first time.
    pub version_chunks_allocated: AtomicU64,
    /// `DatasetStateId`s allocated, i.e. genuinely new dataset states.
    pub dataset_states_allocated: AtomicU64,
    /// Interning lookups that returned an id already in use — the convergence
    /// win, counted.
    pub dataset_states_recurred: AtomicU64,
}

/// A plain-value reading of [`StateCounters`], safe to subtract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs)] // one-for-one with `StateCounters`, documented there.
pub struct CounterSnapshot {
    pub gen_bumps: u64,
    pub converged_columns: u64,
    pub columns_digested: u64,
    pub digest_bytes: u64,
    pub commits: u64,
    pub rollbacks: u64,
    pub version_chunks_cloned: u64,
    pub version_chunks_allocated: u64,
    pub dataset_states_allocated: u64,
    pub dataset_states_recurred: u64,
}

impl StateCounters {
    /// Read every counter. Not atomic as a group; counters are read from a test
    /// or a bench with no other work in flight.
    #[must_use]
    pub fn snapshot(&self) -> CounterSnapshot {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        CounterSnapshot {
            gen_bumps: g(&self.gen_bumps),
            converged_columns: g(&self.converged_columns),
            columns_digested: g(&self.columns_digested),
            digest_bytes: g(&self.digest_bytes),
            commits: g(&self.commits),
            rollbacks: g(&self.rollbacks),
            version_chunks_cloned: g(&self.version_chunks_cloned),
            version_chunks_allocated: g(&self.version_chunks_allocated),
            dataset_states_allocated: g(&self.dataset_states_allocated),
            dataset_states_recurred: g(&self.dataset_states_recurred),
        }
    }
}

impl CounterSnapshot {
    /// `self - earlier`, field by field, saturating.
    #[must_use]
    pub fn since(&self, earlier: CounterSnapshot) -> CounterSnapshot {
        let s = |a: u64, b: u64| a.saturating_sub(b);
        CounterSnapshot {
            gen_bumps: s(self.gen_bumps, earlier.gen_bumps),
            converged_columns: s(self.converged_columns, earlier.converged_columns),
            columns_digested: s(self.columns_digested, earlier.columns_digested),
            digest_bytes: s(self.digest_bytes, earlier.digest_bytes),
            commits: s(self.commits, earlier.commits),
            rollbacks: s(self.rollbacks, earlier.rollbacks),
            version_chunks_cloned: s(self.version_chunks_cloned, earlier.version_chunks_cloned),
            version_chunks_allocated: s(
                self.version_chunks_allocated,
                earlier.version_chunks_allocated,
            ),
            dataset_states_allocated: s(
                self.dataset_states_allocated,
                earlier.dataset_states_allocated,
            ),
            dataset_states_recurred: s(
                self.dataset_states_recurred,
                earlier.dataset_states_recurred,
            ),
        }
    }
}

/// The process-wide counters.
///
/// This is the aggregate a diagnostics panel wants: every thread's work, summed.
/// It is **not** what a test should assert on — see [`local_snapshot`].
#[must_use]
pub fn counters() -> &'static StateCounters {
    static C: OnceLock<StateCounters> = OnceLock::new();
    C.get_or_init(StateCounters::default)
}

thread_local! {
    /// The calling thread's share of [`counters`]. Every increment lands in
    /// both.
    static LOCAL: StateCounters = StateCounters::default();
}

/// The calling thread's counters.
///
/// **Assert on this, not on [`counters`].** `libtest` runs each `#[test]` on its
/// own thread inside one process, so a delta taken from the process-wide
/// aggregate silently includes whatever the other tests in the same binary did
/// while it was open. That is not a theoretical race: it is why
/// `tests/barrier.rs::convergence_can_be_turned_off_for_a_large_panel` read
/// `columns_digested = 1` for a policy that digests nothing — the digest
/// belonged to a different test. An acceptance bullet that says "exactly one
/// bump" has to be asserted with an instrument that can only see one command.
///
/// Per-thread is the right granularity for the *engine* too, not just for tests:
/// the session worker is the only thread that commits, so its counters are the
/// session's counters, and nothing has to be subtracted to read them.
#[must_use]
pub fn local_snapshot() -> CounterSnapshot {
    LOCAL.with(StateCounters::snapshot)
}

/// Add `n` to one counter, in this thread's copy and in the process-wide one.
///
/// `pick` names the field in both, so there is no way to move one and forget the
/// other. Two relaxed uncontended `fetch_add`s, at most once per column per
/// command — nothing here is on a per-row path (ADR-017, spec §0a).
#[inline]
pub(crate) fn bump(pick: fn(&StateCounters) -> &AtomicU64, n: u64) {
    pick(counters()).fetch_add(n, Ordering::Relaxed);
    LOCAL.with(|c| pick(c).fetch_add(n, Ordering::Relaxed));
}

/// Interns `StateFingerprint`s so `StateId` is a recurring identity, exactly as
/// `DatasetStateId` is.
#[derive(Debug, Default)]
struct StateInterner {
    by_key: FxHashMap<(FingerprintAcc, SessionEpoch, FrameId), Vec<(StateFingerprint, StateId)>>,
    next: u64,
}

impl StateInterner {
    fn intern(&mut self, fp: &mut StateFingerprint) -> StateId {
        let key = (fp.acc, fp.epoch, fp.current_frame);
        let bucket = self.by_key.entry(key).or_default();
        let probe = StateFingerprint {
            id: StateId(0),
            ..fp.clone()
        };
        for (known, id) in bucket.iter() {
            if *known == probe {
                fp.id = *id;
                return *id;
            }
        }
        self.next += 1;
        let id = StateId(self.next);
        fp.id = id;
        bucket.push((
            StateFingerprint {
                id,
                ..probe.clone()
            },
            id,
        ));
        id
    }
}

/// The session's state identity, and everything needed to keep it honest.
///
/// One of these lives inside `ExecCtx`. It owns the current
/// [`StateFingerprint`], both interners, and one [`DigestCache`] per frame.
#[derive(Debug)]
pub struct SessionState {
    fp: StateFingerprint,
    datasets: DatasetInterner,
    states: StateInterner,
    digests: FxHashMap<FrameId, DigestCache>,
    carry: FxHashMap<FrameId, Carry>,
    /// `set stalecheck`.
    pub policy: ConvergencePolicy,
}

impl SessionState {
    /// A session that has run nothing, with one empty frame.
    #[must_use]
    pub fn fresh(epoch: SessionEpoch, current_frame: FrameId) -> Self {
        let mut s = Self {
            fp: StateFingerprint::fresh(epoch, current_frame),
            datasets: DatasetInterner::new(),
            states: StateInterner::default(),
            digests: FxHashMap::default(),
            carry: FxHashMap::default(),
            policy: ConvergencePolicy::default(),
        };
        let mut ds = DatasetFingerprint::empty(current_frame, Carry::default());
        s.datasets.intern(&mut ds);
        s.fp.set_frame(current_frame, ds);
        s.states.intern(&mut s.fp);
        s
    }

    /// The current whole-session fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> &StateFingerprint {
        &self.fp
    }

    /// Mutable access, for the namespaces this module does not own (macros,
    /// scalars, `e()`, settings, cwd). Every mutator on [`StateFingerprint`]
    /// keeps `acc` in step, so there is no way to move a version without moving
    /// the fold.
    pub fn fingerprint_mut(&mut self) -> &mut StateFingerprint {
        &mut self.fp
    }

    /// The current frame's dataset fingerprint.
    #[must_use]
    pub fn dataset(&self) -> &DatasetFingerprint {
        self.fp
            .current()
            .expect("the current frame always has a fingerprint")
    }

    /// Open a recording for one command against the current frame.
    ///
    /// `next_var_id` sizes the lock-free part of the barrier's bitsets; pass the
    /// session's variable counter.
    #[must_use]
    pub fn begin_command(&self, next_var_id: u32) -> FootprintBuilder {
        FootprintBuilder::begin(&self.fp, self.dataset(), next_var_id)
    }

    /// Commit one command: apply the `03` §4.3 table, intern the resulting
    /// dataset and session states, and close the record.
    ///
    /// Returns the footprints alongside the counters, because the caller
    /// (`ExecCtx`) needs both for the `ExecutionRecord`.
    pub fn commit_command(
        &mut self,
        frame: &mut Frame,
        fb: FootprintBuilder,
        exec: ExecutionId,
    ) -> Committed {
        let frame_id = fb.frame();
        let mut ds = self
            .fp
            .frames
            .get(&frame_id)
            .cloned()
            .unwrap_or_else(|| DatasetFingerprint::empty(frame_id, self.carry_for(frame_id)));
        let cache = self.digests.entry(frame_id).or_default();
        let outcome = barrier::commit(frame, &fb, &mut ds, cache, self.policy, exec);
        let dataset = self.datasets.intern(&mut ds);
        self.carry.insert(frame_id, ds.carry());
        let (deps, writes) = fb.finish(&ds);
        self.fp.set_frame(frame_id, ds);
        let state = self.states.intern(&mut self.fp);
        Committed {
            deps,
            writes,
            counts: CommitCounts {
                dataset,
                state,
                bumps: outcome.bumps,
                converged: outcome.converged,
                digested: outcome.digested,
                digest_bytes: outcome.digest_bytes,
                created: outcome.created,
                dropped: outcome.dropped,
            },
        }
    }

    /// Abandon a command: restore the frame and leave state identity untouched.
    pub fn rollback_command(&mut self, frame: &mut Frame) {
        barrier::rollback(frame);
    }

    /// Replace the current frame's dataset wholesale (`use`, `clear`,
    /// `frame change`), with monotone counters so an old id never looks valid.
    pub fn replace_dataset(&mut self, frame_id: FrameId, nobs: u64) -> DatasetStateIdPair {
        let mut ds = DatasetFingerprint::empty(frame_id, self.carry_for(frame_id));
        ds.nobs = nobs;
        self.digests.entry(frame_id).or_default().clear();
        let dataset = self.datasets.intern(&mut ds);
        self.carry.insert(frame_id, ds.carry());
        self.fp.set_frame(frame_id, ds);
        self.fp.current_frame = frame_id;
        let state = self.states.intern(&mut self.fp);
        DatasetStateIdPair { dataset, state }
    }

    /// Interned ids after a mutation this module did not perform (a macro
    /// assignment, a `set`, an `ereturn`).
    pub fn reintern(&mut self) -> StateId {
        self.states.intern(&mut self.fp)
    }

    /// The name a `VarId` currently resolves to, for rendering a `DepKey`.
    #[must_use]
    pub fn name_of(&self, var: VarId) -> Option<String> {
        let ds = self.fp.current()?;
        ds.names
            .iter()
            .find(|(_, id)| **id == var)
            .map(|(n, _)| n.to_string())
    }

    fn carry_for(&self, frame_id: FrameId) -> Carry {
        self.carry.get(&frame_id).copied().unwrap_or_default()
    }
}

/// Both interned ids after a whole-dataset replacement.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct DatasetStateIdPair {
    /// The frame's new dataset state.
    pub dataset: stratum_proto::DatasetStateId,
    /// The session's new state.
    pub state: StateId,
}

/// Counters and ids from one commit.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct CommitCounts {
    /// The interned dataset state — spec §13's "D17".
    pub dataset: stratum_proto::DatasetStateId,
    /// The interned session state.
    pub state: StateId,
    /// Column version bumps. One per written column, never per row.
    pub bumps: u32,
    /// Columns that converged and therefore did not bump.
    pub converged: u32,
    /// Columns digested.
    pub digested: u32,
    /// Bytes digested.
    pub digest_bytes: u64,
    /// Columns created.
    pub created: u32,
    /// Columns dropped.
    pub dropped: u32,
}

/// The full result of [`SessionState::commit_command`].
#[derive(Clone, Debug)]
pub struct Committed {
    /// What the command read.
    pub deps: DepFootprint,
    /// What it wrote.
    pub writes: WriteFootprint,
    /// Ids and counters.
    pub counts: CommitCounts,
}
