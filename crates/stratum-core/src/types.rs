//! The storage-type promotion ladder, and nothing else.
//!
//! [`StorageType`] itself is **declared in `stratum-proto` and nowhere else**
//! (A10, CONTRACTS §8): a structurally identical twin here with no conversion
//! between the two is a bug class the compiler cannot see. This module
//! re-exports it and owns only the lattice over it.
//!
//! The ladder is measured (`04` §2.6). `replace` never errors on range in
//! Stata; it silently rewrites the column:
//!
//! ```text
//! . gen int i = 100
//! . replace i = 40000 in 1
//! variable i was int now long
//! ```
//!
//! ```text
//! byte -> int -> long -> double     integral chain
//! float -> double                   floating chain
//! byte/int/long -> double           non-integral value, or |v| > 2^53
//! ```
//!
//! `long -> double` and never `long -> float`, because `float` cannot hold
//! every `long`.

pub use stratum_proto::StorageType;

/// Rank in the numeric widening lattice; `None` for the two string types.
///
/// `float` sits between `long` and `double` on purpose: it is wider in range
/// than `long` but narrower in integral precision, and the only question this
/// rank answers is "which one do I have to rewrite the column to".
#[must_use]
pub fn numeric_rank(t: StorageType) -> Option<u8> {
    match t {
        StorageType::Byte => Some(0),
        StorageType::Int => Some(1),
        StorageType::Long => Some(2),
        StorageType::Float => Some(3),
        StorageType::Double => Some(4),
        StorageType::Str { .. } | StorageType::StrL => None,
    }
}

/// True for `byte`, `int`, `long`, `float`, `double`.
#[must_use]
pub fn is_numeric(t: StorageType) -> bool {
    numeric_rank(t).is_some()
}

/// True for `str#` and `strL`.
#[must_use]
pub fn is_string(t: StorageType) -> bool {
    !is_numeric(t)
}

/// Bytes one observation of `t` occupies in a `.dta` data section.
///
/// `strL` is 8 because the data section stores a `(v,o)` pair, not the text.
#[must_use]
pub fn storage_width(t: StorageType) -> u16 {
    match t {
        StorageType::Byte => 1,
        StorageType::Int => 2,
        StorageType::Long | StorageType::Float => 4,
        StorageType::Double | StorageType::StrL => 8,
        StorageType::Str { width } => width,
    }
}

/// The join of two types: the narrowest type that can hold either.
///
/// Mixing a string and a numeric has no join — that is `r(109) type mismatch`
/// at the expression layer, not a silent widening — so it answers `None`.
#[must_use]
pub fn promote(a: StorageType, b: StorageType) -> Option<StorageType> {
    match (a, b) {
        (StorageType::StrL, _) | (_, StorageType::StrL) if is_string(a) && is_string(b) => {
            Some(StorageType::StrL)
        }
        (StorageType::Str { width: x }, StorageType::Str { width: y }) => {
            Some(StorageType::Str { width: x.max(y) })
        }
        _ if is_numeric(a) && is_numeric(b) => {
            let (ra, rb) = (numeric_rank(a)?, numeric_rank(b)?);
            // The lattice is NOT a total order at (long, float): neither holds
            // the other, so their join is `double`. Every other pair is the max.
            Some(match (a, b) {
                (StorageType::Long, StorageType::Float)
                | (StorageType::Float, StorageType::Long) => StorageType::Double,
                _ if ra >= rb => a,
                _ => b,
            })
        }
        _ => None,
    }
}

/// The next rung up the ladder from `t`, or `None` at the top.
#[must_use]
pub fn widen_one(t: StorageType) -> Option<StorageType> {
    match t {
        StorageType::Byte => Some(StorageType::Int),
        StorageType::Int => Some(StorageType::Long),
        StorageType::Long | StorageType::Float => Some(StorageType::Double),
        StorageType::Double | StorageType::Str { .. } | StorageType::StrL => None,
    }
}

/// Stata's default display format for a freshly created variable of `t`
/// (measured: `describe` in `semantics.log` §"type promotion and storage").
#[must_use]
pub fn default_format(t: StorageType) -> &'static str {
    match t {
        StorageType::Byte | StorageType::Int => "%8.0g",
        StorageType::Long => "%12.0g",
        StorageType::Float => "%9.0g",
        StorageType::Double => "%10.0g",
        StorageType::Str { .. } | StorageType::StrL => "%9s",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_matches_the_measured_promotions() {
        use StorageType::{Byte, Double, Float, Int, Long};
        assert_eq!(promote(Byte, Int), Some(Int));
        assert_eq!(promote(Int, Byte), Some(Int));
        assert_eq!(promote(Long, Double), Some(Double));
        // Neither holds the other: float loses long's low bits, long loses
        // float's range.
        assert_eq!(promote(Long, Float), Some(Double));
        assert_eq!(promote(Float, Long), Some(Double));
        assert_eq!(promote(Float, Double), Some(Double));
        assert_eq!(widen_one(Long), Some(Double));
        assert_eq!(widen_one(Double), None);
    }

    #[test]
    fn strings_and_numerics_have_no_join() {
        assert_eq!(promote(StorageType::Byte, StorageType::StrL), None);
        assert_eq!(
            promote(StorageType::Str { width: 4 }, StorageType::Str { width: 9 }),
            Some(StorageType::Str { width: 9 })
        );
        assert_eq!(
            promote(StorageType::Str { width: 4 }, StorageType::StrL),
            Some(StorageType::StrL)
        );
    }

    #[test]
    fn default_formats_are_the_measured_ones() {
        // describe in tests/golden/stata18/semantics.log.
        assert_eq!(default_format(StorageType::Byte), "%8.0g");
        assert_eq!(default_format(StorageType::Int), "%8.0g");
        assert_eq!(default_format(StorageType::Long), "%12.0g");
        assert_eq!(default_format(StorageType::Float), "%9.0g");
        assert_eq!(default_format(StorageType::Double), "%10.0g");
    }

    #[test]
    fn widths_match_the_dta_layout() {
        assert_eq!(storage_width(StorageType::Byte), 1);
        assert_eq!(storage_width(StorageType::Int), 2);
        assert_eq!(storage_width(StorageType::Long), 4);
        assert_eq!(storage_width(StorageType::Float), 4);
        assert_eq!(storage_width(StorageType::Double), 8);
        assert_eq!(storage_width(StorageType::StrL), 8);
        assert_eq!(storage_width(StorageType::Str { width: 18 }), 18);
    }
}
