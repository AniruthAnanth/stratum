//! `Frame` — one dataset — and [`Frame::col_mut`], the write barrier.
//!
//! # Everything shareable is behind an `Arc`
//!
//! `vars`, `cols`, `by_name`, `labels` and `chars` are each one pointer. So
//! [`Frame::snapshot`] allocates **nothing at all**, and [`Frame::copy`] — Stata's
//! `frame copy` — allocates a small constant that does not grow with either the
//! variable count or the observation count. `04` §3.2 lists what that buys:
//! `preserve`/`restore` without a temp-file dance, a Data Editor that holds a
//! snapshot across a scroll gesture while the interpreter keeps executing, and
//! the retained clean-state baseline of spec §15.
//!
//! # The barrier
//!
//! [`Frame::col_mut`] is the only way to obtain a mutable column, and it is the
//! only place that:
//!
//! 1. opens the metadata snapshot the rollback needs,
//! 2. bumps [`DataVersion`] and marks the frame changed,
//! 3. invalidates [`SortState`] when the column is a sort key,
//! 4. hands out a [`ColMut`], which journals each chunk **before** dirtying it.
//!
//! There is no second path. `Column`'s chunk accessors are `pub(crate)`, the
//! `Vec`s inside `NumCol`/`FixedStrCol` are private, and the `compile_fail`
//! doctests on the crate root prove from *outside* the crate that neither is
//! reachable. That is what makes INV-2 a property of the type system rather
//! than of everyone remembering.

use std::sync::Arc;

use rustc_hash::FxHashMap;
use stratum_proto::{ColumnDigest, DatasetStateId, FrameInfo, SortDir, StorageType, VarId, VarIdx};

use crate::chars::CharTable;
use crate::chunk::{chunk_of, offset_in_chunk};
use crate::column::{self, Column, ColumnRef, WriteError};
use crate::journal::Journal;
use crate::labels::ValueLabelSet;
use crate::sort::{self, SortError, SortState, Strategy};
use crate::variable::{is_valid_name, Variable};
use crate::version::{DataVersion, FrameEpoch};

/// What a frame operation refused to do.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FrameError {
    /// `r(111)`, measured: "nosuchvar not found".
    #[error("{0} not found")]
    NotFound(String),
    /// `r(110)`: "already defined".
    #[error("variable {0} already defined")]
    Duplicate(String),
    /// `r(198)`: not a legal Stata name.
    #[error("invalid name {0}")]
    InvalidName(String),
    /// A variable index that does not exist. A caller bug, never a user's.
    #[error("no variable at position {0}")]
    BadIndex(u32),
    /// The value did not fit and the column must be promoted first.
    #[error(transparent)]
    Write(#[from] WriteError),
    /// The sort could not run.
    #[error(transparent)]
    Sort(#[from] SortError),
}

impl FrameError {
    /// Stata's return code (measured, `tests/golden/stata18/errors.log`).
    #[must_use]
    pub fn rc(&self) -> u16 {
        match self {
            FrameError::NotFound(_) => 111,
            FrameError::Duplicate(_) => 110,
            FrameError::InvalidName(_) | FrameError::BadIndex(_) => 198,
            FrameError::Write(WriteError::TypeMismatch) => WriteError::RC_TYPE_MISMATCH,
            FrameError::Write(WriteError::NeedsPromotion(_)) => 198,
            FrameError::Sort(_) => 198,
        }
    }
}

/// Everything a rollback has to put back that is not a column.
#[derive(Clone, Debug)]
struct Meta {
    vars: Arc<Vec<Variable>>,
    by_name: Arc<FxHashMap<Arc<str>, VarIdx>>,
    labels: Arc<ValueLabelSet>,
    chars: Arc<CharTable>,
    label: Arc<str>,
    nobs: u64,
    sort: SortState,
    version: DataVersion,
    epoch: FrameEpoch,
    changed: bool,
    next_var_id: u32,
}

/// One dataset.
///
/// There is no global "the dataset" anywhere in this codebase: every API takes
/// a `&Frame`. Retrofitting frames onto a single-dataset engine is the single
/// most expensive mistake available here, and it is free to avoid now
/// (`04` §7).
#[derive(Debug)]
pub struct Frame {
    name: Arc<str>,
    label: Arc<str>,
    vars: Arc<Vec<Variable>>,
    cols: Arc<Vec<ColumnRef>>,
    by_name: Arc<FxHashMap<Arc<str>, VarIdx>>,
    nobs: u64,
    sort: SortState,
    labels: Arc<ValueLabelSet>,
    chars: Arc<CharTable>,
    version: DataVersion,
    epoch: FrameEpoch,
    changed: bool,
    next_var_id: u32,
    journal: Journal,
    saved_meta: Option<Box<Meta>>,
}

impl Frame {
    /// An empty frame with no variables and no observations.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: Arc::from(name),
            label: Arc::from(""),
            vars: Arc::new(Vec::new()),
            cols: Arc::new(Vec::new()),
            by_name: Arc::new(FxHashMap::default()),
            nobs: 0,
            sort: SortState::unsorted(),
            labels: Arc::new(ValueLabelSet::new()),
            chars: Arc::new(CharTable::new()),
            version: DataVersion::INITIAL,
            epoch: FrameEpoch::INITIAL,
            changed: false,
            next_var_id: 1,
            journal: Journal::new(),
            saved_meta: None,
        }
    }

    /// The frame's name (`default` at session start).
    #[must_use]
    pub fn name(&self) -> &Arc<str> {
        &self.name
    }

    /// Rename the frame itself. `pub(crate)`: the name is a key in
    /// [`FrameSet`](crate::frames::FrameSet), so only the set may change it.
    pub(crate) fn set_name(&mut self, name: Arc<str>) {
        self.name = name;
    }

    /// The dataset label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Set the dataset label.
    pub fn set_label(&mut self, label: &str) {
        self.save_meta();
        self.label = Arc::from(label);
        self.touch();
    }

    /// `_N`.
    #[must_use]
    pub fn n_obs(&self) -> u64 {
        self.nobs
    }

    /// How many variables.
    #[must_use]
    pub fn n_vars(&self) -> u32 {
        self.vars.len() as u32
    }

    /// Every variable, in storage order.
    #[must_use]
    pub fn vars(&self) -> &[Variable] {
        &self.vars
    }

    /// One variable's metadata.
    #[must_use]
    pub fn var(&self, idx: VarIdx) -> Option<&Variable> {
        self.vars.get(idx.0 as usize)
    }

    /// One variable's storage.
    #[must_use]
    pub fn col(&self, idx: VarIdx) -> Option<&Column> {
        self.cols.get(idx.0 as usize).map(|c| &**c)
    }

    /// The shared pointer, for a caller that wants to retain the column.
    #[must_use]
    pub fn col_ref(&self, idx: VarIdx) -> Option<&ColumnRef> {
        self.cols.get(idx.0 as usize)
    }

    /// Resolve a name to a position. One hash lookup, never a scan — varlist
    /// resolution happens on every command.
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<VarIdx> {
        self.by_name.get(name).copied()
    }

    /// The current sort state.
    #[must_use]
    pub fn sort_state(&self) -> &SortState {
        &self.sort
    }

    /// The value-label tables.
    #[must_use]
    pub fn labels(&self) -> &ValueLabelSet {
        &self.labels
    }

    /// Mutable access to the value-label tables.
    pub fn labels_mut(&mut self) -> &mut ValueLabelSet {
        self.save_meta();
        self.touch();
        Arc::make_mut(&mut self.labels)
    }

    /// The characteristics (and, through them, the notes).
    #[must_use]
    pub fn chars(&self) -> &CharTable {
        &self.chars
    }

    /// Mutable access to the characteristics.
    pub fn chars_mut(&mut self) -> &mut CharTable {
        self.save_meta();
        self.touch();
        Arc::make_mut(&mut self.chars)
    }

    /// The value version — spec §13's "Dataset state: D17".
    #[must_use]
    pub fn version(&self) -> DataVersion {
        self.version
    }

    /// The shape version.
    #[must_use]
    pub fn epoch(&self) -> FrameEpoch {
        self.epoch
    }

    /// Has anything been written since the last `save`?
    #[must_use]
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// Clear the changed flag, after a successful `save`.
    pub fn mark_saved(&mut self) {
        self.changed = false;
    }

    /// Resident bytes across every column — the input to
    /// [`MemoryPolicy::admit`](crate::perf::MemoryPolicy::admit).
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        self.cols.iter().map(|c| c.heap_bytes()).sum()
    }

    /// The wire projection the frames sidebar renders.
    #[must_use]
    pub fn info(&self) -> FrameInfo {
        FrameInfo {
            name: self.name.to_string(),
            n_obs: self.nobs,
            n_vars: self.n_vars(),
            sorted_by: if self.sort.valid {
                self.sort
                    .keys
                    .iter()
                    .filter_map(|k| self.var(*k))
                    .map(|v| v.name.to_string())
                    .collect()
            } else {
                Vec::new()
            },
            changed: self.changed,
            state: DatasetStateId::from(self.version),
        }
    }

    /// `blake3-128` over one column's bytes (CONTRACTS §1.1).
    #[must_use]
    pub fn digest(&self, idx: VarIdx) -> Option<ColumnDigest> {
        self.col(idx).map(Column::digest)
    }

    // -----------------------------------------------------------------------
    // Command lifecycle
    // -----------------------------------------------------------------------

    /// Open a rollbackable command. Every write from here until
    /// [`commit`](Self::commit) or [`rollback`](Self::rollback) is journalled.
    ///
    /// Outside a command the journal is closed and writes retain nothing, which
    /// is what makes the bulk load path (`use`, which has nothing to roll back
    /// to) free of retention.
    pub fn begin_command(&mut self) {
        self.journal.begin();
        self.saved_meta = None;
    }

    /// The command succeeded: drop everything retained.
    pub fn commit(&mut self) {
        self.journal.commit();
        self.saved_meta = None;
    }

    /// The command failed or was interrupted: restore the frame exactly.
    ///
    /// INV-2. Every dirtied chunk goes back, the column list goes back, any
    /// reordering is undone, and the metadata goes back — so a column digest
    /// taken before the command and after the rollback is the same 16 bytes.
    pub fn rollback(&mut self) {
        self.journal.rollback_into(&mut self.cols);
        if let Some(m) = self.saved_meta.take() {
            self.vars = m.vars;
            self.by_name = m.by_name;
            self.labels = m.labels;
            self.chars = m.chars;
            self.label = m.label;
            self.nobs = m.nobs;
            self.sort = m.sort;
            self.version = m.version;
            self.epoch = m.epoch;
            self.changed = m.changed;
            self.next_var_id = m.next_var_id;
        }
    }

    /// Is a rollbackable command in flight?
    #[must_use]
    pub fn in_command(&self) -> bool {
        self.journal.is_open()
    }

    /// The journal, for the counters an acceptance test reads.
    #[must_use]
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Retain the metadata once per command, on the first thing that changes it.
    fn save_meta(&mut self) {
        if !self.journal.is_open() || self.saved_meta.is_some() {
            return;
        }
        self.saved_meta = Some(Box::new(Meta {
            vars: Arc::clone(&self.vars),
            by_name: Arc::clone(&self.by_name),
            labels: Arc::clone(&self.labels),
            chars: Arc::clone(&self.chars),
            label: Arc::clone(&self.label),
            nobs: self.nobs,
            sort: self.sort.clone(),
            version: self.version,
            epoch: self.epoch,
            changed: self.changed,
            next_var_id: self.next_var_id,
        }));
    }

    fn touch(&mut self) {
        self.version = self.version.next();
        self.changed = true;
    }

    fn touch_shape(&mut self) {
        self.epoch = self.epoch.next();
        self.touch();
    }

    // -----------------------------------------------------------------------
    // The write barrier
    // -----------------------------------------------------------------------

    /// **The only path to a mutable column.**
    ///
    /// # Errors
    ///
    /// [`FrameError::BadIndex`] when `idx` names no variable.
    pub fn col_mut(&mut self, idx: VarIdx) -> Result<ColMut<'_>, FrameError> {
        let i = idx.0 as usize;
        if i >= self.cols.len() {
            return Err(FrameError::BadIndex(idx.0));
        }
        self.save_meta();
        self.version = self.version.next();
        self.changed = true;
        if self.sort.is_key(idx) {
            // `04` §6.1: any write to a key column invalidates the sort. Doing
            // it here rather than in each caller is why it cannot be forgotten.
            self.sort.invalidate();
        }
        let version = self.version;
        Arc::make_mut(&mut self.vars)[i].version = version;

        // The outer `make_mut` clones a `Vec` of chunk POINTERS when a snapshot
        // is alive — 1.2 KB for a 10 M-row column — never the data.
        let col = Arc::make_mut(&mut Arc::make_mut(&mut self.cols)[i]);
        Ok(ColMut {
            col,
            journal: &mut self.journal,
            var: idx,
            live: None,
        })
    }

    /// Mutable access to one variable's metadata — label, display format,
    /// value-label attachment, provenance.
    ///
    /// The metadata sibling of [`col_mut`](Self::col_mut): it opens the same
    /// rollback snapshot, and it bumps the shape epoch (the layout counter a
    /// metadata-sensitive consumer watches) but **not** the variable's own
    /// write [`version`](Variable::version) — relabelling changes no value,
    /// and a convergence check that saw the column's version move would re-run
    /// every dependent block for a label edit. The sort state survives for the
    /// same reason.
    ///
    /// The name and storage type still change only through
    /// [`rename_var`](Self::rename_var) and [`recast_var`](Self::recast_var):
    /// writing `Variable::name` here would desync the name index, and writing
    /// `Variable::ty` would lie about the column underneath. This method
    /// exists for the fields those two do not cover.
    pub fn var_mut(&mut self, idx: VarIdx) -> Option<&mut Variable> {
        let i = idx.0 as usize;
        if i >= self.vars.len() {
            return None;
        }
        self.save_meta();
        self.touch_shape();
        Some(&mut Arc::make_mut(&mut self.vars)[i])
    }

    // -----------------------------------------------------------------------
    // Structure
    // -----------------------------------------------------------------------

    /// Add a variable of `ty`, filled with `.`.
    ///
    /// # Errors
    ///
    /// [`FrameError::InvalidName`] or [`FrameError::Duplicate`].
    pub fn add_var(&mut self, name: &str, ty: StorageType) -> Result<VarIdx, FrameError> {
        self.add_column(name, Column::new_missing(ty, self.nobs))
    }

    /// Add a variable backed by an existing column — the bulk load path.
    ///
    /// # Errors
    ///
    /// [`FrameError::InvalidName`], [`FrameError::Duplicate`], or a length
    /// mismatch reported as [`FrameError::BadIndex`] of the new position.
    pub fn add_column(&mut self, name: &str, col: Column) -> Result<VarIdx, FrameError> {
        if !is_valid_name(name) {
            return Err(FrameError::InvalidName(name.to_owned()));
        }
        if self.by_name.contains_key(name) {
            return Err(FrameError::Duplicate(name.to_owned()));
        }
        // The first variable defines `_N` for an empty frame; after that every
        // column must agree, because a ragged frame has no meaning.
        if self.vars.is_empty() {
            self.nobs = col.len();
        } else if col.len() != self.nobs {
            return Err(FrameError::BadIndex(self.n_vars()));
        }

        self.save_meta();
        self.journal.note_columns(&self.cols);

        let idx = VarIdx(self.n_vars());
        let id = VarId(self.next_var_id);
        self.next_var_id += 1;
        let var = Variable::new(id, name, col.storage_type(), self.version.next());
        let key = Arc::clone(&var.name);
        Arc::make_mut(&mut self.vars).push(var);
        Arc::make_mut(&mut self.cols).push(Arc::new(col));
        Arc::make_mut(&mut self.by_name).insert(key, idx);
        self.touch_shape();
        Ok(idx)
    }

    /// `drop varname`. Positions after `idx` shift down, which is why
    /// [`VarIdx`] is documented as a position and [`VarId`] as an identity.
    ///
    /// # Errors
    ///
    /// [`FrameError::BadIndex`].
    pub fn drop_var(&mut self, idx: VarIdx) -> Result<(), FrameError> {
        let i = idx.0 as usize;
        if i >= self.vars.len() {
            return Err(FrameError::BadIndex(idx.0));
        }
        self.save_meta();
        self.journal.note_columns(&self.cols);
        let name = Arc::clone(&self.vars[i].name);
        Arc::make_mut(&mut self.vars).remove(i);
        Arc::make_mut(&mut self.cols).remove(i);
        Arc::make_mut(&mut self.chars).remove_owner(&name);
        self.reindex();
        // Dropping a key means the frame is no longer sorted by what it says.
        if self.sort.is_key(idx) {
            self.sort = SortState::unsorted();
        }
        self.touch_shape();
        Ok(())
    }

    /// `rename old new`. Characteristics move with the variable.
    ///
    /// # Errors
    ///
    /// [`FrameError::BadIndex`], [`FrameError::InvalidName`] or
    /// [`FrameError::Duplicate`].
    pub fn rename_var(&mut self, idx: VarIdx, new: &str) -> Result<(), FrameError> {
        let i = idx.0 as usize;
        if i >= self.vars.len() {
            return Err(FrameError::BadIndex(idx.0));
        }
        if !is_valid_name(new) {
            return Err(FrameError::InvalidName(new.to_owned()));
        }
        if self.by_name.get(new).is_some_and(|v| *v != idx) {
            return Err(FrameError::Duplicate(new.to_owned()));
        }
        self.save_meta();
        let old = Arc::clone(&self.vars[i].name);
        Arc::make_mut(&mut self.vars)[i].name = Arc::from(new);
        Arc::make_mut(&mut self.chars).rename_owner(&old, new);
        self.reindex();
        self.touch_shape();
        Ok(())
    }

    /// `recast`, and the target of Stata's automatic promotion.
    ///
    /// # Errors
    ///
    /// [`FrameError::BadIndex`].
    pub fn recast_var(&mut self, idx: VarIdx, to: StorageType) -> Result<(), FrameError> {
        let i = idx.0 as usize;
        let Some(col) = self.cols.get(i).map(Arc::clone) else {
            return Err(FrameError::BadIndex(idx.0));
        };
        if col.storage_type() == to {
            return Ok(());
        }
        self.save_meta();
        self.journal.note_column(idx, &col);
        let rebuilt = column::recast(&col, to);
        Arc::make_mut(&mut self.cols)[i] = Arc::new(rebuilt);
        let v = &mut Arc::make_mut(&mut self.vars)[i];
        v.ty = to;
        v.format = stratum_core::fmt::StataFormat::parse(stratum_core::types::default_format(to))
            .expect("default_format returns a parseable format");
        self.touch_shape();
        Ok(())
    }

    /// `set obs` / `expand` / `drop in`: change the observation count.
    ///
    /// New observations are `.` (or `""`). Shrinking truncates.
    pub fn set_n_obs(&mut self, n: u64) {
        if n == self.nobs {
            return;
        }
        self.save_meta();
        self.journal.note_columns(&self.cols);
        let cols = Arc::make_mut(&mut self.cols);
        for slot in cols.iter_mut() {
            *slot = Arc::new(resize(slot, n));
        }
        self.nobs = n;
        self.sort = SortState::unsorted();
        self.touch_shape();
    }

    fn reindex(&mut self) {
        let mut map = FxHashMap::default();
        for (i, v) in self.vars.iter().enumerate() {
            map.insert(Arc::clone(&v.name), VarIdx(i as u32));
        }
        self.by_name = Arc::new(map);
    }

    // -----------------------------------------------------------------------
    // Sorting
    // -----------------------------------------------------------------------

    /// `sort` / `gsort`: physically reorder every column.
    ///
    /// The journal retains the inverse permutation — 4 bytes per observation —
    /// rather than a copy of every column, so rolling a sort back on a 1.2 GB
    /// frame costs 40 MB of retention and one more gather pass.
    ///
    /// # Errors
    ///
    /// [`FrameError::BadIndex`] or [`FrameError::Sort`].
    pub fn sort_by(&mut self, keys: &[(VarIdx, SortDir)]) -> Result<(), FrameError> {
        for (k, _) in keys {
            if k.0 as usize >= self.cols.len() {
                return Err(FrameError::BadIndex(k.0));
            }
        }
        let cols: Vec<(&Column, SortDir)> = keys
            .iter()
            .map(|(k, d)| (&**self.cols.get(k.0 as usize).expect("checked above"), *d))
            .collect();
        let perm = sort::permutation(&cols, self.nobs, Strategy::Auto)?;
        drop(cols);

        self.save_meta();
        self.journal
            .note_row_order(Arc::from(sort::invert(&perm).into_boxed_slice()));
        let cols: &mut Vec<ColumnRef> = Arc::make_mut(&mut self.cols);
        sort::permute_all(cols, &perm);
        self.sort = SortState {
            keys: keys.iter().map(|(k, _)| *k).collect(),
            // `gsort` with a descending key does not produce a `.dta` sortlist
            // Stata would recognise, so only an all-ascending sort is claimed.
            valid: keys.iter().all(|(_, d)| *d == SortDir::Asc),
        };
        self.touch();
        Ok(())
    }

    /// Record a sort order already true of the data, without permuting — the
    /// `.dta` `sortlist` on load, where the rows arrive in file order and
    /// re-sorting would be O(n log n) work to prove what the file asserts.
    ///
    /// The claim is the caller's, exactly as Stata trusts a `sortlist`; any
    /// later write to a key column invalidates it through
    /// [`col_mut`](Self::col_mut) as usual. Empty `keys` records "unsorted".
    /// [`sort_by`](Self::sort_by) remains the only path that moves rows.
    ///
    /// # Errors
    ///
    /// [`FrameError::BadIndex`] when a key names no variable; nothing is
    /// recorded then.
    pub fn set_sort_state(&mut self, keys: &[VarIdx]) -> Result<(), FrameError> {
        for k in keys {
            if k.0 as usize >= self.vars.len() {
                return Err(FrameError::BadIndex(k.0));
            }
        }
        self.save_meta();
        self.sort = SortState {
            keys: keys.to_vec(),
            valid: !keys.is_empty(),
        };
        self.touch();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Sharing
    // -----------------------------------------------------------------------

    /// An immutable view of the frame as it is right now.
    ///
    /// Allocates nothing: five pointer clones and two integers. The Data Editor
    /// holds one of these across a scroll gesture while the interpreter keeps
    /// executing (`04` §3.2).
    #[must_use]
    pub fn snapshot(&self) -> FrameSnapshot {
        crate::perf::bump(&crate::perf::counters().column_arc_clones, 1);
        FrameSnapshot {
            name: Arc::clone(&self.name),
            vars: Arc::clone(&self.vars),
            cols: Arc::clone(&self.cols),
            by_name: Arc::clone(&self.by_name),
            labels: Arc::clone(&self.labels),
            chars: Arc::clone(&self.chars),
            nobs: self.nobs,
            version: self.version,
            epoch: self.epoch,
        }
    }

    /// `frame copy`: a new frame sharing every column until one of them is
    /// written (`04` §7). O(nvars) pointer work, never O(ncells).
    #[must_use]
    pub fn copy(&self, name: &str) -> Frame {
        crate::perf::bump(&crate::perf::counters().column_arc_clones, 1);
        Frame {
            name: Arc::from(name),
            label: Arc::clone(&self.label),
            vars: Arc::clone(&self.vars),
            cols: Arc::clone(&self.cols),
            by_name: Arc::clone(&self.by_name),
            nobs: self.nobs,
            sort: self.sort.clone(),
            labels: Arc::clone(&self.labels),
            chars: Arc::clone(&self.chars),
            version: DataVersion::INITIAL,
            epoch: FrameEpoch::INITIAL,
            changed: false,
            next_var_id: self.next_var_id,
            journal: Journal::new(),
            saved_meta: None,
        }
    }
}

/// Grow or shrink one column to `n` observations.
fn resize(col: &Column, n: u64) -> Column {
    let mut out = Column::new_missing(col.storage_type(), n);
    let keep = col.len().min(n);
    if col.is_numeric() {
        for row in 0..keep {
            column::write_f64(&mut out, row, col.get_f64(row).expect("numeric"))
                .expect("same storage type");
        }
    } else {
        for row in 0..keep {
            let b = col.get_bytes(row).expect("string").to_vec();
            column::write_bytes(&mut out, row, &b).expect("same storage type");
        }
    }
    out
}

/// A mutable column, obtained only from [`Frame::col_mut`].
///
/// Journals the chunk it is about to dirty, once per chunk per command. The
/// `live` field is what keeps that to one hash lookup per **chunk transition**
/// instead of one per row: a `replace` walking a column in order pays 153 hash
/// lookups on 10 M observations.
#[derive(Debug)]
pub struct ColMut<'a> {
    col: &'a mut Column,
    journal: &'a mut Journal,
    var: VarIdx,
    live: Option<usize>,
}

impl ColMut<'_> {
    /// Read-only view of the column being written.
    #[must_use]
    pub fn column(&self) -> &Column {
        self.col
    }

    /// Retain chunk `c` if this command has not already retained it.
    fn arm(&mut self, c: usize) {
        if self.live == Some(c) {
            return;
        }
        self.journal.note_chunk(self.var, c as u32, self.col);
        self.live = Some(c);
    }

    /// Write one observation.
    ///
    /// # Errors
    ///
    /// [`WriteError::NeedsPromotion`] when the value does not fit the storage
    /// type — the caller performs the promotion and retries — or
    /// [`WriteError::TypeMismatch`] for a numeric write to a string column.
    pub fn set_f64(&mut self, row: u64, v: f64) -> Result<(), WriteError> {
        self.arm(chunk_of(row));
        column::write_f64(self.col, row, v)
    }

    /// Write one string observation.
    ///
    /// # Errors
    ///
    /// [`WriteError::NeedsPromotion`] when the value is wider than the declared
    /// `str#`, or [`WriteError::TypeMismatch`] on a numeric column.
    pub fn set_bytes(&mut self, row: u64, value: &[u8]) -> Result<(), WriteError> {
        self.arm(chunk_of(row));
        column::write_bytes(self.col, row, value)
    }

    /// Write one `strL` observation with an explicit GSO binary flag
    /// (type 129 vs 130).
    ///
    /// [`set_bytes`](Self::set_bytes) covers the text case but cannot say
    /// "binary", and a blob written as text would round-trip through `save`
    /// with a spurious NUL terminator. Journals like every other write. The
    /// per-cell arena rewrite is bounded by the chunk; a bulk load builds the
    /// column with [`StrLCol`](crate::column::StrLCol)`::from_rows` instead.
    ///
    /// # Errors
    ///
    /// [`WriteError::TypeMismatch`] on anything but a `strL` column — no
    /// promotion is offered, because a `str#` column has nowhere to store the
    /// flag this call exists to set.
    pub fn set_strl(&mut self, row: u64, value: &[u8], binary: bool) -> Result<(), WriteError> {
        self.arm(chunk_of(row));
        column::write_strl(self.col, row, value, binary)
    }

    /// **The bulk write path.** Hand a whole `double` chunk to `f`.
    ///
    /// One journal entry and one 512 KiB retention per chunk, then a tight loop
    /// over a contiguous slice — which is what makes `replace x = x + 1` on 10 M
    /// rows one extra pass rather than one extra column.
    ///
    /// Answers `false` for a column that is not `Double`; the caller falls back
    /// to [`set_f64`](Self::set_f64), which narrows per value.
    pub fn with_double_chunk<F: FnOnce(u64, &mut [f64])>(&mut self, c: usize, f: F) -> bool {
        if !matches!(self.col, Column::Double(_)) {
            return false;
        }
        self.arm(c);
        let first = c as u64 * crate::chunk::CHUNK_ROWS as u64;
        match self.col {
            Column::Double(col) => f(first, col.chunk_mut(c)),
            _ => unreachable!("checked above"),
        }
        true
    }

    /// How many chunks the column has, for a caller driving `with_double_chunk`.
    #[must_use]
    pub fn n_chunks(&self) -> usize {
        self.col.n_chunks()
    }

    /// The offset of `row` inside its chunk, so a bulk writer can address it.
    #[must_use]
    pub fn offset_of(row: u64) -> usize {
        offset_in_chunk(row)
    }
}

/// An immutable view of a frame at one [`DataVersion`].
///
/// Commands receive one of these, never the live `Frame`: a snapshot cannot be
/// written to, so a statistical kernel cannot mutate the dataset it is
/// summarising even by accident.
#[derive(Clone, Debug)]
pub struct FrameSnapshot {
    name: Arc<str>,
    vars: Arc<Vec<Variable>>,
    cols: Arc<Vec<ColumnRef>>,
    by_name: Arc<FxHashMap<Arc<str>, VarIdx>>,
    labels: Arc<ValueLabelSet>,
    chars: Arc<CharTable>,
    nobs: u64,
    version: DataVersion,
    epoch: FrameEpoch,
}

impl FrameSnapshot {
    /// The frame's name.
    #[must_use]
    pub fn name(&self) -> &Arc<str> {
        &self.name
    }
    /// `_N` at the moment the snapshot was taken.
    #[must_use]
    pub fn n_obs(&self) -> u64 {
        self.nobs
    }
    /// How many variables.
    #[must_use]
    pub fn n_vars(&self) -> u32 {
        self.vars.len() as u32
    }
    /// Every variable, in storage order.
    #[must_use]
    pub fn vars(&self) -> &[Variable] {
        &self.vars
    }
    /// One variable's metadata.
    #[must_use]
    pub fn var(&self, idx: VarIdx) -> Option<&Variable> {
        self.vars.get(idx.0 as usize)
    }
    /// One variable's storage.
    #[must_use]
    pub fn col(&self, idx: VarIdx) -> Option<&Column> {
        self.cols.get(idx.0 as usize).map(|c| &**c)
    }
    /// Resolve a name to a position.
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<VarIdx> {
        self.by_name.get(name).copied()
    }
    /// The value-label tables.
    #[must_use]
    pub fn labels(&self) -> &ValueLabelSet {
        &self.labels
    }
    /// The characteristics.
    #[must_use]
    pub fn chars(&self) -> &CharTable {
        &self.chars
    }
    /// The version this snapshot froze.
    #[must_use]
    pub fn version(&self) -> DataVersion {
        self.version
    }
    /// The shape version this snapshot froze.
    #[must_use]
    pub fn epoch(&self) -> FrameEpoch {
        self.epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_core::missing::{is_missing, SYSMISS};

    fn frame_with(n: u64) -> Frame {
        let mut f = Frame::new("default");
        f.set_n_obs(n);
        f.add_var("x", StorageType::Double).expect("fresh name");
        f
    }

    #[test]
    fn a_new_variable_is_all_missing_and_bumps_the_epoch() {
        let mut f = Frame::new("default");
        f.set_n_obs(5);
        let e = f.epoch();
        let idx = f.add_var("x", StorageType::Double).expect("fresh name");
        assert_eq!(f.n_vars(), 1);
        assert_eq!(f.n_obs(), 5);
        assert!(f.epoch() > e);
        assert!(is_missing(
            f.col(idx).expect("column").get_f64(0).expect("numeric")
        ));
    }

    #[test]
    fn names_are_unique_and_validated() {
        let mut f = frame_with(3);
        assert_eq!(
            f.add_var("x", StorageType::Double).expect_err("duplicate"),
            FrameError::Duplicate("x".into())
        );
        assert_eq!(
            f.add_var("9bad", StorageType::Double)
                .expect_err("bad name")
                .rc(),
            198
        );
        assert_eq!(f.index_of("x"), Some(VarIdx(0)));
        assert_eq!(f.index_of("nope"), None);
    }

    #[test]
    fn a_write_bumps_the_version_and_invalidates_a_sort_key() {
        let mut f = frame_with(4);
        f.sort_by(&[(VarIdx(0), SortDir::Asc)]).expect("sortable");
        assert!(f.sort_state().valid);
        let v = f.version();
        f.col_mut(VarIdx(0))
            .expect("exists")
            .set_f64(0, 1.0)
            .expect("double takes any value");
        assert!(f.version() > v);
        assert!(!f.sort_state().valid, "writing a key must invalidate");
        assert!(f.changed());
    }

    #[test]
    fn promotion_is_reported_rather_than_thrown() {
        // Measured: `gen byte b = 500` is rc = 0 — Stata promotes.
        let mut f = Frame::new("default");
        f.set_n_obs(2);
        let b = f.add_var("b", StorageType::Byte).expect("fresh");
        let err = f
            .col_mut(b)
            .expect("exists")
            .set_f64(0, 500.0)
            .expect_err("500 does not fit a byte");
        assert_eq!(err, WriteError::NeedsPromotion(StorageType::Int));
        f.recast_var(b, StorageType::Int).expect("exists");
        f.col_mut(b)
            .expect("exists")
            .set_f64(0, 500.0)
            .expect("fits an int now");
        assert_eq!(f.col(b).expect("column").get_f64(0), Some(500.0));
    }

    #[test]
    fn recast_preserves_every_value_and_every_tag() {
        let mut f = Frame::new("default");
        f.set_n_obs(3);
        let i = f.add_var("i", StorageType::Int).expect("fresh");
        {
            let mut c = f.col_mut(i).expect("exists");
            c.set_f64(0, 7.0).expect("fits");
            c.set_f64(1, SYSMISS).expect("missing fits");
            c.set_f64(2, stratum_core::missing::missing_f64(3))
                .expect("tag fits");
        }
        f.recast_var(i, StorageType::Double).expect("exists");
        let col = f.col(i).expect("column");
        assert_eq!(col.get_f64(0), Some(7.0));
        assert_eq!(col.get_f64(1), Some(SYSMISS));
        assert_eq!(col.get_f64(2), Some(stratum_core::missing::missing_f64(3)));
    }

    #[test]
    fn dropping_a_variable_moves_the_ones_after_it() {
        let mut f = Frame::new("default");
        f.set_n_obs(2);
        f.add_var("a", StorageType::Byte).expect("fresh");
        f.add_var("b", StorageType::Byte).expect("fresh");
        f.add_var("c", StorageType::Byte).expect("fresh");
        f.drop_var(VarIdx(1)).expect("exists");
        assert_eq!(f.n_vars(), 2);
        assert_eq!(f.index_of("c"), Some(VarIdx(1)));
        assert_eq!(f.index_of("b"), None);
    }

    #[test]
    fn renaming_carries_the_characteristics() {
        let mut f = frame_with(2);
        f.chars_mut().set("x", "units", "USD");
        f.rename_var(VarIdx(0), "y").expect("exists");
        assert_eq!(f.chars().get("y", "units"), Some("USD"));
        assert_eq!(f.index_of("y"), Some(VarIdx(0)));
    }

    #[test]
    fn var_mut_bumps_the_layout_epoch_and_not_the_write_version() {
        // The contract stratum-runtime's `edit_var_meta` doc names: relabelling
        // changes no value, so the column's write version must hold still while
        // the layout epoch moves — and the sort must survive.
        let mut f = frame_with(4);
        f.sort_by(&[(VarIdx(0), SortDir::Asc)]).expect("sortable");
        let epoch = f.epoch();
        let write_version = f.var(VarIdx(0)).expect("exists").version;

        let v = f.var_mut(VarIdx(0)).expect("exists");
        v.label = Arc::from("Price in USD");
        v.format = stratum_core::fmt::StataFormat::parse("%9.2f").expect("parses");

        assert!(f.epoch() > epoch, "a metadata edit is a layout change");
        assert_eq!(
            f.var(VarIdx(0)).expect("exists").version,
            write_version,
            "no value moved, so the write version must not"
        );
        assert!(
            f.sort_state().valid,
            "relabelling must not invalidate a sort"
        );
        assert!(f.changed(), "the edit still needs a save");
        assert_eq!(
            f.var(VarIdx(0)).expect("exists").label.as_ref(),
            "Price in USD"
        );
        assert_eq!(
            f.var_mut(VarIdx(9)),
            None,
            "out of range is None, not a panic"
        );
    }

    #[test]
    fn a_metadata_edit_rolls_back_with_the_command() {
        let mut f = frame_with(2);
        f.begin_command();
        let v = f.var_mut(VarIdx(0)).expect("exists");
        v.label = Arc::from("doomed");
        v.value_label = Some(Arc::from("lbl"));
        f.rollback();
        let v = f.var(VarIdx(0)).expect("exists");
        assert_eq!(v.label.as_ref(), "");
        assert_eq!(v.value_label, None);
    }

    #[test]
    fn set_sort_state_records_without_permuting() {
        let mut f = frame_with(3);
        {
            let mut c = f.col_mut(VarIdx(0)).expect("exists");
            for (row, v) in [(0u64, 3.0), (1, 1.0), (2, 2.0)] {
                c.set_f64(row, v).expect("double");
            }
        }
        // The claim is recorded even though the data is visibly NOT sorted:
        // trusting the caller is the point — a `.dta` sortlist arrives exactly
        // like this, with the rows already in file order.
        f.set_sort_state(&[VarIdx(0)]).expect("exists");
        assert!(f.sort_state().valid);
        assert_eq!(f.sort_state().keys, vec![VarIdx(0)]);
        for (row, v) in [(0u64, 3.0), (1, 1.0), (2, 2.0)] {
            assert_eq!(
                f.col(VarIdx(0)).expect("column").get_f64(row),
                Some(v),
                "recording a sort must move no row"
            );
        }
        // The recorded claim dies the way a performed sort's does.
        f.col_mut(VarIdx(0))
            .expect("exists")
            .set_f64(0, 0.0)
            .expect("double");
        assert!(!f.sort_state().valid, "a key write invalidates the record");

        f.set_sort_state(&[]).expect("empty means unsorted");
        assert_eq!(f.sort_state(), &SortState::unsorted());
        assert_eq!(
            f.set_sort_state(&[VarIdx(7)])
                .expect_err("no such variable"),
            FrameError::BadIndex(7)
        );
    }

    #[test]
    fn a_recorded_sort_state_rolls_back_with_the_command() {
        let mut f = frame_with(2);
        f.begin_command();
        f.set_sort_state(&[VarIdx(0)]).expect("exists");
        assert!(f.sort_state().valid);
        f.rollback();
        assert_eq!(f.sort_state(), &SortState::unsorted());
    }

    #[test]
    fn set_strl_goes_through_the_barrier_and_rolls_back_binary_flag_included() {
        let mut f = Frame::new("default");
        f.set_n_obs(2);
        let s = f.add_var("payload", StorageType::StrL).expect("fresh");
        f.col_mut(s)
            .expect("exists")
            .set_strl(0, b"bin\0blob", true)
            .expect("strL");

        let is_binary = |f: &Frame, row: usize| match f.col(s).expect("column") {
            Column::StrL(c) => c.chunk(0).is_binary(row),
            _ => unreachable!("declared strL"),
        };
        assert_eq!(
            f.col(s).expect("column").get_bytes(0),
            Some(&b"bin\0blob"[..])
        );
        assert!(is_binary(&f, 0));

        let version = f.version();
        f.begin_command();
        f.col_mut(s)
            .expect("exists")
            .set_strl(0, b"text", false)
            .expect("strL");
        assert!(f.version() > version, "a strL write is a write");
        assert!(!is_binary(&f, 0));
        f.rollback();
        assert_eq!(
            f.col(s).expect("column").get_bytes(0),
            Some(&b"bin\0blob"[..])
        );
        assert!(
            is_binary(&f, 0),
            "rollback restores the flag, not only the bytes"
        );

        // Only a strL column can hold the flag; nothing else is promoted into one.
        let x = f.add_var("x", StorageType::Double).expect("fresh");
        assert_eq!(
            f.col_mut(x)
                .expect("exists")
                .set_strl(0, b"nope", true)
                .expect_err("numeric"),
            WriteError::TypeMismatch
        );
    }

    #[test]
    fn a_snapshot_does_not_see_later_writes() {
        let mut f = frame_with(4);
        f.col_mut(VarIdx(0))
            .expect("exists")
            .set_f64(0, 1.0)
            .expect("double");
        let snap = f.snapshot();
        f.col_mut(VarIdx(0))
            .expect("exists")
            .set_f64(0, 2.0)
            .expect("double");
        assert_eq!(snap.col(VarIdx(0)).expect("column").get_f64(0), Some(1.0));
        assert_eq!(f.col(VarIdx(0)).expect("column").get_f64(0), Some(2.0));
        assert!(snap.version() < f.version());
    }

    #[test]
    fn set_n_obs_grows_with_missing_and_shrinks_by_truncation() {
        let mut f = frame_with(2);
        f.col_mut(VarIdx(0))
            .expect("exists")
            .set_f64(1, 9.0)
            .expect("double");
        f.set_n_obs(4);
        let col = f.col(VarIdx(0)).expect("column");
        assert_eq!(col.get_f64(1), Some(9.0));
        assert!(is_missing(col.get_f64(3).expect("numeric")));
        f.set_n_obs(1);
        assert_eq!(f.n_obs(), 1);
        assert_eq!(f.col(VarIdx(0)).expect("column").len(), 1);
    }
}
