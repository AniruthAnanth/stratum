//! `DatasetFingerprint`, the `03` §4.3 bump table, and interning — `03` §§4.2–4.5.
//!
//! Hashing an N×K dataset per command is O(N·K) and disqualifying: a 1 GB
//! dataset would add ~0.7 s to every `gen`. State identity is instead a monotone
//! version vector with content-addressed variable versions — O(columns touched)
//! per command, never O(rows), with the one bounded exception in
//! [`crate::state::digest`].
//!
//! Every entry in `03` §4.3's table is a named method here, and the table is
//! quoted on each one. Two rows do most of the work:
//!
//! * **`rename` keeps the `VarId` and the `gen`.** Only `names` and `var_layout`
//!   move. Data did not change, so a downstream block reading the *new* name
//!   stays Current, and one reading the old name fails name resolution and goes
//!   `Broken` — which is exactly right and is a case pure document order gets
//!   wrong in both directions.
//! * **Row membership is one counter, not a per-column bump.** `drop if` on a
//!   200-column frame is O(1) here, and `eff(v, S)` carries `row_membership`, so
//!   every column is invalidated by that single increment.

use std::sync::Arc;

use rustc_hash::FxHashMap;
use stratum_proto::{DatasetStateId, ExecutionId, FrameId, VarId};

use crate::state::fingerprint::FingerprintAcc;
use crate::state::versions::{VarVersion, VarVersions};

/// The `tsset`/`xtset` declaration, as far as staleness is concerned.
///
/// A time-series operator (`L.`, `F.`, `D.`, `S.`) means the answer depends on
/// this declaration, so re-`tsset`ing on a different time variable must restale
/// every block that used one. Nothing else in the workspace declares this type
/// yet; when the `tsset` command lands it should own the richer version and this
/// should become a projection of it rather than a twin (A10).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TsSpec {
    /// The time variable.
    pub timevar: VarId,
    /// The panel variable, for `xtset`.
    pub panelvar: Option<VarId>,
    /// `delta()`, in the time variable's units, as the literal the user wrote.
    pub delta: Box<str>,
    /// The time variable's display format, which fixes the unit.
    pub format: Box<str>,
}

/// One frame's state identity — `03` §4.2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatasetFingerprint {
    /// Interned identity — spec §13's "Dataset state: D17".
    pub id: DatasetStateId,
    /// Which frame this describes.
    pub frame: FrameId,
    /// Observations.
    pub nobs: u64,
    /// Bumps when the SET of rows changes.
    pub row_membership: u64,
    /// Bumps when the ORDER of rows changes.
    pub row_order: u64,
    /// Bumps on add/drop/rename/reorder of columns, and on metadata changes.
    pub var_layout: u64,
    /// Column identity → provenance version.
    pub vars: VarVersions,
    /// Name → column identity. Only this map moves on `rename`.
    pub names: Arc<FxHashMap<Box<str>, VarId>>,
    /// The sort keys in force, if the sort is valid.
    pub sorted_by: Option<Arc<[VarId]>>,
    /// Whether that sort broke ties stably. Feeds repro lint R008 (ADR-015).
    pub sort_was_stable: bool,
    /// The `tsset` declaration in force.
    pub tsset: Option<Arc<TsSpec>>,
    /// The XOR fold over `(VarId, gen)`, kept incrementally (`03` §4.5).
    pub acc: FingerprintAcc,
}

/// The effective version of a variable — `03` §4.2.
///
/// `row_membership` is part of the triple, which is why `drop if` needs no
/// per-column bump. `row_order` is *not*, and enters a block's dependencies only
/// when the block is structurally order-sensitive (`03` §4.8) — the reason a
/// `sort` inserted for readability does not grey out every model in the file.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Eff {
    /// Column identity.
    pub var: VarId,
    /// Its provenance version.
    pub gen: u32,
    /// The frame's row-membership counter.
    pub row_membership: u64,
}

impl DatasetFingerprint {
    /// An empty frame's fingerprint.
    ///
    /// `row_membership` and `var_layout` are seeded from `carry` so that a
    /// `clear` followed by a `use` never produces counters an older, unrelated
    /// state could match (`03` §4.3, last row: "monotone across the replacement
    /// so old ids never look valid").
    #[must_use]
    pub fn empty(frame: FrameId, carry: Carry) -> Self {
        Self {
            id: DatasetStateId(0),
            frame,
            nobs: 0,
            row_membership: carry.row_membership + 1,
            row_order: carry.row_order + 1,
            var_layout: carry.var_layout + 1,
            vars: VarVersions::new(),
            names: Arc::default(),
            sorted_by: None,
            sort_was_stable: true,
            tsset: None,
            acc: FingerprintAcc::default(),
        }
    }

    /// The monotone counters a replacement must not go below.
    #[must_use]
    pub fn carry(&self) -> Carry {
        Carry {
            row_membership: self.row_membership,
            row_order: self.row_order,
            var_layout: self.var_layout,
        }
    }

    /// `eff(v, S)` — the triple staleness compares.
    #[must_use]
    pub fn eff(&self, var: VarId) -> Option<Eff> {
        self.vars.get(var).map(|v| Eff {
            var,
            gen: v.gen,
            row_membership: self.row_membership,
        })
    }

    /// Resolve a name to column identity.
    #[must_use]
    pub fn id_of(&self, name: &str) -> Option<VarId> {
        self.names.get(name).copied()
    }

    /// The version record for a column.
    #[must_use]
    pub fn version_of(&self, var: VarId) -> Option<VarVersion> {
        self.vars.get(var)
    }

    /// Live columns.
    #[must_use]
    pub fn n_vars(&self) -> usize {
        self.vars.len()
    }

    // -----------------------------------------------------------------------
    // `03` §4.3, row by row.
    // -----------------------------------------------------------------------

    /// > `replace`, `recode`, `egen` on existing, `mvencode`, `destring` in
    /// > place, `encode`/`decode` in place, label changes that alter displayed
    /// > values → `gen += 1` for that column, `origin = current exec`.
    ///
    /// Called **once per column per command commit**, never per element. The
    /// barrier is what guarantees that; see [`crate::state::barrier`].
    pub fn bump_value(&mut self, var: VarId, exec: ExecutionId) -> Option<u32> {
        let prev = self.vars.get(var)?;
        let gen = prev.gen + 1;
        self.acc.revise_var(var, prev.gen, gen);
        self.vars.insert(VarVersion {
            var,
            gen,
            origin: exec,
        });
        Some(gen)
    }

    /// Re-stamp a column's `origin` without moving its `gen` — the convergent
    /// commit path. The data is provably unchanged, so nothing downstream may be
    /// restaled, but "who last ran this" is still news for spec §20.
    pub fn touch_origin(&mut self, var: VarId, exec: ExecutionId) {
        if let Some(mut v) = self.vars.get(var) {
            v.origin = exec;
            self.vars.insert(v);
        }
    }

    /// > `generate`, `egen` new, `clonevar`, `tempvar` creation, `svmat` → new
    /// > `VarId` from the session counter, `gen = 0`, `var_layout += 1`.
    pub fn create(&mut self, var: VarId, name: &str, exec: ExecutionId) {
        let v = VarVersion::created(var, exec);
        if let Some(prev) = self.vars.insert(v) {
            // Reusing a live id would silently alias two columns. The session
            // counter never reuses; this catches a caller that invented one.
            debug_assert!(false, "VarId {var} was already live at gen {}", prev.gen);
            self.acc.toggle_var(var, prev.gen);
        }
        self.acc.toggle_var(var, 0);
        Arc::make_mut(&mut self.names).insert(name.into(), var);
        self.var_layout += 1;
    }

    /// > `drop varlist` → remove from `vars` and `names`, `var_layout += 1`.
    /// > `VarId` is never reused.
    pub fn drop_var(&mut self, var: VarId, name: &str) {
        if let Some(prev) = self.vars.remove(var) {
            self.acc.toggle_var(var, prev.gen);
        }
        Arc::make_mut(&mut self.names).remove(name);
        self.var_layout += 1;
    }

    /// > `rename old new` → **same `VarId`, same `gen`** — only the `names` map
    /// > and `var_layout` change.
    ///
    /// Returns false when `old` does not resolve.
    pub fn rename(&mut self, old: &str, new: &str) -> bool {
        let names = Arc::make_mut(&mut self.names);
        let Some(id) = names.remove(old) else {
            return false;
        };
        names.insert(new.into(), id);
        self.var_layout += 1;
        true
    }

    /// > `order`, `move`, `aorder` → `var_layout += 1` only. No `gen` bumps.
    /// > Column display order is not data.
    pub fn reorder(&mut self) {
        self.var_layout += 1;
    }

    /// > `label var`, `label values`, `notes`, `char` → `var_layout += 1` only.
    ///
    /// Metadata affects output *rendering*, so a command whose
    /// `EffectSet.reads_metadata` is true depends on `var_layout`; `regress`
    /// does not, `tabulate`/`summarize` do.
    pub fn touch_metadata(&mut self) {
        self.var_layout += 1;
    }

    /// > `drop if`, `keep if`, `keep in`, `drop in`, `expand`,
    /// > `duplicates drop`, `sample`, `bsample`, `append`, `merge`, `joinby`,
    /// > `cross`, `collapse`, `contract`, `reshape`, `set obs`, `use`,
    /// > `import *`, `frame put/post` → `row_membership += 1`, `nobs` updated.
    /// > **No per-column `gen` bumps** — one counter invalidates every column
    /// > via `eff()`. O(1), not O(#vars).
    pub fn change_membership(&mut self, nobs: u64) {
        self.row_membership += 1;
        self.nobs = nobs;
    }

    /// > `sort`, `gsort`, `shuffle`, `bysort` reordering, `order`-of-rows
    /// > changes from `merge` → `row_order += 1`, `sorted_by` updated,
    /// > `sort_was_stable` recorded. Membership and `gen`s untouched.
    pub fn change_order(&mut self, sorted_by: Option<Arc<[VarId]>>, stable: bool) {
        self.row_order += 1;
        self.sorted_by = sorted_by;
        self.sort_was_stable = stable;
    }

    /// `tsset`/`xtset`, and `tsset, clear`.
    ///
    /// Filed under `var_layout` because a declaration change is a shape change:
    /// it does not move a byte of data but it changes what `L.x` means.
    pub fn set_tsset(&mut self, spec: Option<Arc<TsSpec>>) {
        if self.tsset != spec {
            self.tsset = spec;
            self.var_layout += 1;
        }
    }

    /// The interning key of `03` §4.5.
    ///
    /// The accumulator alone is never trusted: [`DatasetInterner::intern`]
    /// verifies full structural equality on a hit, so a collision costs a
    /// comparison rather than a wrong `DatasetStateId`.
    #[must_use]
    pub fn key(&self) -> DatasetKey {
        DatasetKey {
            acc: self.acc,
            row_membership: self.row_membership,
            row_order: self.row_order,
            var_layout: self.var_layout,
            nobs: self.nobs,
            frame: self.frame,
        }
    }

    /// Structural equality ignoring the interned id.
    ///
    /// Two fingerprints that describe the same state must compare equal even
    /// when one has not been interned yet, which is exactly the comparison
    /// convergence needs.
    #[must_use]
    pub fn same_state(&self, other: &Self) -> bool {
        self.frame == other.frame
            && self.nobs == other.nobs
            && self.row_membership == other.row_membership
            && self.row_order == other.row_order
            && self.var_layout == other.var_layout
            && self.acc == other.acc
            && self.sorted_by == other.sorted_by
            && self.sort_was_stable == other.sort_was_stable
            && self.tsset == other.tsset
            && self.vars == other.vars
            && self.names == other.names
    }
}

/// Monotone counters carried across a whole-dataset replacement.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Carry {
    /// Highest `row_membership` seen for this frame.
    pub row_membership: u64,
    /// Highest `row_order` seen.
    pub row_order: u64,
    /// Highest `var_layout` seen.
    pub var_layout: u64,
}

/// `(acc, row_membership, row_order, var_layout, nobs, frame)` — `03` §4.5.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct DatasetKey {
    /// The 128-bit fold over `(VarId, gen)`.
    pub acc: FingerprintAcc,
    /// The row-membership counter.
    pub row_membership: u64,
    /// The row-order counter.
    pub row_order: u64,
    /// The column-layout counter.
    pub var_layout: u64,
    /// Observations.
    pub nobs: u64,
    /// The frame.
    pub frame: FrameId,
}

/// Interns `DatasetFingerprint`s so a converged re-run lands on the same `D17`.
///
/// This is what turns spec §13's "Dataset state: D17" from a monotone counter
/// into a genuinely recurring identity. Without it the *convergence* work in
/// [`crate::state::digest`] would still stop the downstream cascade but the id
/// shown to the user would drift, and "am I back where I was?" would be
/// unanswerable.
#[derive(Debug, Default)]
pub struct DatasetInterner {
    by_key: FxHashMap<DatasetKey, Vec<(DatasetFingerprint, DatasetStateId)>>,
    next: u64,
    collisions: u64,
}

impl DatasetInterner {
    /// An interner that has issued no ids. The first id issued is `D1`, leaving
    /// `D0` free to mean "no dataset".
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The id for this state, allocating one only if it is genuinely new.
    ///
    /// Sets `fp.id` and returns it.
    pub fn intern(&mut self, fp: &mut DatasetFingerprint) -> DatasetStateId {
        let key = fp.key();
        let bucket = self.by_key.entry(key).or_default();
        for (known, id) in bucket.iter() {
            if known.same_state(fp) {
                fp.id = *id;
                // `03` §4.4's corollary, counted: a converged re-run comes back
                // to a `DatasetStateId` that already existed. This counter is
                // the difference between "D17 is a recurring identity" and
                // "D17 is a counter that only goes up".
                crate::state::bump(|c| &c.dataset_states_recurred, 1);
                return *id;
            }
        }
        if !bucket.is_empty() {
            // 128 bits of accumulator makes this negligible; counting it means
            // "negligible" stays a measurement rather than a claim.
            self.collisions += 1;
        }
        self.next += 1;
        let id = DatasetStateId(self.next);
        fp.id = id;
        bucket.push((fp.clone(), id));
        crate::state::bump(|c| &c.dataset_states_allocated, 1);
        id
    }

    /// Distinct states interned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.next as usize
    }

    /// True when nothing has been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.next == 0
    }

    /// Keys that held more than one distinct state. Expected to stay 0.
    #[must_use]
    pub fn collisions(&self) -> u64 {
        self.collisions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const E1: ExecutionId = ExecutionId(1);
    const E2: ExecutionId = ExecutionId(2);

    fn frame_with(vars: &[(u32, &str)]) -> DatasetFingerprint {
        let mut fp = DatasetFingerprint::empty(FrameId(0), Carry::default());
        for (id, name) in vars {
            fp.create(VarId(*id), name, E1);
        }
        fp.change_membership(74);
        fp
    }

    #[test]
    fn rename_keeps_identity_and_generation_and_moves_only_the_layout() {
        let mut fp = frame_with(&[(1, "mpg")]);
        let before_gen = fp.version_of(VarId(1)).unwrap().gen;
        let before_acc = fp.acc;
        let layout = fp.var_layout;

        assert!(fp.rename("mpg", "mpg_hwy"));

        assert_eq!(fp.id_of("mpg_hwy"), Some(VarId(1)), "identity survives");
        assert_eq!(fp.id_of("mpg"), None, "the old name stops resolving");
        assert_eq!(fp.version_of(VarId(1)).unwrap().gen, before_gen);
        assert_eq!(fp.acc, before_acc, "no data moved, so the fold cannot move");
        assert_eq!(fp.var_layout, layout + 1);
    }

    #[test]
    fn dropping_rows_costs_one_counter_not_one_bump_per_column() {
        let mut fp = frame_with(&[(1, "a"), (2, "b"), (3, "c")]);
        let acc = fp.acc;
        let gens: Vec<u32> = fp.vars.iter().map(|v| v.gen).collect();

        fp.change_membership(40);

        assert_eq!(fp.acc, acc, "membership is not folded per column");
        assert_eq!(fp.vars.iter().map(|v| v.gen).collect::<Vec<_>>(), gens);
        // …but every column's effective version moved, which is the point.
        assert_eq!(fp.eff(VarId(1)).unwrap().row_membership, fp.row_membership);
    }

    #[test]
    fn reordering_columns_is_not_data() {
        let mut fp = frame_with(&[(1, "a"), (2, "b")]);
        let acc = fp.acc;
        let (m, o) = (fp.row_membership, fp.row_order);
        fp.reorder();
        assert_eq!(fp.acc, acc);
        assert_eq!((fp.row_membership, fp.row_order), (m, o));
    }

    #[test]
    fn a_converged_commit_returns_the_same_dataset_state_id() {
        // `03` §4.4's corollary, which is the whole reason the title says
        // "content-addressed".
        let mut interner = DatasetInterner::new();
        let mut fp = frame_with(&[(1, "income")]);
        let first = interner.intern(&mut fp);

        // A command runs and its column converges: `touch_origin`, not
        // `bump_value`.
        fp.touch_origin(VarId(1), E2);
        let again = interner.intern(&mut fp);
        assert_eq!(first, again, "convergence must recur, not advance");

        // A command that genuinely changes the column does advance.
        fp.bump_value(VarId(1), E2);
        let third = interner.intern(&mut fp);
        assert_ne!(first, third);
        assert_eq!(interner.len(), 2);
        assert_eq!(interner.collisions(), 0);
    }

    #[test]
    fn a_replacement_frame_never_reuses_an_old_states_counters() {
        let mut interner = DatasetInterner::new();
        let mut a = frame_with(&[(1, "x")]);
        let first = interner.intern(&mut a);
        // `clear`, then rebuild something that looks identical.
        let mut b = DatasetFingerprint::empty(FrameId(0), a.carry());
        b.create(VarId(2), "x", E1);
        b.change_membership(74);
        let second = interner.intern(&mut b);
        assert_ne!(first, second);
        assert!(b.var_layout > a.var_layout);
        assert!(b.row_membership > a.row_membership);
    }

    #[test]
    fn dropping_a_column_removes_it_from_the_fold() {
        let mut fp = frame_with(&[(1, "a"), (2, "b")]);
        let with_both = fp.acc;
        fp.drop_var(VarId(2), "b");
        assert_ne!(fp.acc, with_both);
        assert_eq!(fp.id_of("b"), None);
        assert_eq!(fp.n_vars(), 1);
    }
}
