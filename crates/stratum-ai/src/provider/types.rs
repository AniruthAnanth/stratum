//! 07 §2.1 — the vocabulary that crosses from `context` into `provider`.
//!
//! Nothing in this module knows about Stata, prompts, or privacy tiers. That
//! separation is what makes the privacy gate auditable: there is exactly one
//! type that crosses the boundary ([`ChatRequest`]), and by the time it exists
//! the gate has already run.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The three backends 07 §2 specifies. Anthropic is the default.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    /// `api.anthropic.com/v1/messages`, the default (07 §2.3).
    Anthropic,
    /// Anything speaking `POST {base}/chat/completions` (07 §2.4).
    OpenAiCompat,
    /// A local Ollama daemon on loopback (07 §2.5). The only provider whose
    /// [`ProviderCaps::requires_network`] is `false`.
    Ollama,
}

impl ProviderId {
    /// The stable string used as the credential account prefix, in the audit
    /// log and in `ai-pricing.toml`. One function so the spelling cannot drift
    /// between the keychain write and the keychain read.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCompat => "openai_compat",
            Self::Ollama => "ollama",
        }
    }

    /// Every provider, in settings-pane order.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Anthropic, Self::OpenAiCompat, Self::Ollama]
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A model name as the provider spells it. Never parsed, never validated
/// against a list: a user pointing at a private vLLM deployment is a supported
/// case and an allowlist would break it.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ModelId(pub String);

impl ModelId {
    /// Borrow the wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ModelId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `output_config.effort` on the Anthropic Messages API; dropped by backends
/// that have no equivalent.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Interactive surfaces with a sub-10-second deadline.
    Low,
    /// The middle of the table in 07 §5.2.
    Medium,
    /// The default when nothing says otherwise.
    High,
    /// Between `High` and `Max`.
    XHigh,
    /// Correctness matters more than cost.
    Max,
}

impl Effort {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Extended thinking. 07 §2.3: `Adaptive` is the only mode current Anthropic
/// models accept, and a fixed token budget is rejected with a 400.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Thinking {
    /// The model decides. `show_summary` maps to `display`, which defaults to
    /// `"omitted"` — the chat panel asks for `"summarized"` so a long think
    /// reads as progress rather than as a hang.
    Adaptive {
        /// Ask the provider for a readable summary of the reasoning.
        show_summary: bool,
    },
    /// Explicitly off. Only legal at `Effort <= High` on Anthropic, which
    /// [`crate::provider::backends::anthropic`] enforces before sending.
    Off,
}

/// A chunk of the system prompt. Order is stable; `cache` marks a prefix
/// breakpoint. Everything up to and including a `cache: true` chunk must be
/// byte-identical across requests or the provider cache silently misses.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SystemChunk {
    /// The chunk's text, already rendered and already gated.
    pub text: String,
    /// Place a cache breakpoint at the end of this chunk.
    pub cache: bool,
}

/// Who said it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The human, or the application speaking on the human's behalf.
    User,
    /// The model.
    Assistant,
}

impl Role {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// One conversation turn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    /// Who said it.
    pub role: Role,
    /// What they said. Already gated and already fenced (07 §5.5).
    pub content: String,
}

/// The single type that crosses from `context` into `provider`.
///
/// It deliberately carries **no credential**. The key is resolved inside the
/// backend from `Platform::credentials()` at send time and never enters a value
/// that something else could log, clone or serialise.
#[derive(Clone, PartialEq, Debug)]
pub struct ChatRequest {
    /// Which model.
    pub model: ModelId,
    /// System chunks, in order. `[0]` is the cache-stable prefix (07 §5.6).
    pub system: Vec<SystemChunk>,
    /// The conversation.
    pub messages: Vec<Message>,
    /// Hard ceiling on generated tokens.
    pub max_output_tokens: u32,
    /// Thinking depth / token spend.
    pub effort: Effort,
    /// Extended thinking configuration.
    pub thinking: Thinking,
    /// When `Some`, the provider is asked to emit JSON matching this schema.
    /// Backends that cannot enforce it fall back to instruction-plus-strict-parse.
    pub json_schema: Option<serde_json::Value>,
    /// `None` on every current Anthropic model — sampling params are rejected
    /// with a 400. Carried for the OpenAI-compatible and Ollama backends only.
    pub temperature: Option<f32>,
    /// Stop sequences.
    pub stop: Vec<String>,
    /// Wall-clock ceiling for the whole exchange.
    pub deadline: Duration,
}

impl ChatRequest {
    /// The bytes a preview shows and the audit log records: every system chunk
    /// and every message, in send order, with a provenance header per block.
    ///
    /// This is the *only* rendering of a request the product ever shows, so
    /// "what was sent" cannot drift from what was sent.
    #[must_use]
    pub fn transcript(&self) -> String {
        let mut out = String::new();
        for (i, chunk) in self.system.iter().enumerate() {
            out.push_str(&format!(
                "=== system[{i}]{} ===\n",
                if chunk.cache { " (cached prefix)" } else { "" }
            ));
            out.push_str(&chunk.text);
            if !chunk.text.ends_with('\n') {
                out.push('\n');
            }
        }
        for msg in &self.messages {
            out.push_str(&format!("=== {} ===\n", msg.role.as_str()));
            out.push_str(&msg.content);
            if !msg.content.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }
}

/// The shortest `f64` that round-trips a sampling parameter.
///
/// `temperature` is an `f32` because that is the precision a provider accepts,
/// but `serde_json` widens `0.2f32` to `0.20000000298023224`. That number then
/// appears in the request body, in the pre-send preview and in the audit
/// record — three places the user reads back a value they never typed, in a
/// module whose whole claim is that "what was sent" cannot drift from what was
/// sent. `f32`'s `Display` is defined to emit the shortest decimal that
/// round-trips, so re-parsing it recovers exactly the literal the user chose.
#[must_use]
pub fn shortest_f64(v: f32) -> f64 {
    v.to_string()
        .parse::<f64>()
        .unwrap_or_else(|_| f64::from(v))
}

/// Token accounting as the provider reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Uncached input tokens.
    pub input: u32,
    /// Generated tokens.
    pub output: u32,
    /// Tokens written into the provider's prompt cache.
    pub cache_write: u32,
    /// Tokens served from the provider's prompt cache. Zero on the second
    /// request of a session is the cache-health warning in 07 §11.3.
    pub cache_read: u32,
}

impl TokenUsage {
    /// Fold another usage report into this one. `message_delta` frames arrive
    /// piecemeal and the last one wins for output, so this takes the max rather
    /// than the sum for fields the provider restates.
    pub fn merge(&mut self, other: Self) {
        self.input = self.input.max(other.input);
        self.output = self.output.max(other.output);
        self.cache_write = self.cache_write.max(other.cache_write);
        self.cache_read = self.cache_read.max(other.cache_read);
    }
}

/// Why generation stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished.
    EndTurn,
    /// `max_output_tokens` was reached.
    MaxTokens,
    /// A stop sequence matched.
    Stop,
    /// The provider's safety classifiers declined. Rendered as "the provider
    /// declined this request", with the audit record retained so the user can
    /// see exactly what was sent.
    Refusal,
    /// The caller cancelled.
    Cancelled,
}

/// One event from a streaming completion.
#[derive(Clone, PartialEq, Debug)]
pub enum ChatEvent {
    /// Headers received; generation has begun.
    Started {
        /// The provider's own request id, when it supplies one. Goes in the
        /// audit record so a support conversation can name the request.
        provider_request_id: Option<String>,
        /// The model the provider says it actually used.
        model: ModelId,
    },
    /// Only emitted when [`Thinking::Adaptive`] asked for a summary.
    ThinkingDelta(String),
    /// A chunk of the answer.
    TextDelta(String),
    /// Terminal event.
    Finished {
        /// Why it stopped.
        stop: StopReason,
        /// Final accounting.
        usage: TokenUsage,
    },
}

/// What a provider can do with a given model. Read by the offline gate, the
/// budget planner and the settings pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProviderCaps {
    /// `false` only for Ollama on loopback. The offline-mode selection gate is
    /// a single comparison against this field (07 §4.7 layer 1).
    pub requires_network: bool,
    /// Incremental delivery.
    pub streaming: bool,
    /// A cache-control breakpoint on the system prefix does something.
    pub prompt_cache: bool,
    /// The provider can be asked to conform to a JSON schema.
    pub structured_output: bool,
    /// Extended thinking is available.
    pub thinking: bool,
    /// `temperature`/`top_p`/`top_k` are accepted rather than 400'd.
    pub sampling_params: bool,
    /// Context window.
    pub max_input_tokens: u32,
    /// Output ceiling.
    pub max_output_tokens: u32,
}

/// The result of a cheap reachability plus credential check.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HealthReport {
    /// Whether the provider answered.
    pub ok: bool,
    /// A short human-readable line for the settings pane. Passed through
    /// [`crate::provider::redact::scrub`] before it gets here.
    pub detail: String,
    /// Models the provider advertises, when it advertises any. Drives Ollama's
    /// model picker; empty for the cloud backends, which have no such endpoint.
    pub models: Vec<ModelId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_strings_are_stable() {
        // These strings are the credential account prefix and the pricing-table
        // key. Changing one orphans every stored key on every user's machine.
        assert_eq!(ProviderId::Anthropic.as_str(), "anthropic");
        assert_eq!(ProviderId::OpenAiCompat.as_str(), "openai_compat");
        assert_eq!(ProviderId::Ollama.as_str(), "ollama");
    }

    #[test]
    fn usage_merge_takes_the_restated_value_not_the_sum() {
        let mut a = TokenUsage {
            input: 100,
            output: 5,
            ..TokenUsage::default()
        };
        a.merge(TokenUsage {
            input: 100,
            output: 42,
            ..TokenUsage::default()
        });
        assert_eq!(a.input, 100, "input restated, not accumulated");
        assert_eq!(a.output, 42);
    }

    #[test]
    fn transcript_labels_every_block_with_its_provenance() {
        let req = ChatRequest {
            model: ModelId::from("claude-opus-5"),
            system: vec![
                SystemChunk {
                    text: "FRAMING".into(),
                    cache: true,
                },
                SystemChunk {
                    text: "CONTEXT".into(),
                    cache: false,
                },
            ],
            messages: vec![Message {
                role: Role::User,
                content: "hello".into(),
            }],
            max_output_tokens: 10,
            effort: Effort::Low,
            thinking: Thinking::Off,
            json_schema: None,
            temperature: None,
            stop: Vec::new(),
            deadline: Duration::from_secs(1),
        };
        let t = req.transcript();
        assert!(t.contains("=== system[0] (cached prefix) ==="));
        assert!(t.contains("=== system[1] ==="));
        assert!(t.contains("=== user ==="));
        assert!(t.ends_with("hello\n"));
    }
}
