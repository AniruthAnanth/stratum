//! 07 §4–§5 — what leaves the machine, and nothing else.
//!
//! # The gate is structural
//!
//! The packer does not decide what to send. It emits typed [`ContextItem`]s,
//! each carrying the minimum tier at which it is *permitted*, and [`gate`] is
//! one `filter`. There is no code path from a variable summary or a macro value
//! to a [`crate::provider::ChatRequest`] that does not construct a
//! `ContextItem` with a `min_tier`, because [`PackedPrompt`]'s fields are
//! private and its only constructor takes gated items. Adding a new context
//! source therefore *forces* the author to declare a tier at the type level.

pub mod adapter;
pub mod audit;
pub mod budget;
pub mod packer;
pub mod policy;
pub mod redact;
pub mod render;
pub mod tiers;
pub mod want;

use serde::{Deserialize, Serialize};

pub use tiers::PrivacyTier;

use crate::provider::types::{Message, Role, SystemChunk};

/// Where a block of context came from. Rendered as the provenance label in the
/// pre-send preview and in the post-hoc "what was sent" viewer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    /// Task instruction and output contract. Carries no user data at all.
    Task,
    /// Frame, observation count, dataset state, sort keys.
    Session,
    /// The block, selection or error site the user acted on.
    Focus,
    /// Return code, message and the tail of the output.
    Errors,
    /// Names, types, formats, labels — and, at tier 2, summary statistics.
    Variables,
    /// Stored estimates.
    Estimates,
    /// Macros, frames and settings.
    Macros,
    /// Preceding executed blocks.
    Block,
    /// Project file excerpts.
    Files,
    /// What the user typed into the panel.
    UserText,
    /// The packer's own note about what it left out and why. Counts and
    /// category names, never data — it exists because a model that is not told
    /// its context was trimmed will confidently assert that the missing thing
    /// does not exist (`07` §5.3).
    Omissions,
}

impl ContextSource {
    /// The label the preview shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Session => "session",
            Self::Focus => "focus",
            Self::Errors => "errors",
            Self::Variables => "variables",
            Self::Estimates => "estimates",
            Self::Macros => "macros",
            Self::Block => "blocks",
            Self::Files => "files",
            Self::UserText => "your message",
            Self::Omissions => "omissions",
        }
    }
}

/// One rendered block of context, with the floor at which it may be sent.
///
/// `body` is already rendered because the tier decision has to be made on the
/// exact bytes: a struct that rendered later could render a field the gate
/// never saw.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ContextItem {
    /// Provenance.
    pub source: ContextSource,
    /// The floor at which this item may be sent.
    pub min_tier: PrivacyTier,
    /// Budget ordering; lower is placed first (07 §5.3).
    pub priority: u8,
    /// The packer's own estimate, so the budget fill needs no provider call.
    pub est_tokens: u32,
    /// The rendered text.
    pub body: String,
}

/// **The privacy gate.** One comparison, over typed items.
///
/// `filter(|i| i.min_tier <= effective)` — ADR-012, verbatim. Everything else in
/// this module exists to make sure every byte that could reach a provider is
/// inside a `ContextItem` first.
#[must_use]
pub fn gate(items: Vec<ContextItem>, effective: PrivacyTier) -> Vec<ContextItem> {
    items
        .into_iter()
        .filter(|i| i.min_tier <= effective)
        .collect()
}

/// The bytes handed to [`crate::provider`].
///
/// Fields are private and the only constructor is [`PackedPrompt::from_gated`],
/// which takes items that have already been through [`gate`]. That is what makes
/// "there is no path from a value to a request that skips the gate" a property
/// of the module rather than a claim about its callers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PackedPrompt {
    system: Vec<SystemChunk>,
    messages: Vec<Message>,
    /// Retained for the preview and the audit record.
    blocks: Vec<PromptBlock>,
    tier: PrivacyTier,
}

/// One block of the prompt, with the provenance the preview labels it with.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PromptBlock {
    /// Where it came from.
    pub source: ContextSource,
    /// The floor at which it was permitted.
    pub min_tier: PrivacyTier,
    /// Estimated size.
    pub est_tokens: u32,
    /// The exact bytes.
    pub body: String,
}

impl PackedPrompt {
    /// Assemble a prompt from **already gated** items.
    ///
    /// `framing` is the cache-stable system prefix of 07 §5.6 and is never
    /// gated: it contains no user data at all, which an `insta`-style byte
    /// assertion in [`crate::tasks`] locks down.
    #[must_use]
    pub fn from_gated(
        framing: String,
        gated: Vec<ContextItem>,
        user_message: String,
        tier: PrivacyTier,
    ) -> Self {
        let mut context = String::new();
        let mut blocks = Vec::with_capacity(gated.len() + 1);
        blocks.push(PromptBlock {
            source: ContextSource::Task,
            min_tier: PrivacyTier::Off,
            est_tokens: crate::provider::backends::anthropic::estimate_tokens(&framing),
            body: framing.clone(),
        });
        for item in gated {
            if !context.is_empty() {
                context.push_str("\n\n");
            }
            context.push_str(&item.body);
            blocks.push(PromptBlock {
                source: item.source,
                min_tier: item.min_tier,
                est_tokens: item.est_tokens,
                body: item.body,
            });
        }
        blocks.push(PromptBlock {
            source: ContextSource::UserText,
            min_tier: PrivacyTier::Off,
            est_tokens: crate::provider::backends::anthropic::estimate_tokens(&user_message),
            body: user_message.clone(),
        });

        let mut system = vec![SystemChunk {
            text: framing,
            cache: true,
        }];
        if !context.is_empty() {
            system.push(SystemChunk {
                text: context,
                cache: false,
            });
        }
        Self {
            system,
            messages: vec![Message {
                role: Role::User,
                content: user_message,
            }],
            blocks,
            tier,
        }
    }

    /// The system chunks, in send order.
    #[must_use]
    pub fn system(&self) -> &[SystemChunk] {
        &self.system
    }

    /// The messages, in send order.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Per-block provenance, for the preview and the audit record.
    #[must_use]
    pub fn blocks(&self) -> &[PromptBlock] {
        &self.blocks
    }

    /// The effective tier this prompt was packed at.
    #[must_use]
    pub const fn tier(&self) -> PrivacyTier {
        self.tier
    }

    /// Every byte, in send order. The one rendering the preview shows and the
    /// audit log stores, so "what was sent" cannot drift from what was sent.
    #[must_use]
    pub fn transcript(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            out.push_str(&format!(
                "--- {} (tier {}) ---\n",
                b.source.label(),
                b.min_tier
            ));
            out.push_str(&b.body);
            if !b.body.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }

    /// Total estimated input tokens.
    #[must_use]
    pub fn est_tokens(&self) -> u32 {
        self.blocks.iter().map(|b| b.est_tokens).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(min_tier: PrivacyTier, body: &str) -> ContextItem {
        ContextItem {
            source: ContextSource::Variables,
            min_tier,
            priority: 3,
            est_tokens: 10,
            body: body.to_owned(),
        }
    }

    #[test]
    fn the_gate_is_one_comparison_and_it_is_inclusive_at_the_floor() {
        let items = vec![
            item(PrivacyTier::SchemaOnly, "names"),
            item(PrivacyTier::SchemaAndStats, "means"),
            item(PrivacyTier::Full, "values"),
        ];
        let kept = gate(items, PrivacyTier::SchemaAndStats);
        assert_eq!(kept.len(), 2);
        assert!(kept
            .iter()
            .all(|i| i.min_tier <= PrivacyTier::SchemaAndStats));
    }

    #[test]
    fn at_tier_off_nothing_but_the_framing_and_the_users_own_words_survive() {
        let items = PrivacyTier::ALL
            .iter()
            .filter(|t| **t > PrivacyTier::Off)
            .map(|t| item(*t, "data"))
            .collect();
        assert!(gate(items, PrivacyTier::Off).is_empty());
    }

    #[test]
    fn a_packed_prompt_carries_provenance_for_every_block() {
        let p = PackedPrompt::from_gated(
            "FRAMING".to_owned(),
            vec![item(PrivacyTier::SchemaOnly, "price int")],
            "why?".to_owned(),
            PrivacyTier::SchemaOnly,
        );
        assert_eq!(p.blocks().len(), 3);
        assert_eq!(p.blocks()[0].source, ContextSource::Task);
        assert_eq!(p.blocks()[1].source, ContextSource::Variables);
        assert_eq!(p.blocks()[2].source, ContextSource::UserText);
        assert!(p
            .transcript()
            .contains("--- variables (tier schema_only) ---"));
    }

    #[test]
    fn the_cache_breakpoint_sits_on_the_framing_chunk_and_only_there() {
        let p = PackedPrompt::from_gated(
            "FRAMING".to_owned(),
            vec![item(PrivacyTier::SchemaOnly, "x")],
            "q".to_owned(),
            PrivacyTier::SchemaOnly,
        );
        assert!(p.system()[0].cache, "the stable prefix is the cached one");
        assert!(
            !p.system()[1].cache,
            "volatile context must never be cached"
        );
    }

    #[test]
    fn with_no_context_at_all_there_is_only_the_framing_chunk() {
        let p = PackedPrompt::from_gated(
            "FRAMING".to_owned(),
            Vec::new(),
            "q".to_owned(),
            PrivacyTier::Off,
        );
        assert_eq!(p.system().len(), 1);
        assert_eq!(p.messages().len(), 1);
    }
}
