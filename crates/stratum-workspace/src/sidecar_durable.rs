//! The **durable** sidecar — `.<name>.do.workspace`, ARCHITECTURE C19, spec §5.
//!
//! This is the committed half of the two-artifact split. It holds what is
//! genuinely part of the document and would be a real loss on clone — section
//! names and order, collapse *intent*, pinned comparisons, auto-comment anchor
//! hashes, the per-file inline-results mode, and the A24 byte policy — and
//! **nothing else**.
//!
//! Four properties, each of which exists because violating it breaks something
//! specific:
//!
//! * **No output, ever.** That is spec §6 and ADR-010. Output lives in
//!   `.stratum/cache/<hash>/`, which is gitignored.
//! * **No timestamps, no durations, no execution ids.** They change on every
//!   run, so a file carrying them conflicts in version control on every run —
//!   which is precisely the Jupyter defect §35 lists.
//! * **Deterministic bytes.** Sorted collections, stable field order, LF,
//!   trailing newline. Two people with the same intent produce the same file, so
//!   the file only appears in a diff when the intent actually changed.
//! * **Optional.** Delete it and the `.do` opens fine; you lose section *names*
//!   (the `// %%` markers themselves are in the source, so the sections survive)
//!   and collapse intent. `stratum-workspace` must tolerate it being absent or
//!   stale (C19 T).
//!
//! Hashes serialise as 32-character lowercase hex, per CONTRACTS §12's
//! `CodeHash = string`. See [`hex`] for why that needs an adapter here.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use stratum_proto::{CodeHash, InlineResultsMode, SectionId};

use crate::bytes::{DocBytes, Eol};
use crate::write::write_bytes_atomic;

/// The schema version written into every sidecar. CONTRACTS §12 pins it at 1,
/// and CONTRACTS §15 versions it independently of `LayoutSpec.schema`.
pub const SCHEMA: u32 = 1;

/// A section's durable record: its id, its name, and where it was.
///
/// The span is advisory. The `// %%` markers in the source are authoritative, so
/// a sidecar written before an edit is *stale*, not *wrong*, and
/// [`DurableSidecar::reconcile`] is what turns a stale record back into a live
/// one.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SidecarSection {
    /// Matches [`stratum_proto::SectionId`].
    pub id: u32,
    /// The heading text, as it appears after `%%`.
    pub title: String,
    /// `[start, end)` byte offsets when the sidecar was written.
    pub span: (u32, u32),
}

/// A pinned model comparison (spec §19).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PinnedComparison {
    /// User-visible name of the comparison.
    pub name: String,
    /// Stored-estimate names, in display order. Order is meaningful here, so
    /// this vector is **not** sorted by [`DurableSidecar::canonicalise`].
    pub results: Vec<String>,
}

/// Auto-comment idempotency anchor (spec §23).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoCommentAnchor {
    /// The block the comment was written for.
    #[serde(with = "hex::one")]
    pub block_hash: CodeHash,
    /// The comment that was written, so re-running auto-comment on an unchanged
    /// block is a no-op rather than a duplicate.
    #[serde(with = "hex::one")]
    pub comment_hash: CodeHash,
}

/// **A34.** The committed sidecar holds the stable *association* only; the
/// transcript body lives in `.stratum/cache/<hash>/ui/`, which is gitignored.
///
/// A collaborator who clones the repository therefore sees "this block has a
/// conversation you do not have a copy of" rather than either a committed wall
/// of chat text or a silently missing link.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConversationRef {
    /// The block the conversation is about.
    #[serde(with = "hex::one")]
    pub block_hash: CodeHash,
    /// Opaque id, resolvable only against the local cache.
    pub conversation_id: String,
}

/// The durable sidecar. Transcribed from CONTRACTS §12.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableSidecar {
    /// Always [`SCHEMA`].
    pub schema: u32,
    /// Sections, sorted by id.
    pub sections: Vec<SidecarSection>,
    /// Collapse **intent**, keyed by code hash rather than by position, so it
    /// survives an edit above it.
    #[serde(with = "hex::many")]
    pub collapsed: Vec<CodeHash>,
    /// Per-file override of the layout's inline-results default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_results: Option<InlineResultsMode>,
    /// Per-file Document View state (spec §24).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_view: Option<bool>,
    /// Pinned model comparisons (spec §19).
    pub pinned_comparisons: Vec<PinnedComparison>,
    /// Auto-comment anchors (spec §23 idempotency).
    pub auto_comment_anchors: Vec<AutoCommentAnchor>,
    /// AI conversation associations (A34) — ids only, never transcripts.
    pub ai_conversations: Vec<AiConversationRef>,
    /// **A24.** The line ending observed on open and reproduced on save.
    pub eol: Eol,
    /// **A24.** Whether the file carries a UTF-8 BOM.
    pub bom: bool,
}

impl Default for DurableSidecar {
    fn default() -> Self {
        DurableSidecar {
            schema: SCHEMA,
            sections: Vec::new(),
            collapsed: Vec::new(),
            inline_results: None,
            doc_view: None,
            pinned_comparisons: Vec::new(),
            auto_comment_anchors: Vec::new(),
            ai_conversations: Vec::new(),
            eol: Eol::Lf,
            bom: false,
        }
    }
}

/// A partial update — the payload of `sidecar_patch { doc, patch }`.
///
/// Every field is optional and `None` means "leave alone". A patch that replaced
/// the whole document would make two panes racing to persist unrelated intent
/// clobber each other.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DurableSidecarPatch {
    /// Replace the section list.
    pub sections: Option<Vec<SidecarSection>>,
    /// Replace the collapse set.
    #[serde(with = "hex::many_opt")]
    pub collapsed: Option<Vec<CodeHash>>,
    /// Set or clear the per-file inline-results mode.
    pub inline_results: Option<Option<InlineResultsMode>>,
    /// Set or clear the per-file Document View flag.
    pub doc_view: Option<Option<bool>>,
    /// Replace the pinned comparisons.
    pub pinned_comparisons: Option<Vec<PinnedComparison>>,
    /// Replace the auto-comment anchors.
    pub auto_comment_anchors: Option<Vec<AutoCommentAnchor>>,
    /// Replace the AI conversation associations.
    pub ai_conversations: Option<Vec<AiConversationRef>>,
    /// Record a new byte policy (only `doc_open` should ever set this).
    pub doc_bytes: Option<DocBytes>,
}

/// Reading or writing the sidecar failed.
#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    /// The file exists but is not the JSON we wrote.
    #[error("{path} is not a readable sidecar: {source}")]
    Malformed {
        /// The sidecar's path.
        path: Utf8PathBuf,
        /// The parse error.
        #[source]
        source: serde_json::Error,
    },
    /// The filesystem said no.
    #[error("{path}: {source}")]
    Io {
        /// The sidecar's path.
        path: Utf8PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// `analysis.do` → `.analysis.do.workspace`, spec §5's layout.
pub fn sidecar_path(doc: &Utf8Path) -> Utf8PathBuf {
    let name = doc.file_name().unwrap_or("document.do");
    doc.parent()
        .unwrap_or_else(|| Utf8Path::new("."))
        .join(format!(".{name}.workspace"))
}

impl DurableSidecar {
    /// Read the sidecar beside `doc`.
    ///
    /// **An absent sidecar is not an error** — it is the normal state of a
    /// freshly cloned repository, and C19's tolerance requirement. A *malformed*
    /// one is an error, because silently replacing a colleague's section names
    /// with defaults is worse than saying so.
    pub fn load(doc: &Utf8Path) -> Result<Self, SidecarError> {
        let path = sidecar_path(doc);
        let raw = match std::fs::read(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(SidecarError::Io { path, source }),
        };
        serde_json::from_slice(&raw).map_err(|source| SidecarError::Malformed { path, source })
    }

    /// Write the sidecar beside `doc`, canonically.
    pub fn save(&self, doc: &Utf8Path) -> Result<Utf8PathBuf, SidecarError> {
        let path = sidecar_path(doc);
        let bytes = self.to_canonical_bytes();
        write_bytes_atomic(&path, &bytes).map_err(|source| SidecarError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    /// The exact bytes [`DurableSidecar::save`] writes: sorted collections,
    /// stable field order, two-space indent, LF, one trailing newline.
    ///
    /// `serde_json`'s pretty printer emits `\n` on every platform, so this is
    /// byte-identical on Windows without a `.gitattributes` rule — which matters,
    /// because the file is committed.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut c = self.clone();
        c.canonicalise();
        let mut s = serde_json::to_string_pretty(&c).expect("DurableSidecar is always encodable");
        s.push('\n');
        s.into_bytes()
    }

    /// Sort every collection whose order carries no meaning.
    ///
    /// [`PinnedComparison::results`] is deliberately left alone: the order of
    /// models in a comparison table is the user's choice.
    pub fn canonicalise(&mut self) {
        self.schema = SCHEMA;
        self.sections.sort_by_key(|s| s.id);
        self.sections.dedup_by_key(|s| s.id);
        self.collapsed.sort_by_key(|h| h.0);
        self.collapsed.dedup();
        self.pinned_comparisons.sort_by(|a, b| a.name.cmp(&b.name));
        self.auto_comment_anchors
            .sort_by_key(|a| (a.block_hash.0, a.comment_hash.0));
        self.auto_comment_anchors.dedup();
        self.ai_conversations.sort_by(|a, b| {
            (a.block_hash.0, &a.conversation_id).cmp(&(b.block_hash.0, &b.conversation_id))
        });
        self.ai_conversations.dedup();
    }

    /// Apply a `sidecar_patch`.
    pub fn patch(&mut self, p: DurableSidecarPatch) {
        if let Some(v) = p.sections {
            self.sections = v;
        }
        if let Some(v) = p.collapsed {
            self.collapsed = v;
        }
        if let Some(v) = p.inline_results {
            self.inline_results = v;
        }
        if let Some(v) = p.doc_view {
            self.doc_view = v;
        }
        if let Some(v) = p.pinned_comparisons {
            self.pinned_comparisons = v;
        }
        if let Some(v) = p.auto_comment_anchors {
            self.auto_comment_anchors = v;
        }
        if let Some(v) = p.ai_conversations {
            self.ai_conversations = v;
        }
        if let Some(b) = p.doc_bytes {
            self.eol = b.eol;
            self.bom = b.bom;
        }
        self.canonicalise();
    }

    /// The A24 byte policy this sidecar records.
    pub fn doc_bytes(&self) -> DocBytes {
        DocBytes {
            eol: self.eol,
            bom: self.bom,
        }
    }

    /// Re-seat the section records against the markers actually in `text`.
    ///
    /// The source is authoritative: a marker the sidecar does not know about
    /// becomes a section with the title from the source, and a record whose
    /// marker is gone is dropped. This is what makes a stale sidecar harmless.
    pub fn reconcile(&mut self, text: &str) {
        self.sections = crate::sections::index(text)
            .into_iter()
            .map(|s| SidecarSection {
                id: s.id.0,
                title: s.title,
                span: (s.span.start, s.span.end),
            })
            .collect();
        self.canonicalise();
    }

    /// The section ids this sidecar knows, as proto ids.
    pub fn section_ids(&self) -> Vec<SectionId> {
        self.sections.iter().map(|s| SectionId(s.id)).collect()
    }
}

/// Hex adapters for [`stratum_proto::CodeHash`].
///
/// **Contract note (escalated, see the unit's report).** CONTRACTS §12 declares
/// `CodeHash` on the TypeScript side as `string & {...}` — "16 bytes, lowercase
/// hex, 32 chars" — but `stratum_proto::CodeHash` is
/// `struct CodeHash(pub [u8; 16])` with a derived `Serialize`, which encodes as a
/// **JSON array of 16 numbers**. `stratum-proto` is frozen (R1), so rather than
/// patch it, the committed sidecar uses these adapters and matches §12 exactly.
/// A 32-character hex string is also the only form a human can diff, and this
/// file is meant to be read in a pull request.
pub mod hex {
    use serde::{Deserialize, Deserializer, Serializer};
    use stratum_proto::CodeHash;

    fn encode(h: &CodeHash) -> String {
        let mut s = String::with_capacity(32);
        for b in h.0 {
            s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
            s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
        }
        s
    }

    fn decode<E: serde::de::Error>(s: &str) -> Result<CodeHash, E> {
        if s.len() != 32 {
            return Err(E::custom(format!(
                "a CodeHash is 32 hex characters, got {}",
                s.len()
            )));
        }
        let mut out = [0u8; 16];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = (chunk[0] as char)
                .to_digit(16)
                .ok_or_else(|| E::custom("non-hex character in CodeHash"))?;
            let lo = (chunk[1] as char)
                .to_digit(16)
                .ok_or_else(|| E::custom("non-hex character in CodeHash"))?;
            out[i] = ((hi << 4) | lo) as u8;
        }
        Ok(CodeHash(out))
    }

    /// `#[serde(with = "hex::one")]` for a single [`CodeHash`] field.
    pub mod one {
        use super::*;

        /// Serialize as 32 lowercase hex characters.
        pub fn serialize<S: Serializer>(h: &CodeHash, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&encode(h))
        }

        /// Deserialize from 32 lowercase hex characters.
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<CodeHash, D::Error> {
            let s = String::deserialize(d)?;
            decode(&s)
        }
    }

    /// `#[serde(with = "hex::many")]` for a `Vec<CodeHash>` field.
    pub mod many {
        use super::*;

        /// Serialize as an array of hex strings.
        pub fn serialize<S: Serializer>(v: &[CodeHash], s: S) -> Result<S::Ok, S::Error> {
            let hex: Vec<String> = v.iter().map(encode).collect();
            serde::Serialize::serialize(&hex, s)
        }

        /// Deserialize from an array of hex strings.
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<CodeHash>, D::Error> {
            let hex = Vec::<String>::deserialize(d)?;
            hex.iter().map(|s| decode(s)).collect()
        }
    }

    /// `#[serde(with = "hex::many_opt")]` for an `Option<Vec<CodeHash>>` field.
    pub mod many_opt {
        use super::*;

        /// Serialize `Some` as an array of hex strings, `None` as `null`.
        pub fn serialize<S: Serializer>(
            v: &Option<Vec<CodeHash>>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            match v {
                None => s.serialize_none(),
                Some(v) => {
                    let hex: Vec<String> = v.iter().map(encode).collect();
                    serde::Serialize::serialize(&Some(hex), s)
                }
            }
        }

        /// Deserialize `null` as `None`.
        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<Vec<CodeHash>>, D::Error> {
            let hex = Option::<Vec<String>>::deserialize(d)?;
            hex.map(|v| v.iter().map(|s| decode(s)).collect())
                .transpose()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> CodeHash {
        CodeHash([n; 16])
    }

    #[test]
    fn sidecar_path_matches_the_spec_layout() {
        assert_eq!(
            sidecar_path(Utf8Path::new("/p/analysis.do")),
            Utf8PathBuf::from("/p/.analysis.do.workspace")
        );
    }

    #[test]
    fn hashes_are_32_lowercase_hex_characters_on_the_wire() {
        let s = DurableSidecar {
            collapsed: vec![CodeHash([
                0xab, 0xcd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff,
            ])],
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::from_slice(&s.to_canonical_bytes()).unwrap();
        let hex = v["collapsed"][0].as_str().unwrap();
        assert_eq!(hex.len(), 32, "{hex}");
        assert!(hex.starts_with("abcd") && hex.ends_with("ff"), "{hex}");
        assert!(hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn canonical_bytes_are_stable_under_input_order() {
        let a = DurableSidecar {
            collapsed: vec![h(3), h(1), h(2)],
            ..Default::default()
        };
        let b = DurableSidecar {
            collapsed: vec![h(2), h(3), h(1), h(1)],
            ..Default::default()
        };
        assert_eq!(a.to_canonical_bytes(), b.to_canonical_bytes());
    }

    #[test]
    fn canonical_bytes_are_lf_with_a_trailing_newline() {
        let raw = DurableSidecar::default().to_canonical_bytes();
        assert!(!raw.contains(&b'\r'));
        assert_eq!(raw.last(), Some(&b'\n'));
    }

    #[test]
    fn the_sidecar_carries_no_timestamp_no_duration_and_no_output() {
        // The keys that must never appear. C19: "no timestamps, no durations, no
        // execution ids, no output".
        let s = DurableSidecar {
            sections: vec![SidecarSection {
                id: 0,
                title: "Setup".into(),
                span: (0, 12),
            }],
            ai_conversations: vec![AiConversationRef {
                block_hash: h(7),
                conversation_id: "c1".into(),
            }],
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::from_slice(&s.to_canonical_bytes()).unwrap();
        let mut keys = Vec::new();
        collect_keys(&v, &mut keys);
        for k in &keys {
            let lower = k.to_lowercase();
            for banned in [
                "_ms",
                "time",
                "date",
                "duration",
                "elapsed",
                "stamp",
                "execution",
                "output",
                "log",
            ] {
                assert!(
                    !lower.contains(banned),
                    "sidecar key {k:?} leaks {banned:?}"
                );
            }
        }
        assert!(keys.contains(&"sections".to_owned()));
    }

    fn collect_keys(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, v) in m {
                    out.push(k.clone());
                    collect_keys(v, out);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|v| collect_keys(v, out)),
            _ => {}
        }
    }

    #[test]
    fn round_trips_through_json() {
        let s = DurableSidecar {
            inline_results: Some(InlineResultsMode::Compact),
            doc_view: Some(true),
            eol: Eol::Crlf,
            bom: true,
            collapsed: vec![h(9)],
            auto_comment_anchors: vec![AutoCommentAnchor {
                block_hash: h(1),
                comment_hash: h(2),
            }],
            ..Default::default()
        };
        let raw = s.to_canonical_bytes();
        let back: DurableSidecar = serde_json::from_slice(&raw).unwrap();
        let mut expected = s.clone();
        expected.canonicalise();
        assert_eq!(back, expected);
    }

    #[test]
    fn a_patch_only_touches_the_fields_it_names() {
        let mut s = DurableSidecar {
            doc_view: Some(true),
            collapsed: vec![h(1)],
            ..Default::default()
        };
        s.patch(DurableSidecarPatch {
            collapsed: Some(vec![h(4)]),
            ..Default::default()
        });
        assert_eq!(s.collapsed, vec![h(4)]);
        assert_eq!(s.doc_view, Some(true));
    }

    #[test]
    fn reconcile_takes_the_source_as_authoritative() {
        let mut s = DurableSidecar {
            sections: vec![SidecarSection {
                id: 0,
                title: "Whatever the sidecar remembered".into(),
                span: (0, 1),
            }],
            ..Default::default()
        };
        s.reconcile("// %% Real\nlist\n");
        assert_eq!(s.sections.len(), 1);
        assert_eq!(s.sections[0].title, "Real");
    }
}
