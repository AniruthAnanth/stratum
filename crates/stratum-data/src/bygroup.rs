//! By-group iteration and `_n` / `_N` (`04` §6.3).
//!
//! # The index is a fence post list, not a group table
//!
//! [`GroupIndex`] is one `Vec<u64>` of `ngroups + 1` boundaries into the frame's
//! *current physical order*. Group `k` is `starts[k] .. starts[k+1]`. Nothing
//! else is stored: no group keys, no per-group aggregates, no `Vec<Vec<u64>>`.
//!
//! That shape is the whole performance story. `by g: gen r = _n` on 10 M rows
//! resolves `_n` with one subtraction per observation and allocates
//! `8 · (ngroups + 1)` bytes once, against the pre-audit alternative of a group
//! table whose own size is O(rows) when every key is distinct — the case that
//! actually happens (`by id:` on panel data).
//!
//! # Why building it is legal at all
//!
//! `by` requires the frame to be sorted on `group_keys ++ sort_only` already,
//! so *adjacent equal keys are the same group by construction*. Detecting a
//! boundary is therefore an adjacent-row comparison and never a hash, a sort or
//! a second pass. `bysort` is `sort` followed by this; plain `by` on an unsorted
//! frame is `r(5)`, which is [`ByError::NotSorted`].
//!
//! # Missing keys are not a special case
//!
//! `04` §6.3's measured table ends with a row whose `g` is `.`, forming its own
//! group, sorted last. Nothing here implements that: [`crate::sortkey`] encodes
//! numeric missing high (`04` §2.2), the sort puts those rows at the end, and
//! the adjacent-difference scan then sees one more run. A special case here
//! would be a second, disagreeing implementation of §2.2.

use std::ops::Range;

use stratum_proto::{SortDir, VarIdx};

use crate::column::Column;
use crate::frame::Frame;
use crate::frame::FrameSnapshot;
use crate::perf::{bump, counters};
use crate::sortkey::compare_rows;

/// What `by g1 g2 (x):` asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BySpec {
    /// `by g1 g2:` — these form the groups.
    pub group_keys: Vec<VarIdx>,
    /// `bysort g1 (x):` — `x` is sort-only and does **not** form groups.
    pub sort_only: Vec<VarIdx>,
}

impl BySpec {
    /// `by k1 k2:` with no sort-only tail.
    #[must_use]
    pub fn new(group_keys: &[VarIdx]) -> Self {
        Self {
            group_keys: group_keys.to_vec(),
            sort_only: Vec::new(),
        }
    }

    /// The full key list the frame must be sorted on: groups then sort-only.
    #[must_use]
    pub fn required_sort_keys(&self) -> Vec<VarIdx> {
        let mut k = self.group_keys.clone();
        k.extend_from_slice(&self.sort_only);
        k
    }
}

/// Why a `by` prefix could not run.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ByError {
    /// `r(5)`. The frame is not sorted, or not sorted on these keys first.
    #[error("not sorted")]
    NotSorted {
        /// What the frame claims to be sorted by, in priority order.
        have: Vec<VarIdx>,
        /// What `group_keys ++ sort_only` needed it to start with.
        want: Vec<VarIdx>,
    },
    /// A key names a variable the frame does not have.
    #[error("variable {0} not found")]
    NoSuchVar(VarIdx),
    /// `by` with an empty group list. `bysort` with no keys is a plain `sort`,
    /// and the interpreter is expected to have said so before reaching here.
    #[error("by requires at least one group variable")]
    NoKeys,
}

impl ByError {
    /// Stata's return code for this failure.
    #[must_use]
    pub fn rc(&self) -> u16 {
        match self {
            // `errors.log`: "not sorted" is r(5).
            ByError::NotSorted { .. } => 5,
            ByError::NoSuchVar(_) => 111,
            ByError::NoKeys => 198,
        }
    }
}

/// The group boundaries of a sorted frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupIndex {
    /// `starts[k] .. starts[k+1]` is group `k`. `len() == ngroups + 1`, first
    /// element `0`, last element `_N`.
    pub starts: Vec<u64>,
}

impl GroupIndex {
    /// Build from a live frame, checking its [`SortState`](crate::sort::SortState).
    ///
    /// One linear pass over the group-key columns comparing adjacent rows:
    /// `O(n · nkeys)` with no allocation beyond `starts`.
    ///
    /// # Errors
    ///
    /// [`ByError::NoKeys`] for an empty group list, [`ByError::NoSuchVar`] for a
    /// key the frame does not have, and [`ByError::NotSorted`] (`r(5)`) when the
    /// frame's sort state does not begin with `group_keys ++ sort_only`.
    pub fn build(frame: &Frame, spec: &BySpec) -> Result<Self, ByError> {
        let state = frame.sort_state();
        let want = spec.required_sort_keys();
        if !state.valid || state.keys.len() < want.len() || state.keys[..want.len()] != want[..] {
            return Err(ByError::NotSorted {
                have: state.keys.clone(),
                want,
            });
        }
        let cols = resolve(spec, |i| frame.col(i))?;
        Ok(Self::from_sorted_cols(&cols, frame.n_obs()))
    }

    /// Build from a snapshot.
    ///
    /// A [`FrameSnapshot`] carries no sort state — it is the immutable view a
    /// command is handed *after* the interpreter has already checked the `by`
    /// prefix against the live frame — so this trusts the caller for the sort
    /// and checks only that the keys resolve.
    ///
    /// # Errors
    ///
    /// [`ByError::NoKeys`] or [`ByError::NoSuchVar`].
    pub fn build_on(snap: &FrameSnapshot, spec: &BySpec) -> Result<Self, ByError> {
        let cols = resolve(spec, |i| snap.col(i))?;
        Ok(Self::from_sorted_cols(&cols, snap.n_obs()))
    }

    /// The primitive: adjacent-difference over columns already in sorted order.
    ///
    /// `cols` must be the group keys in priority order and already sorted;
    /// nothing here re-checks that, because the caller that can check it cheaply
    /// is the one holding the [`SortState`](crate::sort::SortState).
    #[must_use]
    pub fn from_sorted_cols(cols: &[&Column], nobs: u64) -> Self {
        // One `starts` allocation, sized by a guess that is exact for the two
        // shapes that matter: one group (`by` over a constant) and every row its
        // own group (`by id:` on panel data). Anything between reallocates at
        // most log times, which is invisible next to the scan itself.
        let mut starts: Vec<u64> = Vec::with_capacity(16);
        starts.push(0);
        // `_N == 0` leaves `starts == [0]`, i.e. *no* groups rather than one
        // empty one: `by g: gen r = _n` over an empty frame must iterate zero
        // times, and a fence list of `[0, 0]` would make it iterate once.
        if nobs > 0 {
            for row in 1..nobs {
                // `compare_rows` is the same byte order the sort used, so a
                // boundary here is exactly a key change there. `.` and `.a` are
                // different keys and therefore different groups.
                if cols
                    .iter()
                    .any(|c| compare_rows(c, row - 1, row, SortDir::Asc).is_ne())
                {
                    starts.push(row);
                }
            }
            starts.push(nobs);
        }
        // Once per build, not per row: `04` §6.3's cost is O(n · nkeys) reads
        // and this is the counter ADR-017 wants asserted in place of the 60 ms.
        bump(&counters().rows_touched, nobs);
        Self { starts }
    }

    /// How many groups.
    #[must_use]
    pub fn ngroups(&self) -> usize {
        self.starts.len() - 1
    }

    /// True when the frame had no observations, and therefore no groups.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ngroups() == 0
    }

    /// The physical observation range of group `k`.
    ///
    /// # Panics
    ///
    /// If `k >= ngroups()`.
    #[must_use]
    pub fn group(&self, k: usize) -> Range<u64> {
        self.starts[k]..self.starts[k + 1]
    }

    /// `_N` for group `k`.
    #[must_use]
    pub fn group_len(&self, k: usize) -> u64 {
        self.starts[k + 1] - self.starts[k]
    }

    /// Which group physical observation `obs` belongs to.
    ///
    /// `O(log ngroups)`. The interpreter should hold a [`ByCursor`] and never
    /// call this on a hot path; it exists for the random-access cases (a
    /// diagnostic naming the group of one row).
    #[must_use]
    pub fn group_of(&self, obs: u64) -> Option<usize> {
        if obs >= *self.starts.last().expect("starts always has a last") {
            return None;
        }
        Some(match self.starts.binary_search(&obs) {
            Ok(k) => k,
            Err(k) => k - 1,
        })
    }

    /// A cursor positioned before the first group.
    #[must_use]
    pub fn cursor(&self) -> ByCursor<'_> {
        ByCursor {
            idx: self,
            k: 0,
            started: false,
        }
    }
}

/// The interpreter's `by` driver: `O(1)` `_n` and `_N` for the current group.
///
/// **Exported requirement on the interpreter (`04` §6.3):** `_n` and `_N` are
/// resolved *through this* when a `by` is active and through the frame
/// otherwise. There is no global `_n`.
#[derive(Clone, Debug)]
pub struct ByCursor<'a> {
    idx: &'a GroupIndex,
    k: usize,
    started: bool,
}

impl<'a> ByCursor<'a> {
    /// Move to the next group and answer its physical range, or `None` when the
    /// groups are exhausted. The first call positions on group 0.
    pub fn advance(&mut self) -> Option<Range<u64>> {
        if self.started {
            self.k += 1;
        } else {
            self.started = true;
        }
        (self.k < self.idx.ngroups()).then(|| self.idx.group(self.k))
    }

    /// Which group the cursor is on. Meaningless before the first
    /// [`advance`](Self::advance).
    #[must_use]
    pub fn group(&self) -> usize {
        self.k
    }

    /// The index this cursor walks.
    #[must_use]
    pub fn index(&self) -> &'a GroupIndex {
        self.idx
    }

    /// `_n` — the 1-based position of `physical_obs` **within the current
    /// group** (`04` §6.3).
    #[must_use]
    pub fn obs_n(&self, physical_obs: u64) -> u64 {
        physical_obs - self.idx.starts[self.k] + 1
    }

    /// `_N` — the current group's observation count.
    ///
    /// Spelled `group_len` rather than `obs_N` because `_N` is not a legal Rust
    /// identifier casing; the interpreter's mapping is stated once, here.
    #[must_use]
    pub fn group_len(&self) -> u64 {
        self.idx.group_len(self.k)
    }
}

/// Resolve a spec's group keys through `get`, rejecting an empty list.
fn resolve<'a, F>(spec: &BySpec, get: F) -> Result<Vec<&'a Column>, ByError>
where
    F: Fn(VarIdx) -> Option<&'a Column>,
{
    if spec.group_keys.is_empty() {
        return Err(ByError::NoKeys);
    }
    spec.group_keys
        .iter()
        .map(|&i| get(i).ok_or(ByError::NoSuchVar(i)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column::Column;
    use stratum_core::missing::SYSMISS;
    use stratum_proto::StorageType;

    fn dbl(vals: &[f64]) -> Column {
        let mut c = Column::new_missing(StorageType::Double, vals.len() as u64);
        for (i, &v) in vals.iter().enumerate() {
            crate::column::write_f64(&mut c, i as u64, v).expect("double takes anything");
        }
        c
    }

    #[test]
    fn adjacent_equal_keys_are_one_group() {
        let g = dbl(&[1.0, 1.0, 2.0, 2.0, 3.0]);
        let idx = GroupIndex::from_sorted_cols(&[&g], 5);
        assert_eq!(idx.starts, vec![0, 2, 4, 5]);
        assert_eq!(idx.ngroups(), 3);
        assert_eq!(idx.group(1), 2..4);
    }

    #[test]
    fn an_empty_frame_has_no_groups() {
        let g = dbl(&[]);
        let idx = GroupIndex::from_sorted_cols(&[&g], 0);
        assert_eq!(idx.starts, vec![0]);
        assert_eq!(idx.ngroups(), 0);
        assert!(idx.is_empty());
        assert!(idx.cursor().advance().is_none());
    }

    #[test]
    fn two_keys_split_where_either_changes() {
        let a = dbl(&[1.0, 1.0, 1.0, 2.0]);
        let b = dbl(&[7.0, 7.0, 9.0, 9.0]);
        let idx = GroupIndex::from_sorted_cols(&[&a, &b], 4);
        assert_eq!(idx.starts, vec![0, 2, 3, 4]);
    }

    #[test]
    fn sort_only_keys_do_not_split() {
        // `bysort g (x):` — x is in the sort but not in the groups.
        let g = dbl(&[1.0, 1.0, 2.0]);
        let spec = BySpec {
            group_keys: vec![VarIdx(0)],
            sort_only: vec![VarIdx(1)],
        };
        assert_eq!(spec.required_sort_keys(), vec![VarIdx(0), VarIdx(1)]);
        let idx = GroupIndex::from_sorted_cols(&[&g], 3);
        assert_eq!(idx.ngroups(), 2);
    }

    #[test]
    fn group_of_is_the_inverse_of_group() {
        let g = dbl(&[1.0, 1.0, 2.0, 3.0, 3.0, 3.0]);
        let idx = GroupIndex::from_sorted_cols(&[&g], 6);
        for k in 0..idx.ngroups() {
            for obs in idx.group(k) {
                assert_eq!(idx.group_of(obs), Some(k), "obs {obs}");
            }
        }
        assert_eq!(idx.group_of(6), None);
    }

    #[test]
    fn extended_missings_are_distinct_groups() {
        // `04` §2.2: `. < .a < … < .z`. Two different missings are two keys, so
        // they are two groups — not one "missing" bucket.
        let g = dbl(&[1.0, SYSMISS, stratum_core::missing_f64(1)]);
        let idx = GroupIndex::from_sorted_cols(&[&g], 3);
        assert_eq!(idx.ngroups(), 3);
    }
}
