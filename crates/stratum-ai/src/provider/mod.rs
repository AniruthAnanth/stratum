//! 07 §2 — transport, credentials, streaming, retries and cancellation.
//!
//! This module knows about HTTP. It knows **nothing** about Stata, prompts or
//! privacy tiers. That separation is what makes the privacy gate auditable:
//! there is exactly one type that crosses from [`crate::context`] into here
//! ([`ChatRequest`]), and by the time it exists the gate has already run.

pub mod backends;
pub mod egress;
pub mod error;
pub mod http;
pub mod keys;
pub mod ndjson;
pub mod redact;
pub mod retry;
pub mod sse;
pub mod traits;
pub mod types;

use reqwest::Url;

pub use error::ProviderError;
pub use traits::ChatProvider;
pub use types::{
    ChatEvent, ChatRequest, Effort, HealthReport, Message, ModelId, ProviderCaps, ProviderId, Role,
    StopReason, SystemChunk, Thinking, TokenUsage,
};

/// Everything about a configured endpoint except its credential.
///
/// The credential is deliberately absent: it is resolved at send time from
/// `Platform::credentials()` and lives only inside a `SecretString`, so there is
/// no configuration value anywhere in the product that could be serialised into
/// a settings file with a key in it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProviderConfig {
    /// Which backend.
    pub id: ProviderId,
    /// The endpoint root. Always ends in `/` so `Url::join` appends rather than
    /// replacing the last path segment — a base of `.../v1` without the slash
    /// silently becomes `.../chat/completions`, dropping the version.
    pub base_url: Url,
    /// The model this endpoint uses by default.
    pub model: ModelId,
    /// Extra headers a gateway needs (Azure's `api-key`, OpenRouter's
    /// `HTTP-Referer`).
    pub extra_headers: Vec<(String, String)>,
}

impl ProviderConfig {
    /// The Anthropic default (07 §2.3).
    ///
    /// # Panics
    /// Never: the URL is a literal checked by the test below.
    #[must_use]
    pub fn anthropic_default() -> Self {
        Self {
            id: ProviderId::Anthropic,
            base_url: Url::parse("https://api.anthropic.com/").expect("literal URL"),
            model: ModelId::from(backends::anthropic::DEFAULT_MODEL),
            extra_headers: Vec::new(),
        }
    }

    /// The OpenAI default; a university gateway replaces `base_url`, which also
    /// changes which credential is looked up (07 §3.1).
    ///
    /// # Panics
    /// Never: the URL is a literal checked by the test below.
    #[must_use]
    pub fn openai_compat_default() -> Self {
        Self {
            id: ProviderId::OpenAiCompat,
            base_url: Url::parse("https://api.openai.com/v1/").expect("literal URL"),
            model: ModelId::from("gpt-4o-mini"),
            extra_headers: Vec::new(),
        }
    }

    /// The local Ollama daemon (07 §2.5).
    ///
    /// # Panics
    /// Never: the URL is a literal checked by the test below.
    #[must_use]
    pub fn ollama_default() -> Self {
        Self {
            id: ProviderId::Ollama,
            base_url: Url::parse("http://127.0.0.1:11434/").expect("literal URL"),
            model: ModelId::from(backends::ollama::DEFAULT_MODEL),
            extra_headers: Vec::new(),
        }
    }

    /// The host the credential account key is derived from (07 §3.1).
    #[must_use]
    pub fn host(&self) -> String {
        self.base_url
            .host_str()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_base_url_ends_in_a_slash() {
        // `Url::join("chat/completions")` on a base of `https://host/v1`
        // silently produces `https://host/chat/completions`. The trailing slash
        // is the whole difference and it is invisible at a glance.
        for c in [
            ProviderConfig::anthropic_default(),
            ProviderConfig::openai_compat_default(),
            ProviderConfig::ollama_default(),
        ] {
            assert!(c.base_url.as_str().ends_with('/'), "{}", c.base_url);
        }
    }

    #[test]
    fn joining_preserves_the_api_version_segment() {
        let c = ProviderConfig::openai_compat_default();
        assert_eq!(
            c.base_url.join("chat/completions").unwrap().as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn the_ollama_default_is_loopback_which_is_what_makes_offline_mode_possible() {
        let c = ProviderConfig::ollama_default();
        assert!(egress::is_loopback_host(&c.host()));
    }
}
