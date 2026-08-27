//! The write barrier: the single route from a mutation to a version bump.
//!
//! `stratum-data` already guarantees the half of ADR-008 that a type system can
//! guarantee: raw chunk buffers are private and [`Frame::col_mut`] is the only
//! way to obtain a mutable column, proved from outside the crate by
//! `stratum-data`'s `tests/cow.rs` spawning `rustc` on snippets and asserting
//! E0616/E0624/E0599.
//!
//! What that cannot express is the *runtime's* half: every such mutation must
//! also be recorded, and the recording must cost one version bump per column
//! **per command commit**, not one per element. `replace x = x+1` over 10 M rows
//! is one bump. That is this module, and [`col_mut`] is the only place in
//! `stratum-runtime` allowed to call `Frame::col_mut` — enforced mechanically by
//! `tests/barrier.rs`, which scans the crate's own sources.
//!
//! # The shape of a command
//!
//! ```text
//! frame.begin_command();                     // journal opens (INV-2)
//! let fb = FootprintBuilder::begin(&state, &ds, next_var_id);
//! …  barrier::col_mut(&mut frame, &fb, idx)  // N times, any number of rows
//! commit(&mut frame, &fb, &mut ds, …)        // exactly one bump per column
//! let (deps, writes) = fb.finish(&ds);
//! ```
//!
//! On failure or interrupt, [`rollback`] restores the frame bit-for-bit and the
//! fingerprint is never touched, so the block's `DatasetStateId` is unchanged —
//! which is what makes `ExecStatus::Interrupted { rolled_back: true }` an
//! honest claim rather than a hope.

use stratum_data::{ColMut, Frame, FrameError};
use stratum_proto::{ColumnDigest, DatasetStateId, ExecutionId, VarId, VarIdx};

use crate::footprint::FootprintBuilder;
use crate::state::bump;
use crate::state::dataset::DatasetFingerprint;
use crate::state::digest::{Convergence, ConvergencePolicy, DigestCache};

/// **The only sanctioned mutable column access in `stratum-runtime`.**
///
/// Wraps [`Frame::col_mut`] with the read/write barrier that `03` §4.3 requires.
/// Setting the write bit is one relaxed `fetch_or` and is idempotent, so calling
/// this once per chunk — or once per row, though nothing should — still yields
/// exactly one version bump at commit.
///
/// # Errors
///
/// [`FrameError::BadIndex`] when `idx` names no variable.
pub fn col_mut<'f>(
    frame: &'f mut Frame,
    fb: &FootprintBuilder,
    idx: VarIdx,
) -> Result<ColMut<'f>, FrameError> {
    let var = frame
        .var(idx)
        .ok_or(FrameError::BadIndex(idx.0))
        .map(|v| v.id)?;
    fb.note_write(var);
    frame.col_mut(idx)
}

/// Record a column read. The mirror of [`col_mut`]; kept here so both halves of
/// the barrier are in one file and one test can assert on both.
pub fn note_read(frame: &Frame, fb: &FootprintBuilder, idx: VarIdx) {
    if let Some(v) = frame.var(idx) {
        fb.note_read(v.id);
    }
}

/// What one commit did. Every field is a counter, per ADR-017.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct CommitOutcome {
    /// Columns whose `gen` advanced. **One per written column, never per row.**
    pub bumps: u32,
    /// Columns that were written but whose bytes were unchanged, so `gen` did
    /// not advance and nothing downstream was restaled (`03` §4.4).
    pub converged: u32,
    /// Columns blake3'd at this commit.
    pub digested: u32,
    /// Bytes those digests covered.
    pub digest_bytes: u64,
    /// Columns created.
    pub created: u32,
    /// Columns dropped.
    pub dropped: u32,
    /// The dataset state after the commit.
    pub dataset: DatasetStateId,
}

impl Default for CommitOutcome {
    fn default() -> Self {
        Self {
            bumps: 0,
            converged: 0,
            digested: 0,
            digest_bytes: 0,
            created: 0,
            dropped: 0,
            dataset: DatasetStateId(0),
        }
    }
}

/// Apply the `03` §4.3 table to `ds` for everything `fb` recorded, then close
/// the frame's undo journal.
///
/// The frame must already hold the command's mutations; this is the *commit*
/// half, not the mutation half.
pub fn commit(
    frame: &mut Frame,
    fb: &FootprintBuilder,
    ds: &mut DatasetFingerprint,
    cache: &mut DigestCache,
    policy: ConvergencePolicy,
    exec: ExecutionId,
) -> CommitOutcome {
    let w = fb.writes();
    let mut out = CommitOutcome::default();

    // One pointer walk over the metadata vector resolves every touched VarId to
    // its VarIdx. It is O(#vars) per command, not per row, and it is the only
    // O(#vars) step in a commit. `stratum_data::Frame` has `index_of(name)` but
    // no `index_of_id`; adding one would make this O(touched) and is flagged
    // for W03 rather than duplicated here.
    let mut touched: Vec<(VarId, VarIdx, u64)> = Vec::new();
    for (i, v) in frame.vars().iter().enumerate() {
        let idx = VarIdx(i as u32);
        if w.vars_written.binary_search(&v.id).is_ok()
            || w.vars_created.binary_search(&v.id).is_ok()
        {
            let bytes = frame.col(idx).map_or(0, stratum_data::Column::heap_bytes);
            touched.push((v.id, idx, bytes));
        }
    }

    // Structure first: a created column must exist in the fingerprint before its
    // digest is recorded, and a dropped one must not.
    for (var, idx, _) in &touched {
        if w.vars_created.binary_search(var).is_ok() {
            let name = frame
                .var(*idx)
                .map_or_else(String::new, |v| v.name.to_string());
            ds.create(*var, &name, exec);
            out.created += 1;
        }
    }
    for (var, name) in &w.vars_dropped {
        ds.drop_var(*var, name);
        cache.forget(*var);
        out.dropped += 1;
    }
    for (_, from, to) in &w.renamed {
        ds.rename(from, to);
    }
    if w.changed_layout {
        ds.touch_metadata();
    }
    if w.changed_membership {
        ds.change_membership(frame.n_obs());
    }
    if w.changed_order {
        let st = frame.sort_state();
        let keys: Option<std::sync::Arc<[VarId]>> = st.valid.then(|| {
            st.keys
                .iter()
                .filter_map(|k| frame.var(*k).map(|v| v.id))
                .collect()
        });
        // ADR-015: our sort is always stable, so ties never randomise. R008
        // exists to warn about Stata do-files that relied on the opposite.
        ds.change_order(keys, true);
    }

    // Values last, so a digest is taken against the committed bytes.
    for (var, idx, bytes) in &touched {
        let prev_gen = ds.version_of(*var).map_or(0, |v| v.gen);
        let digest: Option<ColumnDigest> = if policy.admits(*bytes) {
            out.digested += 1;
            out.digest_bytes += bytes;
            bump(|c| &c.columns_digested, 1);
            bump(|c| &c.digest_bytes, *bytes);
            frame.digest(*idx)
        } else {
            None
        };

        let created = w.vars_created.binary_search(var).is_ok();
        if created {
            // `gen = 0` already; there is nothing to converge against.
            if let Some(d) = digest {
                cache.record(*var, prev_gen, d);
            }
            continue;
        }

        let verdict = match digest {
            Some(d) => cache.check(*var, prev_gen, d),
            None => Convergence::NotChecked,
        };
        match verdict {
            Convergence::Converged => {
                // The bytes are provably unchanged. Do not bump: this is the
                // one thing that stops "I re-ran my cleaning block" greying out
                // every model below it.
                ds.touch_origin(*var, exec);
                out.converged += 1;
                bump(|c| &c.converged_columns, 1);
            }
            Convergence::Diverged | Convergence::NotChecked => {
                let gen = ds.bump_value(*var, exec).unwrap_or(0);
                out.bumps += 1;
                bump(|c| &c.gen_bumps, 1);
                match digest {
                    Some(d) => cache.record(*var, gen, d),
                    // Above the policy's ceiling we have no digest for the new
                    // generation, so the stale entry must go: a later commit
                    // must not converge against bytes from two versions back.
                    None => cache.forget(*var),
                }
            }
        }
    }

    frame.commit();
    bump(|c| &c.commits, 1);
    out.dataset = ds.id;
    out
}

/// INV-2: restore the frame exactly as at command entry and leave the
/// fingerprint untouched.
///
/// The fingerprint is deliberately not rolled back, because nothing was applied
/// to it — [`commit`] is the only writer. That asymmetry is what makes an
/// interrupted command unable to leave a half-bumped version behind.
pub fn rollback(frame: &mut Frame) {
    frame.rollback();
    bump(|c| &c.rollbacks, 1);
}

/// A digest of every column in the frame, in storage order — the instrument the
/// INV-2 acceptance is asserted with ("verified by digesting every column before
/// and after").
#[must_use]
pub fn frame_digest(frame: &Frame) -> Vec<(VarId, ColumnDigest)> {
    (0..frame.n_vars())
        .filter_map(|i| {
            let idx = VarIdx(i);
            let id = frame.var(idx)?.id;
            Some((id, frame.digest(idx)?))
        })
        .collect()
}
