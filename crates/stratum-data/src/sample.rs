//! The observation-selection model (`04` §5).
//!
//! A command never sees "the dataset". It is handed a [`Sample`], and the four
//! shapes a sample can take are not an optimisation detail — they are the
//! difference between a memory-bandwidth-bound kernel and a gather-bound one.
//! `summarize` on an unrestricted 10 M-row `double` column must be a straight
//! chunk scan, so `All` and `Range` stay distinct from `Mask` all the way down.
//!
//! Kernels never index observation-by-observation through an enum dispatch;
//! they ask for [`runs`](Sample::runs). That is what makes a `Mask` which
//! happens to select a contiguous block — very common, `if year > 2010` on
//! sorted panel data — cost exactly what a `Range` costs.
//!
//! # Missing is truthy
//!
//! `if exp` selects an observation iff `exp != 0`, and `.` is the enormous
//! number `2^1023`, so **a missing value passes `if x`** (`04` §2.4, measured:
//! `count if x` counted `.` and `.a` and skipped `0`). This module does not
//! "helpfully" treat missing as false, which is why `if x < .` is the idiomatic
//! Stata non-missing guard and works here unchanged.

use std::sync::Arc;

use stratum_core::missing::is_missing;
use stratum_proto::StorageType;

use crate::bitset::{BitRuns, BitSet};
use crate::column::Column;

/// A contiguous span of selected observations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    /// First observation, 0-based.
    pub start: u64,
    /// How many observations.
    pub len: u64,
}

/// How a sample is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SampleKind {
    /// Every observation. Kernels take whole chunks.
    All,
    /// From `in 5/100` — still contiguous. Half-open, 0-based.
    Range {
        /// First selected observation.
        start: u64,
        /// One past the last.
        end: u64,
    },
    /// From `if`, optionally already narrowed by `in`.
    Mask {
        /// One bit per observation.
        bits: Arc<BitSet>,
        /// First set bit, so a kernel can skip the empty prefix.
        lo: u64,
        /// One past the last set bit.
        hi: u64,
    },
    /// Materialised ascending indices, for by-group slices.
    Index(Arc<[u64]>),
}

/// Which observations a command may see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    kind: SampleKind,
    nobs: u64,
    nsel: u64,
}

impl Sample {
    /// Every observation of a frame with `nobs` rows.
    #[must_use]
    pub fn all(nobs: u64) -> Self {
        Self {
            kind: SampleKind::All,
            nobs,
            nsel: nobs,
        }
    }

    /// A half-open, 0-based contiguous range, clamped to `nobs`.
    #[must_use]
    pub fn range(nobs: u64, start: u64, end: u64) -> Self {
        let start = start.min(nobs);
        let end = end.clamp(start, nobs);
        Self {
            kind: SampleKind::Range { start, end },
            nobs,
            nsel: end - start,
        }
    }

    /// From an explicit bitset. The popcount and the set region are computed
    /// once, here, so `len()` and `runs()` are free afterwards.
    #[must_use]
    pub fn mask(nobs: u64, bits: BitSet) -> Self {
        let nsel = bits.count_ones();
        let lo = bits.next_set(0).unwrap_or(0);
        let hi = if nsel == 0 {
            0
        } else {
            let mut last = lo;
            for r in bits.runs() {
                last = r.start + r.len;
            }
            last
        };
        Self {
            kind: SampleKind::Mask {
                bits: Arc::new(bits),
                lo,
                hi,
            },
            nobs,
            nsel,
        }
    }

    /// From ascending, unique observation indices.
    ///
    /// A *selection*, never an ordering: a Data-Editor view order is an
    /// `OrderId` handle (A13), not a `Sample`, because the permutation must
    /// never cross the wire and never reach a statistical kernel.
    #[must_use]
    pub fn index(nobs: u64, idx: Arc<[u64]>) -> Self {
        debug_assert!(
            idx.windows(2).all(|w| w[0] < w[1]),
            "Sample::index requires ascending unique indices"
        );
        let nsel = idx.len() as u64;
        Self {
            kind: SampleKind::Index(idx),
            nobs,
            nsel,
        }
    }

    /// How the sample is stored.
    #[must_use]
    pub fn kind(&self) -> &SampleKind {
        &self.kind
    }

    /// Selected observations.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.nsel
    }

    /// True when nothing is selected — `r(2000) no observations`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nsel == 0
    }

    /// Observations in the frame this sample was built against.
    #[must_use]
    pub fn nobs(&self) -> u64 {
        self.nobs
    }

    /// True when the selection is one span, so a kernel can take a slice.
    #[must_use]
    pub fn is_contiguous(&self) -> bool {
        match &self.kind {
            SampleKind::All | SampleKind::Range { .. } => true,
            SampleKind::Mask { .. } | SampleKind::Index(_) => {
                self.nsel <= 1 || self.runs().nth(1).is_none()
            }
        }
    }

    /// Is observation `obs` selected?
    #[must_use]
    pub fn contains(&self, obs: u64) -> bool {
        match &self.kind {
            SampleKind::All => obs < self.nobs,
            SampleKind::Range { start, end } => obs >= *start && obs < *end,
            SampleKind::Mask { bits, .. } => bits.get(obs),
            SampleKind::Index(idx) => idx.binary_search(&obs).is_ok(),
        }
    }

    /// The maximal contiguous runs of selected observations, ascending.
    #[must_use]
    pub fn runs(&self) -> RunIter<'_> {
        match &self.kind {
            SampleKind::All => RunIter::One(Some(Run {
                start: 0,
                len: self.nobs,
            })),
            SampleKind::Range { start, end } => RunIter::One(if end > start {
                Some(Run {
                    start: *start,
                    len: end - start,
                })
            } else {
                None
            }),
            SampleKind::Mask { bits, .. } => RunIter::Bits(bits.runs()),
            SampleKind::Index(idx) => RunIter::Index { idx, at: 0 },
        }
    }

    /// Materialise `touse` as a real `byte` variable (`04` §5.4).
    ///
    /// User-written ado does `marksample touse` then passes `if \`touse'` down,
    /// and users `list if touse`. The round trip through a real variable is
    /// therefore observable, and v1 does not try to elide it for a microsecond.
    #[must_use]
    pub fn to_touse(&self) -> Column {
        let mut col = Column::new_missing(StorageType::Byte, self.nobs);
        // `new_missing` fills with `.`; touse is 0/1, so start from zero.
        for c in 0..col.n_chunks() {
            let (lo, hi) = crate::chunk::chunk_rows(c, self.nobs);
            for row in lo..hi {
                let v = f64::from(u8::from(self.contains(row)));
                crate::column::write_f64(&mut col, row, v).expect("0 and 1 fit a byte");
            }
        }
        col
    }

    /// Rebuild a sample from a `touse` variable: selected iff non-zero, which
    /// keeps the missing-is-truthy rule (`04` §2.4) intact on the way back.
    #[must_use]
    pub fn from_touse(col: &Column) -> Sample {
        let nobs = col.len();
        let mut bits = BitSet::new(nobs);
        for row in 0..nobs {
            if col.get_f64(row).is_some_and(|v| v != 0.0) {
                bits.set(row, true);
            }
        }
        Sample::mask(nobs, bits)
    }
}

/// Iterator over [`Sample::runs`].
#[derive(Clone, Debug)]
pub enum RunIter<'a> {
    /// `All` and `Range` yield exactly one run.
    One(Option<Run>),
    /// `Mask` scans words.
    Bits(BitRuns<'a>),
    /// `Index` coalesces consecutive indices.
    Index {
        /// The ascending index list.
        idx: &'a [u64],
        /// Cursor into it.
        at: usize,
    },
}

impl Iterator for RunIter<'_> {
    type Item = Run;

    fn next(&mut self) -> Option<Run> {
        match self {
            RunIter::One(r) => r.take(),
            RunIter::Bits(b) => b.next(),
            RunIter::Index { idx, at } => {
                if *at >= idx.len() {
                    return None;
                }
                let start = idx[*at];
                let mut end = start + 1;
                *at += 1;
                while *at < idx.len() && idx[*at] == end {
                    end += 1;
                    *at += 1;
                }
                Some(Run {
                    start,
                    len: end - start,
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// in
// ---------------------------------------------------------------------------

/// One endpoint of an `in` range, before it is resolved against `_N`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bound {
    /// `f` — the first observation.
    First,
    /// `l` — the last observation.
    Last,
    /// A literal 1-based observation number.
    Abs(u64),
    /// `-k`, counted back from the last observation: `-1` is `l`.
    FromEnd(u64),
}

/// `in f/l`, `in 5/100`, `in -10/l`. 1-based and inclusive, as written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InRange {
    /// The first endpoint.
    pub first: Bound,
    /// The second endpoint.
    pub last: Bound,
}

impl InRange {
    /// Resolve to a half-open 0-based range against `nobs`.
    ///
    /// # Errors
    ///
    /// [`SampleError`]. Out-of-range endpoints are an **error, not a clamp**
    /// (`04` §5.2), because Stata says so: `list in 999` on 74 observations is
    /// "observation numbers out of range", and `list in 0` is
    /// "'0' invalid observation number" — both `r(198)`, both in
    /// `tests/golden/stata18/errors.log`.
    pub fn resolve(self, nobs: u64) -> Result<(u64, u64), SampleError> {
        let one = |b: Bound| -> Result<u64, SampleError> {
            Ok(match b {
                Bound::First => 1,
                Bound::Last => nobs,
                Bound::Abs(0) => return Err(SampleError::InvalidObsNumber),
                Bound::Abs(n) => n,
                Bound::FromEnd(0) => return Err(SampleError::InvalidObsNumber),
                Bound::FromEnd(k) => nobs.checked_sub(k - 1).ok_or(SampleError::OutOfRange)?,
            })
        };
        let f = one(self.first)?;
        let l = one(self.last)?;
        if f > nobs || l > nobs || f == 0 || l == 0 {
            return Err(SampleError::OutOfRange);
        }
        // `in 10/5` selects nothing; Stata treats it as an empty range rather
        // than an error, and `count in 10/5` is 0.
        Ok((f - 1, l.max(f - 1)))
    }
}

/// Why a sample could not be built.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SampleError {
    /// `list in 999` on a 74-observation dataset.
    #[error("observation numbers out of range")]
    OutOfRange,
    /// `list in 0`.
    #[error("'0' invalid observation number")]
    InvalidObsNumber,
    /// An `if` vector of the wrong length reached the builder — a caller bug,
    /// not a user error, but it must not silently select the wrong rows.
    #[error("if-expression covered {got} of {want} observations")]
    LengthMismatch {
        /// Values supplied.
        got: u64,
        /// Values the frame has.
        want: u64,
    },
}

impl SampleError {
    /// Stata's return code. Both range errors are `r(198)` (measured).
    #[must_use]
    pub fn rc(self) -> u16 {
        match self {
            SampleError::OutOfRange | SampleError::InvalidObsNumber => 198,
            SampleError::LengthMismatch { .. } => 198,
        }
    }
}

// ---------------------------------------------------------------------------
// SampleBuilder
// ---------------------------------------------------------------------------

/// Builds a [`Sample`] out of `in`, `if` and `markout`, in that order.
///
/// Takes `nobs` rather than `&Frame` (`04` §5.2's sketch): the caller needs to
/// read columns to evaluate the `if` while the builder is alive, and borrowing
/// the whole frame here would make that impossible without an interior-mutable
/// detour.
#[derive(Debug)]
pub struct SampleBuilder {
    nobs: u64,
    range: Option<(u64, u64)>,
    mask: Option<BitSet>,
}

impl SampleBuilder {
    /// Start from "every observation".
    #[must_use]
    pub fn new(nobs: u64) -> Self {
        Self {
            nobs,
            range: None,
            mask: None,
        }
    }

    /// Apply `in`.
    ///
    /// # Errors
    ///
    /// [`SampleError`] — see [`InRange::resolve`].
    pub fn r#in(mut self, spec: InRange) -> Result<Self, SampleError> {
        self.range = Some(spec.resolve(self.nobs)?);
        Ok(self)
    }

    /// Apply `if exp`, given the expression's value for every observation.
    ///
    /// Selected iff the value is non-zero, so missing selects (`04` §2.4).
    ///
    /// # Errors
    ///
    /// [`SampleError::LengthMismatch`] when `values` does not cover the frame.
    pub fn r#if(mut self, values: &[f64]) -> Result<Self, SampleError> {
        if values.len() as u64 != self.nobs {
            return Err(SampleError::LengthMismatch {
                got: values.len() as u64,
                want: self.nobs,
            });
        }
        let mut bits = self.mask.take().unwrap_or_else(|| BitSet::all(self.nobs));
        for (row, &v) in values.iter().enumerate() {
            if v == 0.0 {
                bits.set(row as u64, false);
            }
        }
        self.mask = Some(bits);
        Ok(self)
    }

    /// Apply one chunk of an `if` expression, for a caller evaluating chunk-wise.
    ///
    /// `row0` is the first observation `values` covers.
    pub fn if_chunk(&mut self, row0: u64, values: &[f64]) {
        let bits = self.mask.get_or_insert_with(|| BitSet::all(self.nobs));
        for (i, &v) in values.iter().enumerate() {
            if v == 0.0 {
                bits.set(row0 + i as u64, false);
            }
        }
    }

    /// `markout touse varlist` (`04` §5.2, all three rules measured).
    ///
    /// * numeric variable: drop observations where the value is missing;
    /// * **string variable without `strok`: drop EVERY observation.** Stata
    ///   really does this — `markout t2 x s` leaves `t2 == 0` everywhere — and a
    ///   reimplementation that "sensibly" drops only the empty ones silently
    ///   changes every estimation sample that includes a string variable;
    /// * string variable with `strok`: drop observations whose value is `""`.
    #[must_use]
    pub fn markout(mut self, vars: &[&Column], strok: bool) -> Self {
        let mut bits = self.mask.take().unwrap_or_else(|| BitSet::all(self.nobs));
        for col in vars {
            if col.is_numeric() {
                for row in 0..self.nobs {
                    if col.get_f64(row).is_none_or(is_missing) {
                        bits.set(row, false);
                    }
                }
            } else if strok {
                for row in 0..self.nobs {
                    if col.get_bytes(row).is_none_or(<[u8]>::is_empty) {
                        bits.set(row, false);
                    }
                }
            } else {
                for row in 0..self.nobs {
                    bits.set(row, false);
                }
            }
        }
        self.mask = Some(bits);
        self
    }

    /// Combine everything into a [`Sample`].
    #[must_use]
    pub fn build(self) -> Sample {
        match (self.range, self.mask) {
            (None, None) => Sample::all(self.nobs),
            (Some((s, e)), None) => Sample::range(self.nobs, s, e),
            (None, Some(bits)) => Sample::mask(self.nobs, bits),
            (Some((s, e)), Some(mut bits)) => {
                // `in` narrows what `if` selected: clear everything outside.
                let mut keep = BitSet::new(self.nobs);
                keep.set_range(s, e);
                bits.and_assign(&keep);
                Sample::mask(self.nobs, bits)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column::NumCol;
    use stratum_core::missing::{missing_f64, SYSMISS};

    #[test]
    fn in_resolves_the_three_spellings() {
        let all = InRange {
            first: Bound::First,
            last: Bound::Last,
        };
        assert_eq!(all.resolve(74), Ok((0, 74)));
        let five_to_hundred = InRange {
            first: Bound::Abs(5),
            last: Bound::Abs(74),
        };
        assert_eq!(five_to_hundred.resolve(74), Ok((4, 74)));
        let tail = InRange {
            first: Bound::FromEnd(10),
            last: Bound::Last,
        };
        assert_eq!(tail.resolve(74), Ok((64, 74)));
    }

    #[test]
    fn out_of_range_is_an_error_and_not_a_clamp() {
        // Both measured in tests/golden/stata18/errors.log, both r(198).
        let e = InRange {
            first: Bound::Abs(999),
            last: Bound::Abs(999),
        }
        .resolve(74)
        .expect_err("999 of 74");
        assert_eq!(e, SampleError::OutOfRange);
        assert_eq!(e.rc(), 198);

        let z = InRange {
            first: Bound::Abs(0),
            last: Bound::Last,
        }
        .resolve(74)
        .expect_err("observation 0");
        assert_eq!(z, SampleError::InvalidObsNumber);
        assert_eq!(z.rc(), 198);
    }

    #[test]
    fn missing_is_truthy_in_if() {
        // `04` §2.4, measured: `count if x` counted `.` and `.a`, skipped 0.
        let values = [1.0, 0.0, 3.0, SYSMISS, missing_f64(1)];
        let s = SampleBuilder::new(5).r#if(&values).expect("length").build();
        assert_eq!(s.len(), 4);
        assert!(s.contains(0) && !s.contains(1) && s.contains(3) && s.contains(4));
    }

    #[test]
    fn a_contiguous_mask_is_one_run() {
        let mut b = SampleBuilder::new(1000);
        b.if_chunk(0, &vec![0.0; 300]);
        b.if_chunk(300, &vec![1.0; 400]);
        b.if_chunk(700, &vec![0.0; 300]);
        let s = b.build();
        assert_eq!(s.len(), 400);
        assert!(s.is_contiguous());
        assert_eq!(s.runs().count(), 1);
    }

    #[test]
    fn markout_without_strok_drops_every_observation() {
        // Measured: `markout t2 x s` -> t2 == 0 for every observation.
        let x = Column::Double(NumCol::from_slice(&[1.0, 2.0, 3.0]));
        let s = Column::Str(crate::column::FixedStrCol::empty(4, 3));
        let sample = SampleBuilder::new(3).markout(&[&x, &s], false).build();
        assert_eq!(sample.len(), 0);
    }

    #[test]
    fn markout_with_strok_drops_only_empty_strings_and_missings() {
        let x = Column::Double(NumCol::from_slice(&[1.0, SYSMISS, 3.0]));
        let mut sc = crate::column::FixedStrCol::empty(4, 3);
        sc.chunk_mut(0)[0..4].copy_from_slice(b"ab\0\0");
        sc.chunk_mut(0)[4..8].copy_from_slice(b"cd\0\0");
        // row 2 stays ""
        let s = Column::Str(sc);
        let sample = SampleBuilder::new(3).markout(&[&x, &s], true).build();
        assert_eq!(sample.len(), 1);
        assert!(sample.contains(0));
    }

    #[test]
    fn in_narrows_if() {
        let values: Vec<f64> = (0..100).map(|i| f64::from(i % 2)).collect();
        let s = SampleBuilder::new(100)
            .r#in(InRange {
                first: Bound::Abs(11),
                last: Bound::Abs(20),
            })
            .expect("range")
            .r#if(&values)
            .expect("length")
            .build();
        // Rows 10..20 zero-based, odd ones selected: 11, 13, 15, 17, 19.
        assert_eq!(s.len(), 5);
        assert!(s.contains(11) && !s.contains(9) && !s.contains(21));
    }

    #[test]
    fn touse_round_trips_through_a_real_byte_variable() {
        let values: Vec<f64> = (0..10).map(|i| f64::from(i % 3 == 0)).collect();
        let s = SampleBuilder::new(10)
            .r#if(&values)
            .expect("length")
            .build();
        let col = s.to_touse();
        assert_eq!(col.storage_type(), StorageType::Byte);
        let back = Sample::from_touse(&col);
        assert_eq!(back.len(), s.len());
        for row in 0..10 {
            assert_eq!(back.contains(row), s.contains(row), "row {row}");
        }
    }

    #[test]
    fn index_runs_coalesce() {
        let s = Sample::index(100, Arc::from(vec![1u64, 2, 3, 7, 8, 40]));
        let runs: Vec<Run> = s.runs().collect();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0], Run { start: 1, len: 3 });
        assert_eq!(runs[2], Run { start: 40, len: 1 });
        assert_eq!(s.len(), 6);
    }
}
