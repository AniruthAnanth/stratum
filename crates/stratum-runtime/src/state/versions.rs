//! Variable versions, and the map that makes carrying one per execution cheap.
//!
//! `03` §4.2 gives every column a [`VarVersion`] and `03` §4.5 requires that
//! updating a fingerprint cost O(columns *changed*), not O(columns). The design
//! reached for `imbl`'s persistent HAMT to get that. `imbl` is not in the
//! workspace dependency table (W00's root `Cargo.toml`), and a member crate
//! taking a dependency outside that table is how a workspace ends up resolving
//! two versions of one crate — the same reasoning `stratum_effects::varset`
//! wrote down when it declined `compact_str`.
//!
//! [`VarVersions`] gets the same asymptotics from the idiom this repo already
//! uses for exactly this problem: `stratum_data::column` is
//! `Vec<Arc<chunk>>` and clones a chunk only when it is written. Here a chunk is
//! [`VERSION_CHUNK`] slots of `Option<VarVersion>`, indexed by `VarId`, which is
//! dense because the session allocates ids from a counter. Cloning a
//! `VarVersions` clones the pointer vector; writing one entry deep-copies one
//! 1.5 KiB chunk. For Stata's 32 767-variable ceiling that is a 4 KiB pointer
//! copy plus one chunk, against a 512 KiB copy for a flat vector — and the map
//! is cloned once per command commit, which is a path we hold to O(changed) on
//! purpose (spec §0a).
//!
//! The chunk is deliberately *not* the 65 536-row granule `stratum_data` uses.
//! That constant sizes a data buffer; this one sizes a metadata slot table, and
//! 64 keeps a chunk inside two cache lines' worth of pointer chasing while
//! leaving the pointer vector short.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use stratum_proto::{ExecutionId, VarId};

use crate::state::bump;

/// Slots per structurally shared chunk of [`VarVersions`].
pub const VERSION_CHUNK: usize = 64;

/// One column's provenance version — `03` §4.2.
///
/// `gen` is monotone per column and bumps **once per command commit**, never per
/// element (`03` §4.3, ADR-008). `origin` answers spec §13's "income was
/// modified at E44" without a ledger scan.
///
/// # `origin` is deliberately outside equality
///
/// `Eq` and `Hash` cover `(var, gen)` only. `03` §4.2 defines the effective
/// version as `eff(v, S) = (var, gen, row_membership)` — `origin` is not in it,
/// and it must not be: a convergent re-run re-stamps `origin` with the new
/// execution while proving the bytes did not move, and if that changed the
/// fingerprint it would mint a fresh `DatasetStateId` and defeat the whole of
/// `03` §4.4. Use [`VarVersion::provenance_eq`] on the rare path that wants an
/// exact comparison.
#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct VarVersion {
    /// Column identity. Survives `rename`, dies with `drop`, never reused.
    pub var: VarId,
    /// Monotone per column.
    pub gen: u32,
    /// The execution that last wrote this column.
    pub origin: ExecutionId,
}

impl PartialEq for VarVersion {
    fn eq(&self, other: &Self) -> bool {
        self.var == other.var && self.gen == other.gen
    }
}

impl Eq for VarVersion {}

impl std::hash::Hash for VarVersion {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.var.hash(state);
        self.gen.hash(state);
    }
}

impl VarVersion {
    /// Equality including `origin`. See the type's note on why this is not `==`.
    #[must_use]
    pub fn provenance_eq(&self, other: &Self) -> bool {
        self == other && self.origin == other.origin
    }

    /// The version a freshly created column carries: `gen = 0` (`03` §4.3).
    #[must_use]
    pub fn created(var: VarId, origin: ExecutionId) -> Self {
        Self {
            var,
            gen: 0,
            origin,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Chunk {
    slots: [Option<VarVersion>; VERSION_CHUNK],
    /// Live entries in this chunk. Kept so `VarVersions::len` is O(chunks) and
    /// an all-empty chunk can be dropped back to `None`.
    live: u32,
}

impl Chunk {
    fn empty() -> Self {
        Self {
            slots: [None; VERSION_CHUNK],
            live: 0,
        }
    }
}

/// A persistent map `VarId -> VarVersion` with structural sharing.
///
/// Cloning is O(#chunks) pointer work; inserting or removing one entry
/// deep-copies at most one chunk. Equality short-circuits on `Arc::ptr_eq` per
/// chunk, which is what makes the interning verification in
/// [`crate::state::dataset`] cheap on the common "nothing moved" path.
#[derive(Clone, Debug, Default)]
pub struct VarVersions {
    chunks: Vec<Option<Arc<Chunk>>>,
    len: u32,
}

impl VarVersions {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Live entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// True when no column is present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The version recorded for `var`, if it is still live.
    #[must_use]
    pub fn get(&self, var: VarId) -> Option<VarVersion> {
        let (c, s) = split(var);
        self.chunks.get(c)?.as_ref()?.slots[s]
    }

    /// True when `var` is live.
    #[must_use]
    pub fn contains(&self, var: VarId) -> bool {
        self.get(var).is_some()
    }

    /// Insert or overwrite, returning the previous version.
    ///
    /// Deep-copies at most one chunk, and only when another `VarVersions` still
    /// points at it.
    pub fn insert(&mut self, v: VarVersion) -> Option<VarVersion> {
        let (c, s) = split(v.var);
        if c >= self.chunks.len() {
            self.chunks.resize(c + 1, None);
        }
        let slot = &mut self.chunks[c];
        let chunk = match slot {
            Some(existing) => make_mut(existing),
            None => {
                bump(|c| &c.version_chunks_allocated, 1);
                let fresh = slot.insert(Arc::new(Chunk::empty()));
                Arc::get_mut(fresh).expect("uniquely owned")
            }
        };
        let prev = chunk.slots[s].replace(v);
        if prev.is_none() {
            chunk.live += 1;
            self.len += 1;
        }
        prev
    }

    /// Remove `var`. `VarId`s are never reused, so a removed slot stays empty.
    pub fn remove(&mut self, var: VarId) -> Option<VarVersion> {
        let (c, s) = split(var);
        let slot = self.chunks.get_mut(c)?;
        let chunk = make_mut(slot.as_mut()?);
        let prev = chunk.slots[s].take()?;
        chunk.live -= 1;
        self.len -= 1;
        if chunk.live == 0 {
            *slot = None;
        }
        Some(prev)
    }

    /// Every live version, in ascending `VarId` order.
    ///
    /// Ascending order is not incidental: `DepFootprint.vars` is specified
    /// sorted by `VarId` (`03` §4.7) so two footprints compare without a sort.
    pub fn iter(&self) -> impl Iterator<Item = VarVersion> + '_ {
        self.chunks
            .iter()
            .flat_map(|c| c.iter())
            .flat_map(|c| c.slots.iter())
            .filter_map(|s| *s)
    }
}

impl PartialEq for VarVersions {
    /// Chunk-wise, with a pointer-equality fast path.
    ///
    /// Two fingerprints one command apart share every chunk but the one that
    /// changed, so this is a handful of pointer compares rather than a walk of
    /// 32 767 entries. The interner leans on that (`03` §4.5: "a collision costs
    /// a comparison, not a wrong answer").
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        let n = self.chunks.len().max(other.chunks.len());
        (0..n).all(|i| {
            match (
                self.chunks.get(i).and_then(Option::as_ref),
                other.chunks.get(i).and_then(Option::as_ref),
            ) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
                _ => false,
            }
        })
    }
}

impl Eq for VarVersions {}

impl FromIterator<VarVersion> for VarVersions {
    fn from_iter<T: IntoIterator<Item = VarVersion>>(iter: T) -> Self {
        let mut m = Self::new();
        for v in iter {
            m.insert(v);
        }
        m
    }
}

#[inline]
fn split(var: VarId) -> (usize, usize) {
    let i = var.0 as usize;
    (i / VERSION_CHUNK, i % VERSION_CHUNK)
}

/// `Arc::make_mut` with the deep-copy counted. The count is the instrument the
/// "O(changed)" acceptance is asserted with (ADR-017: counters, not clocks).
fn make_mut(arc: &mut Arc<Chunk>) -> &mut Chunk {
    if Arc::strong_count(arc) > 1 || Arc::weak_count(arc) > 0 {
        bump(|c| &c.version_chunks_cloned, 1);
    }
    Arc::make_mut(arc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::local_snapshot;

    fn v(id: u32, gen: u32) -> VarVersion {
        VarVersion {
            var: VarId(id),
            gen,
            origin: ExecutionId(1),
        }
    }

    #[test]
    fn insert_get_remove_round_trips() {
        let mut m = VarVersions::new();
        assert!(m.is_empty());
        assert_eq!(m.insert(v(1, 0)), None);
        assert_eq!(m.insert(v(200, 3)), None);
        assert_eq!(m.get(VarId(1)), Some(v(1, 0)));
        assert_eq!(m.get(VarId(200)), Some(v(200, 3)));
        assert_eq!(m.get(VarId(2)), None);
        assert_eq!(m.len(), 2);
        assert_eq!(m.insert(v(1, 1)), Some(v(1, 0)));
        assert_eq!(m.len(), 2);
        assert_eq!(m.remove(VarId(1)), Some(v(1, 1)));
        assert_eq!(m.remove(VarId(1)), None);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn iteration_is_ascending_by_var_id() {
        let m: VarVersions = [v(300, 1), v(2, 1), v(65, 1), v(64, 1)]
            .into_iter()
            .collect();
        let ids: Vec<u32> = m.iter().map(|x| x.var.0).collect();
        assert_eq!(ids, vec![2, 64, 65, 300]);
    }

    #[test]
    fn a_write_deep_copies_one_chunk_and_shares_the_rest() {
        // The O(changed) property, as a counter (ADR-017).
        let mut a: VarVersions = (0..1024).map(|i| v(i, 0)).collect();
        let before = local_snapshot();
        let b = a.clone();
        assert_eq!(
            local_snapshot().since(before).version_chunks_cloned,
            0,
            "cloning a VarVersions must copy pointers only"
        );
        a.insert(v(700, 1));
        let d = local_snapshot().since(before);
        assert_eq!(d.version_chunks_cloned, 1, "one write, one chunk copied");
        assert_eq!(
            b.get(VarId(700)),
            Some(v(700, 0)),
            "the clone is unaffected"
        );
        assert_eq!(a.get(VarId(700)), Some(v(700, 1)));
    }

    #[test]
    fn equality_ignores_representation() {
        let a: VarVersions = [v(1, 0), v(70, 2)].into_iter().collect();
        let b: VarVersions = [v(70, 2), v(1, 0)].into_iter().collect();
        assert_eq!(a, b);
        let mut c = a.clone();
        c.insert(v(70, 3));
        assert_ne!(a, c);
        // A chunk emptied by `remove` compares equal to one that never existed.
        let mut d = a.clone();
        d.remove(VarId(70));
        let e: VarVersions = [v(1, 0)].into_iter().collect();
        assert_eq!(d, e);
    }
}
