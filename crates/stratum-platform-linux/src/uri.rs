//! `file://` URIs, because the FileChooser portal speaks them and the rest of
//! Stratum speaks [`Utf8PathBuf`].
//!
//! Pure, and tested on every host, because this is a correctness boundary
//! rather than a formatting convenience. A researcher's dataset lives in
//! `~/Dropbox/Field work 2024/wave 2 (final).dta`; a naive
//! `uri.strip_prefix("file://")` hands the rest of the program a path with
//! `%20` in it, and the file "does not exist". The reverse direction is worse:
//! failing to encode `#` truncates the path at the fragment separator, and a
//! path containing `#` is a file the user will then be told they cannot save
//! to.

use camino::{Utf8Path, Utf8PathBuf};

/// Decode a `file://` URI into a path.
///
/// `None` for anything that is not a local `file://` URI — a portal that
/// returned `sftp://` or `smb://` gave us a GVFS location we cannot open with
/// `std::fs`, and pretending otherwise produces a "no such file" error pointing
/// at a path the user never typed.
#[must_use]
pub fn file_uri_to_path(uri: &str) -> Option<Utf8PathBuf> {
    // A `file://host/path` URI with a non-empty, non-`localhost` authority is
    // a remote file. `file:///path` has an empty authority, which is the local
    // case and the only one we accept.
    let rest = uri.strip_prefix("file://")?;
    // `file://localhost/x` and `file:///x` both mean the local `/x`.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    percent_decode(path)
}

/// Encode a path as a `file://` URI.
#[must_use]
pub fn path_to_file_uri(path: &Utf8Path) -> String {
    let mut out = String::with_capacity(path.as_str().len() + 8);
    out.push_str("file://");
    for b in path.as_str().bytes() {
        if is_unreserved(b) {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(hex(b >> 4));
            out.push(hex(b & 0xf));
        }
    }
    out
}

/// RFC 3986 unreserved, plus `/` because we are encoding a path and its
/// separators must survive.
const fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/')
}

const fn hex(nibble: u8) -> char {
    // Uppercase: RFC 3986 §2.1 says producers should emit uppercase hex.
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + nibble - 10) as char,
    }
}

/// Percent-decode into UTF-8. `None` when a `%` is not followed by two hex
/// digits, or when the decoded bytes are not UTF-8 — a path we cannot
/// represent is a path we must not silently mangle, because the whole point of
/// [`camino`] in this codebase is that a non-UTF-8 path is refused loudly.
fn percent_decode(s: &str) -> Option<Utf8PathBuf> {
    if !s.contains('%') {
        return Some(Utf8PathBuf::from(s));
    }
    let src = s.as_bytes();
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' {
            let hi = unhex(*src.get(i + 1)?)?;
            let lo = unhex(*src.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(src[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().map(Utf8PathBuf::from)
}

const fn unhex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_researchers_path_survives_both_directions() {
        let p = Utf8Path::new("/home/jo/Dropbox/Field work 2024/wave 2 (final).dta");
        let uri = path_to_file_uri(p);
        assert_eq!(
            uri,
            "file:///home/jo/Dropbox/Field%20work%202024/wave%202%20%28final%29.dta"
        );
        assert_eq!(file_uri_to_path(&uri).as_deref(), Some(p));
    }

    /// `#` is the fragment separator. An unencoded one truncates the path, and
    /// the user is told a file they can see does not exist.
    #[test]
    fn the_characters_that_would_truncate_a_uri_are_encoded() {
        for name in ["a#b.do", "a?b.do", "a%b.do", "a b.do", "a\u{e9}b.do"] {
            let p = Utf8PathBuf::from(format!("/tmp/{name}"));
            let uri = path_to_file_uri(&p);
            assert!(!uri.contains('#'), "{uri}");
            assert!(!uri.contains('?'), "{uri}");
            assert_eq!(file_uri_to_path(&uri), Some(p));
        }
    }

    #[test]
    fn a_uri_with_no_escapes_is_not_copied_through_a_decoder() {
        assert_eq!(
            file_uri_to_path("file:///usr/share/stratum/auto.dta"),
            Some(Utf8PathBuf::from("/usr/share/stratum/auto.dta"))
        );
    }

    #[test]
    fn localhost_is_the_only_authority_we_accept() {
        assert_eq!(
            file_uri_to_path("file://localhost/etc/hosts"),
            Some(Utf8PathBuf::from("/etc/hosts"))
        );
        // A GVFS location. We cannot open it with std::fs, and turning it into
        // a plausible-looking path is worse than refusing it.
        assert_eq!(file_uri_to_path("file://server/share/x.dta"), None);
        assert_eq!(file_uri_to_path("sftp://server/x.dta"), None);
        assert_eq!(file_uri_to_path("/not/a/uri"), None);
    }

    #[test]
    fn a_malformed_escape_is_refused_rather_than_passed_through() {
        assert_eq!(file_uri_to_path("file:///tmp/a%2"), None);
        assert_eq!(file_uri_to_path("file:///tmp/a%zz"), None);
        // Valid escapes that decode to invalid UTF-8: a path camino cannot
        // hold, which this codebase refuses loudly rather than lossily fixing.
        assert_eq!(file_uri_to_path("file:///tmp/a%FFb"), None);
    }

    #[test]
    fn hex_output_is_uppercase_as_rfc_3986_asks() {
        assert_eq!(path_to_file_uri(Utf8Path::new("/a b")), "file:///a%20b");
        assert_eq!(path_to_file_uri(Utf8Path::new("/\u{e9}")), "file:///%C3%A9");
    }
}
