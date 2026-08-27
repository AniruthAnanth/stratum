//! A zlib stream, written by hand.
//!
//! One fixed-Huffman block (RFC 1951 §3.2.6) with **RLE-only matching**: the
//! only back-references emitted are at distance 1, so the matcher is a run
//! counter and there is no hash chain, no window and no allocation.
//!
//! Why not a dependency: this crate's whole claim is that it renders inside
//! `stratum run` on a machine with no `apps/` directory and almost nothing
//! linked. `miniz_oxide` in the CLI, pulled in for a code path most runs never
//! take, is the wrong trade — and per spec §0a the ratio we give up buys speed,
//! which is the side of that trade we are told to take. The mark layer is mostly
//! flat colour over transparency, filtered with PNG's `Up` predictor into long
//! runs of zero, which is exactly the input RLE-only matching handles well.

/// Fixed-Huffman length codes: `(code, base, extra_bits)` for lengths 3..=258.
/// RFC 1951 §3.2.5, transcribed.
const LENGTH_CODES: [(u16, u16, u8); 29] = [
    (257, 3, 0),
    (258, 4, 0),
    (259, 5, 0),
    (260, 6, 0),
    (261, 7, 0),
    (262, 8, 0),
    (263, 9, 0),
    (264, 10, 0),
    (265, 11, 1),
    (266, 13, 1),
    (267, 15, 1),
    (268, 17, 1),
    (269, 19, 2),
    (270, 23, 2),
    (271, 27, 2),
    (272, 31, 2),
    (273, 35, 3),
    (274, 43, 3),
    (275, 51, 3),
    (276, 59, 3),
    (277, 67, 4),
    (278, 83, 4),
    (279, 99, 4),
    (280, 115, 4),
    (281, 131, 5),
    (282, 163, 5),
    (283, 195, 5),
    (284, 227, 5),
    (285, 258, 0),
];

/// The longest match deflate can express.
const MAX_MATCH: usize = 258;

struct BitWriter {
    out: Vec<u8>,
    bit_buf: u32,
    bit_count: u32,
}

impl BitWriter {
    fn new(capacity: usize) -> BitWriter {
        BitWriter {
            out: Vec::with_capacity(capacity),
            bit_buf: 0,
            bit_count: 0,
        }
    }

    /// Deflate packs bits into bytes starting at the least significant bit.
    fn put(&mut self, value: u32, bits: u32) {
        self.bit_buf |= (value & ((1u32 << bits) - 1)) << self.bit_count;
        self.bit_count += bits;
        while self.bit_count >= 8 {
            self.out.push((self.bit_buf & 0xff) as u8);
            self.bit_buf >>= 8;
            self.bit_count -= 8;
        }
    }

    /// Huffman codes are defined most-significant-bit first, and the bit stream
    /// is least-significant-bit first, so every code is reversed on the way out.
    /// This is the one detail that makes hand-written deflate fail silently when
    /// it is missed: the output is a valid stream of the wrong symbols.
    fn put_code(&mut self, code: u32, bits: u32) {
        let mut reversed = 0u32;
        for i in 0..bits {
            reversed |= ((code >> i) & 1) << (bits - 1 - i);
        }
        self.put(reversed, bits);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bit_count > 0 {
            self.out.push((self.bit_buf & 0xff) as u8);
        }
        self.out
    }
}

/// The fixed literal/length code for `sym`, as `(code, bit length)`.
fn fixed_literal(sym: u16) -> (u32, u32) {
    match sym {
        0..=143 => (0x30 + u32::from(sym), 8),
        144..=255 => (0x190 + u32::from(sym) - 144, 9),
        256..=279 => (u32::from(sym) - 256, 7),
        _ => (0xc0 + u32::from(sym) - 280, 8),
    }
}

/// Deflate `data` into one fixed-Huffman block.
fn deflate_fixed(data: &[u8]) -> Vec<u8> {
    // A conservative reservation: worst case is 9 bits per byte plus the header.
    let mut w = BitWriter::new(data.len() * 9 / 8 + 16);
    w.put(1, 1); // BFINAL
    w.put(1, 2); // BTYPE = 01, fixed Huffman

    let n = data.len();
    let mut i = 0usize;
    while i < n {
        let byte = data[i];
        let (code, bits) = fixed_literal(u16::from(byte));
        w.put_code(code, bits);
        i += 1;

        // Everything after this literal that repeats it is a distance-1 match.
        let mut run = 0usize;
        while i + run < n && data[i + run] == byte {
            run += 1;
        }
        while run >= 3 {
            let take = run.min(MAX_MATCH);
            // Never leave a 1- or 2-byte tail that cannot be a match: taking
            // `MAX_MATCH` out of a 259-byte run leaves 1, and that single byte
            // then costs a literal plus a re-scan. Taking 256 leaves 3.
            let take = if run - take > 0 && run - take < 3 {
                run - 3
            } else {
                take
            };
            if take < 3 {
                break;
            }
            emit_match(&mut w, take);
            run -= take;
            i += take;
        }
    }

    let (code, bits) = fixed_literal(256); // end of block
    w.put_code(code, bits);
    w.finish()
}

fn emit_match(w: &mut BitWriter, len: usize) {
    let len16 = u16::try_from(len).unwrap_or(u16::MAX);
    let entry = LENGTH_CODES
        .iter()
        .rev()
        .find(|(_, base, _)| *base <= len16)
        .copied()
        .unwrap_or((257, 3, 0));
    let (code, base, extra) = entry;
    let (c, b) = fixed_literal(code);
    w.put_code(c, b);
    if extra > 0 {
        w.put(u32::from(len16 - base), u32::from(extra));
    }
    // Distance 1: fixed distance codes are five raw bits, and code 0 has no
    // extra bits.
    w.put_code(0, 5);
}

/// Adler-32, RFC 1950.
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    // Chunked so the modulo runs once per 5552 bytes instead of once per byte;
    // 5552 is the largest block that cannot overflow `u32` here.
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// Wrap a fixed-Huffman deflate block in a zlib stream.
#[must_use]
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let body = deflate_fixed(data);
    let mut out = Vec::with_capacity(body.len() + 6);
    // CMF = 0x78 (deflate, 32 KiB window), FLG = 0x01: no dictionary, and
    // 0x7801 is divisible by 31, which is the header's own check.
    out.push(0x78);
    out.push(0x01);
    out.extend_from_slice(&body);
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny inflate for fixed-Huffman streams, so the encoder is checked
    /// against a decoder rather than against itself. Only the subset the encoder
    /// can emit is implemented — that is the point: if the encoder emits
    /// something outside that subset, this panics.
    fn inflate_fixed(zlib_stream: &[u8]) -> Vec<u8> {
        let body = &zlib_stream[2..zlib_stream.len() - 4];
        let mut bitpos = 0usize;
        let mut take = |n: usize| -> u32 {
            let mut v = 0u32;
            for i in 0..n {
                let byte = body[(bitpos + i) / 8];
                let bit = (byte >> ((bitpos + i) % 8)) & 1;
                v |= u32::from(bit) << i;
            }
            bitpos += n;
            v
        };
        let bfinal = take(1);
        let btype = take(2);
        assert_eq!(
            (bfinal, btype),
            (1, 1),
            "encoder emits exactly one fixed block"
        );

        let mut out: Vec<u8> = Vec::new();
        loop {
            // Fixed literal decoding: read 7 bits MSB-first, extend as needed.
            let mut code = 0u32;
            for _ in 0..7 {
                code = (code << 1) | take(1);
            }
            let sym: u16 = if code <= 0x17 {
                (code + 256) as u16
            } else {
                code = (code << 1) | take(1);
                if (0x30..=0xbf).contains(&code) {
                    (code - 0x30) as u16
                } else if (0xc0..=0xc7).contains(&code) {
                    (code - 0xc0 + 280) as u16
                } else {
                    code = (code << 1) | take(1);
                    assert!((0x190..=0x1ff).contains(&code), "bad code {code:#x}");
                    (code - 0x190 + 144) as u16
                }
            };
            if sym == 256 {
                break;
            }
            if sym < 256 {
                out.push(sym as u8);
                continue;
            }
            let (_, base, extra) = LENGTH_CODES
                .iter()
                .find(|(c, _, _)| *c == sym)
                .copied()
                .expect("length code");
            let len = usize::from(base) + take(usize::from(extra)) as usize;
            let mut dcode = 0u32;
            for _ in 0..5 {
                dcode = (dcode << 1) | take(1);
            }
            assert_eq!(dcode, 0, "encoder only emits distance 1");
            for _ in 0..len {
                let b = out[out.len() - 1];
                out.push(b);
            }
        }
        out
    }

    fn roundtrip(data: &[u8]) {
        let z = zlib(data);
        assert_eq!(&z[..2], &[0x78, 0x01]);
        assert_eq!(&z[z.len() - 4..], &adler32(data).to_be_bytes());
        assert_eq!(inflate_fixed(&z), data, "roundtrip failed");
    }

    #[test]
    fn roundtrips_empty_and_tiny() {
        roundtrip(b"");
        roundtrip(b"a");
        roundtrip(b"ab");
    }

    #[test]
    fn roundtrips_every_byte_value() {
        let all: Vec<u8> = (0..=255u8).collect();
        roundtrip(&all);
    }

    #[test]
    fn roundtrips_long_runs_across_the_258_boundary() {
        for n in [3usize, 4, 257, 258, 259, 260, 261, 600, 5000] {
            roundtrip(&vec![0u8; n]);
        }
    }

    #[test]
    fn roundtrips_a_realistic_filtered_scanline() {
        // What PNG's `Up` filter produces over a sparse mark layer: mostly zero,
        // occasional bursts.
        let mut data = vec![0u8; 4096];
        for (i, slot) in data.iter_mut().enumerate() {
            if i % 997 == 0 {
                *slot = (i % 251) as u8;
            }
        }
        roundtrip(&data);
    }

    #[test]
    fn flat_input_actually_compresses() {
        // The reason RLE-only matching is enough for this input class.
        let flat = vec![0u8; 100_000];
        assert!(zlib(&flat).len() < 2_000, "{} bytes", zlib(&flat).len());
    }

    #[test]
    fn adler_matches_the_rfc_example() {
        // RFC 1950's own worked value for "Wikipedia" is 0x11E60398.
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
