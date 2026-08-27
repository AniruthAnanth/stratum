//! 07 §2.5 — the local / offline backend.
//!
//! The **only** provider whose [`ProviderCaps::requires_network`] is `false`,
//! and therefore the only one offline mode can select (07 §4.7 layer 1).
//!
//! Ollama's native `/api/chat` rather than its `/v1/chat/completions` shim, for
//! two reasons that are worth a second NDJSON decoder: `keep_alive` keeps the
//! model resident between quick-fix requests — the difference between 400 ms and
//! 6 s, which is the difference between a usable and an unusable quick-fix — and
//! `/api/tags` is what the model picker lists. We do not bundle or auto-download
//! a model: a 4 GB download inside an installer is unacceptable, and the daemon
//! is the user's to manage.
//!
//! [`ProviderCaps::requires_network`]: crate::provider::types::ProviderCaps::requires_network

use std::collections::VecDeque;

use futures::stream::BoxStream;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Url;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::provider::error::ProviderError;
use crate::provider::http::{event_stream, FrameMapper, Transport};
use crate::provider::ndjson::NdjsonDecoder;
use crate::provider::traits::ChatProvider;
use crate::provider::types::shortest_f64;
use crate::provider::types::{
    ChatEvent, ChatRequest, HealthReport, ModelId, ProviderCaps, ProviderId, StopReason, TokenUsage,
};
use crate::provider::ProviderConfig;

/// 07 §2.5's default. Not downloaded by us; the settings pane lists whatever
/// `/api/tags` actually returns.
pub const DEFAULT_MODEL: &str = "qwen2.5-coder:7b";

/// How long the daemon keeps the model resident after a request.
const KEEP_ALIVE: &str = "10m";

/// A local Ollama daemon.
pub struct OllamaProvider {
    config: ProviderConfig,
    transport: Transport,
}

impl OllamaProvider {
    /// Build a backend. There is no credential: a daemon on loopback needs none,
    /// and passing one would be a way to hand a cloud key to a local process.
    #[must_use]
    pub fn new(config: ProviderConfig, transport: Transport) -> Self {
        Self { config, transport }
    }

    fn endpoint(&self, path: &str) -> Result<Url, ProviderError> {
        self.config
            .base_url
            .join(path)
            .map_err(|e| ProviderError::protocol(format!("bad base_url: {e}")))
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/x-ndjson"),
        );
        h
    }
}

#[async_trait::async_trait]
impl ChatProvider for OllamaProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Ollama
    }

    fn display_name(&self) -> &str {
        "Ollama (local)"
    }

    fn default_model(&self) -> ModelId {
        self.config.model.clone()
    }

    fn caps(&self, _model: &ModelId) -> ProviderCaps {
        ProviderCaps {
            // THE field. Layer 1 of offline enforcement is a comparison against
            // exactly this, so it must be false only when the endpoint really is
            // on this machine — which `ProviderConfig::ollama_default` and the
            // egress guard together are what make true.
            requires_network: false,
            streaming: true,
            prompt_cache: false,
            structured_output: true,
            thinking: false,
            sampling_params: true,
            max_input_tokens: 32_768,
            max_output_tokens: 8_192,
        }
    }

    fn estimate_tokens(&self, s: &str) -> u32 {
        super::anthropic::estimate_tokens(s)
    }

    fn build_body(&self, req: &ChatRequest) -> Result<Value, ProviderError> {
        let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
        if !req.system.is_empty() {
            let system: Vec<&str> = req.system.iter().map(|c| c.text.as_str()).collect();
            messages.push(json!({ "role": "system", "content": system.join("\n\n") }));
        }
        for m in &req.messages {
            messages.push(json!({ "role": m.role.as_str(), "content": m.content }));
        }

        let mut options = json!({ "num_predict": req.max_output_tokens });
        if let Some(t) = req.temperature {
            options["temperature"] = json!(shortest_f64(t));
        }
        if !req.stop.is_empty() {
            options["stop"] = json!(req.stop.clone());
        }

        let mut body = json!({
            "model": req.model.as_str(),
            "messages": messages,
            "stream": true,
            "keep_alive": KEEP_ALIVE,
            "options": options,
        });
        if let Some(schema) = &req.json_schema {
            // Ollama takes the schema directly in `format`.
            body["format"] = schema.clone();
        }
        // No `tools` key. Ever. 07 §0.2 — and here it matters just as much: a
        // local model is not a more trustworthy model.
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
            .post_json(&self.endpoint("api/chat")?, self.headers(), &body)
            .await?;
        Ok(event_stream(resp, OllamaMapper::default(), cancel))
    }

    async fn health(&self) -> Result<HealthReport, ProviderError> {
        let resp = self
            .transport
            .get(&self.endpoint("api/tags")?, HeaderMap::new())
            .await?;
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::protocol(format!("/api/tags was not JSON: {e}")))?;
        let models: Vec<ModelId> = body
            .get("models")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.get("name").and_then(Value::as_str))
                    .map(ModelId::from)
                    .collect()
            })
            .unwrap_or_default();
        let detail = if models.is_empty() {
            "Ollama is running but has no models pulled. Run `ollama pull qwen2.5-coder:7b`."
                .to_owned()
        } else {
            format!("Ollama is running with {} model(s)", models.len())
        };
        Ok(HealthReport {
            ok: true,
            detail,
            models,
        })
    }
}

/// NDJSON lines → [`ChatEvent`].
#[derive(Default)]
struct OllamaMapper {
    ndjson: NdjsonDecoder,
    started: bool,
    finished: bool,
}

impl OllamaMapper {
    fn line(&mut self, line: &str, out: &mut VecDeque<Result<ChatEvent, ProviderError>>) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            out.push_back(Err(ProviderError::protocol("NDJSON line was not JSON")));
            return;
        };
        if let Some(err) = v.get("error").and_then(Value::as_str) {
            self.finished = true;
            out.push_back(Err(ProviderError::protocol(err)));
            return;
        }
        if !self.started {
            self.started = true;
            out.push_back(Ok(ChatEvent::Started {
                provider_request_id: None,
                model: v
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
            }));
        }
        if let Some(t) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
        {
            if !t.is_empty() {
                out.push_back(Ok(ChatEvent::TextDelta(t.to_owned())));
            }
        }
        if v.get("done").and_then(Value::as_bool).unwrap_or(false) {
            self.finished = true;
            let get = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
            out.push_back(Ok(ChatEvent::Finished {
                stop: match v.get("done_reason").and_then(Value::as_str) {
                    Some("length") => StopReason::MaxTokens,
                    _ => StopReason::EndTurn,
                },
                usage: TokenUsage {
                    input: get("prompt_eval_count"),
                    output: get("eval_count"),
                    ..TokenUsage::default()
                },
            }));
        }
    }
}

impl FrameMapper for OllamaMapper {
    fn push(&mut self, chunk: &[u8], out: &mut VecDeque<Result<ChatEvent, ProviderError>>) {
        let mut lines = Vec::new();
        self.ndjson.push(chunk, &mut lines);
        if self.ndjson.overflowed() {
            out.push_back(Err(ProviderError::protocol("NDJSON line exceeded 8 MiB")));
            self.finished = true;
            return;
        }
        for line in lines {
            self.line(&line, out);
        }
    }

    fn finish(&mut self, out: &mut VecDeque<Result<ChatEvent, ProviderError>>) {
        let mut lines = Vec::new();
        self.ndjson.finish(&mut lines);
        for line in lines {
            self.line(&line, out);
        }
        if !self.finished {
            out.push_back(Err(ProviderError::protocol(
                "stream ended before done:true",
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::provider::egress::{EgressPolicy, NetworkMode};
    use crate::provider::types::{Effort, Message, Role, SystemChunk, Thinking};

    fn provider() -> OllamaProvider {
        OllamaProvider::new(
            ProviderConfig::ollama_default(),
            Transport::new(
                EgressPolicy::new(NetworkMode::Offline),
                Duration::from_secs(5),
            )
            .unwrap(),
        )
    }

    fn request() -> ChatRequest {
        ChatRequest {
            model: ModelId::from(DEFAULT_MODEL),
            system: vec![SystemChunk {
                text: "S".into(),
                cache: true,
            }],
            messages: vec![Message {
                role: Role::User,
                content: "hi".into(),
            }],
            max_output_tokens: 120,
            effort: Effort::Low,
            thinking: Thinking::Off,
            json_schema: None,
            temperature: Some(0.1),
            stop: Vec::new(),
            deadline: Duration::from_millis(800),
        }
    }

    #[test]
    fn body_has_no_tools() {
        let body = provider().build_body(&request()).unwrap();
        assert!(body.get("tools").is_none());
        assert!(!body.to_string().contains("\"tools\""));
    }

    #[test]
    fn keep_alive_is_sent_because_a_cold_model_makes_quick_fix_useless() {
        let body = provider().build_body(&request()).unwrap();
        assert_eq!(body["keep_alive"], KEEP_ALIVE);
        assert_eq!(body["options"]["num_predict"], 120);
    }

    #[test]
    fn this_is_the_only_backend_that_does_not_require_network() {
        assert!(
            !provider()
                .caps(&ModelId::from(DEFAULT_MODEL))
                .requires_network
        );
    }

    fn drain(body: &[u8]) -> Vec<Result<ChatEvent, ProviderError>> {
        let mut m = OllamaMapper::default();
        let mut out = VecDeque::new();
        m.push(body, &mut out);
        m.finish(&mut out);
        out.into_iter().collect()
    }

    #[test]
    fn ndjson_decodes_into_started_deltas_and_finished() {
        let body = concat!(
            "{\"model\":\"qwen2.5-coder:7b\",\"message\":{\"role\":\"assistant\",\"content\":\"He\"},\"done\":false}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":\"llo\"},\"done\":false}\n",
            "{\"done\":true,\"prompt_eval_count\":42,\"eval_count\":9}\n",
        );
        let events = drain(body.as_bytes());
        assert!(matches!(&events[0], Ok(ChatEvent::Started { .. })));
        assert!(matches!(&events[1], Ok(ChatEvent::TextDelta(t)) if t == "He"));
        assert!(matches!(&events[2], Ok(ChatEvent::TextDelta(t)) if t == "llo"));
        match &events[3] {
            Ok(ChatEvent::Finished { usage, .. }) => {
                assert_eq!(usage.input, 42);
                assert_eq!(usage.output, 9);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_stream_that_never_says_done_is_an_error() {
        let events = drain(b"{\"message\":{\"content\":\"x\"},\"done\":false}\n");
        assert!(matches!(
            events.last(),
            Some(Err(ProviderError::Protocol(_)))
        ));
    }
}
