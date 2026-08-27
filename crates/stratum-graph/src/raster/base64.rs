//! Base64 for the raster layer's `data:` URI.
//!
//! Standard alphabet with padding (RFC 4648 §4), because that is what a `data:`
//! URL is defined over and what every webview and every SVG consumer accepts.
//! `data:` is already enumerated in the CSP `xtask csp-check` enforces (A21), so
//! embedding this costs no new scheme.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Append the base64 of `data` to `out`.
pub fn push_encoded(out: &mut String, data: &[u8]) {
    out.reserve(data.len().div_ceil(3) * 4);
    let mut chunks = data.chunks_exact(3);
    for c in &mut chunks {
        let n = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        for shift in [18, 12, 6, 0] {
            out.push(char::from(ALPHABET[((n >> shift) & 0x3f) as usize]));
        }
    }
    let rest = chunks.remainder();
    match rest.len() {
        1 => {
            let n = u32::from(rest[0]) << 16;
            out.push(char::from(ALPHABET[((n >> 18) & 0x3f) as usize]));
            out.push(char::from(ALPHABET[((n >> 12) & 0x3f) as usize]));
            out.push_str("==");
        }
        2 => {
            let n = (u32::from(rest[0]) << 16) | (u32::from(rest[1]) << 8);
            for shift in [18, 12, 6] {
                out.push(char::from(ALPHABET[((n >> shift) & 0x3f) as usize]));
            }
            out.push('=');
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(data: &[u8]) -> String {
        let mut s = String::new();
        push_encoded(&mut s, data);
        s
    }

    /// RFC 4648 §10's own test vectors.
    #[test]
    fn matches_the_rfc_vectors() {
        assert_eq!(enc(b""), "");
        assert_eq!(enc(b"f"), "Zg==");
        assert_eq!(enc(b"fo"), "Zm8=");
        assert_eq!(enc(b"foo"), "Zm9v");
        assert_eq!(enc(b"foob"), "Zm9vYg==");
        assert_eq!(enc(b"fooba"), "Zm9vYmE=");
        assert_eq!(enc(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_png_signature_encodes_the_way_every_decoder_expects() {
        // Every base64 PNG in the world starts with this; a wrong alphabet or a
        // wrong bit order shows up here immediately.
        assert!(enc(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).starts_with("iVBORw0KGgo"));
    }

    #[test]
    fn length_is_always_a_multiple_of_four() {
        for n in 0..64usize {
            assert_eq!(enc(&vec![0xa5u8; n]).len() % 4, 0, "n = {n}");
        }
    }
}
