//! `strL` storage: the `(v,o)` key packing, the read-side GSO index, and the
//! write-side dedup pass.
//!
//! A `strL` cell in `<data>` is **8 bytes holding a packed `(v, o)` key**, not
//! text. `v` is the 1-based variable index and `o` the 1-based observation of
//! the cell the content was first seen in; together they name a record in
//! `<strls>`. Multiple cells may name the same record — that is how Stata
//! deduplicates (`04` §10.4).
//!
//! # The split differs per release, and getting it wrong is silent
//!
//! | release | data-section 8 bytes | GSO record header |
//! |---|---|---|
//! | 117 | `v: u32` (low 4), `o: u32` (high 4) | `"GSO"` + `v: u32` + `o: u32` |
//! | 118 | `v: u16` (low 2), `o: u48` (high 6) | `"GSO"` + `v: u32` + `o: u64` |
//! | 119 | `v: u24` (low 3), `o: u40` (high 5) | `"GSO"` + `v: u32` + `o: u64` |
//!
//! 119 is the one that is easy to get wrong, because 118's rule *also* produces
//! a plausible answer. `04` §9.5 measured it on a 39 981-variable file with one
//! `strL`:
//!
//! ```text
//! row0 raw = 0x0000000001009C2D
//!   low3 => v = 39981, high5 => o = 1     <- matches GSO v=39981 o=1  CORRECT
//!   low2 => v = 39981, high6 => o = 256   <- no such GSO             WRONG
//! ```
//!
//! Both readings give the same `v`. Only `o` discriminates, and only against a
//! file wide enough that `v` spills into the third byte. That measurement is
//! [`tests::the_119_split_is_the_measured_one`] and
//! `tests/golden_releases.rs`, and it is the reason this module exists at all
//! rather than being four lines inside `reader.rs`.

use rustc_hash::FxHashMap;

use crate::spec::Release;
use crate::DtaError;

/// The reserved key. `(0,0)` means the empty string and **emits no GSO record**
/// (`04` §9.5, measured).
pub const EMPTY_KEY: u64 = 0;

/// GSO record type byte: a binary blob, stored with no terminator.
pub const GSO_BINARY: u8 = 129;
/// GSO record type byte: a string, whose stored length includes a trailing NUL.
pub const GSO_STRING: u8 = 130;

/// How many low bits of the packed key hold `v`, for this release.
#[must_use]
const fn v_bits(release: Release) -> u32 {
    (release.spec().gso_v_bytes as u32) * 8
}

/// The largest `v` this release's packing can address.
#[must_use]
pub const fn max_v(release: Release) -> u64 {
    (1u64 << v_bits(release)) - 1
}

/// The largest `o` this release's packing can address.
#[must_use]
pub const fn max_o(release: Release) -> u64 {
    let bits = 64 - v_bits(release);
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// Split the 8 data-section bytes into `(v, o)`.
///
/// Total by construction: every `u64` splits. Validation of the *result* — `v`
/// past `K`, the reserved `(0,0)`, a key naming no record — belongs to the
/// caller, which knows `K` and holds the index.
#[inline]
#[must_use]
pub fn unpack_vo(raw: u64, release: Release) -> (u32, u64) {
    let bits = v_bits(release);
    let mask = (1u64 << bits) - 1;
    ((raw & mask) as u32, raw >> bits)
}

/// Build the 8 data-section bytes from `(v, o)`.
///
/// # Errors
///
/// [`DtaError::GsoKeyRange`] when either component does not fit this release's
/// field. That is not a hypothetical: a 40 000-variable dataset written as 118
/// would silently alias `v = 40000` onto `v = 40000 - 65536`, so the writer must
/// be told to pick 119 instead (`04` §10.1).
pub fn pack_vo(v: u32, o: u64, release: Release) -> Result<u64, DtaError> {
    if u64::from(v) > max_v(release) || o > max_o(release) {
        return Err(DtaError::GsoKeyRange {
            release: release.number(),
            v,
            o,
        });
    }
    Ok(u64::from(v) | (o << v_bits(release)))
}

// ---------------------------------------------------------------------------
// Read side
// ---------------------------------------------------------------------------

/// One record's payload, as a range **into the source buffer**. Nothing is
/// copied while the index is built; the bytes are lifted only when the column
/// arena is filled (`04` §9.4 step 10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GsoEntry {
    /// Byte range of the content inside the buffer the index was built from,
    /// with the type-130 trailing NUL already excluded.
    pub start: usize,
    /// One past the last content byte.
    pub end: usize,
    /// GSO type 129 — a blob, not text.
    pub binary: bool,
}

/// `(v,o)` → record, for one file.
#[derive(Debug, Default)]
pub struct GsoTable {
    by_key: FxHashMap<u64, GsoEntry>,
    /// Records whose `(v,o)` repeated. First wins; this is the count for the
    /// read report.
    pub duplicates: u32,
}

impl GsoTable {
    /// Records in the table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// True when the file had no `strL` content.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// Look one up by its packed key.
    #[must_use]
    pub fn get(&self, key: u64) -> Option<&GsoEntry> {
        self.by_key.get(&key)
    }

    /// Scan a `<strls>` payload into an index.
    ///
    /// `block` is the payload with both tags already stripped; `base` is its
    /// offset inside `buf`, so the ranges in the result index `buf` rather than
    /// `block`. `n_vars` bounds `v`.
    ///
    /// Every length in the record header is checked against the bytes remaining
    /// **in this block** before it is used, so a `len` of `0xFFFF_FFFF` costs a
    /// comparison and not a 4 GB allocation (THREAT_MODEL §2.4 G1).
    ///
    /// # Errors
    ///
    /// [`DtaError`] — `GsoTruncated`, `GsoReservedKey`, `GsoVar`, `GsoType`, or
    /// `GsoKeyRange`. Never a panic: this function is `fuzz_gso`'s entry point.
    pub fn scan(
        block: &[u8],
        base: usize,
        release: Release,
        n_vars: u32,
    ) -> Result<Self, DtaError> {
        let o_width = usize::from(release.spec().gso_hdr_o_width);
        // "GSO" + v:u32 + o:{u32|u64} + type:u8 + len:u32
        let header = 3 + 4 + o_width + 1 + 4;
        let mut by_key: FxHashMap<u64, GsoEntry> = FxHashMap::default();
        let mut duplicates = 0u32;
        let mut p = 0usize;

        while p < block.len() {
            if block.len() - p < header {
                return Err(DtaError::GsoTruncated {
                    at: (base + p) as u64,
                    need: header as u64,
                    have: (block.len() - p) as u64,
                });
            }
            if &block[p..p + 3] != b"GSO" {
                return Err(DtaError::GsoTag {
                    at: (base + p) as u64,
                });
            }
            let v = u32::from_le_bytes(block[p + 3..p + 7].try_into().expect("4 bytes"));
            let o = read_uint(&block[p + 7..p + 7 + o_width]);
            let ty = block[p + 7 + o_width];
            let len_at = p + 8 + o_width;
            let len =
                u32::from_le_bytes(block[len_at..len_at + 4].try_into().expect("4 bytes")) as usize;

            if v == 0 || o == 0 {
                return Err(DtaError::GsoReservedKey {
                    at: (base + p) as u64,
                });
            }
            if v > n_vars {
                return Err(DtaError::GsoVar {
                    at: (base + p) as u64,
                    v,
                    n_vars,
                });
            }
            let binary = match ty {
                GSO_BINARY => true,
                GSO_STRING => false,
                _ => {
                    return Err(DtaError::GsoType {
                        at: (base + p) as u64,
                        ty,
                    })
                }
            };

            let body = p + header;
            // THE allocation guard. `len` is attacker-controlled; it is compared
            // against what is left of the block before it indexes anything.
            if block.len() - body < len {
                return Err(DtaError::GsoTruncated {
                    at: (base + body) as u64,
                    need: len as u64,
                    have: (block.len() - body) as u64,
                });
            }

            // Type 130 stores the terminator inside `len`; type 129 does not.
            // The trim is conditional, never an unconditional `len - 1`
            // (THREAT_MODEL §2.4 G8).
            let mut end = body + len;
            if !binary && end > body && block[end - 1] == 0 {
                end -= 1;
            }

            let key = pack_vo(v, o, release)?;
            // FIRST wins, deliberately. Last-wins would make the meaning of a
            // cell depend on a record that appears *after* it, which is how a
            // malformed file gets to choose which string a reader returns
            // (THREAT_MODEL §2.4 G6).
            match by_key.entry(key) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    duplicates = duplicates.saturating_add(1);
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(GsoEntry {
                        start: base + body,
                        end: base + end,
                        binary,
                    });
                }
            }
            p = body + len;
        }

        Ok(Self { by_key, duplicates })
    }
}

/// Read a 4- or 8-byte little-endian unsigned integer.
fn read_uint(src: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    b[..src.len()].copy_from_slice(src);
    u64::from_le_bytes(b)
}

// ---------------------------------------------------------------------------
// Write side
// ---------------------------------------------------------------------------

/// One record to emit into `<strls>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GsoOut {
    /// 1-based variable index of the first cell holding this content.
    pub v: u32,
    /// 1-based observation of that cell.
    pub o: u64,
    /// GSO type 129 rather than 130.
    pub binary: bool,
    /// Content, without the type-130 terminator (the writer appends it).
    pub content: Vec<u8>,
}

/// The result of the dedup pass: one packed key per `strL` cell, plus the
/// records to write.
#[derive(Debug, Default)]
pub struct GsoPlan {
    /// `cells[i]` is the packed key for the i-th cell fed to [`GsoPlanner`], in
    /// feed order.
    pub cells: Vec<u64>,
    /// Records, **sorted ascending by `(o, v)`** — the order Stata writes and
    /// therefore the order we write (`04` §9.5).
    pub records: Vec<GsoOut>,
}

/// Builds a [`GsoPlan`] from cells fed in column-major order.
///
/// Stata's rule, measured in `04` §10.4: the `(v,o)` pair is an *identifier*
/// sourced from the first cell holding that content, and every later cell with
/// the same content reuses it. Four cells, two distinct values, two records.
///
/// The dedup key is `(binary, content)` — the type byte is part of it, because a
/// blob and a string that happen to share bytes are different values and must
/// round-trip as different values.
///
/// **Cost, accepted (`04` §10.4):** one hash plus one full comparison per
/// `strL` cell on write. Cheap against the I/O, and it makes our files the same
/// shape as Stata's, which is what lets `canonical_eq` be an equality rather
/// than an approximation.
#[derive(Debug, Default)]
pub struct GsoPlanner {
    seen: FxHashMap<(bool, Vec<u8>), (u32, u64)>,
    plan: GsoPlan,
}

impl GsoPlanner {
    /// A planner with room for `cells` keys.
    #[must_use]
    pub fn with_capacity(cells: usize) -> Self {
        Self {
            seen: FxHashMap::default(),
            plan: GsoPlan {
                cells: Vec::with_capacity(cells),
                records: Vec::new(),
            },
        }
    }

    /// Feed one cell. `v` and `o` are 1-based.
    ///
    /// `coalesce` off gives every non-empty cell its own record, keyed on its
    /// own position. That is not what Stata writes and is not the default; it
    /// exists so a test can hold the *only* difference between two files to the
    /// dedup and count what the dedup saved
    /// ([`crate::Counters::gso_records_written`]).
    ///
    /// # Errors
    ///
    /// [`DtaError::GsoKeyRange`] when `(v,o)` does not fit the release's key
    /// packing — i.e. when the writer chose a release too narrow for the data.
    pub fn push(
        &mut self,
        v: u32,
        o: u64,
        content: &[u8],
        binary: bool,
        release: Release,
        coalesce: bool,
    ) -> Result<(), DtaError> {
        if content.is_empty() {
            self.plan.cells.push(EMPTY_KEY);
            return Ok(());
        }
        let hit = if coalesce {
            self.seen.get(&(binary, content.to_vec())).copied()
        } else {
            None
        };
        let (rv, ro) = match hit {
            Some(hit) => hit,
            None => {
                if coalesce {
                    self.seen.insert((binary, content.to_vec()), (v, o));
                }
                self.plan.records.push(GsoOut {
                    v,
                    o,
                    binary,
                    content: content.to_vec(),
                });
                (v, o)
            }
        };
        self.plan.cells.push(pack_vo(rv, ro, release)?);
        Ok(())
    }

    /// Finish, sorting the records into `(o, v)` order.
    #[must_use]
    pub fn finish(mut self) -> GsoPlan {
        self.plan.records.sort_by_key(|r| (r.o, r.v));
        self.plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `04` §9.5, verbatim. This is the constant that discriminates 119 from
    /// 118 and it came off a real file; it is not derived from the table above.
    #[test]
    fn the_119_split_is_the_measured_one() {
        let raw = 0x0000_0000_0100_9C2Du64;
        assert_eq!(unpack_vo(raw, Release::R119), (39_981, 1));
        // The reading a 118-shaped reader would produce for the same bytes.
        // Same `v`; only `o` tells them apart, and there is no GSO record 256.
        assert_eq!(unpack_vo(raw, Release::R118), (39_981, 256));
        assert_eq!(pack_vo(39_981, 1, Release::R119).unwrap(), raw);
    }

    /// `04` §9.5, from `strl118.dta`: cell raw `0x00010002` -> `v=2, o=1`.
    #[test]
    fn the_118_split_is_the_measured_one() {
        assert_eq!(unpack_vo(0x0001_0002, Release::R118), (2, 1));
        assert_eq!(pack_vo(2, 1, Release::R118).unwrap(), 0x0001_0002);
    }

    #[test]
    fn packing_round_trips_across_releases() {
        for release in Release::ALL {
            for (v, o) in [(1u32, 1u64), (7, 3), (255, 1024), (1, max_o(release))] {
                let raw = pack_vo(v, o, release).unwrap();
                assert_eq!(unpack_vo(raw, release), (v, o), "{release:?} ({v},{o})");
            }
            assert!(pack_vo(1, max_o(release) + 1, release).is_err());
            let too_wide = u32::try_from(max_v(release) + 1);
            if let Ok(v) = too_wide {
                assert!(pack_vo(v, 1, release).is_err());
            }
        }
    }

    #[test]
    fn field_widths_match_the_table() {
        assert_eq!(max_v(Release::R117), u64::from(u32::MAX));
        assert_eq!(max_v(Release::R118), 0xFFFF);
        assert_eq!(max_v(Release::R119), 0x00FF_FFFF);
        assert_eq!(max_o(Release::R118), 0xFFFF_FFFF_FFFF);
        assert_eq!(max_o(Release::R119), 0xFF_FFFF_FFFF);
    }

    /// `04` §10.4's measured case, exactly: rows 1,2,4 = "dup", row 3 = "other";
    /// four cells, **two** GSO records, ordered by `(o, v)`.
    #[test]
    fn dedup_reproduces_the_measured_stata_shape() {
        let mut p = GsoPlanner::with_capacity(4);
        for (o, s) in [(1u64, "dup"), (2, "dup"), (3, "other"), (4, "dup")] {
            p.push(1, o, s.as_bytes(), false, Release::R118, true)
                .unwrap();
        }
        let plan = p.finish();
        let k11 = pack_vo(1, 1, Release::R118).unwrap();
        let k13 = pack_vo(1, 3, Release::R118).unwrap();
        assert_eq!(plan.cells, vec![k11, k11, k13, k11]);
        assert_eq!(plan.records.len(), 2);
        assert_eq!((plan.records[0].v, plan.records[0].o), (1, 1));
        assert_eq!((plan.records[1].v, plan.records[1].o), (1, 3));
    }

    /// The acceptance bullet's shape: four cells that pair up into two distinct
    /// values produce exactly two records, ordered by `(o, v)`.
    #[test]
    fn four_cells_two_values_emit_two_records_ordered_by_o_then_v() {
        let mut p = GsoPlanner::with_capacity(4);
        // Column-major feed order: var 1 obs 1..2, then var 2 obs 1..2.
        p.push(1, 1, b"beta", false, Release::R118, true).unwrap();
        p.push(1, 2, b"alpha", false, Release::R118, true).unwrap();
        p.push(2, 1, b"alpha", false, Release::R118, true).unwrap();
        p.push(2, 2, b"beta", false, Release::R118, true).unwrap();
        let plan = p.finish();
        assert_eq!(plan.records.len(), 2);
        assert_eq!(
            plan.records.iter().map(|r| (r.o, r.v)).collect::<Vec<_>>(),
            vec![(1, 1), (2, 1)]
        );
        let k11 = pack_vo(1, 1, Release::R118).unwrap();
        let k12 = pack_vo(1, 2, Release::R118).unwrap();
        assert_eq!(plan.cells, vec![k11, k12, k12, k11]);
    }

    #[test]
    fn identical_cells_collapse_to_one_record_and_empty_emits_none() {
        let mut p = GsoPlanner::with_capacity(5);
        for (v, o) in [(1u32, 1u64), (1, 2), (2, 1), (2, 2)] {
            p.push(v, o, b"same", false, Release::R118, true).unwrap();
        }
        p.push(3, 1, b"", false, Release::R118, true).unwrap();
        let plan = p.finish();
        assert_eq!(plan.records.len(), 1);
        assert_eq!(plan.cells[4], EMPTY_KEY);
    }

    /// The binary flag is part of the identity: two cells with the same bytes
    /// but different GSO types are two records, because they round-trip
    /// differently.
    #[test]
    fn binary_and_text_with_the_same_bytes_do_not_dedup_together() {
        let mut p = GsoPlanner::with_capacity(2);
        p.push(1, 1, b"same", false, Release::R118, true).unwrap();
        p.push(1, 2, b"same", true, Release::R118, true).unwrap();
        let plan = p.finish();
        assert_eq!(plan.records.len(), 2);
        assert!(!plan.records[0].binary && plan.records[1].binary);
    }

    /// The block a well-formed 118 file holds, byte for byte, from
    /// `tests/fixtures/dta/strl.dta`.
    #[test]
    fn scan_reads_a_real_118_strls_block() {
        let mut block = Vec::new();
        block.extend_from_slice(b"GSO");
        block.extend_from_slice(&1u32.to_le_bytes());
        block.extend_from_slice(&1u64.to_le_bytes());
        block.push(GSO_STRING);
        block.extend_from_slice(&6u32.to_le_bytes());
        block.extend_from_slice(b"short\0");
        let t = GsoTable::scan(&block, 100, Release::R118, 2).unwrap();
        let e = t.get(pack_vo(1, 1, Release::R118).unwrap()).unwrap();
        // The trailing NUL is inside `len` and outside the content.
        assert_eq!((e.start, e.end, e.binary), (100 + 20, 100 + 25, false));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn a_gso_len_of_four_gigabytes_costs_a_comparison() {
        let mut block = Vec::new();
        block.extend_from_slice(b"GSO");
        block.extend_from_slice(&1u32.to_le_bytes());
        block.extend_from_slice(&1u64.to_le_bytes());
        block.push(GSO_STRING);
        block.extend_from_slice(&u32::MAX.to_le_bytes());
        block.extend_from_slice(b"tiny");
        assert!(matches!(
            GsoTable::scan(&block, 0, Release::R118, 1),
            Err(DtaError::GsoTruncated { .. })
        ));
    }

    #[test]
    fn reserved_and_out_of_range_keys_are_rejected() {
        let rec = |v: u32, o: u64, ty: u8| {
            let mut b = Vec::new();
            b.extend_from_slice(b"GSO");
            b.extend_from_slice(&v.to_le_bytes());
            b.extend_from_slice(&o.to_le_bytes());
            b.push(ty);
            b.extend_from_slice(&1u32.to_le_bytes());
            b.push(0);
            b
        };
        assert!(matches!(
            GsoTable::scan(&rec(0, 1, GSO_STRING), 0, Release::R118, 4),
            Err(DtaError::GsoReservedKey { .. })
        ));
        assert!(matches!(
            GsoTable::scan(&rec(1, 0, GSO_STRING), 0, Release::R118, 4),
            Err(DtaError::GsoReservedKey { .. })
        ));
        assert!(matches!(
            GsoTable::scan(&rec(9, 1, GSO_STRING), 0, Release::R118, 4),
            Err(DtaError::GsoVar { .. })
        ));
        assert!(matches!(
            GsoTable::scan(&rec(1, 1, 7), 0, Release::R118, 4),
            Err(DtaError::GsoType { .. })
        ));
        assert!(matches!(
            GsoTable::scan(b"GS", 0, Release::R118, 4),
            Err(DtaError::GsoTruncated { .. })
        ));
        assert!(matches!(
            GsoTable::scan(&[0u8; 32], 0, Release::R118, 4),
            Err(DtaError::GsoTag { .. })
        ));
    }
}
