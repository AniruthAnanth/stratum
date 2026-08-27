//! Writing a number into an SVG attribute, in a bounded number of bytes.
//!
//! Three requirements meet here and one function satisfies all of them.
//!
//! * **Determinism** (ADR-013). Two machines must emit the same document, and
//!   the same machine must emit the same document twice. Fixed-point through
//!   `i64` has no rounding mode and no locale.
//! * **The §8.7 grep.** `format!("{}", f64)` for a user-visible number outside
//!   `stratum_core::fmt` is banned. A geometry coordinate is not a user-visible
//!   number, but the way to *prove* that is to have no float formatting in the
//!   crate at all rather than to argue about which `format!` is which.
//! * **A byte bound.** The raster decision in `raster::decide` is made BEFORE
//!   anything is emitted, by comparing `marks × MAX_BYTES_PER_MARK` against the
//!   1.5 MB budget (design note §7). That prediction is only safe if a
//!   coordinate's width has a hard ceiling — [`MAX_COORD_BYTES`], asserted by a
//!   proptest over the whole `f64` range.
//!
//! Resolution is 0.01 pt — 1/7200 in, three orders of magnitude finer than any
//! display or printer resolves.

/// Hundredths of a point.
const SCALE: f64 = 100.0;

/// The largest magnitude a coordinate may carry. Anything past this is clamped:
/// a mark 10 000 points outside a 396-point figure is clipped by `clip-path`
/// either way, and the clamp is what keeps [`MAX_COORD_BYTES`] true.
const LIMIT: i64 = 99_999_999;

/// The ceiling `raster::decide` budgets against: `-999999.99` is ten bytes and
/// nothing this module can emit is longer.
pub const MAX_COORD_BYTES: usize = 10;

/// Append `v` as a fixed-point decimal with at most two places, trailing zeros
/// trimmed. Non-finite input writes `0` — an SVG attribute has no spelling for
/// NaN, and a mark at a missing coordinate has already been dropped upstream.
pub fn push_num(out: &mut String, v: f64) {
    if !v.is_finite() {
        out.push('0');
        return;
    }
    // `as i64` saturates on overflow in Rust, so the clamp below is belt and
    // braces rather than the only guard.
    let fixed = (v * SCALE).round() as i64;
    push_fixed(out, fixed.clamp(-LIMIT, LIMIT));
}

fn push_fixed(out: &mut String, fixed: i64) {
    if fixed < 0 {
        out.push('-');
    }
    let mag = fixed.unsigned_abs();
    let whole = mag / 100;
    let frac = mag % 100;
    push_u64(out, whole);
    if frac == 0 {
        return;
    }
    out.push('.');
    // `0.5` is `.5` in Stata's own numeric output and `.5` in SVG; the tenths
    // digit always prints, the hundredths digit only when it is not zero.
    out.push((b'0' + u8::try_from(frac / 10).unwrap_or(0)) as char);
    if !frac.is_multiple_of(10) {
        out.push((b'0' + u8::try_from(frac % 10).unwrap_or(0)) as char);
    }
}

/// Decimal digits of a `u64`, most significant first, with no allocation.
pub fn push_u64(out: &mut String, mut v: u64) {
    if v == 0 {
        out.push('0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut n = 0;
    while v > 0 {
        buf[n] = b'0' + u8::try_from(v % 10).unwrap_or(0);
        v /= 10;
        n += 1;
    }
    for i in (0..n).rev() {
        out.push(buf[i] as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(v: f64) -> String {
        let mut s = String::new();
        push_num(&mut s, v);
        s
    }

    #[test]
    fn trims_trailing_zeros_and_keeps_stata_spelling() {
        assert_eq!(num(0.0), "0");
        assert_eq!(num(1.0), "1");
        assert_eq!(num(0.5), "0.5");
        assert_eq!(num(-0.5), "-0.5");
        assert_eq!(num(12.34), "12.34");
        assert_eq!(num(12.30), "12.3");
        assert_eq!(num(396.0), "396");
    }

    #[test]
    fn rounds_rather_than_truncates() {
        assert_eq!(num(0.005), "0.01");
        assert_eq!(num(0.004), "0");
        assert_eq!(num(-0.005), "-0.01");
    }

    #[test]
    fn non_finite_is_zero_not_a_panic() {
        assert_eq!(num(f64::NAN), "0");
        assert_eq!(num(f64::INFINITY), "0");
        assert_eq!(num(f64::NEG_INFINITY), "0");
    }

    /// The bound the raster prediction rests on.
    #[test]
    fn clamps_to_the_documented_ceiling() {
        assert_eq!(num(1e300), "999999.99");
        assert_eq!(num(-1e300), "-999999.99");
        assert!(num(-1e300).len() <= MAX_COORD_BYTES);
    }

    proptest::proptest! {
        /// The whole `f64` range, not just the coordinates a figure plausibly
        /// produces.
        ///
        /// `raster::decide` compares `marks × MAX_BYTES_PER_*` against the 1.5 MB
        /// budget BEFORE emitting a byte, and every one of those per-mark
        /// ceilings is built out of [`MAX_COORD_BYTES`]. If a single coordinate
        /// anywhere in `f64` could write an eleventh byte, the prediction would
        /// be an estimate and the budget would be a hope. It cannot, and this is
        /// why.
        #[test]
        fn no_f64_anywhere_writes_more_than_the_ceiling(v in proptest::num::f64::ANY) {
            let s = num(v);
            proptest::prop_assert!(
                s.len() <= MAX_COORD_BYTES,
                "{v:?} wrote {} bytes: {s}",
                s.len()
            );
            // And it is always something SVG can parse as a number.
            proptest::prop_assert!(
                s.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-'),
                "{s} is not a number"
            );
        }
    }
}
