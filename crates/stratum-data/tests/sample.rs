//! W02 acceptance: the observation-selection model (`04` §5), anchored on the
//! measured behaviour rather than on what `if` "obviously" means.
//!
//! The three rules a reimplementation gets wrong, all of them measured:
//!
//! 1. **Missing is truthy.** `.` is `2^1023`, so `if x` selects it. Every
//!    counter here is checked against `tests/golden/stata18/semantics.log`'s
//!    eight-observation dataset.
//! 2. **`markout` without `strok` drops every observation** when a string
//!    variable is in the varlist. Not "the empty ones" — all of them.
//! 3. **An out-of-range `in` is an error, not a clamp**, and both spellings are
//!    `r(198)`.
//!
//! Plus the property the whole design rests on: iterating a sample by *runs*
//! reproduces exactly the selected observations, so a contiguous `Mask` costs
//! what a `Range` costs.

use std::sync::RwLock;

use proptest::prelude::*;
use stratum_core::missing::{missing_f64, SYSMISS};
use stratum_data::column::NumCol;
use stratum_data::sample::{Bound, InRange, Run, SampleError};
use stratum_data::{counters, BitSet, Column, Frame, Sample, SampleBuilder, StorageType};

/// See `tests/sort.rs`: readers of the process-wide counters need the writers'
/// side of this, everything else runs in parallel.
static COUNTERS: RwLock<()> = RwLock::new(());

/// The dataset `semantics.log` runs its counts against, in storage order.
const GOLDEN: [f64; 8] = [
    1.0,
    100.0,
    -50.0,
    f64::NAN, // replaced below; NAN never reaches a column
    0.0,
    0.0,
    0.0,
    0.0,
];

fn golden_column() -> Column {
    let mut v = GOLDEN.to_vec();
    v[3] = SYSMISS;
    v[4] = missing_f64(1);
    v[5] = missing_f64(2);
    v[6] = missing_f64(26);
    v[7] = 0.0;
    Column::Double(NumCol::from_slice(&v))
}

fn values(col: &Column) -> Vec<f64> {
    (0..col.len())
        .map(|r| col.get_f64(r).expect("numeric"))
        .collect()
}

#[test]
fn the_measured_counts_reproduce() {
    let col = golden_column();
    let v = values(&col);
    let count = |f: &dyn Fn(f64) -> bool| -> u64 {
        let sel: Vec<f64> = v.iter().map(|&x| f64::from(u8::from(f(x)))).collect();
        SampleBuilder::new(8)
            .r#if(&sel)
            .expect("length matches")
            .build()
            .len()
    };

    // `. count if x < .`  ->  4
    assert_eq!(count(&|x| x < SYSMISS), 4, "count if x < .");
    // `. count if missing(x)`  ->  4
    assert_eq!(count(&stratum_core::is_missing), 4, "count if missing(x)");
    // `. count if x >= .`  ->  4
    assert_eq!(count(&|x| x >= SYSMISS), 4, "count if x >= .");
    // And the trap: `if x` is `x != 0`, and every missing is enormous, so all
    // four of them select. Only the literal 0 is dropped.
    assert_eq!(count(&|x| x != 0.0), 7, "count if x");
}

#[test]
fn if_selects_on_non_zero_and_never_on_missingness() {
    let sel = [1.0, 0.0, SYSMISS, missing_f64(1), -0.0, 1e-300];
    let s = SampleBuilder::new(6)
        .r#if(&sel)
        .expect("length matches")
        .build();
    assert_eq!(s.len(), 4);
    assert!(s.contains(0) && !s.contains(1) && s.contains(2) && s.contains(3));
    assert!(!s.contains(4), "-0.0 is zero");
    assert!(s.contains(5), "a tiny non-zero still selects");
}

#[test]
fn out_of_range_in_is_an_error_with_the_measured_return_code() {
    // errors.log: `list in 999` -> "observation numbers out of range", rc 198.
    let e = SampleBuilder::new(74)
        .r#in(InRange {
            first: Bound::Abs(999),
            last: Bound::Abs(999),
        })
        .expect_err("999 of 74");
    assert_eq!(e, SampleError::OutOfRange);
    assert_eq!(e.rc(), 198);
    assert_eq!(e.to_string(), "observation numbers out of range");

    // errors.log: `list in 0` -> "'0' invalid observation number", rc 198.
    let z = SampleBuilder::new(74)
        .r#in(InRange {
            first: Bound::Abs(0),
            last: Bound::Last,
        })
        .expect_err("observation 0");
    assert_eq!(z, SampleError::InvalidObsNumber);
    assert_eq!(z.rc(), 198);
    assert_eq!(z.to_string(), "'0' invalid observation number");
}

#[test]
fn in_is_one_based_inclusive_and_resolves_f_and_l() {
    let all = SampleBuilder::new(74)
        .r#in(InRange {
            first: Bound::First,
            last: Bound::Last,
        })
        .expect("f/l")
        .build();
    assert_eq!(all.len(), 74);

    let head = SampleBuilder::new(74)
        .r#in(InRange {
            first: Bound::Abs(1),
            last: Bound::Abs(10),
        })
        .expect("1/10")
        .build();
    assert_eq!(head.len(), 10);
    assert!(head.contains(0) && head.contains(9) && !head.contains(10));

    // `in -10/l` is the last ten observations.
    let tail = SampleBuilder::new(74)
        .r#in(InRange {
            first: Bound::FromEnd(10),
            last: Bound::Last,
        })
        .expect("-10/l")
        .build();
    assert_eq!(tail.len(), 10);
    assert!(tail.contains(64) && !tail.contains(63));
}

#[test]
fn a_reversed_range_selects_nothing_rather_than_erroring() {
    let s = SampleBuilder::new(74)
        .r#in(InRange {
            first: Bound::Abs(10),
            last: Bound::Abs(5),
        })
        .expect("10/5 is empty, not invalid")
        .build();
    assert_eq!(s.len(), 0);
    assert!(s.is_empty());
}

#[test]
fn markout_string_rules_are_the_measured_ones() {
    // `markout t2 x s`        -> t2 == 0 for EVERY observation
    // `markout t3 x s, strok` -> only s=="" and x missing are excluded
    let x = Column::Double(NumCol::from_slice(&[1.0, SYSMISS, 3.0, 4.0]));
    let s = Column::from_row_major(
        StorageType::Str { width: 4 },
        b"ab\0\0cd\0\0\0\0\0\0ef\0\0",
        4,
        0,
        4,
    );

    let without = SampleBuilder::new(4).markout(&[&x, &s], false).build();
    assert_eq!(without.len(), 0, "no strok drops the whole sample");

    let with = SampleBuilder::new(4).markout(&[&x, &s], true).build();
    assert_eq!(with.len(), 2, "row 1 is missing, row 2 is an empty string");
    assert!(with.contains(0) && with.contains(3));
}

#[test]
fn markout_on_numerics_drops_every_tag_not_just_sysmiss() {
    let a = Column::Double(NumCol::from_slice(&[
        1.0,
        SYSMISS,
        missing_f64(1),
        missing_f64(26),
        5.0,
    ]));
    let s = SampleBuilder::new(5).markout(&[&a], false).build();
    assert_eq!(s.len(), 2);
    assert!(s.contains(0) && s.contains(4));
}

#[test]
fn a_contiguous_selection_costs_what_a_range_costs() {
    // The design claim of `04` §5.3, as a counter: one run either way.
    let mut b = SampleBuilder::new(1_000_000);
    b.if_chunk(0, &vec![0.0; 400_000]);
    b.if_chunk(400_000, &vec![1.0; 200_000]);
    b.if_chunk(600_000, &vec![0.0; 400_000]);
    let masked = b.build();
    let ranged = Sample::range(1_000_000, 400_000, 600_000);

    assert_eq!(masked.len(), ranged.len());
    assert!(masked.is_contiguous() && ranged.is_contiguous());
    assert_eq!(masked.runs().count(), 1);
    assert_eq!(masked.runs().next(), ranged.runs().next());
}

#[test]
fn gather_touches_exactly_the_selected_rows() {
    let _guard = COUNTERS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let n = 300_000u64;
    let v: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let col = Column::Double(NumCol::from_slice(&v));
    let s = Sample::range(n, 1_000, 3_000);

    let before = counters().snapshot();
    let mut out = Vec::new();
    col.gather_f64(&s, &mut out);
    let d = counters().snapshot().since(before);

    assert_eq!(out.len(), 2_000);
    assert_eq!(out[0], 1_000.0);
    assert_eq!(
        d.rows_touched, 2_000,
        "a restricted gather must not walk the column"
    );
}

#[test]
fn a_full_scan_touches_each_row_once_and_a_double_never_widens() {
    let _guard = COUNTERS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The `summarize` shape: one double column, no `if`. The budget bullet is a
    // duration, so per ADR-017 what is asserted is the work — every row once,
    // zero rows copied through a widening buffer — and the duration is recorded
    // in `benches/widen.rs` against a flat-slice control.
    let n = 1_000_000u64;
    let v: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let col = Column::Double(NumCol::from_slice(&v));

    let before = counters().snapshot();
    let mut scratch = Vec::new();
    let mut sum = 0.0f64;
    let touched = col.for_each_chunk_f64(&mut scratch, |_, xs| {
        for &x in xs {
            sum += x;
        }
    });
    let d = counters().snapshot().since(before);

    assert_eq!(touched, n);
    assert_eq!(d.rows_touched, n);
    assert_eq!(d.rows_widened, 0, "a Double column is handed out as it is");
    assert!(scratch.is_empty(), "the scratch buffer was never used");
    assert_eq!(sum, (n as f64 - 1.0) * n as f64 / 2.0);
}

#[test]
fn an_int_scan_widens_every_row_exactly_once() {
    let _guard = COUNTERS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let n = 200_000u64;
    let v: Vec<i16> = (0..n).map(|i| (i % 1000) as i16).collect();
    let col = Column::Int(NumCol::from_slice(&v));

    let before = counters().snapshot();
    let mut scratch = Vec::new();
    col.for_each_chunk_f64(&mut scratch, |_, _| {});
    let d = counters().snapshot().since(before);

    assert_eq!(d.rows_touched, n);
    assert_eq!(d.rows_widened, n, "one widening pass, not two");
    // And the scratch buffer is the caller's, reused across chunks: it never
    // grows past one chunk however long the column is.
    assert!(scratch.capacity() <= stratum_data::CHUNK_ROWS);
}

#[test]
fn the_parallel_reduction_is_bit_identical_to_the_sequential_one() {
    let _guard = COUNTERS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // ADR-013 / C35: `map_reduce_f64` folds in ascending chunk index, so it
    // returns the same bits as a single-threaded walk of the same chunks. The
    // values are chosen so a different association order really would differ:
    // one 1.0 among 300 000 values of 1e-17.
    let n = 300_001u64;
    let mut v = vec![1.0f64];
    v.extend(std::iter::repeat_n(1e-17f64, (n - 1) as usize));
    let col = Column::Double(NumCol::from_slice(&v));

    let before = counters().snapshot();
    let parallel = col.map_reduce_f64(
        0.0f64,
        |_, xs| {
            let mut p = 0.0;
            for &x in xs {
                p += x;
            }
            p
        },
        |acc, p| *acc += *p,
    );
    let d = counters().snapshot().since(before);

    let mut scratch = Vec::new();
    let mut sequential = 0.0f64;
    col.for_each_chunk_f64(&mut scratch, |_, xs| {
        let mut p = 0.0;
        for &x in xs {
            p += x;
        }
        sequential += p;
    });

    assert_eq!(parallel.to_bits(), sequential.to_bits());
    assert_eq!(d.rows_touched, n, "every row once, no chunk read twice");
    assert_eq!(d.rows_widened, 0, "a Double column is not copied");

    // And it differs from a naive flat fold, which is why the ordering matters.
    let flat = v.iter().fold(0.0f64, |a, b| a + b);
    assert_ne!(parallel.to_bits(), flat.to_bits());
}

#[test]
fn touse_materialises_and_re_enters() {
    // `04` §5.4: user-written ado does `marksample touse` and passes
    // `if `touse'` down. The round trip is observable, so it is real.
    let sel: Vec<f64> = (0..1000).map(|i| f64::from(u8::from(i % 7 == 0))).collect();
    let s = SampleBuilder::new(1000)
        .r#if(&sel)
        .expect("length matches")
        .build();

    let touse = s.to_touse();
    assert_eq!(touse.storage_type(), StorageType::Byte);
    let back = Sample::from_touse(&touse);
    assert_eq!(back.len(), s.len());
    for row in 0..1000 {
        assert_eq!(back.contains(row), s.contains(row), "row {row}");
    }

    // And it really is a variable: it can be added to a frame and listed.
    let mut f = Frame::new("default");
    f.set_n_obs(1000);
    let idx = f.add_column("touse", touse).expect("fresh name");
    assert_eq!(f.col(idx).expect("column").get_f64(0), Some(1.0));
    assert_eq!(f.col(idx).expect("column").get_f64(1), Some(0.0));
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    /// Iterating by runs reproduces the selected observations exactly. This is
    /// what lets every kernel take slices instead of testing a bit per row.
    #[test]
    fn runs_reproduce_the_selection(bits in prop::collection::vec(any::<bool>(), 0..400)) {
        let n = bits.len() as u64;
        let mut b = BitSet::new(n);
        for (i, &v) in bits.iter().enumerate() {
            b.set(i as u64, v);
        }
        let s = Sample::mask(n, b);

        let want: Vec<u64> = bits
            .iter()
            .enumerate()
            .filter(|(_, &v)| v)
            .map(|(i, _)| i as u64)
            .collect();
        let mut got = Vec::new();
        for Run { start, len } in s.runs() {
            got.extend(start..start + len);
        }
        prop_assert_eq!(&got, &want);
        prop_assert_eq!(s.len(), want.len() as u64);
        for i in 0..n {
            prop_assert_eq!(s.contains(i), bits[i as usize]);
        }
    }

    /// `in` then `if` selects exactly the intersection, whatever the order the
    /// caller applied them in.
    #[test]
    fn in_narrows_if_to_the_intersection(
        lo in 0u64..50,
        span in 0u64..50,
        flags in prop::collection::vec(any::<bool>(), 100..=100),
    ) {
        let n = 100u64;
        let first = lo + 1;
        let last = (lo + span + 1).min(n);
        let sel: Vec<f64> = flags.iter().map(|&f| f64::from(u8::from(f))).collect();

        let s = SampleBuilder::new(n)
            .r#in(InRange { first: Bound::Abs(first), last: Bound::Abs(last) })
            .expect("in range")
            .r#if(&sel)
            .expect("length matches")
            .build();

        for row in 0..n {
            let inside = row >= first - 1 && row < last.max(first - 1);
            prop_assert_eq!(s.contains(row), inside && flags[row as usize], "row {}", row);
        }
    }

    /// A gather writes exactly `sample.len()` values, in ascending order.
    #[test]
    fn gather_writes_the_sample_in_order(flags in prop::collection::vec(any::<bool>(), 1..200)) {
        // Scans bump the process-wide counters, so stay off the toes of the
        // tests that read them.
        let _guard = COUNTERS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let n = flags.len() as u64;
        let v: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let col = Column::Double(NumCol::from_slice(&v));
        let mut b = BitSet::new(n);
        for (i, &f) in flags.iter().enumerate() {
            b.set(i as u64, f);
        }
        let s = Sample::mask(n, b);
        let mut out = Vec::new();
        col.gather_f64(&s, &mut out);

        let want: Vec<f64> = flags
            .iter()
            .enumerate()
            .filter(|(_, &f)| f)
            .map(|(i, _)| i as f64)
            .collect();
        prop_assert_eq!(out, want);
    }
}
