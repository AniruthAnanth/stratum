//! What the caller asks for, and what streams back.
//!
//! # `CommentProposal`, not `CommentPatch`
//!
//! `07` §13.1 sketches `TaskEvent::CommentPatch(Vec<CommentEdit>)` with the
//! comment "ALREADY verified by §8; safe to apply". This crate emits
//! [`TaskEvent::CommentProposal`] instead, and the difference is deliberate.
//!
//! §8's guarantee is that the *runtime's own lexer* sees an identical token
//! stream before and after. That proof lives in `stratum-intel::comment_safety`
//! (W20) and is run by W26's edit gate. ARCHITECTURE §5 gives this crate two
//! dependencies — proto and platform — precisely because the desktop links it
//! and C24 forbids the desktop from reaching the parser. **This crate therefore
//! cannot verify anything**, and a type flowing out of it labelled "already
//! verified, safe to apply" would be exactly the false assurance §8 exists to
//! prevent. What leaves here is a proposal: inert text, structurally incapable
//! of being anything but a comment ([`ProposedComment::validate`]), which
//! something that *can* lex the buffer then proves is comment-only before a
//! byte reaches the editor.

use serde::{Deserialize, Serialize};

use crate::context::budget::CommentScope;
use crate::context::packer::PackRequest;
use crate::provider::types::{Message, TokenUsage};
use crate::service::surface::Surface;

/// What the user asked for. `07` §5.1's `Intent`, with the parameters each
/// variant actually needs rather than a bare tag.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum Intent {
    /// `[Explain]` on a failed result, after the deterministic quick-fixes found
    /// nothing confident.
    ExplainError,
    /// `[Explain]` on a result card.
    ExplainResult,
    /// `[Check model]`.
    CheckModel,
    /// `[Suggest next step]`.
    NextStep,
    /// Inline ghost text.
    Complete,
    /// Spec §23's comment text.
    Comment {
        /// One block, or a whole file in a single request.
        scope: CommentScope,
    },
    /// A reproducibility finding.
    Repro {
        /// `true` for `[Draft fixes]`, which returns a patch confined to the
        /// lines the deterministic checks cited; `false` for `[Explain]`.
        draft_fixes: bool,
    },
    /// "Clean this into a reproducible block".
    HistoryCleanup,
    /// The panel.
    FreeForm,
}

impl Intent {
    /// The surface that runs this intent.
    ///
    /// One function, so the budget, the fetch mask, the privacy ceiling, the
    /// cancellation slot and the prompt cannot disagree about which surface a
    /// task belongs to.
    #[must_use]
    pub const fn surface(&self) -> Surface {
        match self {
            Self::ExplainError => Surface::QuickFix,
            Self::ExplainResult => Surface::ResultExplain,
            Self::CheckModel => Surface::CheckModel,
            Self::NextStep => Surface::NextStep,
            Self::Complete => Surface::GhostCompletion,
            Self::Comment { .. } => Surface::AutoComment,
            Self::Repro { .. } => Surface::ReproExplain,
            Self::HistoryCleanup => Surface::HistoryCleanup,
            Self::FreeForm => Surface::Chat,
        }
    }

    /// The comment scope, for the one intent that has one.
    #[must_use]
    pub const fn comment_scope(&self) -> CommentScope {
        match self {
            Self::Comment { scope } => *scope,
            _ => CommentScope::Block,
        }
    }

    /// Whether this intent expects structured JSON rather than prose.
    #[must_use]
    pub const fn is_structured(&self) -> bool {
        matches!(
            self,
            Self::Comment { .. } | Self::HistoryCleanup | Self::Repro { draft_fixes: true }
        )
    }
}

/// One anchor an auto-comment may attach to.
///
/// The hash closes the race where the user edits the file while the request is
/// in flight: a reply naming an anchor whose line no longer hashes the same is
/// rejected — and the *whole* patch is rejected with it, never partially
/// applied.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CommentAnchor {
    /// One-based line number in the document as sent.
    pub line: u32,
    /// `blake3` over the exact bytes of that line as sent, hex.
    pub hash: String,
}

impl CommentAnchor {
    /// Hash a line exactly as it will be sent.
    #[must_use]
    pub fn hash_line(text: &str) -> String {
        blake3::hash(text.as_bytes()).to_hex()[..32].to_owned()
    }

    /// Build an anchor for a line.
    #[must_use]
    pub fn new(line: u32, text: &str) -> Self {
        Self {
            line,
            hash: Self::hash_line(text),
        }
    }
}

/// One request, everything it needs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AiTask {
    /// What was asked.
    pub intent: Intent,
    /// What may be packed, and at what tier. Its `surface` is forced from
    /// [`Intent::surface`] by [`AiTask::new`].
    pub request: PackRequest,
    /// Legal anchors, for [`Intent::Comment`]. A reply naming anything else is
    /// dropped.
    pub anchors: Vec<CommentAnchor>,
    /// The lines the deterministic reproducibility checks cited, for
    /// [`Intent::Repro`]. A drafted patch touching any other line is rejected
    /// wholesale — enforced in [`super::parse`], not requested in the prompt.
    pub cited_lines: Vec<u32>,
    /// Prior turns, for [`Intent::FreeForm`]. Compacted client-side by
    /// [`super::compact_history`].
    pub history: Vec<Message>,
    /// The opt-in Fast profile of `07` §5.2.
    pub fast_profile: bool,
}

impl AiTask {
    /// Build a task, forcing the request's surface to match the intent.
    #[must_use]
    pub fn new(intent: Intent, mut request: PackRequest) -> Self {
        request.surface = intent.surface();
        Self {
            intent,
            request,
            anchors: Vec::new(),
            cited_lines: Vec::new(),
            history: Vec::new(),
            fast_profile: false,
        }
    }

    /// The surface this task runs on.
    #[must_use]
    pub const fn surface(&self) -> Surface {
        self.request.surface
    }
}

/// Where a proposed comment goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentPosition {
    /// Its own line, above the anchor.
    Above,
    /// Appended to the anchor line. Refused for `*`-style comments, because a
    /// `*` mid-line is multiplication, not a comment — exactly the bug class the
    /// §8 gates exist to make unrepresentable.
    Trailing,
}

/// What kind of comment this is. Drives nothing executable; it lets the UI group
/// a file-scope batch.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentKind {
    /// What this step does.
    Explain,
    /// Why it is done this way.
    Why,
    /// Something the reader should be careful about.
    Caveat,
    /// A section heading.
    Section,
}

/// One comment the model proposed. **Not an edit**: see the module header.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ProposedComment {
    /// The anchor's line number as sent.
    pub anchor_line: u32,
    /// The anchor's hash as sent.
    pub anchor_hash: String,
    /// Above or trailing.
    pub position: CommentPosition,
    /// The comment body — no delimiter, no prefix, one line.
    pub text: String,
    /// Classification.
    pub kind: CommentKind,
}

/// Why a proposed comment was thrown away.
///
/// These mirror `07` §8.1's `Rejected` deliberately: this is the *shape* check
/// at the parse boundary, refusing text that could re-enter the grammar before
/// it ever becomes an edit. It is not §8's proof and does not replace it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentRejection {
    /// Contains a line break, so applying it would produce a second line that is
    /// not a comment.
    Multiline,
    /// Contains `//`, `/*` or `*/`, so applying it could close or open a comment
    /// region.
    ContainsCommentDelimiter,
    /// Contains `///`, which is a line continuation, not a comment.
    ContainsContinuation,
    /// Nothing left after trimming.
    Empty,
    /// Longer than 240 bytes.
    TooLong,
    /// Contains a control character other than tab.
    ControlChar,
    /// Names an anchor the request did not offer, or one whose hash moved.
    UnknownAnchor,
    /// Trailing position on an anchor that cannot take one.
    BadPosition,
}

impl CommentRejection {
    /// The sentence the "3 of 18 comments could not be safely placed" disclosure
    /// shows for this rejection.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Multiline => "it spanned more than one line",
            Self::ContainsCommentDelimiter => "it contained a comment delimiter",
            Self::ContainsContinuation => "it contained a line continuation",
            Self::Empty => "it was empty",
            Self::TooLong => "it was longer than 240 characters",
            Self::ControlChar => "it contained a control character",
            Self::UnknownAnchor => "the line it referred to had changed",
            Self::BadPosition => "it could not be placed at the end of that line",
        }
    }
}

/// The longest comment body that will be accepted (`07` §8.1).
pub const MAX_COMMENT_BYTES: usize = 240;

impl ProposedComment {
    /// The shape check.
    ///
    /// # Errors
    /// [`CommentRejection`] for text that could re-enter the Stata grammar, or
    /// for an anchor the request never offered.
    pub fn validate(&self, anchors: &[CommentAnchor]) -> Result<(), CommentRejection> {
        let body = self.text.trim();
        if body.is_empty() {
            return Err(CommentRejection::Empty);
        }
        if body.len() > MAX_COMMENT_BYTES {
            return Err(CommentRejection::TooLong);
        }
        // U+2028 and U+2029 are line breaks to some editors and not to others,
        // which is precisely why they are listed: "it renders as one line here"
        // is not the property being checked.
        if body.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
            return Err(CommentRejection::Multiline);
        }
        if body.bytes().any(|b| b < 0x20 && b != b'\t') {
            return Err(CommentRejection::ControlChar);
        }
        if body.contains("///") {
            return Err(CommentRejection::ContainsContinuation);
        }
        if body.contains("//") || body.contains("/*") || body.contains("*/") {
            return Err(CommentRejection::ContainsCommentDelimiter);
        }
        let anchor = anchors
            .iter()
            .find(|a| a.line == self.anchor_line)
            .ok_or(CommentRejection::UnknownAnchor)?;
        if anchor.hash != self.anchor_hash {
            return Err(CommentRejection::UnknownAnchor);
        }
        Ok(())
    }
}

/// One line a drafted fix would replace.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ProposedEdit {
    /// One-based line number, which must be one the checks cited.
    pub line: u32,
    /// The whole replacement line.
    pub replacement: String,
    /// Why, for the diff view.
    pub why: String,
}

/// A drafted patch. Rendered as a diff, never auto-applied.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ProposedPatch {
    /// The edits, ordered by line.
    pub edits: Vec<ProposedEdit>,
}

/// What streams back from a task.
#[derive(Clone, PartialEq, Debug)]
pub enum TaskEvent {
    /// A thinking summary, when the surface asked for one.
    Progress(String),
    /// A chunk of prose.
    Text(String),
    /// A parsed, schema-shaped payload for an intent that asked for one.
    Structured(serde_json::Value),
    /// Comments the model proposed. **Inert text** — see the module header; the
    /// second element is what was dropped and why, which the UI reports as
    /// "3 of 18 comments could not be safely placed".
    CommentProposal(Vec<ProposedComment>, Vec<CommentRejection>),
    /// A drafted patch, already confined to the cited lines.
    Diff(ProposedPatch),
    /// Terminal, success.
    Done {
        /// Final accounting.
        usage: TokenUsage,
        /// Estimated cost from the shipped price table.
        cost_usd: f64,
    },
    /// Terminal, failure.
    Failed(crate::service::AiError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchors() -> Vec<CommentAnchor> {
        vec![CommentAnchor::new(42, "gen ln_price = log(price)")]
    }

    fn comment(text: &str) -> ProposedComment {
        ProposedComment {
            anchor_line: 42,
            anchor_hash: CommentAnchor::hash_line("gen ln_price = log(price)"),
            position: CommentPosition::Above,
            text: text.to_owned(),
            kind: CommentKind::Explain,
        }
    }

    #[test]
    fn a_well_formed_comment_survives() {
        assert!(
            comment("Log-transform price; the level model's residuals fan out.")
                .validate(&anchors())
                .is_ok()
        );
    }

    #[test]
    fn every_way_of_re_entering_the_grammar_is_refused() {
        for (text, expect) in [
            ("two\nlines", CommentRejection::Multiline),
            ("a\u{2028}b", CommentRejection::Multiline),
            ("close */ this", CommentRejection::ContainsCommentDelimiter),
            ("open /* this", CommentRejection::ContainsCommentDelimiter),
            ("a // b", CommentRejection::ContainsCommentDelimiter),
            ("continue ///", CommentRejection::ContainsContinuation),
            ("   ", CommentRejection::Empty),
            ("bell\u{7}", CommentRejection::ControlChar),
        ] {
            assert_eq!(comment(text).validate(&anchors()), Err(expect), "{text:?}");
        }
        assert_eq!(
            comment(&"x".repeat(241)).validate(&anchors()),
            Err(CommentRejection::TooLong)
        );
    }

    #[test]
    fn a_continuation_is_reported_as_a_continuation_not_as_a_delimiter() {
        // `///` contains `//`; the more specific rejection has to win or the
        // disclosure tells the user the wrong thing.
        assert_eq!(
            comment("wrap ///").validate(&anchors()),
            Err(CommentRejection::ContainsContinuation)
        );
    }

    #[test]
    fn an_anchor_that_moved_since_the_request_rejects_the_comment() {
        let mut c = comment("fine");
        c.anchor_hash = CommentAnchor::hash_line("gen ln_price = log(price + 1)");
        assert_eq!(c.validate(&anchors()), Err(CommentRejection::UnknownAnchor));
    }

    #[test]
    fn an_invented_anchor_is_refused() {
        let mut c = comment("fine");
        c.anchor_line = 999;
        assert_eq!(c.validate(&anchors()), Err(CommentRejection::UnknownAnchor));
    }

    #[test]
    fn every_intent_maps_to_exactly_one_surface_and_covers_all_nine() {
        let intents = [
            Intent::ExplainError,
            Intent::ExplainResult,
            Intent::CheckModel,
            Intent::NextStep,
            Intent::Complete,
            Intent::Comment {
                scope: CommentScope::Block,
            },
            Intent::Repro { draft_fixes: false },
            Intent::HistoryCleanup,
            Intent::FreeForm,
        ];
        let mut seen: Vec<Surface> = intents.iter().map(Intent::surface).collect();
        seen.sort_by_key(|s| Surface::as_str(*s));
        seen.dedup();
        assert_eq!(seen.len(), Surface::ALL.len());
    }

    #[test]
    fn a_task_cannot_be_built_with_a_surface_that_contradicts_its_intent() {
        let request = PackRequest {
            surface: Surface::Chat,
            ..PackRequest::default()
        };
        let task = AiTask::new(Intent::Complete, request);
        assert_eq!(task.surface(), Surface::GhostCompletion);
    }
}
