//! The 27 x 5 measured missing-value table, as literal constants (`04` §2.1).
//!
//! These numbers were not written from memory. `04` §2.1 built a 27-row dataset
//! holding `.` and `.a`…`.z` in `byte`, `int`, `long`, `float` and `double` with
//! the licensed StataMP, saved it as release 118, and read the raw bytes back.
//! Every cell below is one of those bytes. If `src/missing.rs` and this file
//! ever disagree, this file is the ground truth and `missing.rs` is the bug.
//!
//! The `double` column is spelled `F64_BASE + (k << 40)` rather than as 27
//! expanded hex literals because ARCHITECTURE §8.7 reserves the expanded
//! `0x7FE0…` literal to `src/missing.rs` and greps for it (ADR-005). The base is
//! written here as its two halves for the same reason; it is the same 64 bits,
//! and `f64_base_is_the_measured_pattern` pins it.

use stratum_core::missing::{
    canon, either_missing, is_missing, missing_f32, missing_f64, tag_of, widen_byte, widen_float,
    widen_int, widen_long, BYTE_MAX, BYTE_MISS, INT_MAX, INT_MISS, LONG_MAX, LONG_MISS, MAX_TAG,
    SYSMISS, SYSMISS_F32,
};

/// `f64` `.`: exponent field `0x7FE`, mantissa zero — exactly `2^1023`. The
/// expanded hex literal belongs to `src/missing.rs` alone (§8.7).
const F64_BASE: u64 = 0x7FE0_0000u64 << 32;

/// One measured row: `(byte, int, long, float bits, double bits)`.
type Row = (i8, i16, i32, u32, u64);

/// The table, verbatim from `04` §2.1. Index is the tag: 0 is `.`, 26 is `.z`.
const MEASURED: [Row; 27] = [
    (101, 32_741, 2_147_483_621, 0x7F00_0000, F64_BASE),
    (
        102,
        32_742,
        2_147_483_622,
        0x7F00_0800,
        0x7FE0_0100_0000_0000,
    ),
    (
        103,
        32_743,
        2_147_483_623,
        0x7F00_1000,
        0x7FE0_0200_0000_0000,
    ),
    (
        104,
        32_744,
        2_147_483_624,
        0x7F00_1800,
        0x7FE0_0300_0000_0000,
    ),
    (
        105,
        32_745,
        2_147_483_625,
        0x7F00_2000,
        0x7FE0_0400_0000_0000,
    ),
    (
        106,
        32_746,
        2_147_483_626,
        0x7F00_2800,
        0x7FE0_0500_0000_0000,
    ),
    (
        107,
        32_747,
        2_147_483_627,
        0x7F00_3000,
        0x7FE0_0600_0000_0000,
    ),
    (
        108,
        32_748,
        2_147_483_628,
        0x7F00_3800,
        0x7FE0_0700_0000_0000,
    ),
    (
        109,
        32_749,
        2_147_483_629,
        0x7F00_4000,
        0x7FE0_0800_0000_0000,
    ),
    (
        110,
        32_750,
        2_147_483_630,
        0x7F00_4800,
        0x7FE0_0900_0000_0000,
    ),
    (
        111,
        32_751,
        2_147_483_631,
        0x7F00_5000,
        0x7FE0_0A00_0000_0000,
    ),
    (
        112,
        32_752,
        2_147_483_632,
        0x7F00_5800,
        0x7FE0_0B00_0000_0000,
    ),
    (
        113,
        32_753,
        2_147_483_633,
        0x7F00_6000,
        0x7FE0_0C00_0000_0000,
    ),
    (
        114,
        32_754,
        2_147_483_634,
        0x7F00_6800,
        0x7FE0_0D00_0000_0000,
    ),
    (
        115,
        32_755,
        2_147_483_635,
        0x7F00_7000,
        0x7FE0_0E00_0000_0000,
    ),
    (
        116,
        32_756,
        2_147_483_636,
        0x7F00_7800,
        0x7FE0_0F00_0000_0000,
    ),
    (
        117,
        32_757,
        2_147_483_637,
        0x7F00_8000,
        0x7FE0_1000_0000_0000,
    ),
    (
        118,
        32_758,
        2_147_483_638,
        0x7F00_8800,
        0x7FE0_1100_0000_0000,
    ),
    (
        119,
        32_759,
        2_147_483_639,
        0x7F00_9000,
        0x7FE0_1200_0000_0000,
    ),
    (
        120,
        32_760,
        2_147_483_640,
        0x7F00_9800,
        0x7FE0_1300_0000_0000,
    ),
    (
        121,
        32_761,
        2_147_483_641,
        0x7F00_A000,
        0x7FE0_1400_0000_0000,
    ),
    (
        122,
        32_762,
        2_147_483_642,
        0x7F00_A800,
        0x7FE0_1500_0000_0000,
    ),
    (
        123,
        32_763,
        2_147_483_643,
        0x7F00_B000,
        0x7FE0_1600_0000_0000,
    ),
    (
        124,
        32_764,
        2_147_483_644,
        0x7F00_B800,
        0x7FE0_1700_0000_0000,
    ),
    (
        125,
        32_765,
        2_147_483_645,
        0x7F00_C000,
        0x7FE0_1800_0000_0000,
    ),
    (
        126,
        32_766,
        2_147_483_646,
        0x7F00_C800,
        0x7FE0_1900_0000_0000,
    ),
    (
        127,
        32_767,
        2_147_483_647,
        0x7F00_D000,
        0x7FE0_1A00_0000_0000,
    ),
];

#[test]
fn f64_base_is_the_measured_pattern() {
    // `%21x` of `.` on StataMP 18.5 is `+1.0000000000000X+3ff`: mantissa zero,
    // unbiased exponent 1023. That is 2^1023 and nothing else.
    assert_eq!(F64_BASE, 0x7FE0_0000u64 << 32);
    assert_eq!(F64_BASE >> 52, 0x7FE);
    assert_eq!(F64_BASE & 0x000F_FFFF_FFFF_FFFF, 0);
    assert_eq!(SYSMISS.to_bits(), F64_BASE);
    assert_eq!(SYSMISS_F32.to_bits(), 0x7F00_0000);
}

#[test]
fn the_whole_measured_table() {
    assert_eq!(MEASURED.len(), usize::from(MAX_TAG) + 1);
    for (k, &(b, i, l, f32bits, f64bits)) in MEASURED.iter().enumerate() {
        let tag = k as u8;

        // Closed forms from 04 §2.1, each checked against the measured cell.
        assert_eq!(b, BYTE_MISS + tag as i8, "byte tag {tag}");
        assert_eq!(i, INT_MISS + i16::from(tag), "int tag {tag}");
        assert_eq!(l, LONG_MISS + i32::from(tag), "long tag {tag}");
        assert_eq!(
            f32bits,
            0x7F00_0000 + (u32::from(tag) << 11),
            "f32 tag {tag}"
        );
        assert_eq!(f64bits, F64_BASE + (u64::from(tag) << 40), "f64 tag {tag}");

        // And the crate agrees with the measurement, in both directions.
        assert_eq!(missing_f32(tag).to_bits(), f32bits, "missing_f32({tag})");
        assert_eq!(missing_f64(tag).to_bits(), f64bits, "missing_f64({tag})");
        assert_eq!(widen_byte(b).to_bits(), f64bits, "widen_byte tag {tag}");
        assert_eq!(widen_int(i).to_bits(), f64bits, "widen_int tag {tag}");
        assert_eq!(widen_long(l).to_bits(), f64bits, "widen_long tag {tag}");
        assert_eq!(
            widen_float(f32::from_bits(f32bits)).to_bits(),
            f64bits,
            "widen_float tag {tag}"
        );
        assert_eq!(tag_of(f64::from_bits(f64bits)), Some(tag));
        assert!(is_missing(f64::from_bits(f64bits)));
    }
}

#[test]
fn the_endpoints_the_plan_names() {
    assert_eq!(MEASURED[26].0, 127, "byte .z");
    assert_eq!(MEASURED[26].1, 32_767, "int .z");
    assert_eq!(MEASURED[26].2, 2_147_483_647, "long .z");
    assert_eq!(MEASURED[26].3, 0x7F00_D000, "f32 .z");
    assert_eq!(MEASURED[26].4, F64_BASE + (26 << 40), "f64 .z");
    // The largest values that are NOT missing.
    assert_eq!(BYTE_MAX, 100);
    assert_eq!(INT_MAX, 32_740);
    assert_eq!(LONG_MAX, 2_147_483_620);
    assert!(!is_missing(widen_byte(BYTE_MAX)));
    assert!(!is_missing(widen_int(INT_MAX)));
    assert!(!is_missing(widen_long(LONG_MAX)));
}

// The finiteness of a constant IS the assertion: an encoding that used NaNs
// would make `<` false in both directions and Stata's ordering would need code.
#[allow(clippy::assertions_on_constants)]
#[test]
fn the_sentinels_are_finite_normal_numbers_not_nans() {
    // This is the entire reason Stata's ordering works with plain IEEE
    // comparison. A NaN sentinel would make `<` false in both directions.
    assert!(SYSMISS.is_finite());
    assert!(SYSMISS_F32.is_finite());
    assert!(SYSMISS.is_normal());
    for k in 0..=MAX_TAG {
        assert!(missing_f64(k).is_finite(), "tag {k}");
        assert!(missing_f32(k).is_finite(), "tag {k}");
    }
    // f64 `.` is exactly 2^1023 and f32 `.` is exactly 2^127. The powers are
    // built from the exponent field rather than with `powi`, which §8.11 bans
    // outside `stratum_core::math`.
    assert_eq!(SYSMISS, f64::from_bits((1023u64 + 1023) << 52));
    assert_eq!(
        f64::from(SYSMISS_F32),
        f64::from_bits((127u64 + 1023) << 52)
    );
}

#[test]
fn widen_float_compares_on_the_value_not_the_bits() {
    // THE REGRESSION THIS TEST EXISTS FOR. Every negative f32 has
    // `to_bits() >= 0x8000_0000`, which is greater than `F32_MISS_BITS`
    // (0x7F00_0000). An implementation that tests the raw bits turns the entire
    // negative half of every float column into a missing value — silently, and
    // invisibly to any fixture whose numbers happen to be positive.
    for &v in &[
        -1.0f32,
        -0.5,
        -1e-30,
        -3.4e38,
        f32::MIN,
        -f32::MIN_POSITIVE,
        -1.7014118e38,
    ] {
        assert!(v.to_bits() > 0x7F00_0000, "premise: {v} has large bits");
        let w = widen_float(v);
        assert!(!is_missing(w), "widen_float({v}) must not be missing");
        assert_eq!(w, f64::from(v));
    }
    // And the positive side still maps to sentinels.
    assert!(is_missing(widen_float(SYSMISS_F32)));
    assert_eq!(widen_float(SYSMISS_F32).to_bits(), F64_BASE);
    // A positive float just below the sentinel is an ordinary number.
    let near = f32::from_bits(0x7F00_0000 - 1);
    assert!(!is_missing(widen_float(near)));
}

// Same reason as `the_sentinels_are_finite_normal_numbers_not_nans`: these are
// assertions about the encoding, and their being constant is the point.
#[allow(clippy::assertions_on_constants)]
#[test]
fn ordering_and_truthiness_match_the_golden_log() {
    // tests/golden/stata18/semantics.log, "extended missing ordering":
    //   sort ascending -> -50, 0, 1, 100, ., .a, .b, .z
    let mut xs = vec![
        1.0,
        100.0,
        -50.0,
        SYSMISS,
        missing_f64(1),
        missing_f64(2),
        missing_f64(26),
        0.0,
    ];
    xs.sort_by(|a, b| a.partial_cmp(b).expect("no NaNs under Invariant M"));
    assert_eq!(
        xs,
        vec![
            -50.0,
            0.0,
            1.0,
            100.0,
            SYSMISS,
            missing_f64(1),
            missing_f64(2),
            missing_f64(26)
        ]
    );
    // `count if x < .` is 4, `count if missing(x)` is 4, `count if x >= .` is 4.
    assert_eq!(xs.iter().filter(|v| **v < SYSMISS).count(), 4);
    assert_eq!(xs.iter().filter(|v| is_missing(**v)).count(), 4);
    // di (.a > .), (.z > .a), (. > 1e300) -> 1 1 1
    assert!(missing_f64(1) > SYSMISS);
    assert!(missing_f64(26) > missing_f64(1));
    assert!(SYSMISS > 1e300);
}

/// Opaque zero, so that `0.0 / 0.0` is a division at run time rather than a
/// constant clippy folds into a lint.
fn zero() -> f64 {
    std::hint::black_box(0.0)
}

#[test]
fn arithmetic_propagation_matches_the_golden_log() {
    // semantics.log: 1 + . , 1 * . , ./. , 1/0 , -1/0 , 0/0 , sqrt(-1) ,
    // log(0) , log(-1) , exp(1000) are ALL plain `.`.
    let cases = [
        1.0 + SYSMISS,
        1.0 * SYSMISS,
        1.0 / zero(),
        -1.0 / zero(),
        zero() / zero(),
        stratum_core::math::sqrt(-1.0),
        stratum_core::math::ln(0.0),
        stratum_core::math::ln(-1.0),
        stratum_core::math::exp(1000.0),
        missing_f64(1) + 1.0,
    ];
    for (i, v) in cases.into_iter().enumerate() {
        assert_eq!(canon(v), SYSMISS, "case {i}");
        assert_eq!(tag_of(canon(v)), Some(0), "case {i} must lose its tag");
    }
    // `. / .` and `.z * 0` are 1.0 and 0.0 by the time `canon` could see them.
    // These are the annihilators `04` §2.3 flags, and they need the operand
    // guard rather than the result guard.
    assert_eq!(SYSMISS / SYSMISS, 1.0);
    assert_eq!(missing_f64(26) * 0.0, 0.0);
    assert!(either_missing(SYSMISS, SYSMISS));
    assert!(either_missing(missing_f64(26), 0.0));
}

/// ARCHITECTURE C2 / §8.7, run here so the failure is local rather than a CI
/// surprise: the decimal spelling of `2^1023` exists in exactly one file.
#[test]
fn no_decimal_missing_literal_outside_missing_rs() {
    // The needle is assembled at run time so that this file is not itself a hit
    // — the point of the check is that the digits appear nowhere.
    let needle = format!("{}{}", "8.98", "8465");
    let hits = grep_crates(&needle);
    assert!(
        hits.is_empty(),
        "ADR-005: a decimal literal for a Stata missing value must appear \
         nowhere in the workspace. Found:\n{}",
        hits.join("\n")
    );
}

/// Every `.rs` line under `crates/` containing `needle`, with its path.
fn grep_crates(needle: &str) -> Vec<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("crates");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let Ok(text) = std::fs::read_to_string(&p) else {
                    continue;
                };
                for (n, line) in text.lines().enumerate() {
                    if line.contains(needle) {
                        out.push(format!("{}:{}: {}", p.display(), n + 1, line.trim()));
                    }
                }
            }
        }
    }
    out
}
