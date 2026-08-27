//! Text decoding for release 117, and the UTF-8 policy for 118/119.
//!
//! 118 and 119 are UTF-8. **117 is the writing machine's codepage, which the
//! file does not record.** There is no field to read, so there is nothing to
//! detect; the choice is between guessing deterministically and guessing
//! nondeterministically.
//!
//! `04` §9.4 decided: assume Windows-1252, let the caller override with
//! [`DtaReadOptions::encoding`](crate::DtaReadOptions), and record what was
//! actually used in [`ReadReport::encoding`](crate::ReadReport) so the UI can
//! say "decoded as Windows-1252" and offer a re-read. A charset *detector* was
//! rejected: it returns a different answer for the same bytes depending on how
//! much of the file it sampled, and spec §16 forbids a nondeterministic result
//! for a deterministic input.
//!
//! # Why this is a `const` table and not `encoding_rs`
//!
//! Every function here is **total over all 256 input bytes**. That is not a
//! stylistic preference — it is the whole answer to A27's "codepage tables with
//! out-of-range indices". A single-byte decode that is a `[char; 256]` lookup
//! cannot have an out-of-range index, so the hostile case is unrepresentable
//! rather than merely rejected. `encoding_rs` is in the workspace table for the
//! `.csv`/`.txt` import path, which needs multi-byte encodings; a `.dta` 117
//! file needs 512 bytes of table and no state machine.
//!
//! The five positions Windows-1252 leaves undefined (0x81, 0x8D, 0x8F, 0x90,
//! 0x9D) decode to U+FFFD and are **counted**, so "this file had bytes we could
//! not interpret" is a number in the report rather than silence.

use crate::DtaError;

/// How the bytes in a `.dta` file's text fields are interpreted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Encoding {
    /// Release 118/119, and a 117 file written by a Unicode-aware tool.
    #[default]
    Utf8,
    /// The assumed default for release 117 (`04` §9.4).
    Windows1252,
    /// ISO-8859-1. Differs from Windows-1252 only in 0x80..=0x9F, where it
    /// yields C1 control characters instead of typography.
    Latin1,
}

impl Encoding {
    /// What the report prints and what the UI offers as a re-read option.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Windows1252 => "Windows-1252",
            Encoding::Latin1 => "ISO-8859-1",
        }
    }
}

/// What one decode did, so the caller can accumulate it into the read report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeStat {
    /// Bytes replaced by U+FFFD because they could not be interpreted.
    pub replacements: u64,
}

impl DecodeStat {
    /// Fold another decode's stats in.
    pub fn merge(&mut self, other: DecodeStat) {
        self.replacements += other.replacements;
    }
}

/// Decode one field's bytes.
///
/// Total: every input produces a `String`. A file we cannot interpret opens
/// with U+FFFD in the affected places and a count in the report — we never
/// refuse to open, and we never silently mangle (`04` §9.4).
#[must_use]
pub fn decode(src: &[u8], encoding: Encoding) -> (String, DecodeStat) {
    match encoding {
        Encoding::Utf8 => decode_utf8(src),
        Encoding::Windows1252 => decode_single_byte(src, &CP1252),
        Encoding::Latin1 => decode_single_byte(src, &LATIN1),
    }
}

/// UTF-8 with lossy replacement, counting the replacements.
///
/// `String::from_utf8_lossy` does the replacement but not the count, and the
/// count is what the report needs, so the error positions are walked directly.
fn decode_utf8(src: &[u8]) -> (String, DecodeStat) {
    match std::str::from_utf8(src) {
        Ok(s) => (s.to_owned(), DecodeStat::default()),
        Err(_) => {
            let mut out = String::with_capacity(src.len());
            let mut stat = DecodeStat::default();
            let mut rest = src;
            loop {
                match std::str::from_utf8(rest) {
                    Ok(s) => {
                        out.push_str(s);
                        break;
                    }
                    Err(e) => {
                        let good = e.valid_up_to();
                        // Safe by construction: `valid_up_to` is a char boundary.
                        out.push_str(std::str::from_utf8(&rest[..good]).unwrap_or_default());
                        out.push(char::REPLACEMENT_CHARACTER);
                        let skip = e.error_len().unwrap_or(rest.len() - good);
                        stat.replacements += skip as u64;
                        rest = &rest[good + skip..];
                    }
                }
            }
            (out, stat)
        }
    }
}

fn decode_single_byte(src: &[u8], table: &[char; 256]) -> (String, DecodeStat) {
    let mut out = String::with_capacity(src.len());
    let mut stat = DecodeStat::default();
    for &b in src {
        let c = table[b as usize];
        if c == char::REPLACEMENT_CHARACTER {
            stat.replacements += 1;
        }
        out.push(c);
    }
    (out, stat)
}

/// Encode text for writing.
///
/// # Errors
///
/// [`DtaError::NotRepresentable`] naming the first offending character. Writing
/// a 117 file is `saveold, version(13)`, and `04` §10.1 is explicit that its
/// lossiness is **checked and reported, never silent**: Stata substitutes, we
/// refuse, because a variable label that quietly loses its accents is a lie the
/// user cannot see.
pub fn encode(text: &str, encoding: Encoding, what: &str) -> Result<Vec<u8>, DtaError> {
    match encoding {
        Encoding::Utf8 => Ok(text.as_bytes().to_vec()),
        Encoding::Windows1252 => encode_single_byte(text, &CP1252, encoding, what),
        Encoding::Latin1 => encode_single_byte(text, &LATIN1, encoding, what),
    }
}

fn encode_single_byte(
    text: &str,
    table: &[char; 256],
    encoding: Encoding,
    what: &str,
) -> Result<Vec<u8>, DtaError> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        // U+FFFD is refused rather than mapped: it is what the *decoder* emits
        // for a byte it could not interpret, so encoding it back would turn a
        // reported loss into a silent one.
        let slot = if c == char::REPLACEMENT_CHARACTER {
            None
        } else {
            // 256 comparisons worst case, on a path that runs once per metadata
            // field of a 117 file. A reverse map would be static bytes spent to
            // save nothing measurable.
            table.iter().position(|&t| t == c)
        };
        match slot {
            Some(i) => out.push(i as u8),
            None => {
                return Err(DtaError::NotRepresentable {
                    what: what.to_owned(),
                    ch: c,
                    encoding: encoding.name(),
                })
            }
        }
    }
    Ok(out)
}

/// Bytes up to the first NUL, remainder discarded.
///
/// **This is `04` §0.2 trap 2 and it is not optional.** Stata writes
/// uninitialised memory into fixed-width padding. Measured in the committed
/// fixtures: `auto.dta`'s `formats` block holds `%8.0gc`, `A` and `0x60` *after*
/// the terminator of another variable's format, and its `value_label_names`
/// block holds `ake`, `ivision`, `esno` after empty entries. A reader that takes
/// the whole field reports a value label named `ake` on a variable that has
/// none.
///
/// It applies to `str#` **data cells** as well as to metadata: `strl.dta` stores
/// the `str5` value `"abc"` as `61 62 63 00 6f`, and the `6f` is stale.
#[inline]
#[must_use]
pub fn until_nul(field: &[u8]) -> &[u8] {
    match field.iter().position(|&b| b == 0) {
        Some(i) => &field[..i],
        None => field,
    }
}

/// Windows-1252. 0x00..=0x7F and 0xA0..=0xFF are Latin-1; 0x80..=0x9F is the
/// typography block, with five positions the standard leaves undefined.
static CP1252: [char; 256] = build_cp1252();

/// ISO-8859-1: byte value *is* the code point, for all 256.
static LATIN1: [char; 256] = build_latin1();

const fn build_latin1() -> [char; 256] {
    let mut t = ['\0'; 256];
    let mut i = 0usize;
    while i < 256 {
        // Every u8 is a valid code point in Latin-1 by definition.
        t[i] = match char::from_u32(i as u32) {
            Some(c) => c,
            None => char::REPLACEMENT_CHARACTER,
        };
        i += 1;
    }
    t
}

const fn build_cp1252() -> [char; 256] {
    let mut t = build_latin1();
    // The 0x80..=0x9F block, from the Unicode Consortium's CP1252.TXT. The five
    // gaps (0x81, 0x8D, 0x8F, 0x90, 0x9D) keep the U+FFFD that `build_latin1`
    // did not put there, so they are set explicitly below rather than left to
    // chance.
    t[0x80] = '\u{20AC}';
    t[0x81] = char::REPLACEMENT_CHARACTER;
    t[0x82] = '\u{201A}';
    t[0x83] = '\u{0192}';
    t[0x84] = '\u{201E}';
    t[0x85] = '\u{2026}';
    t[0x86] = '\u{2020}';
    t[0x87] = '\u{2021}';
    t[0x88] = '\u{02C6}';
    t[0x89] = '\u{2030}';
    t[0x8A] = '\u{0160}';
    t[0x8B] = '\u{2039}';
    t[0x8C] = '\u{0152}';
    t[0x8D] = char::REPLACEMENT_CHARACTER;
    t[0x8E] = '\u{017D}';
    t[0x8F] = char::REPLACEMENT_CHARACTER;
    t[0x90] = char::REPLACEMENT_CHARACTER;
    t[0x91] = '\u{2018}';
    t[0x92] = '\u{2019}';
    t[0x93] = '\u{201C}';
    t[0x94] = '\u{201D}';
    t[0x95] = '\u{2022}';
    t[0x96] = '\u{2013}';
    t[0x97] = '\u{2014}';
    t[0x98] = '\u{02DC}';
    t[0x99] = '\u{2122}';
    t[0x9A] = '\u{0161}';
    t[0x9B] = '\u{203A}';
    t[0x9C] = '\u{0153}';
    t[0x9D] = char::REPLACEMENT_CHARACTER;
    t[0x9E] = '\u{017E}';
    t[0x9F] = '\u{0178}';
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A27's "codepage tables with out-of-range indices", answered
    /// structurally: there is no index a byte can produce that the table does
    /// not have.
    #[test]
    fn the_single_byte_decoders_are_total() {
        let all: Vec<u8> = (0..=255u8).collect();
        for enc in [Encoding::Windows1252, Encoding::Latin1] {
            let (s, stat) = decode(&all, enc);
            assert_eq!(s.chars().count(), 256, "{enc:?} dropped or merged a byte");
            let expected = if enc == Encoding::Windows1252 { 5 } else { 0 };
            assert_eq!(stat.replacements, expected, "{enc:?}");
        }
    }

    #[test]
    fn cp1252_typography_block_is_right() {
        let (s, _) = decode(&[0x80, 0x92, 0x99, 0x9F, 0xE9], Encoding::Windows1252);
        assert_eq!(s, "€’™Ÿé");
        let (s, _) = decode(&[0x92], Encoding::Latin1);
        assert_eq!(s, "\u{92}");
    }

    #[test]
    fn undefined_positions_replace_and_count() {
        let (s, stat) = decode(&[0x81, b'a', 0x9D], Encoding::Windows1252);
        assert_eq!(s, "\u{FFFD}a\u{FFFD}");
        assert_eq!(stat.replacements, 2);
    }

    #[test]
    fn invalid_utf8_replaces_and_counts_rather_than_refusing() {
        let (s, stat) = decode(&[b'o', 0xFF, 0xFE, b'k'], Encoding::Utf8);
        assert!(s.starts_with('o') && s.ends_with('k'));
        assert_eq!(stat.replacements, 2);
        // Valid UTF-8 is the zero-copy-shaped path and counts nothing.
        let (s, stat) = decode("héllo".as_bytes(), Encoding::Utf8);
        assert_eq!((s.as_str(), stat.replacements), ("héllo", 0));
    }

    #[test]
    fn single_byte_encode_round_trips_and_refuses_what_it_cannot_say() {
        let bytes = encode("café — ok", Encoding::Windows1252, "label").unwrap();
        let (back, stat) = decode(&bytes, Encoding::Windows1252);
        assert_eq!(back, "café — ok");
        assert_eq!(stat.replacements, 0);
        let e = encode("日本語", Encoding::Windows1252, "variable label of x").unwrap_err();
        assert!(matches!(e, DtaError::NotRepresentable { .. }));
        assert!(e.to_string().contains("variable label of x"));
    }

    /// `04` §0.2 trap 2, on the exact bytes the committed fixtures contain.
    #[test]
    fn until_nul_discards_the_stale_remainder() {
        // auto.dta `formats[0]`: "%-18s" then NUL then another variable's
        // format left over in the padding.
        let field = b"%-18s\0\0\0\0\0\0\0%8.0gc\0";
        assert_eq!(until_nul(field), b"%-18s");
        // auto.dta `value_label_names[0]`: empty, with "ake" behind the NUL.
        assert_eq!(until_nul(b"\0ake\0n\0"), b"");
        // strl.dta's str5 cell: "abc" with a stale 'o' in the padding.
        assert_eq!(until_nul(b"abc\0o"), b"abc");
        // A field with no NUL is the whole field — a str5 holding exactly five
        // characters carries no terminator.
        assert_eq!(until_nul(b"abcde"), b"abcde");
    }
}
