//! THE definition of a Stata missing value. Nothing else in the workspace
//! declares one (ADR-005, ARCHITECTURE C2, CI-grepped by
//! `scripts/check-topology.sh check_missing_values`).
//!
//! Every constant below was measured, not remembered: `04` §2.1 built a 27-row
//! dataset holding `.` and `.a`…`.z` in `byte`, `int`, `long`, `float` and
//! `double`, saved it as release 118, and read the raw bytes back. The closed
//! forms reproduce all 135 measured cells and `tests/golden_missing.rs` asserts
//! the whole table as literal constants.
//!
//! # Why sentinels and not `Option<f64>`
//!
//! Stata has **27 ordered** missing values and they sort *above* every real
//! number. The float and double sentinels are ordinary finite normal numbers —
//! `f32 .` is exactly `2^127`, `f64 .` is exactly `2^1023` — with the tag in
//! five mantissa bits and the twelve high mantissa bits left clear. So `<`,
//! `<=`, `>`, `>=`, `==` and `!=` on the raw `f64` already produce Stata's
//! answers and the engine contains **no** special-case comparison code. A
//! validity bitmap would make the `if` filter 3–5× slower and put a branch in
//! every comparator (ADR-005).
//!
//! # Invariant M (CONTRACTS §13.1)
//!
//! Every `f64` in a `Double` column, and every `f64` out of any expression
//! kernel, is either `> -SYSMISS && < SYSMISS` or one of the 27 sentinels.
//! There are no NaNs, no infinities and nothing below `-SYSMISS`. [`canon`] is
//! what enforces it, and it is called at the end of every arithmetic kernel.

/// Largest non-missing `byte`. 101..=127 are `.`, `.a`..`.z`.
pub const BYTE_MAX: i8 = 100;
/// `byte` encoding of `.`; `.a`..`.z` follow at +1 each, ending at 127.
pub const BYTE_MISS: i8 = 101;
/// Largest non-missing `int`.
pub const INT_MAX: i16 = 32_740;
/// `int` encoding of `.`; `.z` is 32_767.
pub const INT_MISS: i16 = 32_741;
/// Largest non-missing `long`.
pub const LONG_MAX: i32 = 2_147_483_620;
/// `long` encoding of `.`; `.z` is 2_147_483_647.
pub const LONG_MISS: i32 = 2_147_483_621;

/// Bit pattern of `f32` `.` — a finite normal number, `2^127`.
pub const F32_MISS_BITS: u32 = 0x7F00_0000;
/// Distance between consecutive `f32` tags: bit 11 of the 23 mantissa bits.
pub const F32_MISS_STEP: u32 = 0x0000_0800;
/// Bit pattern of `f64` `.` — a finite normal number, `2^1023`.
pub const F64_MISS_BITS: u64 = 0x7FE0_0000_0000_0000;
/// Distance between consecutive `f64` tags: bit 40 of the 52 mantissa bits.
pub const F64_MISS_STEP: u64 = 0x0000_0100_0000_0000;

/// `.` in double space. Every missing produced by arithmetic is exactly this.
pub const SYSMISS: f64 = f64::from_bits(F64_MISS_BITS);
/// `.` in float space.
pub const SYSMISS_F32: f32 = f32::from_bits(F32_MISS_BITS);
/// Highest tag: 0 is `.`, 26 is `.z`.
pub const MAX_TAG: u8 = 26;

/// The `.`, `.a`…`.z` tag letters, indexed by tag. `TAG_NAME[0]` is `"."`.
///
/// Rendering a missing value is one array index, which is why every format in
/// [`crate::fmt`] agrees about it for free.
pub const TAG_NAME: [&str; 27] = [
    ".", ".a", ".b", ".c", ".d", ".e", ".f", ".g", ".h", ".i", ".j", ".k", ".l", ".m", ".n", ".o",
    ".p", ".q", ".r", ".s", ".t", ".u", ".v", ".w", ".x", ".y", ".z",
];

/// `tag` 0 => `.`, 1..=26 => `.a`..=`.z`. Tags above [`MAX_TAG`] collapse to `.`.
#[inline(always)]
#[must_use]
pub const fn missing_f64(tag: u8) -> f64 {
    let t = if tag <= MAX_TAG { tag } else { 0 };
    f64::from_bits(F64_MISS_BITS + ((t as u64) << 40))
}

/// `tag` 0 => `.`, 1..=26 => `.a`..=`.z`, in `float` space.
#[inline(always)]
#[must_use]
pub const fn missing_f32(tag: u8) -> f32 {
    let t = if tag <= MAX_TAG { tag } else { 0 };
    f32::from_bits(F32_MISS_BITS + ((t as u32) << 11))
}

/// Exact under Invariant M, and one branchless compare.
///
/// NaN also answers `true`, because `!(NaN < x)` is `true` — so a value that
/// escaped canonicalisation degrades to "missing" rather than to garbage.
#[inline(always)]
#[must_use]
// The negated comparison is the point, not an oversight: `!(v < SYSMISS)` is
// TRUE for a NaN where `v >= SYSMISS` is false, so a value that escaped
// canonicalisation degrades to "missing" instead of to garbage. It is also one
// instruction and no branch, which is what makes the `if` filter vectorise.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn is_missing(v: f64) -> bool {
    !(v < SYSMISS)
}

/// The tag of `v`, or `None` when `v` is a real number.
#[inline]
#[must_use]
pub fn tag_of(v: f64) -> Option<u8> {
    if !is_missing(v) {
        return None;
    }
    let b = v.to_bits();
    if b < F64_MISS_BITS {
        // NaN with the sign bit clear sorts above SYSMISS by value but its bits
        // are larger; anything else landing here is junk. Both are plain `.`.
        return Some(0);
    }
    let t = ((b - F64_MISS_BITS) >> 40) as u8;
    Some(if t <= MAX_TAG { t } else { 0 })
}

/// Canonicalise an arbitrary computed double into Stata's value domain.
///
/// Call this at the END of every arithmetic kernel. It is one compare and one
/// select, and it implements two of the three measured propagation rules of
/// `04` §2.3: NaN (both comparisons false), `±inf` and out-of-range magnitudes
/// become `.`, and a result that is itself at or beyond a sentinel becomes
/// plain `.` — which is "arithmetic collapses tags", so `.a + 1` is `.` free.
///
/// # What `canon` alone does NOT catch
///
/// `04` §2.3 claims a kernel gets rule 1 ("any missing operand ⇒ `.`") for free
/// by calling `canon` on its result. **That is false for an annihilator.**
/// `.z * 0` is `0.0` before `canon` sees it, and `canon(0.0)` is `0.0`, but
/// Stata answers `.` — missingness dominates. The same applies to `x - x` and
/// to `0 ^ .`. A kernel with an annihilating operand must ask
/// [`either_missing`] FIRST; every other binary operator propagates through the
/// magnitude and is safe.
#[inline(always)]
#[must_use]
pub fn canon(v: f64) -> f64 {
    if v > -SYSMISS && v < SYSMISS {
        v
    } else {
        SYSMISS
    }
}

/// The guard an annihilating kernel needs BEFORE it computes anything.
///
/// `.z * 0` is `.`, not `0` (`04` §2.3, measured). One branchless `or` of two
/// compares; see [`canon`] for why the result-side check cannot do this.
#[inline(always)]
#[must_use]
pub fn either_missing(a: f64, b: f64) -> bool {
    is_missing(a) || is_missing(b)
}

/// Widen a `byte` column value to the double every expression evaluates in.
///
/// A plain `v as f64` is WRONG: `byte` 101 is `.`, and `101.0` is an ordinary
/// number. The 2 KiB table is built at compile time, so this is one L1 hit and
/// no branch.
#[inline(always)]
#[must_use]
pub fn widen_byte(v: i8) -> f64 {
    BYTE_TO_F64[v as u8 as usize]
}

/// Widen an `int` column value. See [`widen_byte`] for why the cast is wrong.
#[inline(always)]
#[must_use]
pub fn widen_int(v: i16) -> f64 {
    if v > INT_MAX {
        missing_f64((v - INT_MISS) as u8)
    } else {
        f64::from(v)
    }
}

/// Widen a `long` column value.
#[inline(always)]
#[must_use]
pub fn widen_long(v: i32) -> f64 {
    if v > LONG_MAX {
        missing_f64((v - LONG_MISS) as u8)
    } else {
        f64::from(v)
    }
}

/// Widen a `float` column value, mapping `f32` `2^127` to `f64` `2^1023`.
///
/// **The comparison is on the VALUE, not on `to_bits()`.** Every negative
/// `f32` has `to_bits() >= 0x8000_0000 > F32_MISS_BITS`, so a raw-bit test
/// turns every negative number in the dataset into a missing value — silently,
/// and only in the sign that a test with positive fixtures never covers.
/// `tests/golden_missing.rs` pins this.
#[inline(always)]
#[must_use]
pub fn widen_float(v: f32) -> f64 {
    if v < SYSMISS_F32 {
        f64::from(v)
    } else {
        let t = ((v.to_bits().wrapping_sub(F32_MISS_BITS)) >> 11) as u8;
        missing_f64(if t <= MAX_TAG { t } else { 0 })
    }
}

/// The result of narrowing a double back into a column's storage type.
///
/// `replace` never errors on range in Stata; it PROMOTES the column
/// (`variable i was int now long`), so the narrow functions report the required
/// type rather than failing. The promotion itself is the data engine's job.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Narrowed<T> {
    /// The value fits the current storage type.
    Ok(T),
    /// The column must first be rewritten as this wider type.
    NeedsPromotion(stratum_proto::StorageType),
}

use stratum_proto::StorageType;

/// Narrow to `byte`, or report the promotion the column needs.
#[inline]
#[must_use]
pub fn narrow_byte(v: f64) -> Narrowed<i8> {
    if let Some(t) = tag_of(v) {
        return Narrowed::Ok(BYTE_MISS + t as i8);
    }
    if v.fract() != 0.0 {
        return Narrowed::NeedsPromotion(StorageType::Double);
    }
    if v < -127.0 || v > f64::from(BYTE_MAX) {
        return Narrowed::NeedsPromotion(promote_integral(v));
    }
    Narrowed::Ok(v as i8)
}

/// Narrow to `int`, or report the promotion the column needs.
#[inline]
#[must_use]
pub fn narrow_int(v: f64) -> Narrowed<i16> {
    if let Some(t) = tag_of(v) {
        return Narrowed::Ok(INT_MISS + i16::from(t));
    }
    if v.fract() != 0.0 {
        return Narrowed::NeedsPromotion(StorageType::Double);
    }
    if v < -32_767.0 || v > f64::from(INT_MAX) {
        return Narrowed::NeedsPromotion(promote_integral(v));
    }
    Narrowed::Ok(v as i16)
}

/// Narrow to `long`, or report the promotion the column needs.
#[inline]
#[must_use]
pub fn narrow_long(v: f64) -> Narrowed<i32> {
    if let Some(t) = tag_of(v) {
        return Narrowed::Ok(LONG_MISS + i32::from(t));
    }
    if v.fract() != 0.0 {
        return Narrowed::NeedsPromotion(StorageType::Double);
    }
    if v < -2_147_483_647.0 || v > f64::from(LONG_MAX) {
        return Narrowed::NeedsPromotion(StorageType::Double);
    }
    Narrowed::Ok(v as i32)
}

/// Narrow to `float`. `double -> float` is lossy, so this only reports a
/// promotion when the value cannot survive the round trip at all.
#[inline]
#[must_use]
pub fn narrow_float(v: f64) -> Narrowed<f32> {
    if let Some(t) = tag_of(v) {
        return Narrowed::Ok(missing_f32(t));
    }
    let f = v as f32;
    if f.is_infinite() || f >= SYSMISS_F32 || f <= -SYSMISS_F32 {
        return Narrowed::NeedsPromotion(StorageType::Double);
    }
    Narrowed::Ok(f)
}

/// The narrowest integral type that holds `v`, for the promotion ladder.
///
/// `long -> double`, never `long -> float`: `float` cannot hold every `long`.
#[inline]
fn promote_integral(v: f64) -> StorageType {
    if v >= -32_767.0 && v <= f64::from(INT_MAX) {
        StorageType::Int
    } else if v >= -2_147_483_647.0 && v <= f64::from(LONG_MAX) {
        StorageType::Long
    } else {
        StorageType::Double
    }
}

/// `byte` widening table. 256 entries, built at compile time.
static BYTE_TO_F64: [f64; 256] = build_byte_table();

const fn build_byte_table() -> [f64; 256] {
    let mut t = [0.0f64; 256];
    let mut i = 0usize;
    while i < 256 {
        // `i` is the u8 reinterpretation of an i8, so 128..=255 are -128..=-1.
        let signed = i as u8 as i8;
        t[i] = if signed > BYTE_MAX {
            missing_f64((signed - BYTE_MISS) as u8)
        } else {
            signed as f64
        };
        i += 1;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    // The comparisons below are const-evaluable, which is exactly the property
    // being asserted: Stata's ordering is a fact about the ENCODING, not about
    // any code we wrote.
    #[allow(clippy::assertions_on_constants, clippy::neg_cmp_op_on_partial_ord)]
    #[test]
    fn ordering_falls_out_of_ieee() {
        // semantics.log: `. < .a`, `.a < .z`, `2 < .`, `. == .`, `!(2 > .)`.
        assert!(SYSMISS < missing_f64(1));
        assert!(missing_f64(1) < missing_f64(26));
        assert!(2.0 < SYSMISS);
        assert!(SYSMISS == SYSMISS);
        assert!(!(2.0 > SYSMISS));
        assert!(SYSMISS > 1e300);
    }

    #[test]
    fn canon_implements_the_three_measured_rules() {
        assert_eq!(canon(1.0 / 0.0), SYSMISS);
        assert_eq!(canon(-1.0 / 0.0), SYSMISS);
        assert_eq!(canon(f64::NAN), SYSMISS);
        // Rule 1: tags do not survive arithmetic.
        assert_eq!(canon(missing_f64(1) + 1.0), SYSMISS);
        assert_eq!(canon(2.0 + SYSMISS), SYSMISS);
        assert_eq!(canon(1.5), 1.5);
        // ... but NOT through an annihilator, which is why `either_missing`
        // exists. `.z * 0` is 0.0 by the time `canon` could see it.
        assert_eq!(canon(missing_f64(26) * 0.0), 0.0);
        assert!(either_missing(missing_f64(26), 0.0));
        assert!(!either_missing(1.0, 0.0));
    }

    #[test]
    fn byte_table_matches_the_scalar_rule() {
        for i in -128i16..=127 {
            let b = i as i8;
            let expect = if b > BYTE_MAX {
                missing_f64((b - BYTE_MISS) as u8)
            } else {
                f64::from(b)
            };
            assert_eq!(widen_byte(b).to_bits(), expect.to_bits(), "byte {b}");
        }
    }

    #[test]
    fn narrow_reports_the_promotion_ladder() {
        assert_eq!(
            narrow_int(40_000.0),
            Narrowed::NeedsPromotion(StorageType::Long)
        );
        assert_eq!(
            narrow_byte(200.0),
            Narrowed::NeedsPromotion(StorageType::Int)
        );
        assert_eq!(
            narrow_int(1.5),
            Narrowed::NeedsPromotion(StorageType::Double)
        );
        assert_eq!(
            narrow_long(3e9),
            Narrowed::NeedsPromotion(StorageType::Double)
        );
        assert_eq!(narrow_int(SYSMISS), Narrowed::Ok(INT_MISS));
        assert_eq!(narrow_int(missing_f64(26)), Narrowed::Ok(32_767));
        assert_eq!(narrow_byte(missing_f64(26)), Narrowed::Ok(127));
    }
}
