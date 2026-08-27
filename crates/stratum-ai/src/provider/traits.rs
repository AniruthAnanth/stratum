//! 07 §2.2 — the one trait the service holds as `Arc<dyn ChatProvider>`.

use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use super::error::ProviderError;
use super::types::{ChatEvent, ChatRequest, HealthReport, ModelId, ProviderCaps, ProviderId};

/// A backend that can answer a [`ChatRequest`].
///
/// Object-safe via `async_trait`; providers are selected at runtime from
/// configuration, so the service needs `Arc<dyn ChatProvider>` and pays one
/// boxed future per call — irrelevant at network latency.
#[async_trait::async_trait]
pub trait ChatProvider: Send + Sync + 'static {
    /// Which backend this is.
    fn id(&self) -> ProviderId;

    /// The name Settings shows.
    fn display_name(&self) -> &str;

    /// The model used when the surface's configuration names none.
    fn default_model(&self) -> ModelId;

    /// What this backend can do with `model`.
    fn caps(&self, model: &ModelId) -> ProviderCaps;

    /// Offline token estimate used by the context packer (07 §4.6).
    ///
    /// MUST NOT make a network call — the packer has to know sizes *before*
    /// sending, so it cannot use a network token counter — and MUST
    /// over-estimate rather than under, because under-estimating produces a 413
    /// in the middle of somebody's workflow.
    fn estimate_tokens(&self, text: &str) -> u32;

    /// The exact JSON body this backend would send.
    ///
    /// On the trait, not private to each backend, for two reasons: it is what
    /// "preview what will be sent" renders without touching the network, and it
    /// is what the **zero-tools** test inspects. `v1 ships zero tool
    /// definitions` is only a guarantee if it can be asserted for every backend
    /// through one seam.
    ///
    /// # Errors
    /// When the request asks for something this backend cannot express at all.
    fn build_body(&self, req: &ChatRequest) -> Result<serde_json::Value, ProviderError>;

    /// Streaming completion. Returns as soon as headers are received; the stream
    /// yields incrementally. Dropping the stream, or cancelling the token,
    /// aborts the HTTP body and closes the connection.
    ///
    /// # Errors
    /// Anything in [`ProviderError`]; a failure before the first byte is
    /// reported here rather than as a stream item.
    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ChatEvent, ProviderError>>, ProviderError>;

    /// Cheap reachability plus credential check, for the settings pane and for
    /// `availability()`. Never called on a hot path.
    ///
    /// # Errors
    /// Anything in [`ProviderError`].
    async fn health(&self) -> Result<HealthReport, ProviderError>;
}
