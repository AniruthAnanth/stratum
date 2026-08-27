//! Order-preserving key encoding: `memcmp(key(a), key(b)) == stata_order(a, b)`.
//!
//! # The point of this module
//!
//! Stata's ordering rules for absent values look like two contradictory special
//! cases — numeric absent values sort *above* every real number, `""` sorts
//! *below* every string — and every reimplementation that treats them as special
//! cases ends up with a branch per comparison in the hottest loop in the engine.
//!
//! They are not special cases. They are the natural ascending order of the
//! stored representation:
//!
//! * `byte`/`int`/`long` sentinels are the **largest** values of the type — the
//!   `MAX_TAG + 1` codes running from `stratum_core::missing`'s `*_MISS` up to
//!   the integer type's maximum — so flipping the sign bit and reading
//!   big-endian puts them last, in tag order, for free.
//! * `float`/`double` sentinels are **large positive finite normals** (`2^127`,
//!   `2^1023`) with the tag in five mantissa bits, so the standard
//!   IEEE-754-to-unsigned monotone map puts them last, in tag order, for free.
//! * `str#` is NUL-padded to a fixed width, and `""` is all-NUL, so raw
//!   `memcmp` puts it first — which *is* Stata's string rule.
//!
//! **So this file contains no test for an absent value, and that is the
//! invariant.** `tests/sort.rs` asserts it mechanically by scanning this
//! module's source for the vocabulary of `stratum_core`'s sentinel API; a
//! comparator that ever needs to name one has stopped being branch-free.
//!
//! # `gsort`
//!
//! Descending is the bitwise complement of the ascending key
//! ([`complement`]), applied to that key's field only. `gsort -price price2`
//! therefore uses the same encoder and the same sorter.

use stratum_proto::{SortDir, StorageType};

use crate::column::Column;

/// Widest key any single column contributes. `str#` can declare more; those
/// columns take the comparator path, which needs no materialised key.
pub const MAX_INLINE_KEY: usize = 8;

/// A value that can be flattened into order-preserving big-endian bytes.
pub trait OrdKey: Copy {
    /// Significant bytes of the key.
    const WIDTH: usize;

    /// The key, big-endian, in the low [`WIDTH`](Self::WIDTH) bytes.
    fn key(self) -> [u8; MAX_INLINE_KEY];
}

macro_rules! int_key {
    ($t:ty, $u:ty, $w:expr) => {
        impl OrdKey for $t {
            const WIDTH: usize = $w;
            #[inline(always)]
            fn key(self) -> [u8; MAX_INLINE_KEY] {
                // Flip the sign bit: the signed order becomes the unsigned one.
                let u = (self as $u) ^ (1 << ($w * 8 - 1));
                let mut out = [0u8; MAX_INLINE_KEY];
                out[..$w].copy_from_slice(&u.to_be_bytes());
                out
            }
        }
    };
}

int_key!(i8, u8, 1);
int_key!(i16, u16, 2);
int_key!(i32, u32, 4);

impl OrdKey for f32 {
    const WIDTH: usize = 4;
    #[inline(always)]
    fn key(self) -> [u8; MAX_INLINE_KEY] {
        // `+ 0.0` normalises -0.0 to +0.0 and is the identity on everything
        // else. Without it the two zeros get different keys while Stata's
        // numeric comparison ties them, and `sort` would put a `-0` before a
        // `0` for no reason a user could ever see. One add, no branch.
        let b = (self + 0.0).to_bits();
        // The standard IEEE-754 monotone map: negatives reverse, positives get
        // their sign bit set so they sort above every negative.
        let u = if b & 0x8000_0000 != 0 {
            !b
        } else {
            b ^ 0x8000_0000
        };
        let mut out = [0u8; MAX_INLINE_KEY];
        out[..4].copy_from_slice(&u.to_be_bytes());
        out
    }
}

impl OrdKey for f64 {
    const WIDTH: usize = 8;
    #[inline(always)]
    fn key(self) -> [u8; MAX_INLINE_KEY] {
        // See the `f32` impl: -0.0 and +0.0 must produce the same key.
        let b = (self + 0.0).to_bits();
        let u = if b & 0x8000_0000_0000_0000 != 0 {
            !b
        } else {
            b ^ 0x8000_0000_0000_0000
        };
        u.to_be_bytes()
    }
}

/// Bytes one observation of `ty` contributes to a composite key.
///
/// `strL` answers `None`: its content is unbounded, so it has no fixed-width
/// key and always takes the comparator path.
#[must_use]
pub fn key_width(ty: StorageType) -> Option<usize> {
    Some(match ty {
        StorageType::Byte => 1,
        StorageType::Int => 2,
        StorageType::Long | StorageType::Float => 4,
        StorageType::Double => 8,
        StorageType::Str { width } => width as usize,
        StorageType::StrL => return None,
    })
}

/// Write observation `row`'s key for `col` into `out`, which must be exactly
/// `key_width(col.storage_type())` bytes.
///
/// # Panics
///
/// If `out` is the wrong length, or if `col` is a `strL` — both are caller bugs
/// that would otherwise produce a silently wrong order.
pub fn encode_key(col: &Column, row: u64, dir: SortDir, out: &mut [u8]) {
    match col {
        Column::Byte(c) => out.copy_from_slice(&c.get(row).key()[..1]),
        Column::Int(c) => out.copy_from_slice(&c.get(row).key()[..2]),
        Column::Long(c) => out.copy_from_slice(&c.get(row).key()[..4]),
        Column::Float(c) => out.copy_from_slice(&c.get(row).key()[..4]),
        Column::Double(c) => out.copy_from_slice(&c.get(row).key()[..8]),
        // The stored field already IS the key: fixed stride, NUL-padded.
        Column::Str(c) => out.copy_from_slice(c.raw(row)),
        Column::StrL(_) => panic!("strL has no fixed-width key; use the comparator path"),
    }
    if dir == SortDir::Desc {
        complement(out);
    }
}

/// Turn an ascending key field into a descending one.
#[inline]
pub fn complement(bytes: &mut [u8]) {
    for b in bytes {
        *b = !*b;
    }
}

/// Compare two observations of one column, in `dir`.
///
/// Used by the comparator path, which needs no materialised key. For every
/// fixed-width type this is the byte order [`encode_key`] would have produced,
/// so the two sorters cannot disagree by construction — `tests/sort.rs` asserts
/// they do not on 10 000 randomly generated frames.
#[must_use]
pub fn compare_rows(col: &Column, a: u64, b: u64, dir: SortDir) -> std::cmp::Ordering {
    let ord = match col {
        Column::Byte(c) => c.get(a).key()[..1].cmp(&c.get(b).key()[..1]),
        Column::Int(c) => c.get(a).key()[..2].cmp(&c.get(b).key()[..2]),
        Column::Long(c) => c.get(a).key()[..4].cmp(&c.get(b).key()[..4]),
        Column::Float(c) => c.get(a).key()[..4].cmp(&c.get(b).key()[..4]),
        Column::Double(c) => c.get(a).key().cmp(&c.get(b).key()),
        Column::Str(c) => c.raw(a).cmp(c.raw(b)),
        // Unbounded content: compare it all. `04` §6.2's "first 8 bytes then a
        // tie-break pass" is the radix shape, and radix is not offered here.
        Column::StrL(c) => c.get(a).cmp(c.get(b)),
    };
    if dir == SortDir::Desc {
        ord.reverse()
    } else {
        ord
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column::NumCol;
    use std::cmp::Ordering;

    fn key_of<T: OrdKey>(v: T) -> Vec<u8> {
        v.key()[..T::WIDTH].to_vec()
    }

    #[test]
    fn integer_keys_are_monotone_across_the_sign_boundary() {
        let mut seen = key_of(i16::MIN);
        for v in [i16::MIN + 1, -1, 0, 1, 32_740, 32_741, 32_767] {
            let k = key_of(v);
            assert!(k > seen, "{v} did not increase the key");
            seen = k;
        }
    }

    #[test]
    fn float_keys_are_monotone_across_the_sign_boundary() {
        let mut seen = key_of(f64::MIN);
        for v in [-1e300f64, -1.0, -0.0, 0.0, 1.0, 1e300, f64::MAX] {
            let k = key_of(v);
            assert!(k >= seen, "{v} went backwards");
            seen = k;
        }
    }

    #[test]
    fn a_fixed_string_key_is_the_stored_field() {
        let mut c = crate::column::FixedStrCol::empty(4, 2);
        c.chunk_mut(0)[0..4].copy_from_slice(b"ab\0\0");
        let col = Column::Str(c);
        let mut out = [0u8; 4];
        encode_key(&col, 0, SortDir::Asc, &mut out);
        assert_eq!(&out, b"ab\0\0");
        // Row 1 was never written, so it is `""`, all-NUL, and sorts first.
        let mut e = [0u8; 4];
        encode_key(&col, 1, SortDir::Asc, &mut e);
        assert!(e < out);
    }

    #[test]
    fn descending_is_the_complement_of_ascending() {
        let col = Column::Double(NumCol::from_slice(&[1.0, 2.0]));
        let (mut a, mut b) = ([0u8; 8], [0u8; 8]);
        encode_key(&col, 0, SortDir::Asc, &mut a);
        encode_key(&col, 1, SortDir::Asc, &mut b);
        assert!(a < b);
        encode_key(&col, 0, SortDir::Desc, &mut a);
        encode_key(&col, 1, SortDir::Desc, &mut b);
        assert!(a > b);
        assert_eq!(compare_rows(&col, 0, 1, SortDir::Desc), Ordering::Greater);
    }

    #[test]
    fn key_widths_match_the_storage_widths() {
        assert_eq!(key_width(StorageType::Byte), Some(1));
        assert_eq!(key_width(StorageType::Double), Some(8));
        assert_eq!(key_width(StorageType::Str { width: 18 }), Some(18));
        assert_eq!(key_width(StorageType::StrL), None);
    }
}
