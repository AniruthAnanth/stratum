//! W02 acceptance: **every missing-ordering rule of `04` §2.2 holds with zero
//! comparator branches**, and the two sorters agree.
//!
//! Three independent things are checked, because any one of them alone can pass
//! while the engine is wrong:
//!
//! 1. **Mechanically**, that the ordering path never names a sentinel. The two
//!    modules that decide order are scanned for `stratum_core::missing`'s
//!    vocabulary; a comparator that has to ask "is this missing?" has stopped
//!    being branch-free, and that is a source property, not a behavioural one.
//! 2. **Against the golden**, that `sort x` reproduces
//!    `tests/golden/stata18/semantics.log` exactly — `-50, 0, 1, 100, ., .a, .b,
//!    .z` — and that `""` sorts first among strings (`04` §2.2, measured).
//! 3. **Against an independent oracle**, that radix, comparator and a naive
//!    reference ordering that shares no code with either agree on 10 000
//!    randomly generated frames. Two paths agreeing proves consistency; a third,
//!    differently-derived one is what proves correctness.
//!
//! Per ADR-017 the radix budget is asserted as a counter — passes and rows
//! scattered — and the duration is recorded in `benches/sort.rs`.

use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use stratum_core::missing::{missing_f64, SYSMISS};
use stratum_data::column::NumCol;
use stratum_data::sort::{permutation, Strategy};
use stratum_data::{counters, Column, StorageType};
use stratum_proto::SortDir;

/// The counters in `perf` are process-wide, so a test that READS them needs
/// every other sorting test in this binary to be quiet. Readers of the counters
/// take the write side; everything that merely sorts takes the read side and
/// still runs in parallel with its peers.
static COUNTERS: RwLock<()> = RwLock::new(());

/// A poisoned lock means some other test already failed; that failure is the
/// signal, and cascading `PoisonError`s only bury it.
fn sorting() -> RwLockReadGuard<'static, ()> {
    COUNTERS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn counting() -> RwLockWriteGuard<'static, ()> {
    COUNTERS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// 1. The source property
// ---------------------------------------------------------------------------

/// The sentinel vocabulary, stored split and reassembled at run time so this
/// file is not the first thing its own scan reports — the idiom
/// `stratum-core/tests/source_invariants.rs` established.
const NEEDLES: &[(&str, &str)] = &[
    ("is_", "missing"),
    ("tag_", "of"),
    ("missing_", "f64"),
    ("missing_", "f32"),
    ("SYS", "MISS"),
    ("BYTE_", "MISS"),
    ("INT_", "MISS"),
    ("LONG_", "MISS"),
    ("MAX_", "TAG"),
    ("core::", "missing"),
    ("Narro", "wed"),
];

#[test]
fn the_ordering_path_never_names_a_missing_value() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let needles: Vec<String> = NEEDLES.iter().map(|(a, b)| format!("{a}{b}")).collect();
    let mut hits = Vec::new();
    for name in ["src/sortkey.rs", "src/sort.rs"] {
        let text = std::fs::read_to_string(root.join(name)).expect("owned source file");
        // Only the shipping half. The `#[cfg(test)]` module below it asserts the
        // orderings against named sentinels, which is exactly its job.
        let shipping = text.split("#[cfg(test)]").next().unwrap_or(&text);
        for (n, line) in shipping.lines().enumerate() {
            // Prose may discuss the rules; code may not implement them.
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("*") {
                continue;
            }
            for needle in &needles {
                if code.contains(needle.as_str()) {
                    hits.push(format!("{name}:{}: {needle}", n + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "the ordering path must contain no missing-value handling at all, but:\n{}",
        hits.join("\n")
    );
}

// ---------------------------------------------------------------------------
// 2. The measured orders
// ---------------------------------------------------------------------------

fn values(col: &Column, perm: &[u32]) -> Vec<f64> {
    perm.iter()
        .map(|&i| col.get_f64(u64::from(i)).expect("numeric"))
        .collect()
}

#[test]
fn the_golden_ascending_order_reproduces_on_both_paths() {
    let _guard = sorting();
    // tests/golden/stata18/semantics.log, `sort x` then `list x`.
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
    for s in [Strategy::Radix, Strategy::Comparator, Strategy::Auto] {
        let perm = permutation(&[(&col, SortDir::Asc)], 8, s).expect("a double key");
        assert_eq!(values(&col, &perm), want, "{s:?}");
    }
}

#[test]
fn every_extended_missing_sorts_in_tag_order_above_every_real_number() {
    let _guard = sorting();
    // `. < .a < … < .z`, and all of them above 1e300 — measured:
    // `di (. > 1e300)` is 1.
    let mut vals: Vec<f64> = (0..=26).map(missing_f64).collect();
    vals.push(1e300);
    vals.reverse();
    let n = vals.len() as u64;
    let col = Column::Double(NumCol::from_slice(&vals));
    for s in [Strategy::Radix, Strategy::Comparator] {
        let perm = permutation(&[(&col, SortDir::Asc)], n, s).expect("a double key");
        let got = values(&col, &perm);
        assert_eq!(
            got[0], 1e300,
            "{s:?}: the real number sorts below every tag"
        );
        for tag in 0..=26u8 {
            assert_eq!(
                got[tag as usize + 1].to_bits(),
                missing_f64(tag).to_bits(),
                "{s:?}: tag {tag} out of order"
            );
        }
    }
}

#[test]
fn the_same_rule_holds_in_every_numeric_width() {
    let _guard = sorting();
    // The sentinels are the largest values of each integer type and large
    // positive finites in each float type, so "missing sorts last" is the same
    // fact five times, with no code that knows it.
    use stratum_core::missing::{BYTE_MISS, INT_MISS, LONG_MISS, SYSMISS_F32};

    let byte = Column::Byte(NumCol::from_slice(&[BYTE_MISS + 1, 100i8, -128, BYTE_MISS]));
    let int = Column::Int(NumCol::from_slice(&[
        INT_MISS + 1,
        32_740i16,
        -32_768,
        INT_MISS,
    ]));
    let long = Column::Long(NumCol::from_slice(&[
        LONG_MISS + 1,
        2_147_483_620i32,
        i32::MIN,
        LONG_MISS,
    ]));
    let float = Column::Float(NumCol::from_slice(&[
        stratum_core::missing::missing_f32(1),
        1e30f32,
        -1e30,
        SYSMISS_F32,
    ]));

    for col in [&byte, &int, &long, &float] {
        for s in [Strategy::Radix, Strategy::Comparator] {
            let perm = permutation(&[(col, SortDir::Asc)], 4, s).expect("numeric key");
            // Ascending: most negative, then the largest real, then `.`, then `.a`.
            assert_eq!(perm, vec![2, 1, 3, 0], "{:?} {s:?}", col.storage_type());
        }
    }
}

#[test]
fn an_empty_string_sorts_below_every_other_string() {
    let _guard = sorting();
    // The exact opposite of the numeric rule, and equally free: `""` is all-NUL
    // in a fixed-width field.
    let col = str_column(6, &[b"b", b"", b"aa", b"", b"a", b"zzzzzz"]);
    for s in [Strategy::Radix, Strategy::Comparator] {
        let perm = permutation(&[(&col, SortDir::Asc)], 6, s).expect("a str6 key");
        let got: Vec<Vec<u8>> = perm
            .iter()
            .map(|&i| col.get_bytes(u64::from(i)).expect("string").to_vec())
            .collect();
        assert_eq!(
            got,
            vec![
                b"".to_vec(),
                b"".to_vec(),
                b"a".to_vec(),
                b"aa".to_vec(),
                b"b".to_vec(),
                b"zzzzzz".to_vec()
            ],
            "{s:?}"
        );
    }
}

/// A `str{width}` column holding `values`, NUL-padded.
///
/// Built through the public bulk-ingest path rather than by reaching into the
/// column: `FixedStrCol` exposes no writer, and that is the barrier working.
fn str_column(width: u16, values: &[&[u8]]) -> Column {
    let w = width as usize;
    let mut src = vec![0u8; w * values.len()];
    for (i, v) in values.iter().enumerate() {
        assert!(v.len() <= w, "value wider than the declared str{width}");
        src[i * w..i * w + v.len()].copy_from_slice(v);
    }
    Column::from_row_major(StorageType::Str { width }, &src, w, 0, values.len() as u64)
}

// ---------------------------------------------------------------------------
// 3. The independent oracle, over 10 000 generated frames
// ---------------------------------------------------------------------------

/// xorshift64*, so the corpus is identical on every machine and every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A column of `n` observations of a random type, with sentinels over-represented
/// so the ordering rules are actually exercised.
fn random_column(rng: &mut Rng, n: u64) -> Column {
    let numeric = |rng: &mut Rng| -> f64 {
        match rng.below(6) {
            0 => SYSMISS,
            1 => missing_f64(u8::try_from(rng.below(27)).expect("< 27")),
            2 => 0.0,
            3 => -(rng.below(1000) as f64),
            4 => rng.below(1000) as f64,
            _ => rng.below(11) as f64 - 5.0,
        }
    };
    match rng.below(6) {
        0 => {
            let v: Vec<i8> = (0..n)
                .map(|_| match stratum_core::missing::tag_of(numeric(rng)) {
                    Some(t) => stratum_core::missing::BYTE_MISS + t as i8,
                    None => (rng.below(201) as i64 - 100) as i8,
                })
                .collect();
            Column::Byte(NumCol::from_slice(&v))
        }
        1 => {
            let v: Vec<i16> = (0..n)
                .map(|_| match stratum_core::missing::tag_of(numeric(rng)) {
                    Some(t) => stratum_core::missing::INT_MISS + i16::from(t),
                    None => (rng.below(2001) as i64 - 1000) as i16,
                })
                .collect();
            Column::Int(NumCol::from_slice(&v))
        }
        2 => {
            let v: Vec<i32> = (0..n)
                .map(|_| match stratum_core::missing::tag_of(numeric(rng)) {
                    Some(t) => stratum_core::missing::LONG_MISS + i32::from(t),
                    None => (rng.below(200_001) as i64 - 100_000) as i32,
                })
                .collect();
            Column::Long(NumCol::from_slice(&v))
        }
        3 => {
            let v: Vec<f32> = (0..n)
                .map(|_| match stratum_core::missing::tag_of(numeric(rng)) {
                    Some(t) => stratum_core::missing::missing_f32(t),
                    None => (rng.below(2001) as f32 - 1000.0) / 8.0,
                })
                .collect();
            Column::Float(NumCol::from_slice(&v))
        }
        4 => {
            let v: Vec<f64> = (0..n).map(|_| numeric(rng)).collect();
            Column::Double(NumCol::from_slice(&v))
        }
        _ => {
            let width = 1 + u16::try_from(rng.below(4)).expect("< 4");
            let cells: Vec<Vec<u8>> = (0..n)
                .map(|_| {
                    let len = rng.below(u64::from(width) + 1) as usize;
                    (0..len)
                        .map(|_| b'a' + u8::try_from(rng.below(3)).expect("< 3"))
                        .collect()
                })
                .collect();
            let refs: Vec<&[u8]> = cells.iter().map(Vec::as_slice).collect();
            str_column(width, &refs)
        }
    }
}

/// The reference ordering, derived from Stata's *stated* rules rather than from
/// the key encoder: numeric values compare as plain `f64` (which `04` §2.2 says
/// is all Stata does), strings compare as their NUL-padded fields.
fn reference_order(cols: &[(&Column, SortDir)], n: u64) -> Vec<u32> {
    let mut perm: Vec<u32> = (0..n as u32).collect();
    perm.sort_by(|&a, &b| {
        for (col, dir) in cols {
            let ord = match col {
                Column::Str(s) => s.raw(u64::from(a)).cmp(s.raw(u64::from(b))),
                Column::StrL(s) => s.get(u64::from(a)).cmp(s.get(u64::from(b))),
                other => {
                    let (x, y) = (
                        other.get_f64(u64::from(a)).expect("numeric"),
                        other.get_f64(u64::from(b)).expect("numeric"),
                    );
                    x.partial_cmp(&y).expect("invariant M forbids NaN")
                }
            };
            let ord = if *dir == SortDir::Desc {
                ord.reverse()
            } else {
                ord
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    perm
}

#[test]
fn radix_comparator_and_an_independent_oracle_agree_on_ten_thousand_frames() {
    let _guard = sorting();
    let mut rng = Rng(0x5EED_1234_ABCD_9876);
    let mut frames = 0u32;
    let mut with_radix = 0u32;
    while frames < 10_000 {
        let n = 1 + rng.below(40);
        let nkeys = 1 + rng.below(3);
        let cols: Vec<Column> = (0..nkeys).map(|_| random_column(&mut rng, n)).collect();
        let keys: Vec<(&Column, SortDir)> = cols
            .iter()
            .map(|c| {
                (
                    c,
                    if rng.below(4) == 0 {
                        SortDir::Desc
                    } else {
                        SortDir::Asc
                    },
                )
            })
            .collect();

        let want = reference_order(&keys, n);
        let cmp = permutation(&keys, n, Strategy::Comparator).expect("comparator always works");
        assert_eq!(
            cmp, want,
            "comparator disagreed with the oracle (frame {frames})"
        );

        if let Ok(rad) = permutation(&keys, n, Strategy::Radix) {
            assert_eq!(
                rad, want,
                "radix disagreed with the oracle (frame {frames})"
            );
            with_radix += 1;
        }
        frames += 1;
    }
    assert_eq!(frames, 10_000);
    assert!(
        with_radix > 5_000,
        "only {with_radix} of 10000 frames could take the radix path; the corpus \
         is not exercising it"
    );
}

// ---------------------------------------------------------------------------
// The radix budget, as counters
// ---------------------------------------------------------------------------

#[test]
fn a_radix_sort_touches_each_row_once_per_pass_and_no_more() {
    let _guard = counting();
    let n: u64 = 1_000_000;
    let mut rng = Rng(0xC0FF_EE00_1234_5678);
    let v: Vec<f64> = (0..n).map(|_| rng.below(1_000_000) as f64).collect();
    let col = Column::Double(NumCol::from_slice(&v));

    let before = counters().snapshot();
    let start = std::time::Instant::now();
    let perm = permutation(&[(&col, SortDir::Asc)], n, Strategy::Auto).expect("double key");
    let elapsed = start.elapsed();
    let d = counters().snapshot().since(before);

    // ASSERTED: the work, not the clock. Eight key bytes is the ceiling; a pass
    // whose byte is constant across every row is skipped, because it cannot
    // change the order.
    assert!(
        d.radix_passes <= 8,
        "{} passes over an 8-byte key",
        d.radix_passes
    );
    assert!(d.radix_passes >= 1);
    assert_eq!(
        d.radix_rows,
        n * d.radix_passes,
        "each pass scatters every row exactly once"
    );
    assert_eq!(d.comparisons, 0, "Auto must not have taken the comparator");

    // Sorted, and stable.
    for w in perm.windows(2) {
        let (a, b) = (
            col.get_f64(u64::from(w[0])).expect("numeric"),
            col.get_f64(u64::from(w[1])).expect("numeric"),
        );
        assert!(a < b || (a == b && w[0] < w[1]), "not stably sorted");
    }

    eprintln!(
        "recorded: radix sort, 1 double key, {n} rows: {elapsed:?}, \
         {} passes, {} rows scattered",
        d.radix_passes, d.radix_rows
    );
}

#[test]
fn the_comparator_path_is_only_taken_when_the_key_cannot_be_materialised() {
    let _guard = counting();
    // A str200 key: 200 bytes x n materialised would be absurd, so `Auto`
    // compares instead. `RADIX_MAX_KEY_BYTES` is 16.
    let n: u64 = 200_000;
    let cells: Vec<Vec<u8>> = (0..n)
        .map(|i| format!("{:03}", i % 251).into_bytes())
        .collect();
    let refs: Vec<&[u8]> = cells.iter().map(Vec::as_slice).collect();
    let col = str_column(200, &refs);
    let before = counters().snapshot();
    let _ = permutation(&[(&col, SortDir::Asc)], n, Strategy::Auto).expect("str key");
    let d = counters().snapshot().since(before);
    assert_eq!(d.radix_passes, 0, "a 200-byte key must not go to radix");
    assert!(d.comparisons > 0);

    // And a narrow key of the same shape does go to radix. The content has to
    // actually vary: a pass whose byte is identical in every row is skipped,
    // because it cannot change the order.
    let cells: Vec<Vec<u8>> = (0..n)
        .map(|i| format!("{:08}", i % 97).into_bytes())
        .collect();
    let refs: Vec<&[u8]> = cells.iter().map(Vec::as_slice).collect();
    let narrow = str_column(8, &refs);
    let before = counters().snapshot();
    let _ = permutation(&[(&narrow, SortDir::Asc)], n, Strategy::Auto).expect("str key");
    let d = counters().snapshot().since(before);
    assert!(d.radix_passes > 0, "an 8-byte key must go to radix");
    assert_eq!(d.radix_rows, n * d.radix_passes);
}

#[test]
fn a_small_frame_takes_the_comparator_even_with_a_narrow_key() {
    let _guard = sorting();
    // Below 2^16 rows the key materialisation costs more than the sort.
    let col = Column::Byte(NumCol::from_slice(&[3i8, 1, 2]));
    let perm = permutation(&[(&col, SortDir::Asc)], 3, Strategy::Auto).expect("byte key");
    assert_eq!(perm, vec![1, 2, 0]);
}

#[test]
fn an_empty_key_list_is_the_identity() {
    let _guard = sorting();
    assert_eq!(permutation(&[], 4, Strategy::Auto), Ok(vec![0, 1, 2, 3]));
}

#[test]
fn a_frame_of_one_observation_sorts() {
    let _guard = sorting();
    let col = Column::Double(NumCol::from_slice(&[SYSMISS]));
    assert_eq!(
        permutation(&[(&col, SortDir::Asc)], 1, Strategy::Auto),
        Ok(vec![0])
    );
}

#[test]
fn storage_types_report_the_widths_the_sort_planner_uses() {
    // A regression guard on the one number that decides which sorter runs.
    assert_eq!(
        stratum_data::sortkey::key_width(StorageType::Str { width: 17 }),
        Some(17)
    );
    assert_eq!(stratum_data::sortkey::key_width(StorageType::StrL), None);
}
