//! `strL` — GSO `(v, o)` packing per release, and the write-side dedup pass.
//!
//! [`StrLCol`] and [`StrLChunk`] are storage and live in
//! [`column`](crate::column)/[`chunk`](crate::chunk) (W02). What lives here is
//! everything about how that storage becomes, and comes from, the eight bytes a
//! `.dta` `<data>` section stores for a `strL` cell.
//!
//! # The split differs by release, and getting it wrong is silent
//!
//! `04` §9.5, measured on real files:
//!
//! | release | data-section 8 bytes | GSO record header |
//! |---|---|---|
//! | 117 | `v: u32` (low 4), `o: u32` (high 4) | `"GSO"` + `v: u32` + `o: u32` |
//! | 118 | `v: u16` (low 2), `o: u48` (high 6) | `"GSO"` + `v: u32` + `o: u64` |
//! | 119 | `v: u24` (low 3), `o: u40` (high 5) | `"GSO"` + `v: u32` + `o: u64` |
//!
//! 119 is the one a reasonable person guesses wrong, and the evidence is a
//! 39,981-variable file: cell `0x0000_0000_0100_9C2D` unpacks as `v = 39981,
//! o = 1` under low-3/high-5 and as `v = 39981, o = 256` under 118's
//! low-2/high-6. Only the first names a GSO record that exists. There is no
//! diagnostic for choosing wrong — the reader simply returns other rows'
//! strings — so [`unpack`] is total and [`pack`] refuses out-of-range inputs
//! rather than truncating them.
//!
//! # Why the release is a number and not an enum
//!
//! `stratum-dta` (W03) owns `ReleaseSpec`, the authority on every *other*
//! per-release width. A second enum here would be exactly the twin A10 bans, so
//! this module is parameterised by the release number itself and
//! [`StrLPacking::for_release`] answers `None` for anything outside 117/118/119
//! (`04` §9.6: v1 reads those three only).
//!
//! # Dedup is a property of the file format, not an optimisation
//!
//! `04` §10.4: Stata coalesces identical `strL` values, so four identical cells
//! become two GSO records. [`GsoPlan::build`] reproduces that — including the
//! ordering rule, ascending by `(o, v)` — because a writer that emits one record
//! per cell produces a file that is *valid but not shaped like Stata's*, and
//! `11.3`'s byte-identity metric would then never be reportable.

use rustc_hash::FxHashMap;

use crate::chunk::{chunk_of, offset_in_chunk};
use crate::column::{Column, StrLCol};

/// GSO type byte for a text value: content carries a trailing NUL, counted in
/// the record's length (`04` §9.5).
pub const GSO_TYPE_TEXT: u8 = 130;

/// GSO type byte for a binary blob: no terminator.
pub const GSO_TYPE_BINARY: u8 = 129;

/// How one release splits the 8-byte cell into `(v, o)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrLPacking {
    /// The `.dta` release this describes: 117, 118 or 119.
    pub release: u16,
    /// Bytes of the low half that hold `v`. `o` takes the remaining `8 - v_bytes`.
    pub v_bytes: u8,
}

impl StrLPacking {
    /// The packing for a release, or `None` for one v1 does not read.
    #[must_use]
    pub const fn for_release(release: u16) -> Option<StrLPacking> {
        let v_bytes = match release {
            117 => 4,
            118 => 2,
            119 => 3,
            _ => return None,
        };
        Some(StrLPacking { release, v_bytes })
    }

    /// The largest `v` this release can name.
    #[must_use]
    pub const fn max_v(self) -> u32 {
        // `v_bytes` is 2, 3 or 4, so the shift is always < 32 and the `u64`
        // intermediate never loses the 4-byte case.
        ((1u64 << (self.v_bytes as u32 * 8)) - 1) as u32
    }

    /// The largest `o` this release can name.
    #[must_use]
    pub const fn max_o(self) -> u64 {
        let bits = (8 - self.v_bytes as u32) * 8;
        if bits >= 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        }
    }
}

/// A `(v, o)` pair as stored in a cell: a *name* for a GSO record, not a
/// coordinate. `04` §10.4 — several cells may name one record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Vo {
    /// 1-based variable number, or `0` for the empty string.
    pub v: u32,
    /// 1-based observation number, or `0` for the empty string.
    pub o: u64,
}

impl Vo {
    /// `(0, 0)` — the empty `strL`, which emits no GSO record (`04` §9.5).
    pub const EMPTY: Vo = Vo { v: 0, o: 0 };

    /// Is this the empty-string sentinel?
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.v == 0 && self.o == 0
    }
}

/// Why a `(v, o)` could not be packed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StrLError {
    /// The release is not one of 117/118/119.
    #[error("release {0} has no strL packing; v1 reads 117, 118 and 119")]
    UnknownRelease(u16),
    /// `v` does not fit this release's low half.
    #[error("variable number {v} does not fit release {release}'s {bytes}-byte v field")]
    VarTooLarge {
        /// The offending variable number.
        v: u32,
        /// The release attempted.
        release: u16,
        /// How many bytes that release gives `v`.
        bytes: u8,
    },
    /// `o` does not fit this release's high half.
    #[error("observation number {o} does not fit release {release}'s {bytes}-byte o field")]
    ObsTooLarge {
        /// The offending observation number.
        o: u64,
        /// The release attempted.
        release: u16,
        /// How many bytes that release gives `o`.
        bytes: u8,
    },
}

/// Pack `(v, o)` into the 8 bytes a `<data>` cell stores.
///
/// # Errors
///
/// [`StrLError`] when the release is unknown or either field overflows. It is
/// deliberately not a truncation: a silently wrapped `o` names a record that
/// exists and holds a different string.
pub fn pack(vo: Vo, release: u16) -> Result<u64, StrLError> {
    let p = StrLPacking::for_release(release).ok_or(StrLError::UnknownRelease(release))?;
    if vo.v > p.max_v() {
        return Err(StrLError::VarTooLarge {
            v: vo.v,
            release,
            bytes: p.v_bytes,
        });
    }
    if vo.o > p.max_o() {
        return Err(StrLError::ObsTooLarge {
            o: vo.o,
            release,
            bytes: p.v_bytes,
        });
    }
    Ok(u64::from(vo.v) | (vo.o << (p.v_bytes as u32 * 8)))
}

/// Unpack a `<data>` cell. Total: every 64-bit pattern names some `(v, o)`.
///
/// # Errors
///
/// [`StrLError::UnknownRelease`].
pub fn unpack(raw: u64, release: u16) -> Result<Vo, StrLError> {
    let p = StrLPacking::for_release(release).ok_or(StrLError::UnknownRelease(release))?;
    let shift = p.v_bytes as u32 * 8;
    Ok(Vo {
        v: (raw & u64::from(p.max_v())) as u32,
        o: raw >> shift,
    })
}

/// One GSO record as it appears in the `<strls>` block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GsoRecord {
    /// The `(v, o)` that names it.
    pub vo: Vo,
    /// [`GSO_TYPE_TEXT`] or [`GSO_TYPE_BINARY`].
    pub ty: u8,
    /// The content **without** the type-130 trailing NUL; the writer appends it
    /// and `len` on the wire is `content.len() + 1` for text.
    pub content: Vec<u8>,
}

impl GsoRecord {
    /// The `len` field the `<strls>` block carries: text counts its NUL.
    #[must_use]
    pub fn wire_len(&self) -> u32 {
        let n = self.content.len() as u32;
        if self.ty == GSO_TYPE_TEXT {
            n + 1
        } else {
            n
        }
    }
}

/// The result of the coalescing pass: one `(v, o)` per cell plus the deduplicated
/// records those cells name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GsoPlan {
    /// `cells[c][row]` for the `c`-th `strL` column handed to [`build`](Self::build),
    /// in the order they were handed over.
    pub cells: Vec<Vec<Vo>>,
    /// The records to emit, already ordered ascending by `(o, v)` (`04` §9.5).
    pub records: Vec<GsoRecord>,
}

impl GsoPlan {
    /// Run `04` §10.4's writer algorithm over the `strL` columns of a frame.
    ///
    /// `cols` is `(variable_number_1_based, column)` in the order the `<data>`
    /// section will write them. Empty cells become [`Vo::EMPTY`] and emit no
    /// record; every other distinct `(type, content)` is named by the `(v, o)`
    /// of the first cell that held it, scanning column-major exactly as `04`
    /// §10.4 spells it — the order decides which `(v, o)` a record gets, and a
    /// row-major scan would produce a valid file that does not match Stata's.
    #[must_use]
    pub fn build(cols: &[(u32, &StrLCol)]) -> GsoPlan {
        // Content hash -> the candidates that hashed there. Collisions are
        // resolved by full comparison (`04` §10.4's accepted tradeoff), so a
        // hash collision costs a memcmp and never a wrong string.
        let mut dedup: FxHashMap<u64, Vec<usize>> = FxHashMap::default();
        let mut records: Vec<GsoRecord> = Vec::new();
        let mut cells: Vec<Vec<Vo>> = Vec::with_capacity(cols.len());

        for &(v, col) in cols {
            let mut this: Vec<Vo> = Vec::with_capacity(col.len() as usize);
            for row in 0..col.len() {
                let bytes = col.get(row);
                if bytes.is_empty() {
                    this.push(Vo::EMPTY);
                    continue;
                }
                let ty = if col.chunk(chunk_of(row)).is_binary(offset_in_chunk(row)) {
                    GSO_TYPE_BINARY
                } else {
                    GSO_TYPE_TEXT
                };
                let h = content_hash(ty, bytes);
                let slot = dedup.entry(h).or_default();
                let hit = slot
                    .iter()
                    .copied()
                    .find(|&i| records[i].ty == ty && records[i].content == bytes);
                match hit {
                    Some(i) => this.push(records[i].vo),
                    None => {
                        let vo = Vo { v, o: row + 1 };
                        slot.push(records.len());
                        records.push(GsoRecord {
                            vo,
                            ty,
                            content: bytes.to_vec(),
                        });
                        this.push(vo);
                    }
                }
            }
            cells.push(this);
        }

        // "GSO records are ordered ascending by `o`, then by `v`" (`04` §9.5).
        records.sort_by_key(|r| (r.vo.o, r.vo.v));
        GsoPlan { cells, records }
    }

    /// The plan's cells packed for `release`, column by column.
    ///
    /// # Errors
    ///
    /// [`StrLError`] from [`pack`] — an unknown release, or a `(v, o)` this
    /// release cannot name.
    pub fn packed(&self, release: u16) -> Result<Vec<Vec<u64>>, StrLError> {
        self.cells
            .iter()
            .map(|col| col.iter().map(|&vo| pack(vo, release)).collect())
            .collect()
    }
}

/// blake3 over the type byte and the content.
///
/// Not a security boundary and not persisted; it is the bucket key for a map
/// whose collisions are resolved by comparison. blake3 rather than `FxHash`
/// because the inputs are whole `strL` payloads (megabytes are legal) where a
/// word-at-a-time non-cryptographic hash has no advantage worth its collision
/// rate.
fn content_hash(ty: u8, bytes: &[u8]) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(&[ty]);
    h.update(bytes);
    let mut out = [0u8; 8];
    h.finalize_xof().fill(&mut out);
    u64::from_le_bytes(out)
}

/// The `strL` columns of a frame's column list, numbered 1-based as the `<data>`
/// section numbers them.
///
/// Convenience for a writer that has `&[Column]` and wants the `(v, col)` pairs
/// [`GsoPlan::build`] takes. `v` is the **variable position in the file**, which
/// is the position in `cols` — not a position among the `strL` columns only.
#[must_use]
pub fn strl_columns(cols: &[Column]) -> Vec<(u32, &StrLCol)> {
    cols.iter()
        .enumerate()
        .filter_map(|(i, c)| match c {
            Column::StrL(s) => Some((i as u32 + 1, s)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(values: &[(&[u8], bool)]) -> StrLCol {
        let mut c = StrLCol::empty(values.len() as u64);
        for (i, (v, bin)) in values.iter().enumerate() {
            let row = i as u64;
            c.chunk_mut(chunk_of(row))
                .set(offset_in_chunk(row), v, *bin);
        }
        c
    }

    #[test]
    fn release_119_unpacks_the_measured_cell() {
        // `04` §9.5's evidence, verbatim: the 39,981-variable file.
        let raw = 0x0000_0000_0100_9C2Du64;
        assert_eq!(unpack(raw, 119).unwrap(), Vo { v: 39_981, o: 1 });
        // The 118 reading of the same bytes names a record that does not exist.
        assert_eq!(unpack(raw, 118).unwrap(), Vo { v: 39_981, o: 256 });
        assert_ne!(unpack(raw, 119).unwrap(), unpack(raw, 118).unwrap());
    }

    #[test]
    fn release_118_unpacks_the_measured_cell() {
        // `strl118.dta`: cell 0x00010002 -> v = 2, o = 1.
        assert_eq!(unpack(0x0001_0002, 118).unwrap(), Vo { v: 2, o: 1 });
    }

    #[test]
    fn packing_round_trips_on_every_supported_release() {
        for release in [117u16, 118, 119] {
            for vo in [Vo { v: 1, o: 1 }, Vo { v: 39_981, o: 1 }, Vo::EMPTY] {
                let raw = pack(vo, release).expect("in range");
                assert_eq!(unpack(raw, release).unwrap(), vo, "release {release}");
            }
        }
    }

    #[test]
    fn an_out_of_range_field_is_an_error_and_never_a_truncation() {
        // 118 gives `v` two bytes, so 65 536 variables is one too many. (39,981
        // — the file that discriminates 118 from 119 — still *fits* u16; what
        // breaks there is `o`, which is why that case is a wrong answer rather
        // than an error and why the release table has to be right.)
        let e = pack(Vo { v: 65_536, o: 1 }, 118).expect_err("v overflows u16");
        assert!(matches!(e, StrLError::VarTooLarge { v: 65_536, .. }));
        // 119 gives `v` three bytes, so the same variable number is fine there.
        assert!(pack(Vo { v: 65_536, o: 1 }, 119).is_ok());
        // 117 gives `o` four bytes.
        let e = pack(
            Vo {
                v: 1,
                o: u64::from(u32::MAX) + 1,
            },
            117,
        )
        .expect_err("o overflows u32");
        assert!(matches!(e, StrLError::ObsTooLarge { .. }));
        assert!(matches!(
            pack(Vo::EMPTY, 116),
            Err(StrLError::UnknownRelease(116))
        ));
    }

    #[test]
    fn field_widths_match_the_measured_table() {
        for (release, v_bytes, max_v) in [
            (117u16, 4u8, u32::MAX),
            (118, 2, 0xFFFF),
            (119, 3, 0x00FF_FFFF),
        ] {
            let p = StrLPacking::for_release(release).expect("supported");
            assert_eq!(p.v_bytes, v_bytes);
            assert_eq!(p.max_v(), max_v);
        }
        assert_eq!(
            StrLPacking::for_release(117).unwrap().max_o(),
            u64::from(u32::MAX)
        );
        assert_eq!(StrLPacking::for_release(120), None);
    }

    #[test]
    fn four_identical_cells_become_two_records_ordered_by_o_then_v() {
        // `04` §10.4's measured example: rows 1, 2 and 4 are "dup", row 3 is
        // "other".
        let c = col(&[
            (b"dup", false),
            (b"dup", false),
            (b"other", false),
            (b"dup", false),
        ]);
        let plan = GsoPlan::build(&[(1, &c)]);
        assert_eq!(
            plan.cells[0],
            vec![
                Vo { v: 1, o: 1 },
                Vo { v: 1, o: 1 },
                Vo { v: 1, o: 3 },
                Vo { v: 1, o: 1 },
            ]
        );
        assert_eq!(plan.records.len(), 2);
        assert_eq!(plan.records[0].vo, Vo { v: 1, o: 1 });
        assert_eq!(plan.records[0].content, b"dup");
        assert_eq!(plan.records[1].vo, Vo { v: 1, o: 3 });
        // Type 130 counts its terminator in `len`; the arena does not store it.
        assert_eq!(plan.records[0].wire_len(), 4);
    }

    #[test]
    fn the_empty_cell_emits_no_record() {
        let c = col(&[(b"", false), (b"x", false), (b"", false)]);
        let plan = GsoPlan::build(&[(1, &c)]);
        assert_eq!(plan.cells[0][0], Vo::EMPTY);
        assert_eq!(plan.cells[0][2], Vo::EMPTY);
        assert_eq!(plan.records.len(), 1);
        assert_eq!(pack(Vo::EMPTY, 118).unwrap(), 0);
    }

    #[test]
    fn identical_bytes_with_different_types_are_different_records() {
        // Type is part of the dedup key: a binary blob and a string that happen
        // to share bytes must round-trip to their own types.
        let c = col(&[(b"same", false), (b"same", true)]);
        let plan = GsoPlan::build(&[(1, &c)]);
        assert_eq!(plan.records.len(), 2);
        assert_eq!(plan.records[0].ty, GSO_TYPE_TEXT);
        assert_eq!(plan.records[1].ty, GSO_TYPE_BINARY);
        assert_eq!(plan.records[1].wire_len(), 4);
    }

    #[test]
    fn dedup_crosses_columns_and_keeps_the_first_naming() {
        let a = col(&[(b"", false), (b"shared", false)]);
        let b = col(&[(b"shared", false), (b"", false)]);
        let plan = GsoPlan::build(&[(1, &a), (2, &b)]);
        // Column-major: (1,2) is reached before (2,1), so it names the record.
        assert_eq!(plan.cells[0][1], Vo { v: 1, o: 2 });
        assert_eq!(plan.cells[1][0], Vo { v: 1, o: 2 });
        assert_eq!(plan.records.len(), 1);
    }
}
