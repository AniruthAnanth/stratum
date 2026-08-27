//! `sort`, `gsort`, and the permutation that applies them (`04` §6.2).
//!
//! Two sorters over **one** encoder ([`crate::sortkey`]):
//!
//! * **LSD radix** over the materialised composite key — chosen when the key is
//!   at most [`RADIX_MAX_KEY_BYTES`] wide and there are at least
//!   [`RADIX_MIN_ROWS`] rows. Counting-sort passes, two `u32` permutation
//!   buffers ping-ponged, stable by construction.
//! * **`slice::sort_by`** (pdqsort, stable) over the permutation, comparing
//!   columns directly — chosen for small inputs, for wide `str#` keys where
//!   materialising `n × 200` bytes would be absurd, and for `strL`, whose
//!   content has no fixed width.
//!
//! Both sorters order by exactly the same bytes, so *which* one ran can only
//! change wall time, never the answer. `tests/sort.rs` proves that on 10 000
//! randomly generated frames rather than asserting it.
//!
//! # Ordering rules
//!
//! There are none here. Every one of Stata's placement rules for absent values
//! falls out of [`crate::sortkey`]'s encoding, and this module contains no
//! branch that knows they exist. That is the property `tests/sort.rs` scans the
//! source for.
//!
//! # Always stable
//!
//! Stata's `sort` does not promise an order among ties unless you ask for
//! `sort …, stable`; we are always stable. We are therefore *stricter* than
//! Stata and our output can differ from Stata's on tied observations even
//! though both are correct. For a tool whose headline feature is
//! reproducibility that is the right call, and it is recorded rather than
//! discovered later.

use std::sync::Arc;

use stratum_proto::{SortDir, VarIdx};

use crate::chunk::chunk_rows;
use crate::column::{Column, ColumnRef, FixedStrCol, NumCol, StrLCol};
use crate::perf::{bump, counters, RADIX_MAX_KEY_BYTES, RADIX_MIN_ROWS};
use crate::sortkey::{compare_rows, encode_key, key_width};

/// Which sorter to use. `Auto` is what production takes; the other two exist so
/// a test can run both over the same input and compare.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Strategy {
    /// Radix when the shape allows it, comparator otherwise.
    #[default]
    Auto,
    /// Force the radix path. Errors when the key has no fixed width.
    Radix,
    /// Force the comparator path.
    Comparator,
}

/// Why a sort could not run.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SortError {
    /// The permutation is `u32`, which halves the traffic of every radix pass
    /// and of every gather that applies it. 4.29 billion observations of a
    /// single `double` is 34 GB, so this bound is not the one that binds first.
    #[error("cannot sort {0} observations; the limit is 4294967295")]
    TooManyObs(u64),
    /// `Strategy::Radix` was asked for over a key with no fixed width.
    #[error("this key has no fixed width; the radix path cannot encode it")]
    NoFixedWidthKey,
}

/// Which variables a frame is sorted by, and whether that is still true.
///
/// This maps exactly onto the `.dta` `sortlist` block, so `save`/`use` preserve
/// `Sorted by:` with no extra bookkeeping (`04` §6.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SortState {
    /// Key variables in priority order. Empty means unsorted.
    pub keys: Vec<VarIdx>,
    /// Cleared by any write to a key column and by any reordering.
    pub valid: bool,
}

impl SortState {
    /// The unsorted state.
    #[must_use]
    pub fn unsorted() -> Self {
        Self::default()
    }

    /// Is `var` one of the keys?
    #[must_use]
    pub fn is_key(&self, var: VarIdx) -> bool {
        self.keys.contains(&var)
    }

    /// Invalidate, keeping the key list for diagnostics.
    pub fn invalidate(&mut self) {
        self.valid = false;
    }
}

/// The permutation that sorts `cols`: `perm[i]` is the observation that ends up
/// at position `i`.
///
/// # Errors
///
/// [`SortError`].
pub fn permutation(
    cols: &[(&Column, SortDir)],
    nobs: u64,
    strategy: Strategy,
) -> Result<Vec<u32>, SortError> {
    let n = u32::try_from(nobs).map_err(|_| SortError::TooManyObs(nobs))?;
    let identity: Vec<u32> = (0..n).collect();
    if cols.is_empty() || n <= 1 {
        return Ok(identity);
    }

    let widths: Option<Vec<usize>> = cols
        .iter()
        .map(|(c, _)| key_width(c.storage_type()))
        .collect();
    let total: Option<usize> = widths.as_ref().map(|w| w.iter().sum());

    let radix_ok = matches!(total, Some(t) if t <= RADIX_MAX_KEY_BYTES);
    let use_radix = match strategy {
        Strategy::Radix => {
            if !radix_ok {
                return Err(SortError::NoFixedWidthKey);
            }
            true
        }
        Strategy::Comparator => false,
        Strategy::Auto => radix_ok && nobs >= RADIX_MIN_ROWS,
    };

    if use_radix {
        let widths = widths.expect("radix_ok implies every width is known");
        Ok(radix(cols, &widths, identity))
    } else {
        Ok(comparator(cols, identity))
    }
}

/// `slice::sort_by` over the permutation, comparing key columns in order.
fn comparator(cols: &[(&Column, SortDir)], mut perm: Vec<u32>) -> Vec<u32> {
    perm.sort_by(|&a, &b| {
        bump(&counters().comparisons, 1);
        for (col, dir) in cols {
            let ord = compare_rows(col, u64::from(a), u64::from(b), *dir);
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });
    perm
}

/// LSD radix over a materialised `n × width` key buffer.
fn radix(cols: &[(&Column, SortDir)], widths: &[usize], mut perm: Vec<u32>) -> Vec<u32> {
    let n = perm.len();
    let w: usize = widths.iter().sum();

    // BYTE-PLANE LAYOUT, and it is worth a paragraph. The obvious layout is
    // row-major — `keys[row * w + p]` — and it is 3x slower here, because a
    // scatter pass reads `keys[perm[i] * w + p]` with `perm` already shuffled:
    // one random access per row into the WHOLE `n * w` buffer, which at 10 M
    // doubles is 80 MB and one cache line touched per byte actually used.
    // Storing plane `p` contiguously means a pass touches only its own `n`
    // bytes — 10 MB, small enough to stay resident — and the sequence of passes
    // walks the planes one at a time.
    let mut planes = vec![0u8; n * w];
    let mut off = 0usize;
    let mut cell = [0u8; 16];
    for ((col, dir), kw) in cols.iter().zip(widths) {
        for row in 0..n {
            let key = &mut cell[..*kw];
            encode_key(col, row as u64, *dir, key);
            for (j, b) in key.iter().enumerate() {
                planes[(off + j) * n + row] = *b;
            }
        }
        off += kw;
    }

    let mut scratch = vec![0u32; n];
    // Least significant byte first: that is what makes the pass sequence stable
    // and the composite key work without a second sort.
    for p in (0..w).rev() {
        let plane = &planes[p * n..(p + 1) * n];
        let hist = histogram(plane);
        // A byte position where every row agrees cannot change the order, and
        // skipping it is the difference between 8 passes and 3 on the integer
        // columns real datasets are full of.
        if hist.iter().any(|&c| c as usize == n) {
            continue;
        }
        let mut pos = [0u32; 256];
        let mut running = 0u32;
        for (b, count) in hist.iter().enumerate() {
            pos[b] = running;
            running += count;
        }
        for &row in &perm {
            let b = plane[row as usize] as usize;
            scratch[pos[b] as usize] = row;
            pos[b] += 1;
        }
        std::mem::swap(&mut perm, &mut scratch);
        bump(&counters().radix_passes, 1);
        bump(&counters().radix_rows, n as u64);
    }
    perm
}

/// Byte histogram of one plane. A sequential read of `n` bytes.
#[cfg(feature = "parallel")]
fn histogram(plane: &[u8]) -> [u32; 256] {
    use rayon::prelude::*;

    let tile = stratum_core::CHUNK_ROWS;
    if plane.len() < tile * crate::perf::PAR_MIN_CHUNKS {
        return histogram_seq(plane);
    }
    // Per-tile local histograms folded together. The fold is over exact
    // integers, so unlike a float reduction it needs no ordering guarantee —
    // but the tiling is `CHUNK_ROWS` anyway, because that is the granule.
    plane.par_chunks(tile).map(histogram_seq).reduce(
        || [0u32; 256],
        |mut a, b| {
            for (x, y) in a.iter_mut().zip(b.iter()) {
                *x += *y;
            }
            a
        },
    )
}

#[cfg(not(feature = "parallel"))]
fn histogram(plane: &[u8]) -> [u32; 256] {
    histogram_seq(plane)
}

fn histogram_seq(plane: &[u8]) -> [u32; 256] {
    let mut h = [0u32; 256];
    for &b in plane {
        h[b as usize] += 1;
    }
    h
}

/// `inverse[perm[i]] == i`: the permutation that undoes `perm`.
///
/// This is what the undo journal retains for a reordering — one `Arc<[u32]>`,
/// 4 bytes per observation, instead of a copy of every column (ARCHITECTURE
/// §7.6). Rolling a `sort` back costs one more gather pass and 40 MB at 10 M
/// rows, not 1.2 GB.
#[must_use]
pub fn invert(perm: &[u32]) -> Vec<u32> {
    let mut inv = vec![0u32; perm.len()];
    for (i, &p) in perm.iter().enumerate() {
        inv[p as usize] = i as u32;
    }
    inv
}

/// Build a new column with `out[i] == col[perm[i]]`.
///
/// One gather pass per column: the permutation is sorted once and the wide
/// `str#` payloads move exactly once (`04` §6.2).
#[must_use]
pub fn permute_column(col: &Column, perm: &[u32]) -> Column {
    let n = perm.len() as u64;
    bump(&counters().rows_touched, n);
    match col {
        Column::Byte(c) => Column::Byte(gather_num(c, perm)),
        Column::Int(c) => Column::Int(gather_num(c, perm)),
        Column::Long(c) => Column::Long(gather_num(c, perm)),
        Column::Float(c) => Column::Float(gather_num(c, perm)),
        Column::Double(c) => Column::Double(gather_num(c, perm)),
        Column::Str(c) => {
            let w = c.width() as usize;
            let mut out = FixedStrCol::empty(c.width(), n);
            for ch in 0..out.n_chunks() {
                let (lo, hi) = chunk_rows(ch, n);
                let dst = out.chunk_mut(ch);
                for (j, row) in (lo..hi).enumerate() {
                    dst[j * w..(j + 1) * w].copy_from_slice(c.raw(u64::from(perm[row as usize])));
                }
            }
            Column::Str(out)
        }
        Column::StrL(c) => {
            let mut out = StrLCol::empty(n);
            for ch in 0..out.n_chunks() {
                let (lo, hi) = chunk_rows(ch, n);
                let src: Vec<Vec<u8>> = (lo..hi)
                    .map(|row| c.get(u64::from(perm[row as usize])).to_vec())
                    .collect();
                let dst = out.chunk_mut(ch);
                for (j, bytes) in src.iter().enumerate() {
                    dst.set(j, bytes, false);
                }
            }
            Column::StrL(out)
        }
    }
}

fn gather_num<T: crate::column::NumElem>(c: &NumCol<T>, perm: &[u32]) -> NumCol<T> {
    let n = perm.len() as u64;
    let mut out = NumCol::<T>::missing(n);
    for ch in 0..out.n_chunks() {
        let (lo, hi) = chunk_rows(ch, n);
        let dst = out.chunk_mut(ch);
        for (j, row) in (lo..hi).enumerate() {
            dst[j] = c.get(u64::from(perm[row as usize]));
        }
    }
    out
}

/// Apply `perm` to every column in place.
pub(crate) fn permute_all(cols: &mut [ColumnRef], perm: &[u32]) {
    for slot in cols.iter_mut() {
        *slot = Arc::new(permute_column(slot, perm));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_core::missing::{missing_f64, SYSMISS};

    fn sorted_values(col: &Column, perm: &[u32]) -> Vec<f64> {
        perm.iter()
            .map(|&i| col.get_f64(u64::from(i)).expect("numeric"))
            .collect()
    }

    #[test]
    fn the_measured_ascending_order_reproduces_exactly() {
        // tests/golden/stata18/semantics.log, `sort x`:
        //   -50, 0, 1, 100, ., .a, .b, .z
        let col = Column::Double(NumCol::from_slice(&[
            1.0,
            100.0,
            -50.0,
            SYSMISS,
            missing_f64(1),
            missing_f64(2),
            missing_f64(26),
            0.0,
        ]));
        let want = vec![
            -50.0,
            0.0,
            1.0,
            100.0,
            SYSMISS,
            missing_f64(1),
            missing_f64(2),
            missing_f64(26),
        ];
        for strategy in [Strategy::Comparator, Strategy::Radix] {
            let perm =
                permutation(&[(&col, SortDir::Asc)], 8, strategy).expect("a double key sorts");
            assert_eq!(sorted_values(&col, &perm), want, "{strategy:?}");
        }
    }

    #[test]
    fn an_empty_string_sorts_first() {
        // `04` §2.2: sort s -> "", "", "a", "b". The opposite of the numeric
        // rule, and it costs no code.
        let mut c = FixedStrCol::empty(4, 4);
        c.chunk_mut(0)[0..4].copy_from_slice(b"b\0\0\0");
        c.chunk_mut(0)[4..8].copy_from_slice(b"\0\0\0\0");
        c.chunk_mut(0)[8..12].copy_from_slice(b"a\0\0\0");
        c.chunk_mut(0)[12..16].copy_from_slice(b"\0\0\0\0");
        let col = Column::Str(c);
        for strategy in [Strategy::Comparator, Strategy::Radix] {
            let perm = permutation(&[(&col, SortDir::Asc)], 4, strategy).expect("a str4 key");
            let got: Vec<&[u8]> = perm
                .iter()
                .map(|&i| col.get_bytes(u64::from(i)).expect("string"))
                .collect();
            assert_eq!(
                got,
                vec![&b""[..], &b""[..], &b"a"[..], &b"b"[..]],
                "{strategy:?}"
            );
        }
    }

    #[test]
    fn ties_keep_their_original_order_on_both_paths() {
        let key = Column::Byte(NumCol::from_slice(&[1i8, 1, 1, 0, 0]));
        for strategy in [Strategy::Comparator, Strategy::Radix] {
            let perm = permutation(&[(&key, SortDir::Asc)], 5, strategy).expect("byte key");
            assert_eq!(perm, vec![3, 4, 0, 1, 2], "{strategy:?}");
        }
    }

    #[test]
    fn a_multi_key_sort_orders_by_the_first_key_then_the_second() {
        let a = Column::Byte(NumCol::from_slice(&[1i8, 0, 1, 0]));
        let b = Column::Int(NumCol::from_slice(&[20i16, 30, 10, 40]));
        for strategy in [Strategy::Comparator, Strategy::Radix] {
            let perm = permutation(&[(&a, SortDir::Asc), (&b, SortDir::Asc)], 4, strategy)
                .expect("two numeric keys");
            assert_eq!(perm, vec![1, 3, 2, 0], "{strategy:?}");
        }
    }

    #[test]
    fn descending_reverses_without_a_second_code_path() {
        let col = Column::Double(NumCol::from_slice(&[1.0, SYSMISS, -3.0]));
        for strategy in [Strategy::Comparator, Strategy::Radix] {
            let perm = permutation(&[(&col, SortDir::Desc)], 3, strategy).expect("double key");
            assert_eq!(
                sorted_values(&col, &perm),
                vec![SYSMISS, 1.0, -3.0],
                "{strategy:?}"
            );
        }
    }

    #[test]
    fn a_strl_key_cannot_take_the_radix_path() {
        let col = Column::StrL(StrLCol::empty(4));
        assert_eq!(
            permutation(&[(&col, SortDir::Asc)], 4, Strategy::Radix),
            Err(SortError::NoFixedWidthKey)
        );
        assert!(permutation(&[(&col, SortDir::Asc)], 4, Strategy::Auto).is_ok());
    }

    #[test]
    fn inverting_a_permutation_undoes_it() {
        let col = Column::Long(NumCol::from_slice(&[5i32, 2, 9, 2, -1]));
        let perm = permutation(&[(&col, SortDir::Asc)], 5, Strategy::Comparator).expect("long key");
        let sorted = permute_column(&col, &perm);
        let back = permute_column(&sorted, &invert(&perm));
        assert_eq!(back, col);
    }
}
