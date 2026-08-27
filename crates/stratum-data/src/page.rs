//! `SDP1` — the binary `DataPage` transport (CONTRACTS §8.1).
//!
//! ```text
//! offset   size  field
//! 0        4     magic = b"SDP1"
//! 4        4     header_len : u32
//! 8        H     header : UTF-8 JSON, right-padded with ASCII spaces
//! 8+H      …     column payloads
//! ```
//!
//! The normative bytes are `tests/fixtures/sdp1/auto_40x12.bin`, owned by W00
//! and asserted from both sides: this module must emit it byte-for-byte and
//! W12's `decodeDataPage` must parse it. Neither implementation owns the file,
//! so the two are compared against a third thing rather than against each other.
//!
//! # The four rulings the fixture's README made, implemented here
//!
//! `CONTRACTS.md` §8.1 underdetermines four things that a byte-exact fixture
//! cannot be agnostic about. `tests/fixtures/sdp1/README.md` §2 ruled on them,
//! and those rulings are as normative as §8.1 itself:
//!
//! 1. **The header is compact JSON, right-padded with spaces so the payload
//!    starts 8-aligned.** Not tidiness: §8.1 says the client decodes with typed
//!    array views over one `ArrayBuffer`, and `new Float64Array(buf, off, n)`
//!    throws a `RangeError` unless `off % 8 == 0`. Without the rule, whether a
//!    `num` column is viewable at all depends on how many digits the row count
//!    happens to have. `header_len` counts the padding, and JSON tolerates
//!    trailing whitespace so `JSON.parse` over the raw slice is unaffected.
//! 2. **Every region is aligned to its element** — `f64` data to 8, `u32`
//!    offsets to 4, tags and arenas to 1. Because `8 + H` is 8-aligned, aligning
//!    the *relative* offset aligns the absolute one. Padding bytes are zero and
//!    belong to no region. Columns are laid down in request order, and within a
//!    column in the order §8.1's own table lists the two regions for that kind:
//!    `aux` then `data` for `text`/`blob`, `data` then `aux` for `num`.
//! 3. **The `blob` bitmap is the last `ceil(nrows/8)` bytes of `data`.** Any
//!    other reading leaves it outside every declared extent, which a decoder
//!    cannot bounds-check. Here both the arena length and the bitmap length are
//!    derivable two ways, and [`decode`] checks them against each other.
//! 4. **The payload ends at the last region's end**, so
//!    `file_len == 8 + header_len + payload_len` is checkable — and checked.
//!
//! # Why the JSON is hand-written and hand-parsed
//!
//! `stratum-data` has no JSON dependency (`04` §1.1 rejected one for bulk data
//! and the workspace table has none for this crate), and the header's schema is
//! fixed and tiny. [`encode`] writes it with `write!`; [`decode`] reads exactly
//! the key order §8.1 specifies and rejects anything else, which is stricter
//! than a general parser and is the right strictness for a wire format whose
//! writer is specified down to the space characters.
//!
//! Every length and offset taken from a buffer is checked arithmetic against the
//! measured buffer length before it is used to slice. A `DataPage` may arrive
//! from a cache file or a crash-truncated write, so `decode` is total: it
//! returns [`PageError`], never a panic and never an out-of-bounds read.

use std::fmt::Write as _;

use stratum_proto::{DatasetStateId, PageRequest, VarIdx};

use crate::frame::FrameSnapshot;
use crate::order::OrderRegistry;
use crate::view::{ColumnBlock, DataPage, ViewError};

/// The four magic bytes every `SDP1` buffer starts with.
pub const MAGIC: [u8; 4] = *b"SDP1";

/// The payload must start 8-aligned so a `Float64Array` view over it is legal.
const PAYLOAD_ALIGN: u64 = 8;

/// Build and encode the page a [`PageRequest`] names — the whole path in one
/// call, which is what the asset handler serving
/// `stratum-asset://localhost/frame/{session}/{frame}/page?…` wants.
///
/// # Errors
///
/// [`ViewError`] from building the page; encoding itself cannot fail.
pub fn page(
    snap: &FrameSnapshot,
    req: &PageRequest,
    orders: &OrderRegistry,
) -> Result<Vec<u8>, ViewError> {
    Ok(encode(&DataPage::build(snap, req, orders)?))
}

/// One column's declared extents, relative to the first payload byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Extents {
    off: u64,
    len: u64,
    aux_off: u64,
    aux_len: u64,
}

#[inline]
fn align_to(cursor: u64, align: u64) -> u64 {
    cursor.div_ceil(align) * align
}

/// Lay the payload out, answering each column's extents and the total length.
fn layout(page: &DataPage) -> (Vec<Extents>, u64) {
    let nrows = u64::from(page.nrows);
    let mut cursor = 0u64;
    let mut out = Vec::with_capacity(page.cols.len());
    for c in &page.cols {
        let e = match c {
            ColumnBlock::Text { offsets, bytes, .. } => {
                // `aux` then `data`, §8.1's own order for this kind.
                let aux_off = align_to(cursor, 4);
                let aux_len = offsets.len() as u64 * 4;
                let off = aux_off + aux_len;
                let len = bytes.len() as u64;
                cursor = off + len;
                Extents {
                    off,
                    len,
                    aux_off,
                    aux_len,
                }
            }
            ColumnBlock::Blob {
                offsets,
                bytes,
                binary,
                ..
            } => {
                let aux_off = align_to(cursor, 4);
                let aux_len = offsets.len() as u64 * 4;
                let off = aux_off + aux_len;
                // The bitmap lives INSIDE `data` (README §2.3), so `len` covers
                // the arena and the bitmap together.
                let len = bytes.len() as u64 + binary.len() as u64;
                cursor = off + len;
                Extents {
                    off,
                    len,
                    aux_off,
                    aux_len,
                }
            }
            ColumnBlock::Num { .. } => {
                // `data` then `aux`, and the `f64` block carries the alignment.
                let off = align_to(cursor, PAYLOAD_ALIGN);
                let len = nrows * 8;
                let aux_off = off + len;
                let aux_len = nrows;
                cursor = aux_off + aux_len;
                Extents {
                    off,
                    len,
                    aux_off,
                    aux_len,
                }
            }
        };
        out.push(e);
    }
    (out, cursor)
}

/// Serialise a page to `SDP1`.
///
/// Infallible: every bound a `DataPage` could violate was already checked when
/// it was built ([`crate::view::ViewError::ArenaTooLarge`],
/// [`crate::view::ViewError::TooManyRows`]).
#[must_use]
pub fn encode(page: &DataPage) -> Vec<u8> {
    let (ext, payload_len) = layout(page);

    let mut json = String::with_capacity(64 + page.cols.len() * 96);
    // Compact: no space after `:` or `,`, keys in exactly §8.1's order.
    write!(
        json,
        "{{\"state\":{},\"row0\":{},\"nrows\":{},\"seq\":{},\"cols\":[",
        page.state.0, page.row0, page.nrows, page.seq
    )
    .expect("writing to a String cannot fail");
    for (i, (c, e)) in page.cols.iter().zip(&ext).enumerate() {
        if i > 0 {
            json.push(',');
        }
        write!(
            json,
            "{{\"idx\":{},\"kind\":\"{}\",\"off\":{},\"len\":{},\"aux_off\":{},\"aux_len\":{}}}",
            c.idx().0,
            c.kind(),
            e.off,
            e.len,
            e.aux_off,
            e.aux_len
        )
        .expect("writing to a String cannot fail");
    }
    json.push_str("]}");

    // `header_len` counts the padding, and `8 + header_len` is what the payload
    // starts at, so the padding is computed against `8 + json.len()`.
    let pad = (PAYLOAD_ALIGN - ((8 + json.len() as u64) % PAYLOAD_ALIGN)) % PAYLOAD_ALIGN;
    let header_len = json.len() as u64 + pad;

    let mut out = Vec::with_capacity(8 + header_len as usize + payload_len as usize);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&(header_len as u32).to_le_bytes());
    out.extend_from_slice(json.as_bytes());
    out.resize(8 + header_len as usize, b' ');

    let base = out.len();
    out.resize(base + payload_len as usize, 0);
    for (c, e) in page.cols.iter().zip(&ext) {
        let at = |o: u64| base + o as usize;
        match c {
            ColumnBlock::Text { offsets, bytes, .. } => {
                write_u32s(&mut out[at(e.aux_off)..], offsets);
                out[at(e.off)..at(e.off) + bytes.len()].copy_from_slice(bytes);
            }
            ColumnBlock::Blob {
                offsets,
                bytes,
                binary,
                ..
            } => {
                write_u32s(&mut out[at(e.aux_off)..], offsets);
                let a = at(e.off);
                out[a..a + bytes.len()].copy_from_slice(bytes);
                out[a + bytes.len()..a + bytes.len() + binary.len()].copy_from_slice(binary);
            }
            ColumnBlock::Num { values, tags, .. } => {
                let a = at(e.off);
                for (i, v) in values.iter().enumerate() {
                    out[a + i * 8..a + i * 8 + 8].copy_from_slice(&v.to_le_bytes());
                }
                let b = at(e.aux_off);
                out[b..b + tags.len()].copy_from_slice(tags);
            }
        }
    }
    out
}

fn write_u32s(dst: &mut [u8], src: &[u32]) {
    for (i, v) in src.iter().enumerate() {
        dst[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// decode
// ---------------------------------------------------------------------------

/// Why a buffer is not a valid `SDP1` page.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    /// The first four bytes are not `SDP1`.
    #[error("not an SDP1 buffer")]
    BadMagic,
    /// The buffer ends before a declared extent does.
    #[error("buffer is {len} bytes; {what} needs {need}")]
    Truncated {
        /// What was being read.
        what: &'static str,
        /// Bytes required.
        need: u64,
        /// Bytes present.
        len: u64,
    },
    /// The header does not match §8.1's grammar, at this byte offset into it.
    #[error("malformed SDP1 header at byte {at}: expected {want}")]
    BadHeader {
        /// Offset into the header.
        at: usize,
        /// What the grammar required there.
        want: &'static str,
    },
    /// A declared region is inconsistent with the column kind or with `nrows`.
    #[error("column {idx} declares {what}")]
    BadColumn {
        /// Which column.
        idx: u32,
        /// The inconsistency.
        what: &'static str,
    },
    /// The payload has bytes past the last declared region, or stops short of
    /// it. §2.2: "there is no trailing slack".
    #[error("payload is {got} bytes; the declared regions end at {want}")]
    PayloadLength {
        /// Bytes present after the header.
        got: u64,
        /// Where the last region ends.
        want: u64,
    },
}

/// Parse an `SDP1` buffer.
///
/// Total: every input either yields a page or a [`PageError`]. Nothing here
/// indexes without having checked the bound first, and every offset arithmetic
/// is `checked_*` — a `DataPage` can arrive from a cache file or a truncated
/// write, and `debug_assert` is not a check.
///
/// # Errors
///
/// [`PageError`].
pub fn decode(buf: &[u8]) -> Result<DataPage, PageError> {
    let buf_len = buf.len() as u64;
    if buf.len() < 8 || buf[..4] != MAGIC {
        return Err(PageError::BadMagic);
    }
    let header_len = u64::from(u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]));
    let base = 8u64.checked_add(header_len).ok_or(PageError::Truncated {
        what: "header",
        need: u64::MAX,
        len: buf_len,
    })?;
    if base > buf_len {
        return Err(PageError::Truncated {
            what: "header",
            need: base,
            len: buf_len,
        });
    }
    let header = &buf[8..base as usize];
    let payload = &buf[base as usize..];

    let mut c = Scan { s: header, i: 0 };
    c.lit(b"{\"state\":", "{\"state\":")?;
    let state = c.int()?;
    c.lit(b",\"row0\":", ",\"row0\":")?;
    let row0 = c.int()?;
    c.lit(b",\"nrows\":", ",\"nrows\":")?;
    let nrows_u64 = c.int()?;
    c.lit(b",\"seq\":", ",\"seq\":")?;
    let seq = c.int()?;
    c.lit(b",\"cols\":[", ",\"cols\":[")?;

    let nrows = u32::try_from(nrows_u64).map_err(|_| PageError::BadColumn {
        idx: 0,
        what: "an nrows past u32",
    })?;

    let mut decls: Vec<(u32, Kind, Extents)> = Vec::new();
    if c.peek() != Some(b']') {
        loop {
            c.lit(b"{\"idx\":", "{\"idx\":")?;
            let idx = u32::try_from(c.int()?).map_err(|_| PageError::BadColumn {
                idx: 0,
                what: "a variable index past u32",
            })?;
            c.lit(b",\"kind\":\"", ",\"kind\":\"")?;
            let kind = c.kind()?;
            c.lit(b",\"off\":", ",\"off\":")?;
            let off = c.int()?;
            c.lit(b",\"len\":", ",\"len\":")?;
            let len = c.int()?;
            c.lit(b",\"aux_off\":", ",\"aux_off\":")?;
            let aux_off = c.int()?;
            c.lit(b",\"aux_len\":", ",\"aux_len\":")?;
            let aux_len = c.int()?;
            c.lit(b"}", "}")?;
            decls.push((
                idx,
                kind,
                Extents {
                    off,
                    len,
                    aux_off,
                    aux_len,
                },
            ));
            if c.peek() == Some(b',') {
                c.i += 1;
            } else {
                break;
            }
        }
    }
    c.lit(b"]}", "]}")?;
    // §2.1: a writer emits spaces and nothing else after the JSON.
    if c.s[c.i..].iter().any(|&b| b != b' ') {
        return Err(PageError::BadHeader {
            at: c.i,
            want: "ASCII space padding",
        });
    }

    let mut end = 0u64;
    let mut cols = Vec::with_capacity(decls.len());
    for (idx, kind, e) in decls {
        let a = region(payload, e.aux_off, e.aux_len, "aux")?;
        let d = region(payload, e.off, e.len, "data")?;
        end = end.max(e.aux_off + e.aux_len).max(e.off + e.len);
        cols.push(rebuild(idx, kind, nrows, a, d)?);
    }
    if end != payload.len() as u64 {
        return Err(PageError::PayloadLength {
            got: payload.len() as u64,
            want: end,
        });
    }

    Ok(DataPage {
        state: DatasetStateId(state),
        row0,
        nrows,
        seq: u32::try_from(seq).map_err(|_| PageError::BadColumn {
            idx: 0,
            what: "a seq past u32",
        })?,
        cols,
    })
}

/// Slice one declared region, checking it against the measured payload length.
fn region<'a>(
    payload: &'a [u8],
    off: u64,
    len: u64,
    what: &'static str,
) -> Result<&'a [u8], PageError> {
    let end = off.checked_add(len).ok_or(PageError::Truncated {
        what,
        need: u64::MAX,
        len: payload.len() as u64,
    })?;
    if end > payload.len() as u64 {
        return Err(PageError::Truncated {
            what,
            need: end,
            len: payload.len() as u64,
        });
    }
    Ok(&payload[off as usize..end as usize])
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Text,
    Num,
    Blob,
}

fn rebuild(
    idx: u32,
    kind: Kind,
    nrows: u32,
    aux: &[u8],
    data: &[u8],
) -> Result<ColumnBlock, PageError> {
    let bad = |what: &'static str| PageError::BadColumn { idx, what };
    match kind {
        Kind::Num => {
            if data.len() as u64 != u64::from(nrows) * 8 {
                return Err(bad("a data region that is not nrows f64"));
            }
            if aux.len() as u64 != u64::from(nrows) {
                return Err(bad("an aux region that is not nrows tags"));
            }
            let values = data
                .chunks_exact(8)
                .map(|w| f64::from_le_bytes(w.try_into().expect("chunks_exact(8)")))
                .collect();
            Ok(ColumnBlock::Num {
                idx: VarIdx(idx),
                values,
                tags: aux.to_vec(),
            })
        }
        Kind::Text | Kind::Blob => {
            if aux.len() as u64 != (u64::from(nrows) + 1) * 4 {
                return Err(bad("an aux region that is not (nrows + 1) offsets"));
            }
            let offsets: Vec<u32> = aux
                .chunks_exact(4)
                .map(|w| u32::from_le_bytes(w.try_into().expect("chunks_exact(4)")))
                .collect();
            if offsets[0] != 0 || offsets.windows(2).any(|w| w[0] > w[1]) {
                return Err(bad("offsets that are not ascending from zero"));
            }
            let arena_len = u64::from(*offsets.last().expect("nrows + 1 >= 1"));
            let bitmap_len = if kind == Kind::Blob {
                u64::from(nrows).div_ceil(8)
            } else {
                0
            };
            // README §2.3: both lengths are derivable two ways, so disagreement
            // is detectable. This is that check.
            if arena_len + bitmap_len != data.len() as u64 {
                return Err(bad("an arena length its offsets do not agree with"));
            }
            let (bytes, binary) = data.split_at(arena_len as usize);
            match kind {
                Kind::Text => Ok(ColumnBlock::Text {
                    idx: VarIdx(idx),
                    offsets,
                    bytes: bytes.to_vec(),
                }),
                _ => Ok(ColumnBlock::Blob {
                    idx: VarIdx(idx),
                    offsets,
                    bytes: bytes.to_vec(),
                    binary: binary.to_vec(),
                }),
            }
        }
    }
}

/// A cursor over the header, accepting exactly §8.1's grammar.
struct Scan<'a> {
    s: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn lit(&mut self, t: &[u8], want: &'static str) -> Result<(), PageError> {
        if self.s.len() < self.i + t.len() || &self.s[self.i..self.i + t.len()] != t {
            return Err(PageError::BadHeader { at: self.i, want });
        }
        self.i += t.len();
        Ok(())
    }

    /// A plain decimal integer, as §2.1 requires (no exponent, no sign).
    fn int(&mut self) -> Result<u64, PageError> {
        let start = self.i;
        let mut v = 0u64;
        while let Some(b @ b'0'..=b'9') = self.peek() {
            v = v
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(b - b'0')))
                .ok_or(PageError::BadHeader {
                    at: start,
                    want: "an integer that fits u64",
                })?;
            self.i += 1;
        }
        if self.i == start {
            return Err(PageError::BadHeader {
                at: start,
                want: "a decimal integer",
            });
        }
        Ok(v)
    }

    fn kind(&mut self) -> Result<Kind, PageError> {
        for (name, k) in [
            (&b"text\""[..], Kind::Text),
            (&b"num\""[..], Kind::Num),
            (&b"blob\""[..], Kind::Blob),
        ] {
            if self.s.len() >= self.i + name.len() && &self.s[self.i..self.i + name.len()] == name {
                self.i += name.len();
                return Ok(k);
            }
        }
        Err(PageError::BadHeader {
            at: self.i,
            want: "\"text\", \"num\" or \"blob\"",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(idx: u32, cells: &[&str]) -> ColumnBlock {
        let mut offsets = vec![0u32];
        let mut bytes = Vec::new();
        for c in cells {
            bytes.extend_from_slice(c.as_bytes());
            offsets.push(bytes.len() as u32);
        }
        ColumnBlock::Text {
            idx: VarIdx(idx),
            offsets,
            bytes,
        }
    }

    fn page_of(cols: Vec<ColumnBlock>, nrows: u32) -> DataPage {
        DataPage {
            state: DatasetStateId(17),
            row0: 0,
            nrows,
            seq: 1,
            cols,
        }
    }

    #[test]
    fn the_payload_starts_eight_aligned_whatever_the_header_length() {
        // The rule exists because `new Float64Array(buf, off, n)` throws unless
        // `off % 8 == 0`, and a one-digit change in `nrows` moves the header by
        // one byte. Sweep the row count so every residue is exercised.
        for nrows in [0u32, 1, 9, 40, 99, 100, 999, 1000] {
            let cells: Vec<String> = (0..nrows).map(|i| i.to_string()).collect();
            let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
            let bytes = encode(&page_of(vec![text(0, &refs)], nrows));
            let h = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
            assert_eq!((8 + h) % 8, 0, "nrows = {nrows}");
            // The padding is spaces and nothing else.
            let tail = &bytes[8..8 + h];
            let json_end = tail.iter().rposition(|&b| b != b' ').expect("non-empty") + 1;
            assert!(tail[json_end..].iter().all(|&b| b == b' '));
            assert_eq!(tail[json_end - 1], b'}');
        }
    }

    #[test]
    fn a_num_column_lands_eight_aligned_behind_a_ragged_text_column() {
        // 5 bytes of arena leaves the cursor at an odd offset; the `num` block
        // must still start on a multiple of 8.
        let p = page_of(
            vec![
                text(0, &["ab", "cde"]),
                ColumnBlock::Num {
                    idx: VarIdx(1),
                    values: vec![1.0, 2.0],
                    tags: vec![255, 255],
                },
            ],
            2,
        );
        let bytes = encode(&p);
        let back = decode(&bytes).expect("round trip");
        assert_eq!(back, p);
        let h = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let hdr = std::str::from_utf8(&bytes[8..8 + h]).expect("utf8");
        // aux 0..12, data 12..17, then the f64 block aligned up from 17 to 24.
        assert!(hdr.contains("\"kind\":\"num\",\"off\":24"), "{hdr}");
    }

    #[test]
    fn round_trip_holds_for_all_three_kinds() {
        let p = page_of(
            vec![
                text(0, &["alpha", "", "gamma"]),
                ColumnBlock::Num {
                    idx: VarIdx(1),
                    values: vec![1.5, stratum_core::missing::SYSMISS, 3.0],
                    tags: vec![255, 0, 255],
                },
                ColumnBlock::Blob {
                    idx: VarIdx(2),
                    offsets: vec![0, 3, 3, 7],
                    bytes: b"abc\0\x01\x02\x03".to_vec(),
                    binary: vec![0b0000_0100],
                },
            ],
            3,
        );
        assert_eq!(decode(&encode(&p)).expect("round trip"), p);
    }

    #[test]
    fn an_empty_page_is_legal() {
        let p = page_of(Vec::new(), 0);
        let b = encode(&p);
        assert_eq!(b.len() % 8, 0);
        assert_eq!(decode(&b).expect("round trip"), p);
    }

    #[test]
    fn truncation_is_an_error_and_never_a_panic() {
        let full = encode(&page_of(vec![text(0, &["abcdef"])], 1));
        for n in 0..full.len() {
            // Every prefix must be rejected cleanly. The prefix that stops
            // inside the payload is the one that used to index out of bounds.
            let _ = decode(&full[..n]).expect_err("a prefix is not a page");
        }
        assert!(decode(&full).is_ok());
    }

    #[test]
    fn a_lying_extent_is_rejected_rather_than_read() {
        let mut b = encode(&page_of(vec![text(0, &["abc"])], 1));
        // Rewrite `"len":3` to `"len":9`, which reaches past the payload.
        let h = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
        let hdr = std::str::from_utf8(&b[8..8 + h]).expect("utf8").to_owned();
        let at = hdr.find("\"len\":3").expect("present") + 8 + 6;
        b[at] = b'9';
        assert!(matches!(
            decode(&b),
            Err(PageError::Truncated { .. } | PageError::BadColumn { .. })
        ));
    }

    #[test]
    fn a_header_that_is_not_exactly_the_specified_grammar_is_rejected() {
        // §2.1 makes compactness and key order normative, so a pretty-printed
        // header is not a valid page even though it is valid JSON.
        let pretty = b"SDP1\x18\x00\x00\x00{ \"state\": 1, \"row0\": 0 }      ";
        assert!(matches!(decode(pretty), Err(PageError::BadHeader { .. })));
    }

    #[test]
    fn trailing_slack_after_the_last_region_is_rejected() {
        let mut b = encode(&page_of(vec![text(0, &["abc"])], 1));
        b.push(0);
        assert!(matches!(decode(&b), Err(PageError::PayloadLength { .. })));
    }
}
