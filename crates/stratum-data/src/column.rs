//! Columns: chunked, copy-on-write, and native-width.
//!
//! # Native widths, not "everything is `f64`"
//!
//! `04` §3.1. Storage type is part of a dataset's identity — users manage it
//! explicitly with `compress`, `recast`, `gen byte` — and it must round-trip
//! through `.dta`. It is also a 2.2× memory difference: an `auto`-shaped dataset
//! at 10 M rows is 430 MB in native widths and 960 MB as all-`f64`. Under the
//! §0a speed priority that is not a size argument, it is a *residency* argument:
//! the 960 MB frame is the one that pages, and a paged column turns a 6 ms
//! `summarize` into a disk seek per chunk.
//!
//! The price is a widening pass per expression evaluation.
//! [`Column::for_each_chunk_f64`] is that pass, it is a tight vectorisable loop,
//! and for a `Double` column it hands out the chunk itself and copies nothing —
//! which is the common case, because `gen` produces float/double.
//!
//! # Copy-on-write, at two levels
//!
//! `Arc<Column>` is shared by snapshots, `preserve`, and `frame copy`; cloning
//! the `Column` behind it clones a `Vec` of chunk pointers (1.2 KB for 10 M
//! rows), never data. `Arc<[T]>` per chunk is what the write barrier makes
//! unique, one 512 KiB chunk at a time (A18). Both levels are pointer work
//! until somebody writes.
//!
//! # No contiguous slice
//!
//! Deliberate, and the accepted consequence of A18: there is no
//! `fn as_slice(&self) -> &[f64]` here and there will not be one. Every kernel
//! iterates chunk-wise, which `map_reduce_blocks` already required.

use std::sync::Arc;

use stratum_core::missing::{
    is_missing, narrow_byte, narrow_float, narrow_int, narrow_long, widen_byte, widen_float,
    widen_int, widen_long, Narrowed, BYTE_MISS, INT_MISS, LONG_MISS, SYSMISS, SYSMISS_F32,
};
use stratum_proto::{ColumnDigest, StorageType};

use crate::bitset::BitSet;
use crate::chunk::{chunk_len, chunk_of, chunk_rows, n_chunks, offset_in_chunk, StrLChunk};
use crate::perf::{bump, counters};
use crate::sample::Sample;

/// A column behind the shared pointer every snapshot holds (`04` §3.2).
pub type ColumnRef = Arc<Column>;

// ---------------------------------------------------------------------------
// The element trait
// ---------------------------------------------------------------------------

/// What a numeric column's element must be able to do.
///
/// Widening and narrowing are `stratum_core::missing`'s, never reimplemented:
/// an `int` holding `INT_MISS` is `.`, while a `double` holding that same
/// numeric value is an ordinary number, and that one distinction is the
/// difference between correct statistics and confident wrong ones
/// (ADR-005, `04` §2.6).
pub trait NumElem: Copy + Send + Sync + 'static {
    /// The storage type a column of these elements reports.
    const TYPE: StorageType;
    /// This width's encoding of `.`.
    const MISSING: Self;
    /// Bytes one element occupies on disk and in a sort key.
    const WIDTH: usize;

    /// To the `f64` every Stata expression evaluates in.
    fn widen(self) -> f64;
    /// Back down, or the promotion the column needs first.
    fn narrow(v: f64) -> Narrowed<Self>;
    /// Little-endian bytes, for the digest and the `.dta` writer.
    fn le_bytes(self) -> [u8; 8];
    /// From `WIDTH` little-endian bytes.
    fn from_le_slice(src: &[u8]) -> Self;
}

macro_rules! int_elem {
    ($t:ty, $ty:expr, $miss:expr, $widen:path, $narrow:path) => {
        impl NumElem for $t {
            const TYPE: StorageType = $ty;
            const MISSING: Self = $miss;
            const WIDTH: usize = std::mem::size_of::<$t>();

            #[inline(always)]
            fn widen(self) -> f64 {
                $widen(self)
            }
            #[inline(always)]
            fn narrow(v: f64) -> Narrowed<Self> {
                $narrow(v)
            }
            #[inline(always)]
            fn le_bytes(self) -> [u8; 8] {
                let mut out = [0u8; 8];
                out[..Self::WIDTH].copy_from_slice(&self.to_le_bytes());
                out
            }
            #[inline(always)]
            fn from_le_slice(src: &[u8]) -> Self {
                let mut b = [0u8; Self::WIDTH];
                b.copy_from_slice(&src[..Self::WIDTH]);
                <$t>::from_le_bytes(b)
            }
        }
    };
}

int_elem!(i8, StorageType::Byte, BYTE_MISS, widen_byte, narrow_byte);
int_elem!(i16, StorageType::Int, INT_MISS, widen_int, narrow_int);
int_elem!(i32, StorageType::Long, LONG_MISS, widen_long, narrow_long);

impl NumElem for f32 {
    const TYPE: StorageType = StorageType::Float;
    const MISSING: Self = SYSMISS_F32;
    const WIDTH: usize = 4;

    #[inline(always)]
    fn widen(self) -> f64 {
        widen_float(self)
    }
    #[inline(always)]
    fn narrow(v: f64) -> Narrowed<Self> {
        narrow_float(v)
    }
    #[inline(always)]
    fn le_bytes(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[..4].copy_from_slice(&self.to_bits().to_le_bytes());
        out
    }
    #[inline(always)]
    fn from_le_slice(src: &[u8]) -> Self {
        let mut b = [0u8; 4];
        b.copy_from_slice(&src[..4]);
        f32::from_bits(u32::from_le_bytes(b))
    }
}

impl NumElem for f64 {
    const TYPE: StorageType = StorageType::Double;
    const MISSING: Self = SYSMISS;
    const WIDTH: usize = 8;

    #[inline(always)]
    fn widen(self) -> f64 {
        self
    }
    #[inline(always)]
    fn narrow(v: f64) -> Narrowed<Self> {
        Narrowed::Ok(v)
    }
    #[inline(always)]
    fn le_bytes(self) -> [u8; 8] {
        self.to_bits().to_le_bytes()
    }
    #[inline(always)]
    fn from_le_slice(src: &[u8]) -> Self {
        let mut b = [0u8; 8];
        b.copy_from_slice(&src[..8]);
        f64::from_bits(u64::from_le_bytes(b))
    }
}

// ---------------------------------------------------------------------------
// NumCol
// ---------------------------------------------------------------------------

/// A numeric column, chunked at [`CHUNK_ROWS`](crate::chunk::CHUNK_ROWS).
///
/// The fields are private and there is no accessor that hands out the whole
/// buffer: mutation is reachable only through [`Frame::col_mut`](crate::Frame::col_mut),
/// which is what makes the undo journal impossible to bypass. `tests/cow.rs` and
/// the `compile_fail` doctests on [`crate`] prove it from outside the crate.
#[derive(Clone, Debug, PartialEq)]
pub struct NumCol<T> {
    chunks: Vec<Arc<[T]>>,
    len: u64,
}

impl<T: NumElem> NumCol<T> {
    /// `len` observations, every one of them `.`.
    #[must_use]
    pub fn missing(len: u64) -> Self {
        let chunks = (0..n_chunks(len))
            .map(|c| Arc::from(vec![T::MISSING; chunk_len(c, len)]))
            .collect();
        Self { chunks, len }
    }

    /// Build from a flat slice. The slice is copied once, chunk by chunk.
    #[must_use]
    pub fn from_slice(values: &[T]) -> Self {
        let len = values.len() as u64;
        let chunks = (0..n_chunks(len))
            .map(|c| {
                let (s, e) = chunk_rows(c, len);
                Arc::from(&values[s as usize..e as usize])
            })
            .collect();
        Self { chunks, len }
    }

    /// Observations.
    #[inline]
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True when there are no observations.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many chunks back this column.
    #[inline]
    #[must_use]
    pub fn n_chunks(&self) -> usize {
        self.chunks.len()
    }

    /// Chunk `c`, read-only. This is the widest window the outside world gets.
    #[inline]
    #[must_use]
    pub fn chunk(&self, c: usize) -> &[T] {
        &self.chunks[c]
    }

    /// One observation.
    #[inline]
    #[must_use]
    pub fn get(&self, row: u64) -> T {
        self.chunks[chunk_of(row)][offset_in_chunk(row)]
    }

    /// Bytes the chunks hold, for resident-memory accounting (Q9).
    #[must_use]
    pub fn heap_bytes(&self) -> u64 {
        self.len * T::WIDTH as u64 + (self.chunks.len() * std::mem::size_of::<usize>() * 2) as u64
    }

    /// Assemble from chunks that were built independently — the parallel
    /// ingest's exit. Each chunk must be `chunk_len(c, len)` long.
    pub(crate) fn from_chunks(chunks: Vec<Arc<[T]>>, len: u64) -> Self {
        debug_assert_eq!(chunks.len(), n_chunks(len));
        Self { chunks, len }
    }

    /// The chunk pointer, for the undo journal to retain.
    pub(crate) fn chunk_arc(&self, c: usize) -> Arc<[T]> {
        Arc::clone(&self.chunks[c])
    }

    /// Make chunk `c` unique and hand out a mutable view.
    ///
    /// This is the ONE place a numeric chunk becomes writable, and the only
    /// place `Arc::make_mut` deep-copies data in this crate. It is
    /// `pub(crate)`: the write barrier calls it *after* journalling.
    pub(crate) fn chunk_mut(&mut self, c: usize) -> &mut [T] {
        let shared = Arc::strong_count(&self.chunks[c]) > 1;
        if shared {
            bump(&counters().chunks_cloned, 1);
            bump(
                &counters().chunk_bytes_cloned,
                (self.chunks[c].len() * T::WIDTH) as u64,
            );
        }
        Arc::make_mut(&mut self.chunks[c])
    }

    /// Put a retained chunk back, byte for byte. Rollback's only primitive.
    pub(crate) fn restore_chunk(&mut self, c: usize, saved: Arc<[T]>) {
        self.chunks[c] = saved;
    }
}

// ---------------------------------------------------------------------------
// FixedStrCol
// ---------------------------------------------------------------------------

/// `str1`..`str2045`: flat, NUL-padded, byte-width — bit-identical to the
/// `.dta` on-disk layout (`04` §3.1).
///
/// Flat and fixed-stride, not `Vec<String>`: read and write become a strided
/// `memcpy` with no per-row allocation, a sort key is a `memcmp` at a known
/// offset, and 10 M rows of `String` would be 10 M allocations and ~240 MB of
/// pointer overhead before a single character. The waste on a `str200` column
/// holding five-character values is Stata's own behaviour, and `compress` is the
/// user-facing fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedStrCol {
    width: u16,
    chunks: Vec<Arc<[u8]>>,
    len: u64,
}

impl FixedStrCol {
    /// `len` observations of `""`, each `width` bytes of NUL.
    #[must_use]
    pub fn empty(width: u16, len: u64) -> Self {
        let w = width as usize;
        let chunks = (0..n_chunks(len))
            .map(|c| Arc::from(vec![0u8; chunk_len(c, len) * w]))
            .collect();
        Self { width, chunks, len }
    }

    /// The declared `str#` width, in bytes.
    #[inline]
    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Observations.
    #[inline]
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True when there are no observations.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many chunks back this column.
    #[inline]
    #[must_use]
    pub fn n_chunks(&self) -> usize {
        self.chunks.len()
    }

    /// The full fixed-width field for `row`, padding included. This is the sort
    /// key: `""` is all-NUL and therefore sorts first, which is exactly Stata's
    /// string-missing-low rule with no code (`04` §2.2).
    #[inline]
    #[must_use]
    pub fn raw(&self, row: u64) -> &[u8] {
        let w = self.width as usize;
        let o = offset_in_chunk(row) * w;
        &self.chunks[chunk_of(row)][o..o + w]
    }

    /// The value for `row`, trimmed at the first NUL.
    #[inline]
    #[must_use]
    pub fn get(&self, row: u64) -> &[u8] {
        let f = self.raw(row);
        let end = f.iter().position(|&b| b == 0).unwrap_or(f.len());
        &f[..end]
    }

    /// Chunk `c`'s raw bytes: `chunk_len(c) * width` of them.
    #[inline]
    #[must_use]
    pub fn chunk(&self, c: usize) -> &[u8] {
        &self.chunks[c]
    }

    /// Bytes the chunks hold (Q9 accounting).
    #[must_use]
    pub fn heap_bytes(&self) -> u64 {
        self.len * u64::from(self.width) + (self.chunks.len() * 16) as u64
    }

    /// Assemble from independently built chunks — the parallel ingest's exit.
    pub(crate) fn from_chunks(width: u16, chunks: Vec<Arc<[u8]>>, len: u64) -> Self {
        debug_assert_eq!(chunks.len(), n_chunks(len));
        Self { width, chunks, len }
    }

    pub(crate) fn chunk_arc(&self, c: usize) -> Arc<[u8]> {
        Arc::clone(&self.chunks[c])
    }

    pub(crate) fn chunk_mut(&mut self, c: usize) -> &mut [u8] {
        if Arc::strong_count(&self.chunks[c]) > 1 {
            bump(&counters().chunks_cloned, 1);
            bump(&counters().chunk_bytes_cloned, self.chunks[c].len() as u64);
        }
        Arc::make_mut(&mut self.chunks[c])
    }

    pub(crate) fn restore_chunk(&mut self, c: usize, saved: Arc<[u8]>) {
        self.chunks[c] = saved;
    }
}

// ---------------------------------------------------------------------------
// StrLCol
// ---------------------------------------------------------------------------

/// A `strL` column: one [`StrLChunk`] per chunk, so the undo journal treats it
/// exactly like every other type.
///
/// **Partition note.** Storage only. GSO `(v,o)` packing per release, the write
/// dedup pass, and the binary/text distinction's `.dta` encoding are W02b's
/// `strl.rs` and W03's `gso.rs`; they extend this type rather than replace it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StrLCol {
    chunks: Vec<Arc<StrLChunk>>,
    len: u64,
}

impl StrLCol {
    /// `len` empty observations.
    #[must_use]
    pub fn empty(len: u64) -> Self {
        let chunks = (0..n_chunks(len))
            .map(|c| Arc::new(StrLChunk::empty(chunk_len(c, len))))
            .collect();
        Self { chunks, len }
    }

    /// **The bulk ingest path.** Build a column from observations delivered in
    /// ascending row order, each with its GSO binary flag (type 129 vs 130).
    ///
    /// Each value is appended to its chunk's arena exactly once — O(total
    /// bytes) — where the reachable per-cell route
    /// (`Frame::col_mut().set_bytes()`) rewrites the arena tail per cell,
    /// O(bytes²) over a load, and cannot say "binary" at all. This is the
    /// constructor `stratum-dta`'s `Dataset` → `Frame` bridge is blocked on.
    /// `counters().ingest_cells` records one cell per row, same as
    /// [`Column::from_row_major`], so the bulk-path copy floor stays a number.
    #[must_use]
    pub fn from_rows<'a, I>(rows: I) -> Self
    where
        I: IntoIterator<Item = (&'a [u8], bool)>,
    {
        let mut chunks: Vec<Arc<StrLChunk>> = Vec::new();
        let mut buf: Vec<(&[u8], bool)> = Vec::new();
        let mut len: u64 = 0;
        for row in rows {
            buf.push(row);
            len += 1;
            if buf.len() == crate::chunk::CHUNK_ROWS {
                chunks.push(Arc::new(StrLChunk::from_rows(buf.drain(..))));
            }
        }
        if !buf.is_empty() {
            chunks.push(Arc::new(StrLChunk::from_rows(buf.drain(..))));
        }
        bump(&counters().ingest_cells, len);
        Self { chunks, len }
    }

    /// Observations.
    #[inline]
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True when there are no observations.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many chunks back this column.
    #[inline]
    #[must_use]
    pub fn n_chunks(&self) -> usize {
        self.chunks.len()
    }

    /// Chunk `c`.
    #[inline]
    #[must_use]
    pub fn chunk(&self, c: usize) -> &StrLChunk {
        &self.chunks[c]
    }

    /// The bytes stored at `row`.
    #[inline]
    #[must_use]
    pub fn get(&self, row: u64) -> &[u8] {
        self.chunks[chunk_of(row)].get(offset_in_chunk(row))
    }

    /// Bytes the chunks hold (Q9 accounting).
    #[must_use]
    pub fn heap_bytes(&self) -> u64 {
        self.chunks.iter().map(|c| c.heap_bytes()).sum()
    }

    pub(crate) fn chunk_arc(&self, c: usize) -> Arc<StrLChunk> {
        Arc::clone(&self.chunks[c])
    }

    pub(crate) fn chunk_mut(&mut self, c: usize) -> &mut StrLChunk {
        if Arc::strong_count(&self.chunks[c]) > 1 {
            bump(&counters().chunks_cloned, 1);
            bump(&counters().chunk_bytes_cloned, self.chunks[c].heap_bytes());
        }
        Arc::make_mut(&mut self.chunks[c])
    }

    pub(crate) fn restore_chunk(&mut self, c: usize, saved: Arc<StrLChunk>) {
        self.chunks[c] = saved;
    }
}

// ---------------------------------------------------------------------------
// Column
// ---------------------------------------------------------------------------

/// One variable's storage.
#[derive(Clone, Debug, PartialEq)]
pub enum Column {
    /// `byte`, i8 with sentinels `BYTE_MISS..=i8::MAX`.
    Byte(NumCol<i8>),
    /// `int`, i16 with sentinels `INT_MISS..=i16::MAX`.
    Int(NumCol<i16>),
    /// `long`, i32 with sentinels `LONG_MISS..=i32::MAX`.
    Long(NumCol<i32>),
    /// `float`, f32 whose sentinels are finite normals from `2^127` up.
    Float(NumCol<f32>),
    /// `double`, f64 whose sentinels are finite normals from `2^1023` up.
    Double(NumCol<f64>),
    /// `str1`..`str2045`.
    Str(FixedStrCol),
    /// `strL`.
    StrL(StrLCol),
}

/// Run `$body` with `$c` bound to whichever `NumCol` a numeric column holds.
macro_rules! numeric {
    ($col:expr, |$c:ident| $body:expr, else $other:expr) => {
        match $col {
            Column::Byte($c) => $body,
            Column::Int($c) => $body,
            Column::Long($c) => $body,
            Column::Float($c) => $body,
            Column::Double($c) => $body,
            _ => $other,
        }
    };
}

impl Column {
    /// A fresh column of `len` missing observations.
    #[must_use]
    pub fn new_missing(ty: StorageType, len: u64) -> Self {
        match ty {
            StorageType::Byte => Column::Byte(NumCol::missing(len)),
            StorageType::Int => Column::Int(NumCol::missing(len)),
            StorageType::Long => Column::Long(NumCol::missing(len)),
            StorageType::Float => Column::Float(NumCol::missing(len)),
            StorageType::Double => Column::Double(NumCol::missing(len)),
            StorageType::Str { width } => Column::Str(FixedStrCol::empty(width, len)),
            StorageType::StrL => Column::StrL(StrLCol::empty(len)),
        }
    }

    /// The declared storage type.
    #[must_use]
    pub fn storage_type(&self) -> StorageType {
        match self {
            Column::Byte(_) => StorageType::Byte,
            Column::Int(_) => StorageType::Int,
            Column::Long(_) => StorageType::Long,
            Column::Float(_) => StorageType::Float,
            Column::Double(_) => StorageType::Double,
            Column::Str(s) => StorageType::Str { width: s.width() },
            Column::StrL(_) => StorageType::StrL,
        }
    }

    /// Observations.
    #[must_use]
    pub fn len(&self) -> u64 {
        match self {
            Column::Str(s) => s.len(),
            Column::StrL(s) => s.len(),
            other => numeric!(other, |c| c.len(), else 0),
        }
    }

    /// True when there are no observations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many chunks back this column.
    #[must_use]
    pub fn n_chunks(&self) -> usize {
        match self {
            Column::Str(s) => s.n_chunks(),
            Column::StrL(s) => s.n_chunks(),
            other => numeric!(other, |c| c.n_chunks(), else 0),
        }
    }

    /// True for `byte`/`int`/`long`/`float`/`double`.
    #[must_use]
    pub fn is_numeric(&self) -> bool {
        stratum_core::types::is_numeric(self.storage_type())
    }

    /// One observation widened to `f64`, or `None` for a string column — where
    /// a numeric read is `r(109) type mismatch`, not a silent missing.
    #[inline]
    #[must_use]
    pub fn get_f64(&self, row: u64) -> Option<f64> {
        Some(numeric!(self, |c| c.get(row).widen(), else return None))
    }

    /// The bytes at `row` for a string column, `None` for a numeric one.
    #[inline]
    #[must_use]
    pub fn get_bytes(&self, row: u64) -> Option<&[u8]> {
        match self {
            Column::Str(s) => Some(s.get(row)),
            Column::StrL(s) => Some(s.get(row)),
            _ => None,
        }
    }

    /// Resident bytes, for [`MemoryPolicy`](crate::perf::MemoryPolicy).
    #[must_use]
    pub fn heap_bytes(&self) -> u64 {
        match self {
            Column::Str(s) => s.heap_bytes(),
            Column::StrL(s) => s.heap_bytes(),
            other => numeric!(other, |c| c.heap_bytes(), else 0),
        }
    }

    /// **The widening accessor** (`04` §3.3, chunked per A18).
    ///
    /// Calls `f(first_row, values)` once per chunk, ascending. For a `Double`
    /// column `values` IS the chunk — zero copy, which is the common case for
    /// generated variables. For every other numeric type the chunk is widened
    /// into `scratch`, which the caller owns and reuses across commands
    /// (`CommandArena`), so a steady-state do-file allocates nothing here.
    ///
    /// Returns the number of rows visited, which is the counter every scan
    /// budget in this unit is asserted against. String columns visit nothing.
    pub fn for_each_chunk_f64<F>(&self, scratch: &mut Vec<f64>, mut f: F) -> u64
    where
        F: FnMut(u64, &[f64]),
    {
        let len = self.len();
        match self {
            Column::Double(c) => {
                for i in 0..c.n_chunks() {
                    let s = c.chunk(i);
                    bump(&counters().rows_touched, s.len() as u64);
                    f(chunk_rows(i, len).0, s);
                }
                len
            }
            Column::Byte(c) => widen_chunks(c, scratch, len, &mut f),
            Column::Int(c) => widen_chunks(c, scratch, len, &mut f),
            Column::Long(c) => widen_chunks(c, scratch, len, &mut f),
            Column::Float(c) => widen_chunks(c, scratch, len, &mut f),
            Column::Str(_) | Column::StrL(_) => 0,
        }
    }

    /// **The parallel, deterministic scan.** Fold the column chunk-wise through
    /// `stratum_core::reduce::map_reduce_blocks`.
    ///
    /// This is the primitive `summarize` and every other column reduction is
    /// built on, and it is the reason the chunk granule had to be *the same*
    /// constant as the reduction granule (C35): `map_reduce_blocks` splits `n`
    /// rows into `CHUNK_ROWS`-row blocks, and those blocks are exactly this
    /// column's chunks, so the map phase reads one contiguous slice per task
    /// with no straddling and no re-slicing.
    ///
    /// The fold is sequential in ascending chunk index, so the answer depends on
    /// `n` and on nothing else — not on thread count, not on scheduling
    /// (ADR-013). A `Double` column's map phase sees the chunk itself; every
    /// other type widens one chunk into a task-local buffer.
    ///
    /// [`for_each_chunk_f64`](Self::for_each_chunk_f64) is the sequential
    /// sibling, for a caller that must see the chunks in order (a running sum,
    /// a `by:` body). Reductions should use this one.
    pub fn map_reduce_f64<T, M, F>(&self, init: T, map: M, fold: F) -> T
    where
        T: Send + Clone,
        M: Fn(u64, &[f64]) -> T + Sync,
        F: Fn(&mut T, &T),
    {
        let n = usize::try_from(self.len()).unwrap_or(usize::MAX);
        match self {
            Column::Double(c) => stratum_core::reduce::map_reduce_blocks(
                n,
                init,
                |s, _| {
                    let xs = c.chunk(chunk_of(s as u64));
                    bump(&counters().rows_touched, xs.len() as u64);
                    map(s as u64, xs)
                },
                fold,
            ),
            Column::Byte(c) => widen_map_reduce(c, n, init, &map, fold),
            Column::Int(c) => widen_map_reduce(c, n, init, &map, fold),
            Column::Long(c) => widen_map_reduce(c, n, init, &map, fold),
            Column::Float(c) => widen_map_reduce(c, n, init, &map, fold),
            Column::Str(_) | Column::StrL(_) => init,
        }
    }

    /// Sample-restricted gather. Writes exactly `sample.len()` values into `out`
    /// (which is cleared first), in ascending observation order.
    ///
    /// A contiguous sample — `All`, `in 5/100`, or an `if` that happens to
    /// select a block — copies run by run and never touches an unselected row.
    /// That is the whole reason [`Sample`] keeps `All`/`Range` distinct from
    /// `Mask` (`04` §5.1).
    pub fn gather_f64(&self, sample: &Sample, out: &mut Vec<f64>) {
        out.clear();
        out.reserve(usize::try_from(sample.len()).unwrap_or(usize::MAX));
        numeric!(
            self,
            |c| {
                for run in sample.runs() {
                    for row in run.start..run.start + run.len {
                        out.push(c.get(row).widen());
                    }
                }
                bump(&counters().rows_touched, sample.len());
            },
            else ()
        );
    }

    /// blake3-128 over `(dtype tag, nobs LE, little-endian value bytes,
    /// missing-mask bitset, length-prefixed UTF-8 for string columns)`,
    /// CONTRACTS §1.1.
    ///
    /// Endianness is normalised so two machines agree (spec §38-E). This is what
    /// "rollback left the frame bit-identical" is asserted with: a digest that
    /// matched before and after cannot have been produced by a column that lost
    /// a byte.
    #[must_use]
    pub fn digest(&self) -> ColumnDigest {
        let mut h = blake3::Hasher::new();
        let ty = self.storage_type();
        h.update(&[type_tag(ty)]);
        if let StorageType::Str { width } = ty {
            h.update(&width.to_le_bytes());
        }
        let len = self.len();
        h.update(&len.to_le_bytes());

        let mut missing = BitSet::new(len);
        let mut buf: Vec<u8> = Vec::with_capacity(1 << 16);
        match self {
            Column::Str(s) => {
                for row in 0..len {
                    let v = s.get(row);
                    buf.clear();
                    buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
                    buf.extend_from_slice(v);
                    h.update(&buf);
                    missing.set(row, v.is_empty());
                }
            }
            Column::StrL(s) => {
                for row in 0..len {
                    let v = s.get(row);
                    buf.clear();
                    buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
                    buf.extend_from_slice(v);
                    h.update(&buf);
                    missing.set(row, v.is_empty());
                }
            }
            other => numeric!(
                other,
                |c| {
                    let w = elem_width(c);
                    for i in 0..c.n_chunks() {
                        buf.clear();
                        for (j, v) in c.chunk(i).iter().enumerate() {
                            buf.extend_from_slice(&v.le_bytes()[..w]);
                            missing.set(chunk_rows(i, len).0 + j as u64, is_missing(v.widen()));
                        }
                        h.update(&buf);
                    }
                },
                else ()
            ),
        }
        for w in missing.words() {
            h.update(&w.to_le_bytes());
        }
        let mut out = [0u8; 16];
        out.copy_from_slice(&h.finalize().as_bytes()[..16]);
        ColumnDigest(out)
    }

    /// **The bulk ingest path.** Build a column by reading one field out of a
    /// row-major buffer — the `.dta` `<data>` block's shape (`04` §12.1).
    ///
    /// `src` is `nobs * row_width` bytes; this column's field starts at
    /// `col_offset` in each row. Each destination cell is written exactly once,
    /// which is the floor for a layout change, and `counters().ingest_cells`
    /// records it so "copies on the bulk path" is a number rather than a claim.
    ///
    /// The reader (W03) owns endianness and canonicalisation of the *source*;
    /// this function assumes little-endian input, which is what `LSF` files and
    /// every target in the release matrix give.
    #[must_use]
    pub fn from_row_major(
        ty: StorageType,
        src: &[u8],
        row_width: usize,
        col_offset: usize,
        nobs: u64,
    ) -> Self {
        match ty {
            StorageType::Byte => Column::Byte(gather_strided(src, row_width, col_offset, nobs)),
            StorageType::Int => Column::Int(gather_strided(src, row_width, col_offset, nobs)),
            StorageType::Long => Column::Long(gather_strided(src, row_width, col_offset, nobs)),
            StorageType::Float => Column::Float(gather_strided(src, row_width, col_offset, nobs)),
            StorageType::Double => Column::Double(gather_strided(src, row_width, col_offset, nobs)),
            StorageType::Str { width } => {
                let w = width as usize;
                let build = |c: usize| -> Arc<[u8]> {
                    let (lo, hi) = chunk_rows(c, nobs);
                    let mut dst = vec![0u8; (hi - lo) as usize * w];
                    for (j, row) in (lo..hi).enumerate() {
                        let o = row as usize * row_width + col_offset;
                        dst[j * w..(j + 1) * w].copy_from_slice(&src[o..o + w]);
                    }
                    Arc::from(dst)
                };
                let chunks = build_chunks(n_chunks(nobs), src.len(), build);
                bump(&counters().ingest_cells, nobs);
                Column::Str(FixedStrCol::from_chunks(width, chunks, nobs))
            }
            // A `strL` data-section field is an 8-byte `(v,o)` pair, not text:
            // resolving it needs the GSO block, which is W03's reader. An
            // all-empty column is the correct pre-resolution state.
            StorageType::StrL => Column::StrL(StrLCol::empty(nobs)),
        }
    }
}

/// Widen one chunk at a time into the caller's scratch buffer.
fn widen_chunks<T: NumElem, F: FnMut(u64, &[f64])>(
    c: &NumCol<T>,
    scratch: &mut Vec<f64>,
    len: u64,
    f: &mut F,
) -> u64 {
    for i in 0..c.n_chunks() {
        let src = c.chunk(i);
        scratch.clear();
        scratch.extend(src.iter().map(|v| v.widen()));
        bump(&counters().rows_touched, src.len() as u64);
        bump(&counters().rows_widened, src.len() as u64);
        f(chunk_rows(i, len).0, scratch);
    }
    len
}

/// One chunk widened into a task-local buffer, so the map phase stays parallel.
///
/// The buffer is allocated per chunk rather than shared: a `&mut Vec` cannot
/// cross into rayon's map phase, and one 512 KiB allocation per 65 536 rows is
/// far cheaper than giving up the parallelism to avoid it.
fn widen_map_reduce<T, E, M, F>(c: &NumCol<E>, n: usize, init: T, map: &M, fold: F) -> T
where
    T: Send + Clone,
    E: NumElem,
    M: Fn(u64, &[f64]) -> T + Sync,
    F: Fn(&mut T, &T),
{
    stratum_core::reduce::map_reduce_blocks(
        n,
        init,
        |s, _| {
            let src = c.chunk(chunk_of(s as u64));
            bump(&counters().rows_touched, src.len() as u64);
            bump(&counters().rows_widened, src.len() as u64);
            let buf: Vec<f64> = src.iter().map(|v| v.widen()).collect();
            map(s as u64, &buf)
        },
        fold,
    )
}

/// The strided read `04` §12.1 calls the whole cost of loading.
///
/// Each destination chunk is built independently, so the whole transpose is
/// `par_iter().map(...)` with no synchronisation and no `unsafe`: rayon owns the
/// parallelism and each task returns its own `Arc<[T]>`. That is the tiling
/// `04` §12.1 describes, expressed as ownership rather than as disjoint mutable
/// borrows.
fn gather_strided<T: NumElem>(
    src: &[u8],
    row_width: usize,
    col_offset: usize,
    nobs: u64,
) -> NumCol<T> {
    let build = |c: usize| -> Arc<[T]> {
        let (lo, hi) = chunk_rows(c, nobs);
        let mut dst = vec![T::MISSING; (hi - lo) as usize];
        for (j, row) in (lo..hi).enumerate() {
            let o = row as usize * row_width + col_offset;
            dst[j] = T::from_le_slice(&src[o..o + T::WIDTH]);
        }
        Arc::from(dst)
    };
    let chunks = build_chunks(n_chunks(nobs), src.len(), build);
    bump(&counters().ingest_cells, nobs);
    NumCol::from_chunks(chunks, nobs)
}

/// Build `count` chunks, in parallel once the source is big enough to pay for
/// the dispatch (`04` §12.3: "above a 1 MiB threshold").
fn build_chunks<T, B>(count: usize, src_bytes: usize, build: B) -> Vec<T>
where
    T: Send,
    B: Fn(usize) -> T + Sync + Send,
{
    #[cfg(feature = "parallel")]
    if count >= crate::perf::PAR_MIN_CHUNKS && src_bytes >= crate::perf::PAR_MIN_INGEST_BYTES {
        use rayon::prelude::*;
        return (0..count).into_par_iter().map(build).collect();
    }
    let _ = src_bytes;
    (0..count).map(build).collect()
}

fn elem_width<T: NumElem>(_: &NumCol<T>) -> usize {
    T::WIDTH
}

/// A stable byte per storage type, for the digest. Never reordered: changing
/// one changes every committed digest.
fn type_tag(ty: StorageType) -> u8 {
    match ty {
        StorageType::Byte => 1,
        StorageType::Int => 2,
        StorageType::Long => 3,
        StorageType::Float => 4,
        StorageType::Double => 5,
        StorageType::Str { .. } => 6,
        StorageType::StrL => 7,
    }
}

/// What a write to a column could not do.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum WriteError {
    /// `replace` never errors on range in Stata — it silently rewrites the
    /// column one rung up the ladder (`04` §2.6, measured: `gen byte b = 500`
    /// is `rc = 0`). The barrier reports the rung; the frame performs it.
    #[error("value needs storage type {0:?}")]
    NeedsPromotion(StorageType),
    /// A numeric write to a string column or the reverse: `r(109)`.
    #[error("type mismatch")]
    TypeMismatch,
}

impl WriteError {
    /// Stata's return code for a type mismatch (measured, `errors.log`).
    pub const RC_TYPE_MISMATCH: u16 = 109;
}

/// Write `v` into `row`, narrowing per the storage type.
///
/// `pub(crate)`: the only caller is [`ColMut`](crate::frame::ColMut), which has
/// already journalled the chunk. `chunk_mut` is idempotent-cheap after the
/// first write into a chunk, because the retained `Arc` was taken before the
/// first `make_mut` and the strong count is 1 from then on.
pub(crate) fn write_f64(col: &mut Column, row: u64, v: f64) -> Result<(), WriteError> {
    let c = chunk_of(row);
    let o = offset_in_chunk(row);
    macro_rules! put {
        ($col:expr, $t:ty) => {
            match <$t as NumElem>::narrow(v) {
                Narrowed::Ok(x) => {
                    $col.chunk_mut(c)[o] = x;
                    Ok(())
                }
                Narrowed::NeedsPromotion(t) => Err(WriteError::NeedsPromotion(t)),
            }
        };
    }
    match col {
        Column::Byte(x) => put!(x, i8),
        Column::Int(x) => put!(x, i16),
        Column::Long(x) => put!(x, i32),
        Column::Float(x) => put!(x, f32),
        Column::Double(x) => put!(x, f64),
        Column::Str(_) | Column::StrL(_) => Err(WriteError::TypeMismatch),
    }
}

/// Write `value` into `row` of a string column.
///
/// A `str#` value longer than the declared width asks for the wider type rather
/// than truncating; silent truncation of a researcher's data is not a behaviour
/// this engine has.
pub(crate) fn write_bytes(col: &mut Column, row: u64, value: &[u8]) -> Result<(), WriteError> {
    match col {
        Column::Str(s) => {
            let w = s.width() as usize;
            if value.len() > w {
                let wider = u16::try_from(value.len()).map_or(StorageType::StrL, |width| {
                    if width <= 2045 {
                        StorageType::Str { width }
                    } else {
                        StorageType::StrL
                    }
                });
                return Err(WriteError::NeedsPromotion(wider));
            }
            let c = chunk_of(row);
            let o = offset_in_chunk(row) * w;
            let dst = &mut s.chunk_mut(c)[o..o + w];
            dst.fill(0);
            dst[..value.len()].copy_from_slice(value);
            Ok(())
        }
        Column::StrL(s) => {
            let c = chunk_of(row);
            s.chunk_mut(c).set(offset_in_chunk(row), value, false);
            Ok(())
        }
        _ => Err(WriteError::TypeMismatch),
    }
}

/// Write `value` into `row` of a `strL` column, with an explicit GSO binary
/// flag.
///
/// `pub(crate)` for the reason [`write_bytes`] is: the only caller is
/// [`ColMut`](crate::frame::ColMut), which has already journalled the chunk.
/// Deliberately narrower than `write_bytes` — only `strL` accepted, no
/// promotion offer — because the flag is the whole point of the call and a
/// `str#` column has nowhere to store it; silently dropping it would round-trip
/// a binary blob through `save` as NUL-terminated text.
pub(crate) fn write_strl(
    col: &mut Column,
    row: u64,
    value: &[u8],
    binary: bool,
) -> Result<(), WriteError> {
    match col {
        Column::StrL(s) => {
            let c = chunk_of(row);
            s.chunk_mut(c).set(offset_in_chunk(row), value, binary);
            Ok(())
        }
        _ => Err(WriteError::TypeMismatch),
    }
}

/// Rewrite a whole column into `to`, preserving every value and every missing
/// tag. This is Stata's automatic promotion (`variable i was int now long`).
///
/// It is O(rows) and allocates a second column by construction — that is what a
/// storage-type change *is*. The journal records it as one whole-column entry,
/// which is why `counters().column_rewrites` is separate from `chunks_cloned`:
/// a promotion is not the cost model A18 is about, and conflating them would
/// hide a `replace` that promotes on its first observation.
#[must_use]
pub fn recast(col: &Column, to: StorageType) -> Column {
    bump(&counters().column_rewrites, 1);
    let len = col.len();
    let mut out = Column::new_missing(to, len);
    if col.is_numeric() && stratum_core::types::is_numeric(to) {
        for row in 0..len {
            let v = col.get_f64(row).expect("numeric");
            // A promotion is up the ladder, so narrowing cannot fail; if it
            // ever does, storing `.` would silently destroy data, so we do not.
            write_f64(&mut out, row, v).expect("promotion target holds every source value");
        }
    } else {
        for row in 0..len {
            if let Some(b) = col.get_bytes(row) {
                let b = b.to_vec();
                write_bytes(&mut out, row, &b).expect("promotion target holds every source value");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_core::missing::{missing_f64, SYSMISS};

    #[test]
    fn a_fresh_column_is_all_missing_in_every_width() {
        for ty in [
            StorageType::Byte,
            StorageType::Int,
            StorageType::Long,
            StorageType::Float,
            StorageType::Double,
        ] {
            let c = Column::new_missing(ty, 100);
            assert_eq!(c.len(), 100);
            for row in [0u64, 1, 99] {
                assert!(
                    is_missing(c.get_f64(row).expect("numeric")),
                    "{ty:?} row {row}"
                );
                assert_eq!(c.get_f64(row), Some(SYSMISS), "{ty:?}");
            }
        }
    }

    #[test]
    fn widening_goes_through_core_and_not_through_a_cast() {
        // An `int` holding `INT_MISS` is `.`; a `double` holding that same
        // numeric value is an ordinary number. A column holding the sentinel
        // must widen to SYSMISS, never to the sentinel's own arithmetic value.
        let c = Column::Int(NumCol::from_slice(&[1i16, INT_MISS, INT_MISS + 3, -5]));
        assert_eq!(c.get_f64(0), Some(1.0));
        assert_eq!(c.get_f64(1), Some(SYSMISS));
        assert_eq!(c.get_f64(2), Some(missing_f64(3)));
        assert_eq!(c.get_f64(3), Some(-5.0));
    }

    #[test]
    fn a_negative_float_is_not_a_missing_value() {
        // The raw-bit trap from `04` §2.6: every negative f32 has
        // to_bits() > F32_MISS_BITS.
        let c = Column::Float(NumCol::from_slice(&[-1.0f32, -1e30, 0.0, SYSMISS_F32]));
        assert_eq!(c.get_f64(0), Some(-1.0));
        assert!(!is_missing(c.get_f64(1).expect("numeric")));
        assert_eq!(c.get_f64(3), Some(SYSMISS));
    }

    #[test]
    fn double_chunks_are_handed_out_without_a_copy() {
        let n = crate::chunk::CHUNK_ROWS as u64 * 2 + 7;
        let c = Column::Double(NumCol::from_slice(&vec![1.5f64; n as usize]));
        let before = counters().snapshot();
        let mut scratch = Vec::new();
        let mut seen = 0u64;
        let touched = c.for_each_chunk_f64(&mut scratch, |_, v| seen += v.len() as u64);
        let d = counters().snapshot().since(before);
        assert_eq!(touched, n);
        assert_eq!(seen, n);
        assert_eq!(d.rows_touched, n);
        // The zero here is the point: a Double column never widens.
        assert_eq!(d.rows_widened, 0);
        assert!(scratch.is_empty(), "the scratch buffer was never needed");
    }

    #[test]
    fn a_string_cell_is_trimmed_at_the_first_nul() {
        let mut s = FixedStrCol::empty(8, 3);
        s.chunk_mut(0)[0..8].copy_from_slice(b"abc\0XXXX");
        assert_eq!(s.get(0), b"abc");
        assert_eq!(s.raw(0), b"abc\0XXXX");
        assert_eq!(s.get(1), b"");
    }

    #[test]
    fn the_bulk_ingest_writes_each_cell_exactly_once() {
        // Two variables, int then double, four observations, row-major.
        let row_width = 2 + 8;
        let mut src = vec![0u8; row_width * 4];
        for r in 0..4usize {
            src[r * row_width..r * row_width + 2].copy_from_slice(&(r as i16).to_le_bytes());
            src[r * row_width + 2..r * row_width + 10]
                .copy_from_slice(&(r as f64 * 0.5).to_le_bytes());
        }
        let before = counters().snapshot();
        let a = Column::from_row_major(StorageType::Int, &src, row_width, 0, 4);
        let b = Column::from_row_major(StorageType::Double, &src, row_width, 2, 4);
        let d = counters().snapshot().since(before);
        assert_eq!(d.ingest_cells, 8);
        assert_eq!(a.get_f64(3), Some(3.0));
        assert_eq!(b.get_f64(3), Some(1.5));
    }

    #[test]
    fn the_strl_bulk_constructor_tiles_chunks_and_keeps_the_binary_flag() {
        // One row past a chunk boundary, so the tiling itself is exercised:
        // the boundary row must land at offset 0 of chunk 1, not at the tail
        // of chunk 0.
        let n = crate::chunk::CHUNK_ROWS + 1;
        let before = counters().snapshot();
        let col = StrLCol::from_rows((0..n).map(|i| -> (&[u8], bool) {
            if i == 1 {
                (b"blob\0with\0nuls", true)
            } else if i == n - 1 {
                (b"boundary", false)
            } else {
                (b"text", false)
            }
        }));
        let d = counters().snapshot().since(before);
        assert_eq!(col.len(), n as u64);
        assert_eq!(col.n_chunks(), 2);
        assert_eq!(col.get(1), b"blob\0with\0nuls");
        assert_eq!(col.get(n as u64 - 1), b"boundary");
        assert_eq!(col.get(n as u64 - 2), b"text");
        assert!(col.chunk(0).is_binary(1), "the GSO type-129 flag survives");
        assert!(!col.chunk(0).is_binary(0));
        // One ingest cell per row — the same floor `from_row_major` counts.
        // A floor and not an equality: `counters()` is process-global, and
        // under a runner that shares one process across tests (plain `cargo
        // test`, as opposed to nextest's process per test) every other test
        // building a column adds to the same total.
        assert!(
            d.ingest_cells >= n as u64,
            "{} rows ingested, counter moved {}",
            n,
            d.ingest_cells
        );
    }

    #[test]
    fn the_strl_bulk_constructor_accepts_zero_rows() {
        let col = StrLCol::from_rows(std::iter::empty::<(&[u8], bool)>());
        assert_eq!(col.len(), 0);
        assert_eq!(col.n_chunks(), 0);
        assert_eq!(col, StrLCol::empty(0));
    }

    #[test]
    fn the_digest_separates_types_widths_and_missing_tags() {
        let a = Column::Double(NumCol::from_slice(&[1.0, 2.0, SYSMISS]));
        let b = Column::Double(NumCol::from_slice(&[1.0, 2.0, missing_f64(1)]));
        assert_ne!(a.digest(), b.digest(), ". and .a must not collide");

        let c = Column::Float(NumCol::from_slice(&[1.0f32, 2.0, SYSMISS_F32]));
        assert_ne!(a.digest(), c.digest(), "type is part of identity");

        let d = Column::Double(NumCol::from_slice(&[1.0, 2.0, SYSMISS]));
        assert_eq!(a.digest(), d.digest(), "same bytes, same digest");
    }
}
