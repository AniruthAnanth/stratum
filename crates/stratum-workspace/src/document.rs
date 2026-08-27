//! Document text buffers — the desktop-side model of an open `.do` file.
//!
//! ARCHITECTURE C24 puts the buffer here rather than in `stratum-session`:
//! `stratum-desktop` links this crate and links none of the engine, so the text
//! the user is typing into must live on this side of the process boundary. The
//! engine's [`stratum_proto::BlockMap`] arrives as an event and is *attached* to
//! the buffer; it is never the source of truth for the text.
//!
//! A [`Document`] carries three things beyond its text: the [`DocBytes`] policy
//! observed when it was opened (A24, see [`crate::bytes`]), a monotonic
//! `version` that `doc_change` and every writer bump, and a `read_only` flag —
//! set when the file's bytes are not UTF-8 and we declined to transcode them.

use camino::{Utf8Path, Utf8PathBuf};
use stratum_proto::{Diagnostic, DocumentId, Edit, Span, TextHash};

use crate::bytes::{decode, DocBytes, EncodingRefusal, Eol};
use crate::write::text_hash_of;

/// Why an edit list could not be applied.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum EditError {
    /// An edit's span reaches past the end of the buffer.
    #[error("edit span {start}..{end} is outside the {len}-byte buffer")]
    OutOfRange {
        /// Span start.
        start: u32,
        /// Span end.
        end: u32,
        /// Buffer length.
        len: usize,
    },
    /// An edit's span is inverted.
    #[error("edit span {start}..{end} is inverted")]
    Inverted {
        /// Span start.
        start: u32,
        /// Span end.
        end: u32,
    },
    /// An edit's span starts or ends inside a multi-byte character.
    #[error("edit span {start}..{end} is not on a character boundary")]
    NotCharBoundary {
        /// Span start.
        start: u32,
        /// Span end.
        end: u32,
    },
    /// Two edits in one list touch the same bytes. Applying them is
    /// order-dependent, so it is refused rather than resolved.
    #[error("edits {a} and {b} overlap")]
    Overlap {
        /// Index of the first edit.
        a: usize,
        /// Index of the second.
        b: usize,
    },
}

/// Apply a set of edits to a buffer, returning the new text.
///
/// Edits are expressed against the **original** buffer — that is what makes an
/// edit list from the engine, from a gate and from the frontend all mean the same
/// thing — so they are applied back-to-front and overlaps are an error rather
/// than a race.
pub fn apply_edits(before: &str, edits: &[Edit]) -> Result<String, EditError> {
    let len = before.len();
    let mut order: Vec<usize> = (0..edits.len()).collect();
    order.sort_by_key(|&i| (edits[i].span.start, edits[i].span.end));

    for w in order.windows(2) {
        let (a, b) = (&edits[w[0]], &edits[w[1]]);
        if a.span.end > b.span.start {
            return Err(EditError::Overlap { a: w[0], b: w[1] });
        }
    }

    let mut out = String::with_capacity(len);
    let mut cursor = 0usize;
    for &i in &order {
        let Span { start, end } = edits[i].span;
        let (s, e) = (start as usize, end as usize);
        if s > e {
            return Err(EditError::Inverted { start, end });
        }
        if e > len {
            return Err(EditError::OutOfRange { start, end, len });
        }
        if !before.is_char_boundary(s) || !before.is_char_boundary(e) {
            return Err(EditError::NotCharBoundary { start, end });
        }
        out.push_str(&before[cursor..s]);
        out.push_str(&edits[i].text);
        cursor = e;
    }
    out.push_str(&before[cursor..]);
    Ok(out)
}

/// An open document.
#[derive(Clone, PartialEq, Debug)]
pub struct Document {
    /// Engine-visible id.
    pub id: DocumentId,
    /// `None` for an untitled buffer, which has no bytes to be faithful to yet.
    pub path: Option<Utf8PathBuf>,
    /// The text, always LF-normalised (see [`crate::bytes`]).
    pub text: String,
    /// The byte policy observed on open and reproduced on save.
    pub bytes: DocBytes,
    /// Bumped by every `doc_change` and by every writer. The frontend drops any
    /// `BlockMap` computed against an older version.
    pub version: u64,
    /// Set when the file could not be decoded as UTF-8 (`STRATUM0601`). A
    /// read-only document is displayed and never written.
    pub read_only: bool,
    /// Diagnostics produced by opening the file: `L013` for mixed EOL,
    /// `STRATUM0601` when read-only.
    pub diagnostics: Vec<Diagnostic>,
}

/// The result of a refused open — the caller offers "open read-only".
#[derive(Clone, PartialEq, Debug)]
pub struct RefusedOpen {
    /// Why it was refused.
    pub refusal: EncodingRefusal,
    /// The diagnostic to show, `STRATUM0601`.
    pub diagnostic: Diagnostic,
    /// Always true today: every refusal at this layer is an encoding refusal, and
    /// showing the file without the ability to write it is always safe.
    pub can_open_read_only: bool,
}

impl Document {
    /// An untitled buffer. LF, no BOM — a file we are inventing has no history to
    /// be faithful to, and LF is what every version-control system prefers.
    pub fn untitled(id: DocumentId, text: impl Into<String>) -> Self {
        Document {
            id,
            path: None,
            text: text.into(),
            bytes: DocBytes {
                eol: Eol::Lf,
                bom: false,
            },
            version: 0,
            read_only: false,
            diagnostics: Vec::new(),
        }
    }

    /// `doc_open` for a file on disk.
    ///
    /// Returns `Err` when the bytes are not UTF-8. **The file is not touched**;
    /// the caller shows `STRATUM0601` and offers [`Document::open_read_only`].
    /// We never lossily transcode a researcher's source.
    pub fn open(id: DocumentId, path: &Utf8Path, raw: &[u8]) -> Result<Self, Box<RefusedOpen>> {
        match decode(raw) {
            Ok(d) => Ok(Document {
                id,
                path: Some(path.to_owned()),
                text: d.text,
                bytes: d.bytes,
                version: 0,
                read_only: false,
                diagnostics: d
                    .diagnostics
                    .into_iter()
                    .map(|mut x| {
                        x.file = Some(path.to_owned());
                        x
                    })
                    .collect(),
            }),
            Err(refusal) => {
                let mut diagnostic = refusal.diagnostic();
                diagnostic.file = Some(path.to_owned());
                Err(Box::new(RefusedOpen {
                    refusal,
                    diagnostic,
                    can_open_read_only: true,
                }))
            }
        }
    }

    /// Show a file we refused to decode, without the ability to write it.
    ///
    /// The undecodable bytes are replaced for display only; `read_only` is set,
    /// so no writer can ever reach [`crate::write::write_document`] with this
    /// document and the bytes on disk stay exactly as they were.
    pub fn open_read_only(id: DocumentId, path: &Utf8Path, raw: &[u8]) -> Self {
        let lossy = String::from_utf8_lossy(raw).into_owned();
        let mut doc = Document {
            id,
            path: Some(path.to_owned()),
            text: lossy.replace("\r\n", "\n"),
            bytes: DocBytes::default(),
            version: 0,
            read_only: true,
            diagnostics: Vec::new(),
        };
        if let Err(refusal) = decode(raw) {
            let mut d = refusal.diagnostic();
            d.file = Some(path.to_owned());
            doc.diagnostics.push(d);
        }
        doc
    }

    /// `doc_change`. Bumps [`Document::version`].
    pub fn apply(&mut self, edits: &[Edit]) -> Result<(), EditError> {
        self.text = apply_edits(&self.text, edits)?;
        self.version += 1;
        Ok(())
    }

    /// Replace the whole buffer, e.g. after a gated writer produced new text.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.version += 1;
    }

    /// blake3-128 over the bytes this document *would* write, so it can be
    /// compared against a [`crate::write::SavedAck`] to answer "is the buffer
    /// dirty relative to disk?".
    pub fn text_hash(&self) -> TextHash {
        TextHash(text_hash_of(&crate::bytes::encode(&self.text, self.bytes)))
    }

    /// Byte offset of the start of each line, plus a sentinel at `text.len()`.
    ///
    /// The sentinel is what makes `line_span` total: without it the last line
    /// needs a special case at every call site, and that special case is where
    /// off-by-one bugs in an editor live.
    pub fn line_starts(&self) -> Vec<u32> {
        let mut v = vec![0u32];
        for (i, c) in self.text.bytes().enumerate() {
            if c == b'\n' {
                v.push(i as u32 + 1);
            }
        }
        if *v.last().unwrap() as usize != self.text.len() {
            v.push(self.text.len() as u32);
        }
        v
    }

    /// Span of physical line `line` (0-based) **including** its terminator.
    pub fn line_span(&self, line: usize) -> Option<Span> {
        let starts = self.line_starts();
        let start = *starts.get(line)?;
        let end = *starts.get(line + 1)?;
        Some(Span { start, end })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(start: u32, end: u32, text: &str) -> Edit {
        Edit {
            span: Span { start, end },
            text: text.to_owned(),
        }
    }

    #[test]
    fn edits_are_against_the_original_buffer() {
        // Both spans index the ORIGINAL text; the second must not shift because
        // the first inserted bytes before it.
        let out = apply_edits("abcdef", &[e(0, 1, "XX"), e(4, 5, "YY")]).unwrap();
        assert_eq!(out, "XXbcdYYf");
    }

    #[test]
    fn overlapping_edits_are_refused_not_resolved() {
        let err = apply_edits("abcdef", &[e(0, 3, "X"), e(2, 4, "Y")]).unwrap_err();
        assert!(matches!(err, EditError::Overlap { .. }));
    }

    #[test]
    fn an_edit_past_the_end_is_an_error() {
        assert!(matches!(
            apply_edits("abc", &[e(2, 9, "x")]).unwrap_err(),
            EditError::OutOfRange { .. }
        ));
    }

    #[test]
    fn an_edit_inside_a_multibyte_char_is_an_error() {
        assert!(matches!(
            apply_edits("a£b", &[e(1, 2, "x")]).unwrap_err(),
            EditError::NotCharBoundary { .. }
        ));
    }

    #[test]
    fn refused_open_offers_read_only_and_keeps_the_bytes() {
        let raw = b"label var y \"Wage (\xa3)\"\n";
        let path = Utf8Path::new("/tmp/x.do");
        let refused = Document::open(DocumentId(1), path, raw).unwrap_err();
        assert!(refused.can_open_read_only);
        assert_eq!(refused.diagnostic.code, "STRATUM0601");

        let ro = Document::open_read_only(DocumentId(1), path, raw);
        assert!(ro.read_only);
        assert_eq!(ro.diagnostics.len(), 1);
    }

    #[test]
    fn line_starts_end_with_a_sentinel() {
        let d = Document::untitled(DocumentId(1), "a\nbb\n");
        assert_eq!(d.line_starts(), vec![0, 2, 5]);
        assert_eq!(d.line_span(0), Some(Span { start: 0, end: 2 }));
        assert_eq!(d.line_span(1), Some(Span { start: 2, end: 5 }));
        assert_eq!(d.line_span(2), None);
    }

    #[test]
    fn line_starts_handle_a_file_with_no_trailing_newline() {
        let d = Document::untitled(DocumentId(1), "a\nbb");
        assert_eq!(d.line_starts(), vec![0, 2, 4]);
    }
}
