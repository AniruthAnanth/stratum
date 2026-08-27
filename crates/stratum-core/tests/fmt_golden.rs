//! The published `%g` goldens, the `esig` clamp (A28), and the C12 identity.
//!
//! `tests/fmt_corpus.rs` replays ~10^5 measured cells; this file pins the
//! fourteen that were published in the design documents plus every case the
//! plan names by hand, so that a regression names itself instead of arriving as
//! "4557 of 99135 cells disagree".

use proptest::prelude::*;
use stratum_core::fmt::{fmt_e, fmt_f, fmt_fc, fmt_g, fmt_g5, fmt_macro, FormatKind, StataFormat};
use stratum_core::missing::{missing_f64, SYSMISS};

/// `04` §8 — `di %fmt value` on StataMP 18.5, leading spaces significant.
#[test]
fn design_04_section_8() {
    assert_eq!(fmt_g(1234567.891, 9), "  1234568");
    assert_eq!(fmt_g(0.000012345, 9), " .0000123");
    assert_eq!(fmt_g(123456789.0, 9), " 1.23e+08");
    assert_eq!(fmt_g(12345678901.0, 9), " 1.23e+10");
    assert_eq!(fmt_g(1.5, 10), "       1.5");
    // The `c` variant falls back to exponential when the grouped form will not
    // fit the width. This is `%8.0gc`, not `%8.0fc`: the two differ.
    assert_eq!(
        StataFormat::parse("%8.0gc").unwrap().format_f64(1234567.0),
        " 1.2e+06"
    );
}

/// `05` §4 — the four-width table for five values.
#[test]
fn design_05_section_4() {
    let cases: &[(f64, [&str; 4])] = &[
        (
            317252881.2439711,
            ["3.17e+08", "3.173e+08", "317252881", "317252881.2"],
        ),
        (
            4540178.784,
            ["4540179", "4540178.8", "4540178.78", "4540178.784"],
        ),
        (
            8699525.974,
            ["8699526", "8699526", "8699525.97", "8699525.974"],
        ),
    ];
    for &(v, expect) in cases {
        for (i, w) in (9usize..=12).enumerate() {
            assert_eq!(fmt_g(v, w).trim(), expect[i], "value {v}, width {w}");
        }
    }
    // The single-width rows of the same table.
    assert_eq!(fmt_g(2997197234.5, 10).trim(), "2.997e+09");
    assert_eq!(fmt_g(2997197234.5, 11).trim(), "2.9972e+09");
    assert_eq!(fmt_g(2997197234.5, 12).trim(), "2997197235");
    assert_eq!(fmt_g(2130.769528589715, 9).trim(), "2130.77");
    assert_eq!(fmt_g(0.63074906, 9).trim(), ".6307491");
    assert_eq!(fmt_g(-5853.6957, 9).trim(), "-5853.696");
    assert_eq!(fmt_g(0.158902485820707, 9).trim(), ".1589025");
}

/// `05` §4's `fmt_g5`, which is Root MSE's format and nothing else in v1.
#[test]
fn five_significant_digits() {
    assert_eq!(fmt_g5(2130.7695, 9).trim(), "2130.8");
    assert_eq!(fmt_g5(0.158902, 9).trim(), ".1589");
    assert_eq!(fmt_g5(2513.9942, 9).trim(), "2514");
}

/// ARCHITECTURE C12: the two layers are one function, asserted rather than
/// assumed. Three `%g` implementations were planned and this is what collapsed
/// them.
#[test]
fn fmt_g_is_the_general_format() {
    let values = [
        0.0f64,
        1.0,
        -1.0,
        0.5,
        1.5,
        1234567.891,
        -1234567.891,
        0.000012345,
        317252881.2439711,
        1e15,
        1e-15,
        1e300,
        1e-300,
        SYSMISS,
        missing_f64(1),
        missing_f64(26),
    ];
    for w in 1usize..=40 {
        let spec = format!("%{w}.0g");
        let parsed = StataFormat::parse(&spec).expect("legal format");
        for &v in &values {
            assert_eq!(fmt_g(v, w), parsed.format_f64(v), "{spec} of {v}");
        }
    }
}

/// A28 — `esig = w - 6` requests **-1** significant digits at `w = 6` and
/// panics inside a `format!` that takes `esig - 1`. The floor is measured, and
/// it is not the `max(1)` the audit proposed: a two-digit exponent keeps TWO
/// significant digits at every width, because the scientific body is never
/// shorter than seven characters.
#[test]
fn esig_is_clamped_at_every_legal_width() {
    // A value that is scientific at every one of these widths.
    let x = 1234567.891;
    let expect = [
        (1usize, " 1.2e+06"),
        (2, " 1.2e+06"),
        (3, " 1.2e+06"),
        (4, " 1.2e+06"),
        (5, " 1.2e+06"),
        (6, " 1.2e+06"),
        (7, " 1.2e+06"),
        (8, " 1.2e+06"),
    ];
    for (w, want) in expect {
        assert_eq!(fmt_g(x, w), want, "w = {w}");
    }
    // Three-digit exponents floor one digit lower, at a bare `1.e+300`.
    for w in 1usize..=8 {
        assert_eq!(fmt_g(1e300, w), " 1.e+300", "w = {w}");
    }
    assert_eq!(fmt_g(1e300, 9), " 1.0e+300");
    // And nothing panics anywhere in the legal grammar.
    for w in 1u16..=244 {
        for d in 0..w {
            let f = StataFormat {
                kind: FormatKind::General,
                width: w,
                prec: u8::try_from(d.min(99)).expect("prec fits u8"),
                left: false,
                zero_pad: false,
                commas: false,
            };
            let _ = f.format_f64(x);
        }
    }
}

/// A28's literal `esig`, evaluated side by side with the measurement.
///
/// `esig_is_clamped_at_every_legal_width` pins what Stata does; `fmt_corpus.rs`
/// scores A28's rule over the whole corpus and pins that it is wrong on 829 603
/// of 1 552 057 scientific cells. This is the same contradiction in six lines,
/// so that it is on the record without a 75 MB file, and so that the two halves
/// of the bullet are visibly separated: the SAFETY half of A28 holds, the
/// ARITHMETIC half does not.
#[test]
fn a28_esig_formula_contradicts_the_measurement() {
    // A28: `let esig = (w as i32 - 6).max(1) as usize;` — significant digits,
    // hence `esig - 1` decimals, with no dependence on the exponent's width.
    let a28_decimals = |w: usize| (w as i32 - 6).max(1) as usize - 1;

    // Two-digit exponent. A28 asks for none, Stata prints one, at every width
    // through 7. 62 558 corpus cells at `%6.0g` alone.
    for w in 1usize..=7 {
        assert_eq!(a28_decimals(w), 0, "A28 at w = {w}");
        assert_eq!(fmt_g(1234567.891, w), " 1.2e+06", "Stata at w = {w}");
    }
    // Three-digit exponent, the other direction: A28 asks for one from w = 8 up,
    // Stata prints none. 26 330 corpus cells at `%8.0g`.
    assert_eq!(a28_decimals(8), 1);
    assert_eq!(fmt_g(1e300, 8), " 1.e+300");

    // What A28 was actually FOR: `%6.0g` must not compute -1 digits and panic
    // inside `format!("{:.*e}", esig - 1, x)`. That half holds — the shipped
    // floor is a saturating subtraction under a floor of 7 and cannot underflow.
    // The widths A28 names are exactly the ones that would have panicked.
    for w in 1usize..=6 {
        assert!(!fmt_g(1234567.891, w).is_empty(), "w = {w} survived");
    }
}

/// The other three families, at the widths the classic renderers use.
#[test]
fn fixed_exponential_and_comma() {
    assert_eq!(fmt_f(core::f64::consts::PI, 9, 2), "     3.14");
    assert_eq!(fmt_f(-0.5, 9, 2), "    -0.50");
    assert_eq!(fmt_fc(12481.0, 10, 0), "    12,481");
    assert_eq!(fmt_e(1234567.891, 12, 4), "  1.2346e+06");
    // Missing renders identically in every family.
    for s in [
        fmt_g(SYSMISS, 9),
        fmt_f(SYSMISS, 9, 2),
        fmt_e(SYSMISS, 9, 3),
        fmt_fc(SYSMISS, 9, 0),
    ] {
        assert_eq!(s, "        .");
    }
    assert_eq!(fmt_g(missing_f64(26), 9), "       .z");
}

/// `02`'s macro stringification is `fmt_g(v, 18)` trimmed (C12).
#[test]
fn macro_stringification() {
    assert_eq!(fmt_macro(1.0), "1");
    assert_eq!(fmt_macro(0.5), ".5");
    assert_eq!(fmt_macro(-0.5), "-.5");
    assert_eq!(fmt_macro(SYSMISS), ".");
    // `%18.0g` of 1/3, which is what `local a = 1/3` puts in the macro:
    // sixteen significant digits, the most that fit seventeen columns once the
    // sign column is reserved.
    assert_eq!(fmt_macro(1.0 / 3.0), ".3333333333333333");
}

proptest! {
    /// A28's second half: no `(value, width, prec)` in the legal grammar
    /// panics, and every result is either exactly the field width or an
    /// overflow that Stata would also overflow.
    #[test]
    fn no_legal_format_panics(
        bits in any::<u64>(),
        w in 1u16..=244,
        d in 0u8..=99,
        kind in 0u8..=4,
        left in any::<bool>(),
        zero_pad in any::<bool>(),
        commas in any::<bool>(),
    ) {
        let x = f64::from_bits(bits);
        prop_assume!(x.is_finite());
        prop_assume!(u32::from(d) < u32::from(w));
        let kind = match kind {
            0 => FormatKind::General,
            1 => FormatKind::Fixed,
            2 => FormatKind::Exponential,
            3 => FormatKind::Hex,
            _ => FormatKind::Str,
        };
        let f = StataFormat { kind, width: w, prec: d, left, zero_pad, commas };
        let s = f.format_f64(x);
        prop_assert!(!s.is_empty());
        prop_assert!(s.chars().count() >= 1);
    }

    /// `fmt_g` never emits a NaN, an infinity, or an `e` without an exponent.
    #[test]
    fn general_output_is_always_well_formed(bits in any::<u64>(), w in 1usize..=60) {
        let x = f64::from_bits(bits);
        prop_assume!(x.is_finite());
        let s = fmt_g(x, w);
        prop_assert!(!s.contains("NaN"));
        prop_assert!(!s.contains("inf"));
        if let Some(i) = s.find('e') {
            let tail = &s[i + 1..];
            prop_assert!(tail.starts_with('+') || tail.starts_with('-'), "{s}");
            prop_assert!(tail.len() >= 3, "{s}");
        }
    }
}
