//! A PNG container around [`super::deflate`].
//!
//! Colour type 6 (8-bit RGBA), because the mark layer sits over the plot
//! background and has to be transparent where nothing was drawn. Every scanline
//! uses filter type 2 (`Up`), which turns a sparse layer into long runs of zero
//! — the input the RLE-only deflate is good at, and the reason the two modules
//! were designed together rather than one being chosen for the other.

use super::deflate;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// CRC-32 (the PNG/zlib polynomial, 0xEDB88320), table built at compile time.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c = CRC_TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
    }
    c ^ 0xffff_ffff
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Encode an RGBA8 buffer. `rgba` must be `w * h * 4` bytes, row-major.
#[must_use]
pub fn encode_rgba(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = w as usize * 4;
    debug_assert_eq!(rgba.len(), stride * h as usize);

    // Filter type 2 (`Up`) on every row, with an implicit all-zero row above the
    // first — which makes row 0's `Up` output identical to `None`, so there is
    // no special case and no branch in the loop.
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    let mut prev = vec![0u8; stride];
    for y in 0..h as usize {
        let row = &rgba[y * stride..(y + 1) * stride];
        raw.push(2);
        for x in 0..stride {
            raw.push(row[x].wrapping_sub(prev[x]));
        }
        prev.copy_from_slice(row);
    }

    let mut out = Vec::with_capacity(raw.len() / 4 + 128);
    out.extend_from_slice(&SIGNATURE);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // depth, colour type RGBA, deflate, adaptive, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &deflate::zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_known_check_value() {
        // The standard CRC-32 check value for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn the_container_is_well_formed() {
        let px = vec![0u8; 4 * 4 * 4];
        let png = encode_rgba(4, 4, &px);
        assert_eq!(&png[..8], &SIGNATURE);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..20], &4u32.to_be_bytes());
        assert_eq!(&png[20..24], &4u32.to_be_bytes());
        assert_eq!(png[24], 8, "bit depth");
        assert_eq!(png[25], 6, "colour type RGBA");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn every_chunk_carries_a_correct_crc() {
        let png = encode_rgba(3, 2, &[7u8; 3 * 2 * 4]);
        let mut i = 8usize;
        let mut seen = Vec::new();
        while i < png.len() {
            let len = u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]) as usize;
            let body = &png[i + 4..i + 8 + len];
            let want = u32::from_be_bytes([
                png[i + 8 + len],
                png[i + 9 + len],
                png[i + 10 + len],
                png[i + 11 + len],
            ]);
            assert_eq!(crc32(body), want, "chunk at {i}");
            seen.push(String::from_utf8_lossy(&body[..4]).to_string());
            i += 12 + len;
        }
        assert_eq!(seen, ["IHDR", "IDAT", "IEND"]);
    }

    #[test]
    fn a_transparent_layer_stays_small() {
        // The property the whole encoder exists for: 400x300 of nothing is not
        // 480 KB of PNG. Stated as a RATIO rather than as a byte count, because
        // a byte count would be an assertion about the deflate implementation's
        // current tuning and this is an assertion about `Up` + RLE working at
        // all. 100:1 on flat input is the floor; the encoder currently does
        // rather better, and is allowed to get worse before this fails.
        const RAW: usize = 400 * 300 * 4;
        let png = encode_rgba(400, 300, &vec![0u8; RAW]);
        assert!(png.len() * 100 < RAW, "{} bytes for {RAW} raw", png.len());
    }
}
