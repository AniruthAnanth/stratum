//! The one HTTP path all three backends share.
//!
//! Everything that must not be duplicated per backend lives here: the egress
//! guard (07 §4.7 layer 2), the status-code classification of 07 §2.6, the
//! sensitive-header handling that keeps a key out of any log `reqwest` or
//! `hyper` might emit, and the cancellation wiring of 07 §2.7.
//!
//! A backend contributes only two things: the URL and headers for its endpoint,
//! and a [`FrameMapper`] that turns its own frame vocabulary into [`ChatEvent`].

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{BoxStream, Stream, StreamExt as _};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Response, StatusCode, Url};
use secrecy::{ExposeSecret as _, SecretString};
use tokio_util::sync::CancellationToken;

use super::egress::{loopback_only_resolver, EgressPolicy, NetworkMode, SystemResolver};
use super::error::ProviderError;
use super::retry::parse_retry_after;
use super::types::ChatEvent;

/// Shared HTTP client plus the egress decision it was built for.
#[derive(Clone, Debug)]
pub struct Transport {
    client: reqwest::Client,
    egress: EgressPolicy,
}

impl Transport {
    /// The client the product uses: loopback-only DNS in offline mode, the OS
    /// resolver otherwise.
    ///
    /// # Errors
    /// Propagates `reqwest`'s client-construction failures.
    pub fn new(egress: EgressPolicy, timeout: Duration) -> Result<Self, ProviderError> {
        let resolver: Arc<dyn reqwest::dns::Resolve> = match egress.mode() {
            NetworkMode::Offline => loopback_only_resolver(),
            NetworkMode::Enabled => Arc::new(SystemResolver),
        };
        Self::with_resolver(egress, timeout, resolver)
    }

    /// As [`Transport::new`], with the resolver supplied.
    ///
    /// See [`super::egress::build_client`] for why this is public: the offline
    /// test has to be able to install a resolver that *would* reach a listening
    /// socket, or it proves DNS rather than the guard.
    ///
    /// # Errors
    /// Propagates `reqwest`'s client-construction failures.
    pub fn with_resolver(
        egress: EgressPolicy,
        timeout: Duration,
        resolver: Arc<dyn reqwest::dns::Resolve>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            client: super::egress::build_client(timeout, resolver)?,
            egress,
        })
    }

    /// The egress policy, for the counters.
    #[must_use]
    pub const fn egress(&self) -> &EgressPolicy {
        &self.egress
    }

    /// POST a JSON body and return the response, having already classified a
    /// non-2xx status into a [`ProviderError`].
    ///
    /// # Errors
    /// [`ProviderError::EgressBlocked`] before any socket exists when offline
    /// mode forbids the host; otherwise the classified status or a network error.
    pub async fn post_json(
        &self,
        url: &Url,
        headers: HeaderMap,
        body: &serde_json::Value,
    ) -> Result<Response, ProviderError> {
        // Layer 2, pre-flight. Before the request is built, before DNS, before
        // a socket. The counter it bumps is what the offline test asserts.
        self.egress.guard(url)?;

        let resp = self
            .client
            .post(url.clone())
            .headers(headers)
            .json(body)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        // The body is read for its message only. It routinely echoes request
        // fragments, which is why every constructor here scrubs.
        let text = resp.text().await.unwrap_or_default();
        Err(classify(status, retry_after, &text))
    }

    /// GET, for the health and model-list endpoints.
    ///
    /// # Errors
    /// As [`Transport::post_json`].
    pub async fn get(&self, url: &Url, headers: HeaderMap) -> Result<Response, ProviderError> {
        self.egress.guard(url)?;
        let resp = self
            .client
            .get(url.clone())
            .headers(headers)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let text = resp.text().await.unwrap_or_default();
        Err(classify(status, None, &text))
    }
}

/// 07 §2.6 rules 1 and 2 as a single function, so a backend cannot decide for
/// itself that its 429 is special.
#[must_use]
pub fn classify(status: StatusCode, retry_after: Option<Duration>, body: &str) -> ProviderError {
    let head: String = body.chars().take(400).collect();
    match status.as_u16() {
        401 | 403 => ProviderError::Unauthorized,
        413 => ProviderError::TooLarge { sent: 0, limit: 0 },
        429 => ProviderError::RateLimited(retry_after),
        529 => ProviderError::Overloaded,
        // Retryable server-side classes.
        408 | 409 | 500 | 502 | 503 | 504 => ProviderError::network(format!("{status}: {head}")),
        // Everything else — 400, 404, 422 and friends — is our bug or the
        // user's configuration. Surfaced immediately, never retried.
        _ => ProviderError::protocol(format!("{status}: {head}")),
    }
}

fn map_reqwest_error(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        return ProviderError::network(format!("timed out: {e}"));
    }
    if e.is_connect() || e.is_request() {
        return ProviderError::network(e.to_string());
    }
    ProviderError::network(e.to_string())
}

/// Build a header value that `reqwest`/`hyper` will not print.
///
/// `set_sensitive(true)` is not cosmetic: it is what stops the value appearing
/// in HTTP/2 HPACK debug output and in any middleware that honours the flag.
///
/// # Errors
/// When the secret contains bytes that are not legal in a header value — which
/// means the user pasted something that is not an API key, and saying so beats
/// sending it.
pub fn sensitive_header(secret: &SecretString) -> Result<HeaderValue, ProviderError> {
    let mut v = HeaderValue::from_str(secret.expose_secret()).map_err(|_| {
        ProviderError::key_store("the stored API key contains characters a header cannot carry")
    })?;
    v.set_sensitive(true);
    Ok(v)
}

/// Add a user-configured extra header (Azure's `api-key`, OpenRouter's
/// `HTTP-Referer`).
///
/// # Errors
/// When the name or value is not legal HTTP.
pub fn push_extra(headers: &mut HeaderMap, name: &str, value: &str) -> Result<(), ProviderError> {
    let n = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| ProviderError::protocol(format!("not a header name: {name}")))?;
    let v = HeaderValue::from_str(value)
        .map_err(|_| ProviderError::protocol(format!("not a header value for {name}")))?;
    headers.insert(n, v);
    Ok(())
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// Turns one backend's frames into [`ChatEvent`]s.
pub trait FrameMapper: Send + 'static {
    /// Consume bytes; append whatever became complete.
    fn push(&mut self, chunk: &[u8], out: &mut VecDeque<Result<ChatEvent, ProviderError>>);
    /// End of body. Emit a terminal event if the provider did not.
    fn finish(&mut self, out: &mut VecDeque<Result<ChatEvent, ProviderError>>);
}

type ByteStream = Pin<Box<dyn Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>;

struct StreamState<M: FrameMapper> {
    body: ByteStream,
    mapper: M,
    pending: VecDeque<Result<ChatEvent, ProviderError>>,
    cancel: CancellationToken,
    done: bool,
}

/// Wrap a response body in a [`ChatEvent`] stream.
///
/// Cancellation drops the `reqwest` body stream, which closes the connection.
/// 07 §2.7: we deliberately do not attempt a graceful HTTP/2 stream reset —
/// dropping is correct and simpler.
pub fn event_stream<M: FrameMapper>(
    response: Response,
    mapper: M,
    cancel: CancellationToken,
) -> BoxStream<'static, Result<ChatEvent, ProviderError>> {
    let state = StreamState {
        body: Box::pin(response.bytes_stream()),
        mapper,
        pending: VecDeque::new(),
        cancel,
        done: false,
    };

    futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(ev) = st.pending.pop_front() {
                return Some((ev, st));
            }
            if st.done {
                return None;
            }
            let next = tokio::select! {
                biased;
                () = st.cancel.cancelled() => {
                    st.done = true;
                    return Some((Err(ProviderError::Cancelled), st));
                }
                n = st.body.next() => n,
            };
            match next {
                Some(Ok(bytes)) => st.mapper.push(&bytes, &mut st.pending),
                Some(Err(e)) => {
                    st.done = true;
                    return Some((Err(ProviderError::network(e.to_string())), st));
                }
                None => {
                    st.done = true;
                    st.mapper.finish(&mut st.pending);
                }
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classification_matches_the_retry_rules() {
        // Never-retry classes.
        assert!(!classify(StatusCode::UNAUTHORIZED, None, "").retryable());
        assert!(!classify(StatusCode::BAD_REQUEST, None, "").retryable());
        assert!(!classify(StatusCode::NOT_FOUND, None, "").retryable());
        assert!(!classify(StatusCode::UNPROCESSABLE_ENTITY, None, "").retryable());
        assert!(!classify(StatusCode::PAYLOAD_TOO_LARGE, None, "").retryable());
        // Retry classes.
        for code in [408u16, 409, 429, 500, 502, 503, 504, 529] {
            let s = StatusCode::from_u16(code).unwrap();
            assert!(
                classify(s, None, "").retryable(),
                "{code} should be retryable"
            );
        }
    }

    #[test]
    fn a_retry_after_header_survives_classification() {
        let e = classify(
            StatusCode::TOO_MANY_REQUESTS,
            Some(Duration::from_secs(3)),
            "",
        );
        assert_eq!(e, ProviderError::RateLimited(Some(Duration::from_secs(3))));
    }

    #[test]
    fn an_error_body_is_scrubbed_and_truncated() {
        crate::provider::redact::forget_all();
        crate::provider::redact::register(&SecretString::from("ZZKEY_ABCDEFGHIJKL".to_owned()));
        let body = format!("{} key ZZKEY_ABCDEFGHIJKL", "x".repeat(1000));
        let e = classify(StatusCode::BAD_REQUEST, None, &body);
        let msg = format!("{e}");
        assert!(!msg.contains("ZZKEY_ABCDEFGHIJKL"));
        assert!(
            msg.len() < 600,
            "an error message is not a place to paste a kilobyte"
        );
        crate::provider::redact::forget_all();
    }

    #[test]
    fn an_api_key_becomes_a_sensitive_header() {
        let h =
            sensitive_header(&SecretString::from("sk-ant-abcdefghijklmnop".to_owned())).unwrap();
        assert!(h.is_sensitive());
        // And the header's own Debug does not print the value.
        assert!(!format!("{h:?}").contains("abcdefghijklmnop"));
    }

    #[test]
    fn a_key_with_a_newline_in_it_is_refused_rather_than_sent() {
        let e = sensitive_header(&SecretString::from("sk-abc\ndef".to_owned())).unwrap_err();
        assert!(matches!(e, ProviderError::KeyStore(_)));
    }
}
