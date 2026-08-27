//! 07 §2.8 — the provider error type.
//!
//! Every constructor that carries provider-supplied text runs it through
//! [`super::redact::scrub`] first, so the scrubbing cannot be forgotten at a
//! call site. That is why the string-carrying variants are built through
//! functions rather than by struct literal.

use std::time::Duration;

use super::redact::scrub;
use super::types::ProviderId;

/// Everything that can go wrong below the task layer.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ProviderError {
    /// No key for this provider, in any of the four resolution sources.
    #[error("no credential configured for {0}")]
    NoCredential(ProviderId),
    /// The OS secret store could not be reached — a headless Linux box with no
    /// Secret Service on the bus is the common case, and it is expected, not
    /// exceptional (07 §3.2).
    #[error("credential store unavailable: {0}")]
    KeyStore(String),
    /// 401/403.
    #[error("authentication rejected")]
    Unauthorized,
    /// 429, with `Retry-After` when the provider sent one.
    #[error("rate limited")]
    RateLimited(Option<Duration>),
    /// 529, or 503 from a provider that overloads rather than rate-limits.
    #[error("provider is overloaded")]
    Overloaded,
    /// 413, or a local pre-flight that refused to send.
    #[error("request too large: {sent} tokens > {limit}")]
    TooLarge {
        /// What we estimated we were about to send.
        sent: u32,
        /// The model's input ceiling.
        limit: u32,
    },
    /// `stop_reason == "refusal"`, or an explicit provider decline.
    #[error("provider declined the request ({0})")]
    Refused(String),
    /// Connect, TLS, DNS or read failure.
    #[error("network: {0}")]
    Network(String),
    /// The provider answered with something we cannot parse.
    #[error("malformed response: {0}")]
    Protocol(String),
    /// The caller's `CancellationToken` fired.
    #[error("cancelled")]
    Cancelled,
    /// Offline mode refused this destination (07 §4.7 layer 2).
    #[error("offline mode forbids host {0}")]
    EgressBlocked(String),
    /// The surface's wall-clock budget ran out.
    #[error("deadline exceeded after {0:?}")]
    Deadline(Duration),
    /// The project policy or the settings pane forbids this provider.
    #[error("provider {0} is not permitted by the active policy")]
    ProviderNotPermitted(ProviderId),
}

impl ProviderError {
    /// Build a [`ProviderError::Network`] from provider- or OS-supplied text.
    #[must_use]
    pub fn network(detail: impl AsRef<str>) -> Self {
        Self::Network(scrub(detail.as_ref()))
    }

    /// Build a [`ProviderError::Protocol`] from a provider body.
    #[must_use]
    pub fn protocol(detail: impl AsRef<str>) -> Self {
        Self::Protocol(scrub(detail.as_ref()))
    }

    /// Build a [`ProviderError::Refused`] from a provider body.
    #[must_use]
    pub fn refused(detail: impl AsRef<str>) -> Self {
        Self::Refused(scrub(detail.as_ref()))
    }

    /// Build a [`ProviderError::KeyStore`] from a platform error.
    #[must_use]
    pub fn key_store(detail: impl AsRef<str>) -> Self {
        Self::KeyStore(scrub(detail.as_ref()))
    }

    /// Whether a retry could possibly help. 07 §2.6 rules 1 and 2: a 400 or a
    /// 401 is our bug or the user's config, and retrying only burns the budget.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited(_) | Self::Overloaded | Self::Network(_)
        )
    }

    /// The actionable sentence the AI panel renders. Distinct from `Display`,
    /// which is the log line: a user who sees "authentication rejected" needs to
    /// be told where the fix is.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::NoCredential(p) => {
                format!("No API key for {p}. Add one in Settings › AI.")
            }
            Self::KeyStore(detail) => {
                format!("The system credential store is unavailable ({detail}). Settings › AI lists the alternatives.")
            }
            Self::Unauthorized => {
                "The API key was rejected. Re-enter it in Settings › AI.".to_owned()
            }
            Self::RateLimited(_) => {
                "The provider is rate limiting this key. Try again shortly.".to_owned()
            }
            Self::Overloaded => "The provider is overloaded. Try again shortly.".to_owned(),
            Self::TooLarge { sent, limit } => format!(
                "This request needs about {sent} tokens and the model accepts {limit}. Narrow the selection or lower the context budget."
            ),
            Self::Refused(why) => {
                format!("The provider declined this request ({why}). The exact bytes sent are in the request viewer.")
            }
            Self::Network(detail) => format!("Could not reach the provider: {detail}"),
            Self::Protocol(detail) => format!("The provider sent something unexpected: {detail}"),
            Self::Cancelled => "Cancelled.".to_owned(),
            Self::EgressBlocked(host) => format!(
                "Offline AI is on, so nothing may be sent to {host}. Only a local provider on this machine is permitted."
            ),
            Self::Deadline(d) => format!("The request did not finish within {d:?}."),
            Self::ProviderNotPermitted(p) => format!(
                "{p} is not on this project's allowlist (.stratum/ai-policy.toml)."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::redact::{forget_all, register, REDACTED};
    use secrecy::SecretString;

    #[test]
    fn constructors_scrub_before_the_error_exists() {
        forget_all();
        register(&SecretString::from("ZZKEYMATERIAL_ABCDEFGH".to_owned()));
        let e = ProviderError::network("connect failed for key ZZKEYMATERIAL_ABCDEFGH");
        // Both the Display form and the Debug form, because a panic prints Debug.
        assert!(!format!("{e}").contains("ZZKEYMATERIAL_ABCDEFGH"));
        assert!(!format!("{e:?}").contains("ZZKEYMATERIAL_ABCDEFGH"));
        assert!(format!("{e}").contains(REDACTED));
        forget_all();
    }

    #[test]
    fn only_the_three_transient_classes_are_retryable() {
        assert!(ProviderError::RateLimited(None).retryable());
        assert!(ProviderError::Overloaded.retryable());
        assert!(ProviderError::network("reset").retryable());
        assert!(!ProviderError::Unauthorized.retryable());
        assert!(!ProviderError::TooLarge { sent: 10, limit: 5 }.retryable());
        assert!(!ProviderError::Cancelled.retryable());
        assert!(!ProviderError::EgressBlocked("x".into()).retryable());
    }

    #[test]
    fn every_variant_has_a_user_message_that_says_what_to_do() {
        let all = [
            ProviderError::NoCredential(ProviderId::Anthropic),
            ProviderError::key_store("no dbus"),
            ProviderError::Unauthorized,
            ProviderError::RateLimited(None),
            ProviderError::Overloaded,
            ProviderError::TooLarge { sent: 9, limit: 8 },
            ProviderError::refused("policy"),
            ProviderError::network("reset"),
            ProviderError::protocol("bad json"),
            ProviderError::Cancelled,
            ProviderError::EgressBlocked("evil.example".into()),
            ProviderError::Deadline(Duration::from_secs(1)),
            ProviderError::ProviderNotPermitted(ProviderId::OpenAiCompat),
        ];
        for e in all {
            assert!(!e.user_message().is_empty(), "{e:?}");
        }
    }
}
