//! 07 §2.3 — the default backend.
//!
//! Four things this backend does that the other two do not, each because the
//! current Anthropic model family rejects the alternative with a 400:
//!
//! * **`temperature`/`top_p`/`top_k` are never sent.** [`ChatRequest::temperature`]
//!   is dropped with a debug warning if it was set.
//! * **No assistant prefill.** Output shaping goes through
//!   `output_config.format`, never by seeding an assistant turn.
//! * **No `budget_tokens`.** Thinking is `{"type":"adaptive"}`; a fixed budget is
//!   rejected.
//! * **`{"type":"disabled"}` is only legal at effort ≤ high**, so a request that
//!   asks for both is corrected here rather than sent and 400'd.
//!
//! And one thing it does that matters more than all of them: it sends **no
//! `tools` key**, ever. 07 §0.2 — the model cannot execute a command, read a
//! file it was not given, or mutate session state. `body_has_no_tools` in this
//! module's tests is the assertion.

use std::collections::VecDeque;

use futures::stream::BoxStream;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Url;
use secrecy::SecretString;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::provider::error::ProviderError;
use crate::provider::http::{event_stream, sensitive_header, FrameMapper, Transport};
use crate::provider::sse::{SseDecoder, SseFrame};
use crate::provider::traits::ChatProvider;
use crate::provider::types::{
    ChatEvent, ChatRequest, Effort, HealthReport, ModelId, ProviderCaps, ProviderId, StopReason,
    Thinking, TokenUsage,
};
use crate::provider::ProviderConfig;

/// The API version header. A constant, not configuration: a build that speaks a
/// version it was not written against is a silent wire break.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The default model on every surface (ADR D-AI-10). We do not silently
/// downgrade an interactive surface to a cheaper model; that is the user's call,
/// and Settings ships an opt-in Fast profile for it.
pub const DEFAULT_MODEL: &str = "claude-opus-5";

/// The Fast-profile model, 07 §5.2.
pub const FAST_MODEL: &str = "claude-haiku-4-5";

/// The Anthropic Messages backend.
pub struct AnthropicProvider {
    config: ProviderConfig,
    transport: Transport,
    key: Option<SecretString>,
}

impl AnthropicProvider {
    /// Build a backend. `key` is `None` when nothing is configured, which is a
    /// supported state: `health()` and `caps()` still answer, and `stream()`
    /// fails with [`ProviderError::NoCredential`] rather than panicking.
    #[must_use]
    pub fn new(config: ProviderConfig, transport: Transport, key: Option<SecretString>) -> Self {
        Self {
            config,
            transport,
            key,
        }
    }

    fn endpoint(&self) -> Result<Url, ProviderError> {
        self.config
            .base_url
            .join("v1/messages")
            .map_err(|e| ProviderError::protocol(format!("bad base_url: {e}")))
    }

    fn headers(&self) -> Result<HeaderMap, ProviderError> {
        let key = self
            .key
            .as_ref()
            .ok_or(ProviderError::NoCredential(ProviderId::Anthropic))?;
        let mut h = HeaderMap::new();
        h.insert("x-api-key", sensitive_header(key)?);
        h.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        h.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        for (name, value) in &self.config.extra_headers {
            crate::provider::http::push_extra(&mut h, name, value)?;
        }
        Ok(h)
    }
}

/// Context window and output ceiling per model.
///
/// A small table with a conservative default rather than a live capability
/// lookup: querying `/v1/models` would be a network call made before the user
/// has consented to any network activity, which is the same reason 07 §11.1
/// refuses to fetch a live pricing endpoint.
fn model_limits(model: &ModelId) -> (u32, u32) {
    let m = model.as_str();
    if m.contains("haiku") {
        (200_000, 64_000)
    } else if m.contains("opus") || m.contains("sonnet") || m.contains("fable") {
        (1_000_000, 128_000)
    } else {
        (200_000, 8_192)
    }
}

#[async_trait::async_trait]
impl ChatProvider for AnthropicProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Anthropic
    }

    fn display_name(&self) -> &str {
        "Anthropic"
    }

    fn default_model(&self) -> ModelId {
        self.config.model.clone()
    }

    fn caps(&self, model: &ModelId) -> ProviderCaps {
        let (max_input_tokens, max_output_tokens) = model_limits(model);
        ProviderCaps {
            requires_network: true,
            streaming: true,
            prompt_cache: true,
            structured_output: true,
            thinking: true,
            // The whole reason `ChatRequest::temperature` is an Option that this
            // backend drops.
            sampling_params: false,
            max_input_tokens,
            max_output_tokens,
        }
    }

    fn estimate_tokens(&self, s: &str) -> u32 {
        estimate_tokens(s)
    }

    fn build_body(&self, req: &ChatRequest) -> Result<Value, ProviderError> {
        if req.temperature.is_some() {
            tracing::debug!(
                "dropping `temperature`: the current Anthropic model family rejects sampling \
                 parameters with a 400"
            );
        }

        let system: Vec<Value> = req
            .system
            .iter()
            .map(|chunk| {
                let mut block = json!({ "type": "text", "text": chunk.text });
                if chunk.cache {
                    // 07 §5.6: exactly one breakpoint, at the end of the stable
                    // prefix. Max 4 are allowed; we use one.
                    block["cache_control"] = json!({ "type": "ephemeral" });
                }
                block
            })
            .collect();

        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| json!({ "role": m.role.as_str(), "content": m.content }))
            .collect();

        let mut body = json!({
            "model": req.model.as_str(),
            "max_tokens": req.max_output_tokens,
            "system": system,
            "messages": messages,
            "stream": true,
            "thinking": thinking_value(req.thinking, req.effort),
            "output_config": { "effort": req.effort.as_str() },
        });

        if let Some(schema) = &req.json_schema {
            body["output_config"]["format"] =
                json!({ "type": "json_schema", "schema": schema.clone() });
        }
        if !req.stop.is_empty() {
            body["stop_sequences"] = json!(req.stop.clone());
        }

        // NOTHING adds a `tools` key. v1 ships zero tool definitions (07 §0.2),
        // and the test below is the guarantee.
        Ok(body)
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ChatEvent, ProviderError>>, ProviderError> {
        let body = self.build_body(&req)?;
        let resp = self
            .transport
            .post_json(&self.endpoint()?, self.headers()?, &body)
            .await?;
        Ok(event_stream(resp, AnthropicMapper::default(), cancel))
    }

    async fn health(&self) -> Result<HealthReport, ProviderError> {
        if self.key.is_none() {
            return Err(ProviderError::NoCredential(ProviderId::Anthropic));
        }
        // The cheapest legal request: one token of output on the configured
        // model. There is no unauthenticated health endpoint, and a HEAD on the
        // messages URL answers 405 regardless of the key's validity, which would
        // report a rejected key as healthy.
        let body = json!({
            "model": self.config.model.as_str(),
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "ping" }],
        });
        self.transport
            .post_json(&self.endpoint()?, self.headers()?, &body)
            .await?;
        Ok(HealthReport {
            ok: true,
            detail: format!("{} reachable", self.config.model),
            models: Vec::new(),
        })
    }
}

/// `{"type":"adaptive"}` unless thinking was explicitly turned off at an effort
/// where that is legal.
fn thinking_value(thinking: Thinking, effort: Effort) -> Value {
    match thinking {
        Thinking::Adaptive { show_summary } => json!({
            "type": "adaptive",
            "display": if show_summary { "summarized" } else { "omitted" },
        }),
        // `{"type":"disabled"}` is rejected with a 400 above effort high. A
        // request that asks for both is a configuration mistake, and correcting
        // it is better than sending a request we know will fail: the user's
        // intent ("do not spend on thinking") is served by the effort setting,
        // which we leave alone.
        Thinking::Off if matches!(effort, Effort::XHigh | Effort::Max) => {
            tracing::debug!(
                effort = effort.as_str(),
                "thinking cannot be disabled above effort=high; sending adaptive"
            );
            json!({ "type": "adaptive", "display": "omitted" })
        }
        Thinking::Off => json!({ "type": "disabled" }),
    }
}

/// 07 §4.6, verbatim. Calibrated on packed Stata prompts (code plus tabular
/// output) and deliberately biased high: under-estimating produces a 413 in the
/// middle of somebody's workflow, over-estimating costs a slightly smaller
/// context.
#[must_use]
pub fn estimate_tokens(s: &str) -> u32 {
    let bytes = s.len() as f64;
    let nonascii = s.bytes().filter(|b| *b >= 0x80).count() as f64;
    (((bytes + nonascii * 1.5) / 3.4) * 1.15).ceil() as u32
}

/// SSE frames → [`ChatEvent`].
#[derive(Default)]
struct AnthropicMapper {
    sse: SseDecoder,
    usage: TokenUsage,
    stop: Option<StopReason>,
    finished: bool,
}

impl AnthropicMapper {
    fn frame(&mut self, data: &str, out: &mut VecDeque<Result<ChatEvent, ProviderError>>) {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            out.push_back(Err(ProviderError::protocol("SSE frame was not JSON")));
            return;
        };
        // The JSON `type` field, not the SSE `event:` name: both are sent, and
        // a proxy that strips event names must not silence the stream.
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "message_start" => {
                let msg = v.get("message");
                let id = msg
                    .and_then(|m| m.get("id"))
                    .and_then(Value::as_str)
                    .map(std::string::ToString::to_string);
                let model = msg
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into();
                if let Some(u) = msg.and_then(|m| m.get("usage")) {
                    self.usage.merge(read_usage(u));
                }
                out.push_back(Ok(ChatEvent::Started {
                    provider_request_id: id,
                    model,
                }));
            }
            "content_block_delta" => {
                let delta = v.get("delta");
                let kind = delta.and_then(|d| d.get("type")).and_then(Value::as_str);
                match kind {
                    Some("text_delta") => {
                        if let Some(t) = delta.and_then(|d| d.get("text")).and_then(Value::as_str) {
                            out.push_back(Ok(ChatEvent::TextDelta(t.to_owned())));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = delta
                            .and_then(|d| d.get("thinking"))
                            .and_then(Value::as_str)
                        {
                            out.push_back(Ok(ChatEvent::ThinkingDelta(t.to_owned())));
                        }
                    }
                    // `signature_delta`, `input_json_delta` and anything added
                    // later are ignored rather than treated as an error: an
                    // unknown block type is a newer server, not a broken one.
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(u) = v.get("usage") {
                    self.usage.merge(read_usage(u));
                }
                if let Some(r) = v
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop = Some(read_stop(r));
                }
            }
            "message_stop" => {
                self.finished = true;
                out.push_back(Ok(ChatEvent::Finished {
                    stop: self.stop.unwrap_or(StopReason::EndTurn),
                    usage: self.usage,
                }));
            }
            "error" => {
                let detail = v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("provider error");
                let kind = v
                    .get("error")
                    .and_then(|e| e.get("type"))
                    .and_then(Value::as_str);
                self.finished = true;
                out.push_back(Err(match kind {
                    Some("overloaded_error") => ProviderError::Overloaded,
                    Some("rate_limit_error") => ProviderError::RateLimited(None),
                    Some("authentication_error") | Some("permission_error") => {
                        ProviderError::Unauthorized
                    }
                    _ => ProviderError::protocol(detail),
                }));
            }
            // `ping`, `content_block_start`, `content_block_stop`.
            _ => {}
        }
    }
}

fn read_usage(u: &Value) -> TokenUsage {
    let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
    TokenUsage {
        input: get("input_tokens"),
        output: get("output_tokens"),
        cache_write: get("cache_creation_input_tokens"),
        cache_read: get("cache_read_input_tokens"),
    }
}

fn read_stop(s: &str) -> StopReason {
    match s {
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::Stop,
        "refusal" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

impl FrameMapper for AnthropicMapper {
    fn push(&mut self, chunk: &[u8], out: &mut VecDeque<Result<ChatEvent, ProviderError>>) {
        let mut frames = Vec::new();
        self.sse.push(chunk, &mut frames);
        if self.sse.overflowed() {
            out.push_back(Err(ProviderError::protocol("SSE frame exceeded 8 MiB")));
            self.finished = true;
            return;
        }
        for f in frames {
            match f {
                SseFrame::Event { data, .. } => self.frame(&data, out),
                // Anthropic does not send `[DONE]`; if a proxy synthesises one,
                // treat it as end-of-message rather than as an error.
                SseFrame::Done => {
                    self.finished = true;
                    out.push_back(Ok(ChatEvent::Finished {
                        stop: self.stop.unwrap_or(StopReason::EndTurn),
                        usage: self.usage,
                    }));
                }
            }
        }
    }

    fn finish(&mut self, out: &mut VecDeque<Result<ChatEvent, ProviderError>>) {
        let mut frames = Vec::new();
        self.sse.finish(&mut frames);
        for f in frames {
            if let SseFrame::Event { data, .. } = f {
                self.frame(&data, out);
            }
        }
        if !self.finished {
            // A body that ends without `message_stop` is a truncated stream. The
            // caller must be able to tell that from a clean finish, so it is an
            // error, not a synthesised `Finished`.
            out.push_back(Err(ProviderError::protocol(
                "stream ended before message_stop",
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::provider::egress::{EgressPolicy, NetworkMode};
    use crate::provider::types::{Message, Role, SystemChunk};

    fn provider() -> AnthropicProvider {
        AnthropicProvider::new(
            ProviderConfig::anthropic_default(),
            Transport::new(
                EgressPolicy::new(NetworkMode::Enabled),
                Duration::from_secs(5),
            )
            .unwrap(),
            Some(SecretString::from(
                "sk-ant-test-abcdefghijklmnop".to_owned(),
            )),
        )
    }

    fn request() -> ChatRequest {
        ChatRequest {
            model: ModelId::from(DEFAULT_MODEL),
            system: vec![
                SystemChunk {
                    text: "STABLE".into(),
                    cache: true,
                },
                SystemChunk {
                    text: "VOLATILE".into(),
                    cache: false,
                },
            ],
            messages: vec![Message {
                role: Role::User,
                content: "why r(111)?".into(),
            }],
            max_output_tokens: 600,
            effort: Effort::Low,
            thinking: Thinking::Adaptive {
                show_summary: false,
            },
            json_schema: None,
            temperature: Some(0.7),
            stop: Vec::new(),
            deadline: Duration::from_secs(6),
        }
    }

    #[test]
    fn body_has_no_tools() {
        // ADR-012 / 07 §0.2. The prompt-injection blast radius of a hostile
        // `.do` file, dataset label or note is bounded to bad prose in a panel
        // precisely because this key is never present.
        let body = provider().build_body(&request()).unwrap();
        assert!(
            body.get("tools").is_none(),
            "v1 ships zero tool definitions"
        );
        assert!(body.get("tool_choice").is_none());
        let text = body.to_string();
        assert!(!text.contains("\"tools\""), "{text}");
    }

    #[test]
    fn temperature_is_dropped_because_the_model_family_rejects_it() {
        let body = provider().build_body(&request()).unwrap();
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("top_k").is_none());
    }

    #[test]
    fn there_is_no_assistant_prefill() {
        let body = provider().build_body(&request()).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert!(msgs.iter().all(|m| m["role"] != "assistant"));
    }

    #[test]
    fn exactly_one_cache_breakpoint_and_it_is_on_the_stable_prefix() {
        let body = provider().build_body(&request()).unwrap();
        let system = body["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
        assert!(system[1].get("cache_control").is_none());
    }

    #[test]
    fn thinking_is_adaptive_and_carries_no_budget_tokens() {
        let body = provider().build_body(&request()).unwrap();
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert!(body["thinking"].get("budget_tokens").is_none());
    }

    #[test]
    fn thinking_off_above_effort_high_is_corrected_rather_than_400d() {
        assert_eq!(
            thinking_value(Thinking::Off, Effort::High),
            json!({"type":"disabled"})
        );
        assert_eq!(
            thinking_value(Thinking::Off, Effort::Max)["type"],
            "adaptive"
        );
        assert_eq!(
            thinking_value(Thinking::Off, Effort::XHigh)["type"],
            "adaptive"
        );
    }

    #[test]
    fn a_json_schema_goes_to_output_config_format_not_to_a_prefill() {
        let mut req = request();
        req.json_schema = Some(json!({"type":"object"}));
        let body = provider().build_body(&req).unwrap();
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["output_config"]["effort"], "low");
    }

    #[test]
    fn token_estimate_is_biased_high() {
        // Real Anthropic tokenisation of ASCII prose is ~4 bytes/token; the
        // estimator must never come in under that.
        let text = "summarize price mpg weight foreign rep78 headroom trunk".repeat(20);
        let est = estimate_tokens(&text);
        let optimistic = (text.len() / 4) as u32;
        assert!(
            est > optimistic,
            "est {est} must exceed the optimistic {optimistic}"
        );
    }

    #[test]
    fn non_ascii_costs_more_than_ascii_in_the_estimate() {
        assert!(estimate_tokens("ééééééééé") > estimate_tokens("aaaaaaaaa"));
    }

    fn drain(mapper: &mut AnthropicMapper, body: &[u8]) -> Vec<Result<ChatEvent, ProviderError>> {
        let mut out = VecDeque::new();
        mapper.push(body, &mut out);
        mapper.finish(&mut out);
        out.into_iter().collect()
    }

    #[test]
    fn a_whole_message_decodes_into_started_deltas_and_finished() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-opus-5\",\"usage\":{\"input_tokens\":120,\"cache_read_input_tokens\":2000}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let events = drain(&mut AnthropicMapper::default(), body.as_bytes());
        assert_eq!(events.len(), 3);
        assert!(
            matches!(&events[0], Ok(ChatEvent::Started { provider_request_id: Some(id), .. }) if id == "msg_1")
        );
        assert!(matches!(&events[1], Ok(ChatEvent::TextDelta(t)) if t == "Hello"));
        match &events[2] {
            Ok(ChatEvent::Finished { stop, usage }) => {
                assert_eq!(*stop, StopReason::EndTurn);
                assert_eq!(usage.input, 120);
                assert_eq!(usage.output, 7);
                assert_eq!(usage.cache_read, 2000);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_refusal_is_a_stop_reason_not_an_error() {
        // 07 §2.3: rendered as "the provider declined this request", with the
        // audit record retained so the user can see exactly what was sent.
        let body = concat!(
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"},\"usage\":{\"output_tokens\":0}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let events = drain(&mut AnthropicMapper::default(), body.as_bytes());
        assert!(matches!(
            events.last(),
            Some(Ok(ChatEvent::Finished {
                stop: StopReason::Refusal,
                ..
            }))
        ));
    }

    #[test]
    fn a_truncated_stream_is_an_error_not_a_clean_finish() {
        let body = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"par\"}}\n\n";
        let events = drain(&mut AnthropicMapper::default(), body.as_bytes());
        assert!(matches!(
            events.last(),
            Some(Err(ProviderError::Protocol(_)))
        ));
    }

    #[test]
    fn a_thinking_delta_is_surfaced_separately_from_text() {
        let body = concat!(
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"considering\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let events = drain(&mut AnthropicMapper::default(), body.as_bytes());
        assert!(matches!(&events[0], Ok(ChatEvent::ThinkingDelta(t)) if t == "considering"));
    }

    #[test]
    fn a_provider_error_frame_maps_to_a_typed_error() {
        let body = "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n\n";
        let events = drain(&mut AnthropicMapper::default(), body.as_bytes());
        assert_eq!(events[0].as_ref().unwrap_err(), &ProviderError::Overloaded);
    }

    #[test]
    fn caps_say_no_sampling_params() {
        let p = provider();
        let c = p.caps(&ModelId::from(DEFAULT_MODEL));
        assert!(!c.sampling_params);
        assert!(
            c.requires_network,
            "the cloud backend is never permitted in offline mode"
        );
        assert!(c.prompt_cache);
    }
}
