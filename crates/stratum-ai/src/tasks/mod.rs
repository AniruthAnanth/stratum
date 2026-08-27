//! `07` §5.6, §8.0, §10, §11 — prompts, strict output parsers, cost, caching.
//!
//! The prompts are versioned, hashed and byte-stable ([`prompt`]); the parsers
//! assume a hostile reply ([`parse`]); the cost table is shipped and never
//! fetched ([`cost`]); the response cache is bounded and TTL'd ([`cache`]).
//!
//! Nothing in this module talks to the network, and nothing in it decides what
//! may be sent — that is [`crate::context`]'s job and it has already happened by
//! the time a task is assembled.

pub mod cache;
pub mod cost;
pub mod parse;
pub mod prompt;
pub mod task;

pub use cost::{BudgetVerdict, Budgets, CostSummary, ModelPrice, PriceTable};
pub use prompt::{framing, framing_hash, PROMPT_VERSION};
pub use task::{
    AiTask, CommentAnchor, CommentKind, CommentPosition, CommentRejection, Intent, ProposedComment,
    ProposedEdit, ProposedPatch, TaskEvent,
};

use crate::provider::backends::anthropic::estimate_tokens;
use crate::provider::types::{Message, Role};

/// `07` §5.2: chat history is compacted client-side once the rendered history
/// exceeds this many estimated tokens.
pub const HISTORY_COMPACT_THRESHOLD: u32 = 40_000;

/// Drop the oldest complete user/assistant pairs until the history fits.
///
/// **The first pair is always kept.** It usually carries the task framing — "I
/// am looking at wage data from the 2019 wave, restricted access" — and dropping
/// it is how a long conversation suddenly starts answering a different question.
///
/// We do not use server-side compaction in v1: it requires echoing
/// provider-specific opaque blocks back verbatim, which would leak a provider
/// concept into this module and break the OpenAI-compatible and Ollama backends
/// for a feature only one provider has.
#[must_use]
pub fn compact_history(history: &[Message], threshold: u32) -> Vec<Message> {
    let total: u32 = history.iter().map(|m| estimate_tokens(&m.content)).sum();
    if total <= threshold || history.len() <= 2 {
        return history.to_vec();
    }

    // Pair boundaries: a user turn starts a pair. Anything before the first user
    // turn is preamble and travels with the first pair.
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    for (i, m) in history.iter().enumerate() {
        if m.role == Role::User && i > start {
            pairs.push((start, i));
            start = i;
        }
    }
    pairs.push((start, history.len()));
    if pairs.len() <= 2 {
        return history.to_vec();
    }

    let cost = |&(a, b): &(usize, usize)| -> u32 {
        history[a..b]
            .iter()
            .map(|m| estimate_tokens(&m.content))
            .sum()
    };
    let mut running = total;
    // Drop from the second pair forward; index 0 is the framing pair and stays.
    let mut dropped = 0usize;
    while running > threshold && dropped + 2 < pairs.len() {
        running = running.saturating_sub(cost(&pairs[dropped + 1]));
        dropped += 1;
    }

    let mut out: Vec<Message> = history[pairs[0].0..pairs[0].1].to_vec();
    for pair in &pairs[dropped + 1..] {
        out.extend_from_slice(&history[pair.0..pair.1]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(role: Role, n: usize) -> Message {
        Message {
            role,
            content: format!("{} turn {n}: {}", role.as_str(), "x".repeat(4_000)),
        }
    }

    fn conversation(pairs: usize) -> Vec<Message> {
        let mut v = Vec::new();
        for i in 0..pairs {
            v.push(turn(Role::User, i));
            v.push(turn(Role::Assistant, i));
        }
        v
    }

    #[test]
    fn a_short_conversation_is_untouched() {
        let c = conversation(2);
        assert_eq!(compact_history(&c, HISTORY_COMPACT_THRESHOLD), c);
    }

    #[test]
    fn a_long_conversation_keeps_the_first_pair_and_the_newest_turns() {
        let c = conversation(40);
        let out = compact_history(&c, 20_000);
        assert!(out.len() < c.len(), "nothing was dropped");
        assert_eq!(out[0], c[0], "the framing pair must survive");
        assert_eq!(out[1], c[1]);
        assert_eq!(
            out[out.len() - 1],
            c[c.len() - 1],
            "the newest turn must survive"
        );
        let total: u32 = out.iter().map(|m| estimate_tokens(&m.content)).sum();
        assert!(
            total <= 20_000 + estimate_tokens(&c[0].content) * 2,
            "{total}"
        );
    }

    #[test]
    fn compaction_never_leaves_an_assistant_turn_without_its_question() {
        // A dangling assistant turn reads to the model as an answer to whatever
        // came before it, which is now a different question.
        let c = conversation(30);
        let out = compact_history(&c, 15_000);
        for (i, m) in out.iter().enumerate() {
            if m.role == Role::Assistant {
                assert_eq!(
                    out[i - 1].role,
                    Role::User,
                    "orphaned assistant turn at {i}"
                );
            }
        }
    }

    #[test]
    fn compaction_is_idempotent() {
        let c = conversation(40);
        let once = compact_history(&c, 20_000);
        assert_eq!(compact_history(&once, 20_000), once);
    }
}
