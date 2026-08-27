//! 07 §2.4 — the escape hatch.
//!
//! Covers Azure OpenAI, OpenRouter, Together, Fireworks, vLLM, llama.cpp server,
//! LM Studio and any institutional gateway. This is the backend that exists
//! because a university or hospital mandates a specific inference endpoint, and
//! the design consequence is that it must assume **nothing** beyond the 2023
//! `/chat/completions` shape: no prompt cache, no thinking, no chunked system
//! prompt, and no `stream_options`.
//!
//! `stream_options: {"include_usage": true}` is deliberately **not** sent, even
//! though it is the only way OpenAI itself reports token usage on a streamed
//! response. A gateway that rejects the unknown field answers 400 and the
//! feature is dead for that user; a gateway that ignores it costs us usage
//! numbers we then fall back to the local estimate for. Losing exact cost
//! accounting on a backend whose users are usually not personally billed is the
//! cheaper half of that trade.

use std::collections::VecDeque;

use futures::stream::BoxStream;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Url;
use secrecy::SecretString;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::provider::error::ProviderError;
use crate::provider::http::{event_stream, push_extra, sensitive_header, FrameMapper, Transport};
use crate::provider::sse::{SseDecoder, SseFrame};
use crate::provider::traits::ChatProvider;
use crate::provider::types::shortest_f64;
use crate::provider::types::{
    ChatEvent, ChatRequest, HealthReport, ModelId, ProviderCaps, ProviderId, StopReason, TokenUsage,
};
use crate::provider::ProviderConfig;

/// An OpenAI-compatible chat-completions backend.
pub struct OpenAiCompatProvider {
    config: ProviderConfig,
    transport: Transport,
    key: Option<SecretString>,
}

impl OpenAiCompatProvider {
    /// Build a backend.
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
            .join("chat/completions")
            .map_err(|e| ProviderError::protocol(format!("bad base_url: {e}")))
    }

    fn headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut h = HeaderMap::new();
        // A gateway may authenticate purely through a configured extra header
        // (Azure's `api-key`), so a missing bearer token is only fatal when no
        // extra header supplies one.
        if let Some(key) = self.key.as_ref() {
            // Validate the bare key first, so a paste with a stray newline is
            // reported as a bad key rather than as a malformed header.
            drop(sensitive_header(key)?);
            let bearer = format!("Bearer {}", secrecy::ExposeSecret::expose_secret(key));
            let mut v = HeaderValue::from_str(&bearer).map_err(|_| {
                ProviderError::key_store(
                    "the stored API key contains characters a header cannot carry",
                )
            })?;
            v.set_sensitive(true);
            h.insert(reqwest::header::AUTHORIZATION, v);
        }
        h.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        for (name, value) in &self.config.extra_headers {
            push_extra(&mut h, name, value)?;
        }
        if self.key.is_none() && self.config.extra_headers.is_empty() {
            return Err(ProviderError::NoCredential(ProviderId::OpenAiCompat));
        }
        Ok(h)
    }
}

#[async_trait::async_trait]
impl ChatProvider for OpenAiCompatProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OpenAiCompat
    }

    fn display_name(&self) -> &str {
        "OpenAI-compatible"
    }

    fn default_model(&self) -> ModelId {
        self.config.model.clone()
    }

    fn caps(&self, _model: &ModelId) -> ProviderCaps {
        ProviderCaps {
            requires_network: true,
            streaming: true,
            // 07 §2.4: no prompt cache, no thinking, sampling params allowed.
            prompt_cache: false,
            structured_output: true,
            thinking: false,
            sampling_params: true,
            // Conservative: this backend fronts anything from GPT-class models
            // to a 3B local build, and over-promising the window turns a
            // recoverable "narrow the selection" into a 400 mid-workflow.
            max_input_tokens: 128_000,
            max_output_tokens: 16_384,
        }
    }

    fn estimate_tokens(&self, s: &str) -> u32 {
        // The same conservative heuristic as the Anthropic backend. 07 §1.1
        // rules out `tiktoken`, and a second wrong tokenizer for a backend that
        // might be fronting any model at all would be wrong differently rather
        // than less.
        super::anthropic::estimate_tokens(s)
    }

    fn build_body(&self, req: &ChatRequest) -> Result<Value, ProviderError> {
        // No chunked system prompt in this API: the chunks are concatenated in
        // order into one system message. The cache breakpoint is dropped
        // because `caps().prompt_cache` is false — carrying it would be a lie
        // the cost display then repeats.
        let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
        if !req.system.is_empty() {
            let system: Vec<&str> = req.system.iter().map(|c| c.text.as_str()).collect();
            messages.push(json!({ "role": "system", "content": system.join("\n\n") }));
        }
        for m in &req.messages {
            messages.push(json!({ "role": m.role.as_str(), "content": m.content }));
        }

        let mut body = json!({
            "model": req.model.as_str(),
            "messages": messages,
            "stream": true,
            "max_tokens": req.max_output_tokens,
        });
        if let Some(t) = req.temperature {
            body["temperature"] = json!(shortest_f64(t));
        }
        if !req.stop.is_empty() {
            body["stop"] = json!(req.stop.clone());
        }
        if let Some(schema) = &req.json_schema {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": { "name": "stratum_result", "strict": true, "schema": schema.clone() },
            });
        }
        // No `tools` key. Ever. 07 §0.2.
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
        Ok(event_stream(resp, OpenAiMapper::default(), cancel))
    }

    async fn health(&self) -> Result<HealthReport, ProviderError> {
        let url = self
            .config
            .base_url
            .join("models")
            .map_err(|e| ProviderError::protocol(format!("bad base_url: {e}")))?;
        let mut headers = self.headers()?;
        headers.remove(reqwest::header::ACCEPT);
        let resp = self.transport.get(&url, headers).await?;
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::protocol(format!("/models was not JSON: {e}")))?;
        let models = body
            .get("data")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.get("id").and_then(Value::as_str))
                    .map(ModelId::from)
                    .collect()
            })
            .unwrap_or_default();
        Ok(HealthReport {
            ok: true,
            detail: format!("{} reachable", self.config.base_url),
            models,
        })
    }
}

/// SSE frames → [`ChatEvent`].
#[derive(Default)]
struct OpenAiMapper {
    sse: SseDecoder,
    started: bool,
    usage: TokenUsage,
    stop: Option<StopReason>,
    finished: bool,
}

impl OpenAiMapper {
    fn frame(&mut self, data: &str, out: &mut VecDeque<Result<ChatEvent, ProviderError>>) {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            out.push_back(Err(ProviderError::protocol("SSE frame was not JSON")));
            return;
        };
        if let Some(err) = v.get("error") {
            self.finished = true;
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("provider error");
            out.push_back(Err(ProviderError::protocol(msg)));
            return;
        }
        if !self.started {
            self.started = true;
            out.push_back(Ok(ChatEvent::Started {
                provider_request_id: v
                    .get("id")
                    .and_then(Value::as_str)
                    .map(std::string::ToString::to_string),
                model: v
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
            }));
        }
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage.merge(TokenUsage {
                input: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
                output: u
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                ..TokenUsage::default()
            });
        }
        let Some(choice) = v
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            return;
        };
        if let Some(t) = choice
            .get("delta")
            .and_then(|d| d.get("content"))
            .and_then(Value::as_str)
        {
            if !t.is_empty() {
                out.push_back(Ok(ChatEvent::TextDelta(t.to_owned())));
            }
        }
        if let Some(r) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop = Some(match r {
                "length" => StopReason::MaxTokens,
                "stop" => StopReason::EndTurn,
                "content_filter" => StopReason::Refusal,
                _ => StopReason::EndTurn,
            });
        }
    }

    fn finish_event(&mut self) -> Result<ChatEvent, ProviderError> {
        self.finished = true;
        Ok(ChatEvent::Finished {
            stop: self.stop.unwrap_or(StopReason::EndTurn),
            usage: self.usage,
        })
    }
}

impl FrameMapper for OpenAiMapper {
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
                // The literal `data: [DONE]`, which the decoder caught before
                // the JSON parser could choke on it.
                SseFrame::Done => {
                    let ev = self.finish_event();
                    out.push_back(ev);
                }
            }
        }
    }

    fn finish(&mut self, out: &mut VecDeque<Result<ChatEvent, ProviderError>>) {
        let mut frames = Vec::new();
        self.sse.finish(&mut frames);
        for f in frames {
            match f {
                SseFrame::Event { data, .. } => self.frame(&data, out),
                SseFrame::Done => {
                    let ev = self.finish_event();
                    out.push_back(ev);
                }
            }
        }
        if !self.finished {
            out.push_back(Err(ProviderError::protocol("stream ended before [DONE]")));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::provider::egress::{EgressPolicy, NetworkMode};
    use crate::provider::types::{Effort, Message, Role, SystemChunk, Thinking};

    fn provider() -> OpenAiCompatProvider {
        OpenAiCompatProvider::new(
            ProviderConfig::openai_compat_default(),
            Transport::new(
                EgressPolicy::new(NetworkMode::Enabled),
                Duration::from_secs(5),
            )
            .unwrap(),
            Some(SecretString::from("sk-openai-abcdefghijklmnop".to_owned())),
        )
    }

    fn request() -> ChatRequest {
        ChatRequest {
            model: ModelId::from("gpt-4o-mini"),
            system: vec![
                SystemChunk {
                    text: "A".into(),
                    cache: true,
                },
                SystemChunk {
                    text: "B".into(),
                    cache: false,
                },
            ],
            messages: vec![Message {
                role: Role::User,
                content: "hi".into(),
            }],
            max_output_tokens: 100,
            effort: Effort::Medium,
            thinking: Thinking::Off,
            json_schema: None,
            temperature: Some(0.2),
            stop: vec!["END".into()],
            deadline: Duration::from_secs(10),
        }
    }

    #[test]
    fn body_has_no_tools() {
        let body = provider().build_body(&request()).unwrap();
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        assert!(!body.to_string().contains("\"tools\""));
    }

    #[test]
    fn system_chunks_collapse_into_one_system_message_in_order() {
        let body = provider().build_body(&request()).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "A\n\nB");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn sampling_parameters_are_carried_here_unlike_anthropic() {
        let body = provider().build_body(&request()).unwrap();
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["stop"][0], "END");
    }

    #[test]
    fn stream_options_is_not_sent() {
        // See the module header: a gateway that rejects the unknown field kills
        // the feature outright for that user.
        let body = provider().build_body(&request()).unwrap();
        assert!(body.get("stream_options").is_none());
    }

    fn drain(body: &[u8]) -> Vec<Result<ChatEvent, ProviderError>> {
        let mut m = OpenAiMapper::default();
        let mut out = VecDeque::new();
        m.push(body, &mut out);
        m.finish(&mut out);
        out.into_iter().collect()
    }

    #[test]
    fn done_terminates_the_stream_and_is_never_handed_to_the_json_parser() {
        let body = concat!(
            "data: {\"id\":\"c1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let events = drain(body.as_bytes());
        assert!(matches!(&events[0], Ok(ChatEvent::Started { .. })));
        assert!(matches!(&events[1], Ok(ChatEvent::TextDelta(t)) if t == "Hel"));
        assert!(matches!(&events[2], Ok(ChatEvent::TextDelta(t)) if t == "lo"));
        assert!(matches!(
            &events[3],
            Ok(ChatEvent::Finished {
                stop: StopReason::EndTurn,
                ..
            })
        ));
        assert_eq!(
            events.len(),
            4,
            "no protocol error was manufactured from [DONE]"
        );
    }

    #[test]
    fn a_content_filter_finish_is_a_refusal() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let events = drain(body.as_bytes());
        assert!(matches!(
            events.last(),
            Some(Ok(ChatEvent::Finished {
                stop: StopReason::Refusal,
                ..
            }))
        ));
    }

    #[test]
    fn a_stream_that_stops_without_done_is_an_error() {
        let events = drain(b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n");
        assert!(matches!(
            events.last(),
            Some(Err(ProviderError::Protocol(_)))
        ));
    }
}
