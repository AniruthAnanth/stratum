//! 07 §2.8 — the last line of defence before a string leaves this crate.
//!
//! Provider error bodies routinely echo request fragments, and a 401 body from a
//! misconfigured gateway has been observed to contain the offending
//! `Authorization` header verbatim. Every error string, every health detail and
//! every audit note passes through [`scrub`] on the way out, so a key cannot
//! reach a log file, a crash report or the audit log even when a provider hands
//! it back to us.

use std::sync::Mutex;

use secrecy::{ExposeSecret, SecretString};

/// What replaces a match. Fixed text rather than a length-preserving mask: a
/// mask that preserves length leaks the length, and key length is a fingerprint.
pub const REDACTED: &str = "«redacted»";

/// Currently-loaded key material, registered by the backends so that a key we
/// hold can be scrubbed out of a body that echoes it even when it does not match
/// any syntactic pattern (an institutional gateway's opaque bearer token, say).
///
/// A `Mutex<Vec<String>>` and not a lock-free structure on purpose: it is
/// written once per credential load and read once per error, which is
/// approximately never on any hot path.
static KNOWN_MATERIAL: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Register key material so [`scrub`] can remove it verbatim.
///
/// The plaintext is copied into a process-lifetime store, which is a deliberate
/// and bounded relaxation of "the key exists only inside a `SecretString`": the
/// alternative is that a provider echoing the key back to us writes it into the
/// audit log, which is a file on disk. A copy in RAM that exists so we can
/// delete the key from a disk write is the better half of that trade.
pub fn register(secret: &SecretString) {
    let value = secret.expose_secret().to_owned();
    if value.len() < 8 {
        // Too short to be a key and short enough to appear in ordinary prose.
        return;
    }
    let Ok(mut guard) = KNOWN_MATERIAL.lock() else {
        return;
    };
    if !guard.iter().any(|k| k == &value) {
        guard.push(value);
    }
}

/// Drop every registered secret. Called when the user clears a credential and
/// by tests that must not leak state into each other.
pub fn forget_all() {
    if let Ok(mut guard) = KNOWN_MATERIAL.lock() {
        guard.clear();
    }
}

/// Remove anything that looks like, or is known to be, key material.
///
/// Two independent passes, for the same reason 07 §8.3 runs three: a syntactic
/// pattern catches keys we have never seen (a key pasted into a config file by a
/// user, echoed by a gateway), and the exact-material pass catches keys whose
/// shape we do not recognise.
#[must_use]
pub fn scrub(s: &str) -> String {
    let mut out = scrub_known(s);
    out = scrub_patterns(&out);
    out
}

fn scrub_known(s: &str) -> String {
    let Ok(guard) = KNOWN_MATERIAL.lock() else {
        return s.to_owned();
    };
    let mut out = s.to_owned();
    for key in guard.iter() {
        if out.contains(key.as_str()) {
            out = out.replace(key.as_str(), REDACTED);
        }
    }
    out
}

/// The syntactic pass. Matches a token of >= 16 key-ish characters that follows
/// one of the prefixes providers actually use, plus a bare `Bearer <token>`.
///
/// Hand-rolled rather than `regex`: this is three prefixes and a character
/// class, the crate is already carrying reqwest's tree, and a scrubber that
/// cannot itself be read in one sitting is a scrubber nobody audits.
fn scrub_patterns(s: &str) -> String {
    const PREFIXES: [&str; 4] = ["sk-", "sk_", "api-key-", "Bearer "];
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    'outer: while i < bytes.len() {
        for prefix in PREFIXES {
            if s.is_char_boundary(i) && s[i..].starts_with(prefix) {
                let start = i + prefix.len();
                let mut end = start;
                while end < bytes.len() && is_key_char(bytes[end]) {
                    end += 1;
                }
                if end - start >= 16 {
                    if prefix == "Bearer " {
                        out.push_str("Bearer ");
                    }
                    out.push_str(REDACTED);
                    i = end;
                    continue 'outer;
                }
            }
        }
        let ch = s[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

const fn is_key_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_anthropic_style_key_never_survives() {
        let body = "401 {\"error\":\"invalid x-api-key: sk-ant-api03-AAAABBBBCCCCDDDDEEEE\"}";
        let out = scrub(body);
        assert!(!out.contains("AAAABBBBCCCCDDDDEEEE"), "{out}");
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn a_bearer_token_never_survives_and_the_word_bearer_does() {
        let out = scrub("Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345");
        assert_eq!(out, format!("Authorization: Bearer {REDACTED}"));
    }

    #[test]
    fn a_short_token_is_not_a_key_and_is_left_alone() {
        // `sk-test` in prose is not a credential, and a scrubber that eats
        // ordinary words trains people to stop reading its output.
        assert_eq!(scrub("see sk-test in the docs"), "see sk-test in the docs");
    }

    #[test]
    fn registered_material_is_removed_even_with_no_recognisable_shape() {
        forget_all();
        register(&SecretString::from("ZZOPAQUEGATEWAYTOKEN9137".to_owned()));
        let out = scrub("gateway said: ZZOPAQUEGATEWAYTOKEN9137 is expired");
        assert!(!out.contains("ZZOPAQUEGATEWAYTOKEN9137"), "{out}");
        forget_all();
    }

    #[test]
    fn scrubbing_is_utf8_safe() {
        // The byte scanner must never split a multibyte character.
        let out = scrub("héllo sk-0123456789abcdefghij wörld");
        assert!(out.contains("héllo"));
        assert!(out.contains("wörld"));
        assert!(!out.contains("0123456789abcdefghij"));
    }
}
