//! The chunking granule (A18) and the index arithmetic every column shares.
//!
//! # Why columns are chunked at all
//!
//! ARCHITECTURE §7.6, amended by audit item A18. The pre-audit rule — "`col_mut`
//! retains the previous `Arc` before making it unique" — raises the strong count
//! to 2, so `Arc::make_mut` deep-copies. **Every** `replace x = x+1` on 10 M
//! doubles therefore allocated and memcpy'd 80 MB, not only the ones that get
//! interrupted, and INV-2 rollback still had no way to be cheaper than the whole
//! column.
//!
//! A column is instead a `Vec<Arc<chunk>>`. The write barrier journals the
//! *chunk* it is about to dirty, so `replace x = 1 in 1` retains 512 KiB and a
//! whole-column `replace` retains one chunk at a time — one extra pass, never a
//! second column.
//!
//! # One constant, everywhere
//!
//! [`CHUNK_ROWS`] is re-exported from `stratum_core::reduce`, not redeclared
//! (C35). It is simultaneously the reduction granule, the storage granule, the
//! undo-journal granule, and a cancellation safepoint boundary (ARCHITECTURE §4:
//! "every 65 536 rows in a row-wise kernel"). A chunk boundary in a fold is a
//! chunk boundary in memory, which is why `map_reduce_blocks` over a chunked
//! column costs nothing extra.
//!
//! # The consequence, stated once
//!
//! **No column exposes a single contiguous `&[f64]`.** Every kernel iterates
//! chunk-wise. That is not a new burden: `map_reduce_blocks` already required it.
//!
//! # Q9 extension point
//!
//! `Vec<Arc<[T]>>` is the shape that makes an out-of-RAM column additive rather
//! than a redesign. A v1.1 `ChunkSource` enum — `Resident(Arc<[T]>)` next to an
//! `Mmap { .. }` — changes `chunk()` and nothing above it, because no signature
//! in this crate hands out a whole column. v1 ships resident-only and refuses
//! precisely; see [`crate::perf::MemoryPolicy`] for the reasoning and the rc.

pub use stratum_core::reduce::CHUNK_ROWS;

use crate::bitset::BitSet;

/// Which chunk row `row` lives in.
#[inline]
#[must_use]
pub fn chunk_of(row: u64) -> usize {
    usize::try_from(row / CHUNK_ROWS as u64).expect("chunk index beyond usize")
}

/// Row `row`'s offset inside its chunk.
#[inline]
#[must_use]
pub fn offset_in_chunk(row: u64) -> usize {
    usize::try_from(row % CHUNK_ROWS as u64).expect("chunk offset beyond usize")
}

/// How many chunks `len` rows occupy.
#[inline]
#[must_use]
pub fn n_chunks(len: u64) -> usize {
    usize::try_from(len.div_ceil(CHUNK_ROWS as u64)).expect("chunk count beyond usize")
}

/// The row count of chunk `c` in a column of `len` rows. Only the last is short.
#[inline]
#[must_use]
pub fn chunk_len(c: usize, len: u64) -> usize {
    let start = c as u64 * CHUNK_ROWS as u64;
    if start >= len {
        return 0;
    }
    usize::try_from((len - start).min(CHUNK_ROWS as u64)).expect("chunk length beyond usize")
}

/// The half-open row range chunk `c` covers in a column of `len` rows.
#[inline]
#[must_use]
pub fn chunk_rows(c: usize, len: u64) -> (u64, u64) {
    let start = (c as u64 * CHUNK_ROWS as u64).min(len);
    (start, (start + CHUNK_ROWS as u64).min(len))
}

/// One chunk of a `strL` column: its own offset table, arena and binary flags.
///
/// Chunking the arena rather than sharing one is what lets the undo journal
/// treat `strL` exactly like every other type — retain one `Arc`, restore one
/// `Arc` — instead of needing a second, arena-shaped rollback path.
///
/// **Partition note.** Only the *storage* is declared here, because
/// `Column::StrL` cannot exist without it and `Column` lives in W02's
/// `column.rs`. The GSO `(v,o)` packing, the per-release width rules and the
/// write-side dedup are W02b's `strl.rs`, which adds them as `impl` blocks on
/// this type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StrLChunk {
    /// `len == rows + 1`, monotonic, into [`bytes`](Self::bytes).
    offsets: Vec<u32>,
    /// The arena. No NUL terminators are stored.
    bytes: Vec<u8>,
    /// Per row: true ⇒ GSO type 129, a binary blob rather than text.
    binary: BitSet,
}

impl StrLChunk {
    /// An all-empty chunk of `rows` observations.
    #[must_use]
    pub fn empty(rows: usize) -> Self {
        Self {
            offsets: vec![0; rows + 1],
            bytes: Vec::new(),
            binary: BitSet::new(rows as u64),
        }
    }

    /// Observations in this chunk.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// True when there are no observations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows() == 0
    }

    /// The bytes of observation `i` within this chunk.
    #[must_use]
    pub fn get(&self, i: usize) -> &[u8] {
        let lo = self.offsets[i] as usize;
        let hi = self.offsets[i + 1] as usize;
        &self.bytes[lo..hi]
    }

    /// Is observation `i` a binary blob (GSO type 129) rather than text (130)?
    #[must_use]
    pub fn is_binary(&self, i: usize) -> bool {
        self.binary.get(i as u64)
    }

    /// Build a chunk from observations delivered in ascending row order — the
    /// bulk-load path.
    ///
    /// Appends each value exactly once, so the whole chunk costs O(total
    /// bytes). Building the same chunk through [`set`](Self::set) rewrites the
    /// arena tail per cell — O(bytes²) over a column load, which is the route
    /// `stratum-dta`'s escalation names as unusable. `pub(crate)`: the outside
    /// world reaches it through `StrLCol::from_rows`, which owns the tiling
    /// into [`CHUNK_ROWS`]-row chunks.
    pub(crate) fn from_rows<'a, I>(rows: I) -> Self
    where
        I: IntoIterator<Item = (&'a [u8], bool)>,
    {
        let mut offsets = vec![0u32];
        let mut bytes: Vec<u8> = Vec::new();
        let mut flags: Vec<bool> = Vec::new();
        for (value, binary) in rows {
            bytes.extend_from_slice(value);
            offsets.push(u32::try_from(bytes.len()).expect("strL chunk arena beyond 4 GiB"));
            flags.push(binary);
        }
        let mut binary = BitSet::new(flags.len() as u64);
        for (i, &b) in flags.iter().enumerate() {
            if b {
                binary.set(i as u64, true);
            }
        }
        Self {
            offsets,
            bytes,
            binary,
        }
    }

    /// Replace observation `i`. Rewrites the arena from `i` on, which is
    /// bounded by [`CHUNK_ROWS`] and is why the arena is chunked in the first
    /// place.
    pub fn set(&mut self, i: usize, value: &[u8], binary: bool) {
        let lo = self.offsets[i] as usize;
        let hi = self.offsets[i + 1] as usize;
        self.bytes.splice(lo..hi, value.iter().copied());
        let delta = value.len() as i64 - (hi - lo) as i64;
        for o in &mut self.offsets[i + 1..] {
            *o = u32::try_from(i64::from(*o) + delta).expect("strL chunk arena beyond 4 GiB");
        }
        self.binary.set(i as u64, binary);
    }

    /// Total arena bytes, for the resident-memory accounting Q9 needs.
    #[must_use]
    pub fn heap_bytes(&self) -> u64 {
        (self.bytes.len() + self.offsets.len() * 4 + self.binary.words().len() * 8) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_granule_is_the_reduction_granule() {
        // C35. If these ever diverge, a fold boundary stops being a memory
        // boundary and every chunk-wise kernel starts straddling.
        assert_eq!(CHUNK_ROWS, stratum_core::CHUNK_ROWS);
        assert_eq!(CHUNK_ROWS, 65_536);
    }

    #[test]
    fn chunk_arithmetic_tiles_exactly() {
        for len in [0u64, 1, 65_535, 65_536, 65_537, 10_000_000] {
            let mut covered = 0u64;
            for c in 0..n_chunks(len) {
                let (s, e) = chunk_rows(c, len);
                assert_eq!(s, covered, "len = {len}");
                assert_eq!(chunk_len(c, len) as u64, e - s);
                covered = e;
            }
            assert_eq!(covered, len, "len = {len}");
        }
    }

    #[test]
    fn ten_million_rows_is_one_hundred_and_fifty_three_chunks() {
        // The A18 number the journal acceptance is written against: a
        // whole-column `replace` retains 153 chunks one at a time, not 80 MB.
        assert_eq!(n_chunks(10_000_000), 153);
    }

    #[test]
    fn row_maps_to_chunk_and_offset() {
        assert_eq!((chunk_of(0), offset_in_chunk(0)), (0, 0));
        assert_eq!((chunk_of(65_535), offset_in_chunk(65_535)), (0, 65_535));
        assert_eq!((chunk_of(65_536), offset_in_chunk(65_536)), (1, 0));
    }

    #[test]
    fn strl_from_rows_matches_the_cell_by_cell_route() {
        // The bulk path must be an equivalence, not a variant: byte-identical
        // to what `set` builds, including the binary flags and the empty rows
        // that store no arena bytes.
        let rows: [(&[u8], bool); 4] = [
            (b"alpha", false),
            (b"", false),
            (b"be", true),
            (b"c", false),
        ];
        let bulk = StrLChunk::from_rows(rows.iter().copied());
        let mut cell_by_cell = StrLChunk::empty(rows.len());
        for (i, (v, bin)) in rows.iter().enumerate() {
            cell_by_cell.set(i, v, *bin);
        }
        assert_eq!(bulk, cell_by_cell);
        assert_eq!(bulk.rows(), 4);
        assert_eq!(bulk.get(0), b"alpha");
        assert_eq!(bulk.get(1), b"");
        assert!(bulk.is_binary(2) && !bulk.is_binary(3));
    }

    #[test]
    fn strl_set_rewrites_only_its_own_arena() {
        let mut c = StrLChunk::empty(3);
        c.set(0, b"alpha", false);
        c.set(1, b"be", true);
        c.set(2, b"gamma", false);
        assert_eq!(c.get(0), b"alpha");
        assert_eq!(c.get(1), b"be");
        assert_eq!(c.get(2), b"gamma");
        assert!(c.is_binary(1) && !c.is_binary(0));
        // Shrinking a middle cell keeps its neighbours intact.
        c.set(1, b"", false);
        assert_eq!(c.get(0), b"alpha");
        assert_eq!(c.get(1), b"");
        assert_eq!(c.get(2), b"gamma");
    }
}
