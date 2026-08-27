//! The per-release constant table, and the block layout it describes.
//!
//! Everything that differs between release 117, 118 and 119 is [`ReleaseSpec`].
//! Every field of it was **measured** — `04` §9.1 records, per cell, the file it
//! came from and the division that produced it — and none of it is assumed.
//!
//! # Why the constants are checked against the file on every read
//!
//! A constants-only reader is silently wrong the day StataCorp widens a field.
//! It would slice `varnames` at the wrong stride and hand back plausible
//! garbage: names that look like names, from the wrong offsets, with no error
//! anywhere. So the reader **derives** each fixed-width block's element size
//! from the map deltas and asserts it equals the constant
//! ([`ReleaseSpec::check_block`]). A future release with new widths then fails
//! loudly, at the exact block, naming both numbers.
//!
//! That same check is what catches a hostile map whose entries are individually
//! inside the file but mutually inconsistent (THREAT_MODEL §2.2 M8).

use stratum_core::StorageType;

use crate::DtaError;

/// The three `.dta` releases Stratum reads and writes.
///
/// 113–116 (Stata 10–12) are a different, untagged binary layout with no map;
/// they are a separate reader, deferred to v1.1 (`04` §9.6). 120 (Stata 18
/// alias variables) needs `frlink`, which is v1.1 scope.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, serde::Serialize, serde::Deserialize)]
pub enum Release {
    /// Stata 13. `saveold, version(13)`. Not UTF-8.
    R117,
    /// Stata 14–17, and Stata 18's default. UTF-8.
    R118,
    /// Stata 15+ with more than 32 767 variables. UTF-8.
    R119,
}

impl Release {
    /// The number that appears between `<release>` and `</release>`.
    #[must_use]
    pub const fn number(self) -> u16 {
        match self {
            Release::R117 => 117,
            Release::R118 => 118,
            Release::R119 => 119,
        }
    }

    /// The constants for this release.
    #[must_use]
    pub const fn spec(self) -> &'static ReleaseSpec {
        match self {
            Release::R117 => &R117,
            Release::R118 => &R118,
            Release::R119 => &R119,
        }
    }

    /// Parse the three bytes inside `<release>`.
    ///
    /// # Errors
    ///
    /// [`DtaError::UnsupportedRelease`] for a release we recognise but do not
    /// read (113–116, 120), [`DtaError::BadRelease`] for anything else.
    pub fn parse(bytes: &[u8]) -> Result<Self, DtaError> {
        match bytes {
            b"117" => Ok(Release::R117),
            b"118" => Ok(Release::R118),
            b"119" => Ok(Release::R119),
            _ => {
                let text = String::from_utf8_lossy(bytes).into_owned();
                match text.trim().parse::<u16>() {
                    Ok(n @ (102..=116 | 120..=199)) => Err(DtaError::UnsupportedRelease(n)),
                    _ => Err(DtaError::BadRelease(text)),
                }
            }
        }
    }

    /// Every release, for a test that must sweep all three.
    pub const ALL: [Release; 3] = [Release::R117, Release::R118, Release::R119];
}

/// `LSF` (little-endian) or `MSF` (big-endian).
///
/// Every platform in the release matrix is little-endian, so `Lsf` is the path
/// with literally no work in it and `Msf` pays one `swap_bytes` per field.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, serde::Serialize, serde::Deserialize)]
pub enum ByteOrder {
    /// Little-endian. What every file we have ever seen uses.
    Lsf,
    /// Big-endian.
    Msf,
}

impl ByteOrder {
    /// The three bytes inside `<byteorder>`.
    #[must_use]
    pub const fn tag(self) -> &'static [u8; 3] {
        match self {
            ByteOrder::Lsf => b"LSF",
            ByteOrder::Msf => b"MSF",
        }
    }

    /// # Errors
    ///
    /// [`DtaError::BadByteOrder`] on anything but `LSF`/`MSF`. There is no
    /// "assume native" fallback: guessing here silently transposes every
    /// numeric value in the file.
    pub fn parse(bytes: &[u8]) -> Result<Self, DtaError> {
        match bytes {
            b"LSF" => Ok(ByteOrder::Lsf),
            b"MSF" => Ok(ByteOrder::Msf),
            _ => Err(DtaError::BadByteOrder(
                String::from_utf8_lossy(bytes).into_owned(),
            )),
        }
    }
}

/// Everything that differs between releases, in one table (`04` §9.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseSpec {
    /// 117 | 118 | 119.
    pub release: u16,
    /// Bytes in `<K>` (the variable count).
    pub k_width: u8,
    /// Bytes in `<N>` (the observation count).
    pub n_width: u8,
    /// Bytes in the dataset-label length prefix.
    pub label_len_width: u8,
    /// Bytes per entry in `<varnames>`.
    pub varname_len: usize,
    /// Bytes per entry in `<formats>`.
    pub format_len: usize,
    /// Bytes per entry in `<value_label_names>`.
    pub vlblname_len: usize,
    /// Bytes per entry in `<variable_labels>`.
    pub varlabel_len: usize,
    /// Bytes per entry in `<sortlist>`, which has `K + 1` entries.
    pub sortlist_elem: usize,
    /// Width of `v` inside the 8-byte `(v,o)` pair in the data section.
    pub gso_v_bytes: usize,
    /// Width of `o` in a GSO record header (`v` is always `u32` there).
    pub gso_hdr_o_width: u8,
    /// True when text in the file is UTF-8; false means the writing machine's
    /// codepage, which the file does not record (`04` §9.4).
    pub utf8: bool,
}

/// Release 117 — `saveold, version(13)`.
///
/// Measured from `strl117.dta` and `mv117.dta`: block payload ÷ K gave exactly
/// 33, 49, 33, 81, and the sortlist payload 8 ÷ (K+1)=4 gave 2 (`04` §9.1).
pub const R117: ReleaseSpec = ReleaseSpec {
    release: 117,
    k_width: 2,
    n_width: 4,
    label_len_width: 1,
    varname_len: 33,
    format_len: 49,
    vlblname_len: 33,
    varlabel_len: 81,
    sortlist_elem: 2,
    gso_v_bytes: 4,
    gso_hdr_o_width: 4,
    utf8: false,
};

/// Release 118 — the default for Stata 14 through 18.
///
/// Measured from `auto.dta` (12 vars) *and* `strl118.dta` (3 vars); both gave
/// 129 / 57 / 129 / 321.
pub const R118: ReleaseSpec = ReleaseSpec {
    release: 118,
    k_width: 2,
    n_width: 8,
    label_len_width: 2,
    varname_len: 129,
    format_len: 57,
    vlblname_len: 129,
    varlabel_len: 321,
    sortlist_elem: 2,
    gso_v_bytes: 2,
    gso_hdr_o_width: 8,
    utf8: true,
};

/// Release 119 — more than 32 767 variables.
///
/// Measured on a 39 990-variable file: `<K>` is 4 bytes (`36 9c 00 00`), the
/// sortlist payload is 159 964 = 4 × (39 990 + 1), and 5 158 710 / 39 990 = 129,
/// 2 279 430 / 39 990 = 57, 12 836 790 / 39 990 = 321 (`04` §9.1).
pub const R119: ReleaseSpec = ReleaseSpec {
    release: 119,
    k_width: 4,
    n_width: 8,
    label_len_width: 2,
    varname_len: 129,
    format_len: 57,
    vlblname_len: 129,
    varlabel_len: 321,
    sortlist_elem: 4,
    gso_v_bytes: 3,
    gso_hdr_o_width: 8,
    utf8: true,
};

impl ReleaseSpec {
    /// Assert a fixed-width block's derived element size against the constant.
    ///
    /// `payload` is the block's byte length with its opening and closing tags
    /// already removed, and it is derived from map deltas — i.e. from the file.
    /// `expected` is our constant. This is the check that turns "StataCorp
    /// widened a field" from silent garbage into a named error, and it is
    /// `04` §9.4 step 4.
    ///
    /// `k == 0` is legal — Stata writes a zero-variable dataset — and makes the
    /// division undefined, so the rule degenerates to "the payload must be
    /// empty".
    ///
    /// # Errors
    ///
    /// [`DtaError::BlockWidth`] when the derived stride disagrees, or
    /// [`DtaError::BlockLen`] when the payload is not a whole number of
    /// elements.
    pub fn check_block(
        block: &'static str,
        payload: u64,
        elems: u64,
        expected: usize,
    ) -> Result<(), DtaError> {
        if elems == 0 {
            return if payload == 0 {
                Ok(())
            } else {
                Err(DtaError::BlockLen { block, payload })
            };
        }
        if !payload.is_multiple_of(elems) {
            return Err(DtaError::BlockLen { block, payload });
        }
        let derived = payload / elems;
        if derived != expected as u64 {
            return Err(DtaError::BlockWidth {
                block,
                derived,
                expected: expected as u64,
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The map
// ---------------------------------------------------------------------------

/// Entries in `<map>`: 13 section offsets plus the declared end of file.
pub const MAP_ENTRIES: usize = 14;

/// Bytes of `<map>` payload: 14 × `u64`, always, in every release.
pub const MAP_BYTES: usize = MAP_ENTRIES * 8;

/// The tagged sections, in map order. Index into this is index into the map.
///
/// `map[0]` is `<stata_dta>` itself and `map[13]` is the declared EOF, which is
/// why the array has 12 named tags and not 14.
pub const SECTIONS: [&str; 12] = [
    "stata_dta",
    "map",
    "variable_types",
    "varnames",
    "sortlist",
    "formats",
    "value_label_names",
    "variable_labels",
    "characteristics",
    "data",
    "strls",
    "value_labels",
];

/// `map` index of `<data>`.
pub const MAP_DATA: usize = 9;
/// `map` index of `<strls>`.
pub const MAP_STRLS: usize = 10;
/// `map` index of `<value_labels>`.
pub const MAP_VALUE_LABELS: usize = 11;
/// `map` index of `</stata_dta>`.
pub const MAP_CLOSE: usize = 12;
/// `map` index of the declared end of file.
pub const MAP_EOF: usize = 13;

// ---------------------------------------------------------------------------
// Type codes (`04` §9.3)
// ---------------------------------------------------------------------------

/// The widest `str#`. 2046..=32767 is a reserved hole.
pub const MAX_STR_WIDTH: u16 = 2045;
/// `strL`.
pub const TC_STRL: u16 = 32768;
/// `double`.
pub const TC_DOUBLE: u16 = 65526;
/// `float`.
pub const TC_FLOAT: u16 = 65527;
/// `long`.
pub const TC_LONG: u16 = 65528;
/// `int`.
pub const TC_INT: u16 = 65529;
/// `byte`.
pub const TC_BYTE: u16 = 65530;

/// Decode one `<variable_types>` entry.
///
/// # Errors
///
/// [`DtaError::TypeCode`] for 0, for a `str#` past 2045, and for either
/// reserved hole. A reader that guessed here would compute the wrong row width
/// and misread every subsequent variable in every observation.
pub fn storage_type(code: u16, var: u32) -> Result<StorageType, DtaError> {
    match code {
        0 => Err(DtaError::TypeCode { var, code }),
        1..=MAX_STR_WIDTH => Ok(StorageType::Str { width: code }),
        TC_STRL => Ok(StorageType::StrL),
        TC_DOUBLE => Ok(StorageType::Double),
        TC_FLOAT => Ok(StorageType::Float),
        TC_LONG => Ok(StorageType::Long),
        TC_INT => Ok(StorageType::Int),
        TC_BYTE => Ok(StorageType::Byte),
        _ => Err(DtaError::TypeCode { var, code }),
    }
}

/// Encode one `<variable_types>` entry. Total: every [`StorageType`] has a code.
#[must_use]
pub fn type_code(ty: StorageType) -> u16 {
    match ty {
        StorageType::Byte => TC_BYTE,
        StorageType::Int => TC_INT,
        StorageType::Long => TC_LONG,
        StorageType::Float => TC_FLOAT,
        StorageType::Double => TC_DOUBLE,
        StorageType::Str { width } => width,
        StorageType::StrL => TC_STRL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_codes_round_trip() {
        let all = [
            StorageType::Byte,
            StorageType::Int,
            StorageType::Long,
            StorageType::Float,
            StorageType::Double,
            StorageType::Str { width: 1 },
            StorageType::Str { width: 2045 },
            StorageType::StrL,
        ];
        for t in all {
            assert_eq!(storage_type(type_code(t), 0).unwrap(), t);
        }
    }

    #[test]
    fn reserved_type_codes_are_rejected() {
        for code in [0u16, 2046, 32767, 32769, 65525, 65531, 65535] {
            assert!(storage_type(code, 7).is_err(), "code {code} was accepted");
        }
    }

    #[test]
    fn block_width_check_names_both_numbers() {
        // 12 variables, a 1548-byte varnames payload: 129 each. auto.dta.
        ReleaseSpec::check_block("varnames", 1548, 12, 129).unwrap();
        // The same file read as though it were release 117.
        let e = ReleaseSpec::check_block("varnames", 1548, 12, 33).unwrap_err();
        assert!(matches!(
            e,
            DtaError::BlockWidth {
                derived: 129,
                expected: 33,
                ..
            }
        ));
        // K = 0 is legal and requires an empty payload.
        ReleaseSpec::check_block("varnames", 0, 0, 129).unwrap();
        assert!(ReleaseSpec::check_block("varnames", 4, 0, 129).is_err());
        // A ragged payload is a malformed file, not a new release.
        assert!(matches!(
            ReleaseSpec::check_block("varnames", 130, 12, 129),
            Err(DtaError::BlockLen { .. })
        ));
    }

    #[test]
    fn release_parse_separates_unsupported_from_garbage() {
        assert_eq!(Release::parse(b"118").unwrap(), Release::R118);
        assert!(matches!(
            Release::parse(b"115"),
            Err(DtaError::UnsupportedRelease(115))
        ));
        assert!(matches!(
            Release::parse(b"120"),
            Err(DtaError::UnsupportedRelease(120))
        ));
        assert!(matches!(
            Release::parse(b"11 "),
            Err(DtaError::BadRelease(_))
        ));
        assert!(matches!(Release::parse(b""), Err(DtaError::BadRelease(_))));
    }

    /// `04` §9.1's evidence table, as an assertion rather than as prose.
    #[test]
    fn the_measured_table_is_what_is_compiled_in() {
        assert_eq!(
            (R117.varname_len, R117.format_len, R117.varlabel_len),
            (33, 49, 81)
        );
        assert_eq!(
            (R118.varname_len, R118.format_len, R118.varlabel_len),
            (129, 57, 321)
        );
        assert_eq!(
            (R119.varname_len, R119.format_len, R119.varlabel_len),
            (129, 57, 321)
        );
        assert_eq!(
            (R117.sortlist_elem, R118.sortlist_elem, R119.sortlist_elem),
            (2, 2, 4)
        );
        assert_eq!((R117.k_width, R118.k_width, R119.k_width), (2, 2, 4));
        assert_eq!((R117.n_width, R118.n_width, R119.n_width), (4, 8, 8));
        assert_eq!(
            (R117.gso_v_bytes, R118.gso_v_bytes, R119.gso_v_bytes),
            (4, 2, 3)
        );
        const { assert!(!R117.utf8 && R118.utf8 && R119.utf8) };
    }
}
