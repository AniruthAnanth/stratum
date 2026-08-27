//! Byte-fidelity policy for user source files — ARCHITECTURE A24, plan W26.
//!
//! # Why this module exists at all
//!
//! Spec §5 promises that `analysis.do` stays "portable, version-control
//! friendly". The pre-audit design specified `DocOpen { text: String }` and said
//! nothing about writing, which means the first save of a CRLF do-file authored
//! in Stata for Windows would have rewritten **every line in the file**. A
//! one-word section rename would arrive in review as a 400-line diff, and §5's
//! claim would be false in the most visible way possible.
//!
//! So the rule here is: the editor works in a normalised LF buffer, and the
//! bytes that go back to disk are reconstituted from what was actually observed
//! when the file was opened. Nothing else in the crate is allowed to invent a
//! line ending.
//!
//! # The three observations
//!
//! * **EOL** — [`Eol::Lf`] or [`Eol::Crlf`]. Majority wins on a mixed file and a
//!   mixed file raises lint `L013`, because "majority wins" *does* rewrite the
//!   minority lines and the user is entitled to know before they see the diff.
//! * **BOM** — whether the file started with `EF BB BF`. Stata for Windows
//!   writes one; stripping it silently changes the bytes, and re-adding one that
//!   was never there breaks `#!`-style tooling downstream.
//! * **Encoding** — UTF-8 or refuse. See [`decode`].
//!
//! # A lone CR is not a line ending here
//!
//! Only `\n` and `\r\n` terminate a line. A bare `\r` (classic Mac OS, or a
//! literal carriage return inside a string literal) is carried through as an
//! ordinary character and never rewritten. Treating it as a terminator would
//! mean [`encode`] could not reproduce it, and an un-reproducible byte is
//! exactly the failure this module exists to prevent.

use serde::{Deserialize, Serialize};
use stratum_proto::{Confidence, Diagnostic, Severity, Span};

/// UTF-8 byte-order mark. Present in most files Stata for Windows writes.
pub const BOM: &[u8; 3] = b"\xef\xbb\xbf";

/// The line ending a file was observed to use, and the one `doc_save` writes.
///
/// CONTRACTS §12 spells this `"lf" | "crlf"` on the wire, so the serde
/// representation is lowercase and not the default PascalCase.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Eol {
    /// `\n`. The default for a file with no line ending at all.
    #[default]
    Lf,
    /// `\r\n`.
    Crlf,
}

impl Eol {
    /// The bytes this line ending writes.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::Crlf => "\r\n",
        }
    }
}

/// Everything `doc_save` needs in order to write bytes indistinguishable from
/// the ones `doc_open` read (A24).
///
/// Carried on `DocumentOpened`, on `SavedAck`, and in the durable sidecar, so a
/// collaborator who clones the repository inherits the same policy rather than
/// re-deriving it from whatever their editor last did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocBytes {
    /// The line ending to write. Majority wins if the file was mixed.
    pub eol: Eol,
    /// Whether to re-emit the UTF-8 BOM.
    pub bom: bool,
}

/// The result of reading a file's bytes into an editable buffer.
#[derive(Clone, PartialEq, Debug)]
pub struct Decoded {
    /// The document text, normalised to LF. This is what the editor edits and
    /// what every span in the crate is an index into.
    pub text: String,
    /// What [`encode`] needs to undo the normalisation.
    pub bytes: DocBytes,
    /// `L013` when the file mixed `\n` and `\r\n`; empty otherwise.
    pub diagnostics: Vec<Diagnostic>,
}

/// Why a file could not be opened as text.
///
/// There is exactly one variant on purpose: the only thing that can go wrong at
/// this layer is "these bytes are not UTF-8", and the answer is never to guess a
/// codepage. See [`decode`].
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum EncodingRefusal {
    /// The byte at `offset` cannot begin (or continue) a UTF-8 sequence.
    #[error("not valid UTF-8 at byte {offset}")]
    NotUtf8 {
        /// Byte offset of the first invalid sequence, from `Utf8Error::valid_up_to`.
        offset: usize,
    },
}

impl EncodingRefusal {
    /// The diagnostic shown to the user, carrying the offer to open read-only.
    ///
    /// `STRATUM0601` is W26's code from A24. It is deliberately *not* a
    /// `STATA…` code: Stata's r(601) is "file not found", and this file was
    /// found — we are declining to guess its codepage.
    pub fn diagnostic(&self) -> Diagnostic {
        let EncodingRefusal::NotUtf8 { offset } = *self;
        Diagnostic {
            severity: Severity::Error,
            code: "STRATUM0601".to_owned(),
            stata_rc: None,
            message: format!(
                "this file is not valid UTF-8 (first bad byte at offset {offset}); \
                 Stratum will not guess an encoding and will not rewrite it"
            ),
            file: None,
            span: Some(Span {
                start: offset as u32,
                end: offset as u32,
            }),
            offending_token: None,
            block: None,
            related: Vec::new(),
            // Deliberately no `Suggestion`: a suggestion carries `edits`, and
            // there is no edit that fixes this without transcoding the user's
            // source. Opening read-only is a UI affordance, not a document edit.
            suggestions: Vec::new(),
            notes: vec![
                "Open read-only to inspect it, or convert it with a tool you \
                 control (`iconv -f latin1 -t utf8`)."
                    .to_owned(),
            ],
            confidence: Confidence::Exact,
        }
    }
}

/// `L013` — this file mixes `\n` and `\r\n`.
fn mixed_eol_lint(lf: usize, crlf: usize, winner: Eol) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        code: "L013".to_owned(),
        stata_rc: None,
        message: format!(
            "mixed line endings ({lf} LF, {crlf} CRLF); saving normalises the \
             whole file to {}",
            match winner {
                Eol::Lf => "LF",
                Eol::Crlf => "CRLF",
            }
        ),
        file: None,
        span: None,
        offending_token: None,
        block: None,
        related: Vec::new(),
        suggestions: Vec::new(),
        notes: vec![
            "This is the one case where saving touches lines you did not edit. \
             Commit the normalisation on its own so the next diff is clean."
                .to_owned(),
        ],
        confidence: Confidence::Exact,
    }
}

/// Read raw file bytes into an editable LF buffer plus the policy to write them
/// back.
///
/// **Non-UTF-8 is refused, never converted.** A latin-1 do-file with a `£` in a
/// variable label comes back as [`EncodingRefusal`], the caller offers read-only,
/// and the bytes on disk are untouched. Lossily transcoding a researcher's
/// source — which is what every "just try latin-1" fallback amounts to — would
/// silently corrupt string literals and value labels, and the corruption would
/// only surface in output months later.
pub fn decode(raw: &[u8]) -> Result<Decoded, EncodingRefusal> {
    let (body, bom) = match raw.strip_prefix(BOM) {
        Some(rest) => (rest, true),
        None => (raw, false),
    };

    let text = std::str::from_utf8(body).map_err(|e| EncodingRefusal::NotUtf8 {
        // Offset in the ORIGINAL file, so the number in the diagnostic is the
        // one a hex editor shows.
        offset: e.valid_up_to() + if bom { BOM.len() } else { 0 },
    })?;

    let (eol, diagnostics) = scan_eol(text);
    Ok(Decoded {
        text: normalise(text, eol),
        bytes: DocBytes { eol, bom },
        diagnostics,
    })
}

/// Count line terminators and decide the file's EOL.
fn scan_eol(text: &str) -> (Eol, Vec<Diagnostic>) {
    let b = text.as_bytes();
    let mut lf = 0usize;
    let mut crlf = 0usize;
    for (i, &c) in b.iter().enumerate() {
        if c == b'\n' {
            if i > 0 && b[i - 1] == b'\r' {
                crlf += 1;
            } else {
                lf += 1;
            }
        }
    }
    // Ties go to CRLF: a tie can only happen on a file that is already mixed, and
    // the ONLY way a tie arises in practice is a Windows-authored file that one
    // Unix tool half-rewrote. Preferring CRLF puts it back where it started.
    let eol = if crlf >= lf && crlf > 0 {
        Eol::Crlf
    } else {
        Eol::Lf
    };
    let diagnostics = if lf > 0 && crlf > 0 {
        vec![mixed_eol_lint(lf, crlf, eol)]
    } else {
        Vec::new()
    };
    (eol, diagnostics)
}

/// Collapse `\r\n` to `\n`. A lone `\r` survives untouched (see the module
/// header).
fn normalise(text: &str, eol: Eol) -> String {
    if eol == Eol::Lf && !text.contains("\r\n") {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("\r\n") {
        out.push_str(&rest[..i]);
        out.push('\n');
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

/// Turn an LF buffer back into the exact bytes the file should contain.
///
/// This is the inverse of [`decode`] for every file whose line endings were
/// uniform, which is the property `tests/roundtrip.rs` asserts over the fixture
/// corpus. For a *mixed* file it is deliberately not an inverse — that is what
/// `L013` warns about.
pub fn encode(text: &str, bytes: DocBytes) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 3 + text.len() / 32);
    if bytes.bom {
        out.extend_from_slice(BOM);
    }
    match bytes.eol {
        Eol::Lf => out.extend_from_slice(text.as_bytes()),
        Eol::Crlf => {
            // EVERY `\n` gets a `\r`, with no look-behind.
            //
            // The tempting guard — "skip it if the previous byte is already
            // `\r`" — is wrong, and proptest found it: in an LF-normalised
            // buffer `\n` is *always* the terminator, so a `\r` in front of one
            // is a literal carriage return the file really contained. A source
            // file holding `\r\r\n` decodes to `\r\n` and must re-encode to
            // `\r\r\n`; the look-behind silently deleted that byte.
            for &c in text.as_bytes() {
                if c == b'\n' {
                    out.push(b'\r');
                }
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lf_file_round_trips_exactly() {
        let raw = b"sysuse auto\nsummarize price\n";
        let d = decode(raw).unwrap();
        assert_eq!(
            d.bytes,
            DocBytes {
                eol: Eol::Lf,
                bom: false
            }
        );
        assert!(d.diagnostics.is_empty());
        assert_eq!(encode(&d.text, d.bytes), raw);
    }

    #[test]
    fn crlf_with_bom_round_trips_exactly() {
        let raw = b"\xef\xbb\xbfsysuse auto\r\nsummarize price\r\n";
        let d = decode(raw).unwrap();
        assert_eq!(
            d.bytes,
            DocBytes {
                eol: Eol::Crlf,
                bom: true
            }
        );
        assert_eq!(d.text, "sysuse auto\nsummarize price\n");
        assert_eq!(encode(&d.text, d.bytes), raw);
    }

    #[test]
    fn mixed_file_raises_l013_and_majority_wins() {
        let raw = b"a\r\nb\r\nc\nd\r\n";
        let d = decode(raw).unwrap();
        assert_eq!(d.bytes.eol, Eol::Crlf);
        assert_eq!(d.diagnostics.len(), 1);
        assert_eq!(d.diagnostics[0].code, "L013");
        assert_eq!(d.diagnostics[0].severity, Severity::Warning);
    }

    #[test]
    fn lone_cr_is_not_a_line_ending_and_survives() {
        // A literal CR inside a string literal. If we treated it as a
        // terminator, `encode` would emit `\r\n` here and change the program.
        let raw = b"di \"x\ry\"\n";
        let d = decode(raw).unwrap();
        assert_eq!(d.bytes.eol, Eol::Lf);
        assert_eq!(encode(&d.text, d.bytes), raw);
    }

    #[test]
    fn lone_cr_survives_a_crlf_round_trip() {
        let raw = b"di \"x\ry\"\r\nlist\r\n";
        let d = decode(raw).unwrap();
        assert_eq!(d.bytes.eol, Eol::Crlf);
        assert_eq!(encode(&d.text, d.bytes), raw);
    }

    #[test]
    fn a_literal_cr_immediately_before_a_crlf_survives() {
        // Regression, found by `tests/roundtrip.rs`'s proptest. `\r\r\n` decodes
        // to `\r\n`, and an encoder that refuses to double a `\r` it already sees
        // writes back `\r\n` — deleting a byte of the user's file.
        let raw = b"di 1\r\r\nlist\r\n";
        let d = decode(raw).unwrap();
        assert_eq!(d.text, "di 1\r\nlist\n");
        assert_eq!(encode(&d.text, d.bytes), raw);
    }

    #[test]
    fn latin1_is_refused_not_transcoded() {
        // `£` in latin-1 is 0xA3, which is not a legal UTF-8 lead byte.
        let raw = b"label var y \"Wage (\xa3)\"\n";
        let err = decode(raw).unwrap_err();
        assert_eq!(err, EncodingRefusal::NotUtf8 { offset: 19 });
        let d = err.diagnostic();
        assert_eq!(d.code, "STRATUM0601");
        assert_eq!(d.severity, Severity::Error);
        // No suggestion means no edit means nothing can "helpfully" rewrite it.
        assert!(d.suggestions.is_empty());
    }

    #[test]
    fn bom_offset_is_reported_in_original_file_coordinates() {
        let mut raw = BOM.to_vec();
        raw.extend_from_slice(b"ab\xff");
        assert_eq!(
            decode(&raw).unwrap_err(),
            EncodingRefusal::NotUtf8 { offset: 5 }
        );
    }

    #[test]
    fn empty_file_defaults_to_lf_without_bom() {
        let d = decode(b"").unwrap();
        assert_eq!(
            d.bytes,
            DocBytes {
                eol: Eol::Lf,
                bom: false
            }
        );
        assert!(encode(&d.text, d.bytes).is_empty());
    }

    #[test]
    fn eol_serialises_as_the_wire_spelling() {
        assert_eq!(serde_json::to_string(&Eol::Crlf).unwrap(), "\"crlf\"");
        assert_eq!(serde_json::to_string(&Eol::Lf).unwrap(), "\"lf\"");
    }
}
