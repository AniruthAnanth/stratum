//! `07` §12–§13.1 — the orchestrator the UI and the CLI both talk to.
//!
//! # One trait, two callers, no second entry point
//!
//! `07` §13.1 exports [`AiService`] as *the* seam. The desktop holds
//! `Arc<dyn AiService>` and the headless CLI holds the same trait object, which
//! is what makes "the CLI can do everything the panel can" a type-level fact
//! rather than a promise. [`runtime::LocalAiService`] is the only implementation
//! that talks to a provider; tests substitute their own.
//!
//! # Why [`availability`] is a first-class module and not a boolean
//!
//! The unconfigured state is the base product (`07` §12). Six variants, six
//! different answers to "what do I do about it", each with a headline, a detail
//! sentence and — where there is one — a remedy. [`surface::INTELLIGENCE_SURFACES`]
//! is the same claim as data: 27 rows, 17 of which need no provider at all.
//!
//! # What this module may write
//!
//! `ui/ui.redb`, and nothing else (A4). The engine holds an exclusive lock on
//! `engine/session.redb` and a second opener would either block it or corrupt
//! it; [`store::UiStore`] therefore takes a cache root and derives its own path,
//! and [`store::ENGINE_DB_RELATIVE`] exists only so a test can assert that path
//! is never opened.

pub mod availability;
pub mod runtime;
pub mod session;
pub mod store;
pub mod surface;

use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

pub use availability::{Availability, AvailabilityInputs, AvailabilityKind, Remedy};
pub use runtime::LocalAiService;
pub use session::{Concurrency, SessionCounters, SurfaceCancellation};
pub use store::UiStore;
pub use surface::{IntelligenceSurface, Surface, INTELLIGENCE_SURFACES};

use crate::context::audit::{SentRecord, TimeRange};
use crate::context::packer::PromptPreview;
use crate::provider::ProviderError;
use crate::tasks::cost::CostSummary;
use crate::tasks::{AiTask, TaskEvent};

/// Everything a caller of [`AiService`] can be told.
///
/// `Clone + PartialEq` because [`TaskEvent::Failed`] carries one and the event
/// stream is compared in tests. The variants that wrap a local failure
/// (`redb`, the audit log, a price table) collapse to an already-scrubbed
/// string rather than the source error: those errors wrap `std::io::Error`,
/// which is neither `Clone` nor `PartialEq`, and forcing them to be would be
/// tail-wagging-dog. The provider error is kept whole because it is the one a
/// caller branches on ([`ProviderError::retryable`]).
#[derive(Clone, PartialEq, Debug, thiserror::Error)]
pub enum AiError {
    /// No request was attempted: the stack is not in a state that permits one.
    /// Carries the state so the caller renders the *same* six-variant surface it
    /// would have rendered before asking.
    #[error("{}", .0.headline())]
    Unavailable(Availability),

    /// The transport, the credential store, or the endpoint.
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// The reply did not satisfy the surface's output contract, after the strict
    /// parse and after one repair attempt. Never partially applied.
    #[error("the model's reply did not match the expected format: {0}")]
    Contract(String),

    /// The user cancelled, or a newer request on the same surface superseded
    /// this one (`07` §2.7). Not an error condition to report loudly.
    #[error("cancelled")]
    Cancelled,

    /// A local store failed: `ui/ui.redb`, the audit log, or the price table.
    /// The request may well have succeeded — see the message.
    #[error("{0}")]
    Local(String),
}

impl AiError {
    /// The sentence a user sees. Never a debug rendering, never a URL with a
    /// query string, never anything that could contain a key.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::Unavailable(a) => a.detail(),
            Self::Provider(e) => e.user_message(),
            Self::Contract(_) => {
                "The model's reply did not match the expected format, so nothing was applied. \
                 Try again."
                    .to_owned()
            }
            Self::Cancelled => "Cancelled.".to_owned(),
            Self::Local(m) => m.clone(),
        }
    }

    /// Whether retrying the identical request could plausibly succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Provider(e) => e.retryable(),
            // An unavailable stack becomes available through a user action, not
            // through a retry, and a contract failure repeats deterministically
            // often enough that an automatic retry is a way to spend money twice.
            _ => false,
        }
    }
}

/// `07` §13.1 — the single entry point for the UI and the CLI.
///
/// Object-safe via `async_trait`: the desktop selects an implementation at
/// runtime (real, or a no-provider one in the unconfigured state) and the CLI
/// constructs its own, so the caller needs `Arc<dyn AiService>`.
#[async_trait::async_trait]
pub trait AiService: Send + Sync {
    /// What the stack can do right now. Cheap, synchronous, and safe to call on
    /// every render: it consults cached state and never the network.
    fn availability(&self) -> Availability;

    /// Pack the prompt without sending it. **Synchronous and network-free by
    /// construction** — this is what "preview what will be sent" renders, and a
    /// preview that could itself send would not be a preview.
    ///
    /// # Errors
    /// [`AiError::Unavailable`] when the stack is not in a state that permits a
    /// request at all; the preview would otherwise describe bytes that could
    /// never leave.
    fn preview(&self, task: &AiTask) -> Result<PromptPreview, AiError>;

    /// Run a task. Returns once the request is admitted; the stream delivers
    /// incrementally and ends in exactly one of [`TaskEvent::Done`] or
    /// [`TaskEvent::Failed`].
    ///
    /// Dropping the stream or cancelling `cancel` aborts the HTTP body and
    /// closes the connection (`07` §2.7).
    ///
    /// # Errors
    /// Anything that fails *before* the first byte: availability, the budget
    /// check, credential resolution, the egress guard.
    async fn run(
        &self,
        task: AiTask,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, TaskEvent>, AiError>;

    /// Every request recorded in `range`, oldest first.
    fn audit(&self, range: TimeRange) -> Vec<SentRecord>;

    /// Running totals for the panel footer.
    fn cost_summary(&self) -> CostSummary;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unavailable_error_carries_the_state_that_produced_it() {
        // The point of carrying `Availability` rather than a string: the caller
        // that has to render "no key" after a failed call renders it from the
        // same six-variant type it renders the idle panel from, so the two
        // cannot describe the situation differently.
        let e = AiError::Unavailable(Availability::NoCredential);
        assert_eq!(e.to_string(), Availability::NoCredential.headline());
        assert_eq!(e.user_message(), Availability::NoCredential.detail());
        assert!(!e.retryable());
    }

    #[test]
    fn no_error_message_is_empty_because_an_empty_banner_is_the_failure_mode() {
        for e in [
            AiError::Unavailable(Availability::NoCredential),
            AiError::Provider(ProviderError::network("connection reset")),
            AiError::Contract("missing `comments` key".to_owned()),
            AiError::Cancelled,
            AiError::Local("ui.redb is read-only".to_owned()),
        ] {
            assert!(!e.to_string().trim().is_empty(), "{e:?} has no headline");
            assert!(!e.user_message().trim().is_empty(), "{e:?} has no detail");
        }
    }

    #[test]
    fn only_a_provider_failure_is_ever_retryable() {
        assert!(AiError::Provider(ProviderError::network("reset")).retryable());
        assert!(!AiError::Provider(ProviderError::refused("bad key")).retryable());
        assert!(!AiError::Cancelled.retryable());
        assert!(!AiError::Contract("x".to_owned()).retryable());
    }
}
