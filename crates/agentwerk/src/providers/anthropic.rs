//! Anthropic Messages API provider, with SSE streaming and cache-aware token accounting.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::error::{ProviderError, ProviderResult};
use super::provider::{
    build_client, ModelRequest, Provider, ProviderToolDefinition, ToolChoice,
    DEFAULT_REQUEST_TIMEOUT,
};
use super::types::{ContentBlock, Message, ModelResponse, ResponseStatus, StreamEvent, TokenUsage};

/// LLM provider for the Anthropic Messages API.
///
/// Reads `ANTHROPIC_API_KEY` (and optional `ANTHROPIC_BASE_URL`) when built
/// via [`provider_from_env`]. Override the endpoint with [`base_url`] and
/// the per-request timeout with [`timeout`].
///
/// # Examples
///
/// Direct construction with an API key:
///
/// ```no_run
/// use agentwerk::providers::AnthropicProvider;
///
/// let _provider = AnthropicProvider::new("sk-ant-...");
/// ```
///
/// Read the API key from the environment:
///
/// ```no_run
/// use agentwerk::providers::provider_from_env;
///
/// let _provider = provider_from_env().expect("LLM provider required");
/// ```
///
/// [`provider_from_env`]: crate::providers::provider_from_env
/// [`base_url`]: AnthropicProvider::base_url
/// [`timeout`]: AnthropicProvider::timeout
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
    timeout: Duration,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com".into(),
            client: build_client(DEFAULT_REQUEST_TIMEOUT),
            timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    pub(crate) fn from_env() -> ProviderResult<Self> {
        use super::environment::{env_or, env_required};
        Ok(Self::new(env_required("ANTHROPIC_API_KEY")?)
            .base_url(env_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com")))
    }

    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self.client = build_client(self.timeout);
        self
    }

    #[cfg(test)]
    pub(crate) fn request_timeout(&self) -> Duration {
        self.timeout
    }

    fn api_headers(&self) -> Vec<(String, String)> {
        vec![
            ("x-api-key".into(), self.api_key.clone()),
            ("anthropic-version".into(), "2023-06-01".into()),
            ("content-type".into(), "application/json".into()),
        ]
    }

    fn serialize_request(&self, request: &ModelRequest) -> Value {
        let mut body = serde_json::json!({
            "model": request.model,
            "system": request.system_prompt,
            "messages": serialize_messages(&request.messages),
        });
        if let Some(n) = request.max_request_tokens {
            body["max_tokens"] = Value::from(n);
        }
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(serialize_tool_definition)
                .collect();
            body["tools"] = Value::Array(tools);
        }
        if let Some(ref choice) = request.tool_choice {
            body["tool_choice"] = serialize_tool_choice(choice);
        }
        // Request thinking only when an effort is set and the model accepts the
        // adaptive form. Older Anthropic models used a different request field we
        // no longer send, so they get none; parsing still keeps the thinking of
        // any model that returns it.
        if let Some(effort) = request.reasoning_effort.label() {
            if supports_adaptive_thinking(&request.model) {
                body["thinking"] = serde_json::json!({"type": "adaptive", "display": "summarized"});
                body["output_config"] = serde_json::json!({"effort": effort});
            }
        }
        body
    }

    async fn send_request(&self, body: Value) -> ProviderResult<reqwest::Response> {
        let url = format!("{}/v1/messages", self.base_url);
        let mut req = self.client.post(&url).json(&body);
        for (k, v) in self.api_headers() {
            req = req.header(k, v);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed {
                message: e.to_string(),
            })?;

        super::map_http_errors(resp, classify_error).await
    }
}

/// Map Anthropic-specific error signatures to typed [`ProviderError`]
/// variants. Any status/body combination the match doesn't recognise
/// returns `None`, causing `map_http_errors` to fall through to
/// [`ProviderError::StatusUnclassified`] (or [`ProviderError::RateLimited`]
/// for 429/529).
fn classify_error(status: u16, body: &str) -> Option<ProviderError> {
    match status {
        401 => Some(ProviderError::AuthenticationFailed {
            message: body.into(),
        }),
        403 => Some(ProviderError::PermissionDenied {
            message: body.into(),
        }),
        404 => Some(ProviderError::ModelNotFound {
            message: body.into(),
        }),
        400 => classify_400(body),
        _ => None,
    }
}

fn classify_400(body: &str) -> Option<ProviderError> {
    let json: Value = serde_json::from_str(body).ok()?;
    let err = &json["error"];
    let type_ = err["type"].as_str().unwrap_or("");
    let message = err["message"].as_str().unwrap_or("").to_string();
    match type_ {
        "invalid_request_error"
            if message.contains("prompt is too long")
                || message.contains("input length and `max_tokens` exceed context limit") =>
        {
            Some(ProviderError::ContextWindowExceeded { message })
        }
        "not_found_error" => Some(ProviderError::ModelNotFound { message }),
        _ => None,
    }
}

impl AnthropicProvider {
    pub(crate) fn lookup_context_window_size(id: &str) -> Option<u64> {
        let m = id.to_ascii_lowercase();
        if m.contains("[1m]") {
            return Some(1_000_000);
        }
        if m.contains("claude-opus-4")
            || m.contains("claude-sonnet-4")
            || m.contains("claude-haiku-4")
            || m.contains("claude-3-7-sonnet")
            || m.contains("claude-3-5-sonnet")
            || m.contains("claude-3-5-haiku")
            || m.contains("claude-3-opus")
            || m.contains("claude-3-sonnet")
            || m.contains("claude-3-haiku")
        {
            return Some(200_000);
        }
        None
    }
}

impl Provider for AnthropicProvider {
    fn prewarm(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async { super::provider::prewarm_with(&self.client, &self.base_url).await })
    }

    fn respond(
        &self,
        request: ModelRequest,
        on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
        let mut body = self.serialize_request(&request);
        body["stream"] = Value::Bool(true);

        Box::pin(async move {
            let resp = self.send_request(body).await?;
            stream_response(resp, &on_event).await
        })
    }
}

async fn stream_response(
    mut response: reqwest::Response,
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) -> ProviderResult<ModelResponse> {
    use super::stream::{SseEvent, StreamParser};

    let mut state = StreamState::default();
    let mut parser = StreamParser::new();

    while let Some(chunk) =
        response
            .chunk()
            .await
            .map_err(|e| ProviderError::StreamInterrupted {
                message: e.to_string(),
            })?
    {
        for event in parser.push(&chunk) {
            match event {
                SseEvent::Done => on_event(StreamEvent::MessageDone),
                SseEvent::Data(json) => handle_stream_event(&json, &mut state, on_event),
            }
        }
    }

    Ok(state.into_response())
}

/// Accumulator for a single Anthropic content block. Replaces four parallel
/// `Vec<Option<String>>` lookups with one typed slot per index.
enum BlockAcc {
    Text(String),
    Tool {
        id: String,
        name: String,
        input: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    Redacted {
        data: String,
    },
}

#[derive(Default)]
struct StreamState {
    model: String,
    usage: TokenUsage,
    status: ResponseStatus,
    content_blocks: Vec<ContentBlock>,
    blocks: Vec<Option<BlockAcc>>,
}

impl StreamState {
    fn slot(&mut self, idx: usize) -> &mut Option<BlockAcc> {
        if self.blocks.len() <= idx {
            self.blocks.resize_with(idx + 1, || None);
        }
        &mut self.blocks[idx]
    }

    fn into_response(self) -> ModelResponse {
        ModelResponse {
            content: self.content_blocks,
            status: self.status,
            usage: self.usage,
            model: if self.model.is_empty() {
                "unknown".into()
            } else {
                self.model
            },
        }
    }
}

fn handle_stream_event(
    json: &Value,
    state: &mut StreamState,
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    match json["type"].as_str().unwrap_or("") {
        "message_start" => handle_message_start(json, state),
        "content_block_start" => handle_block_start(json, state),
        "content_block_delta" => handle_block_delta(json, state, on_event),
        "content_block_stop" => handle_block_stop(json, state, on_event),
        "message_delta" => handle_message_delta(json, state, on_event),
        _ => {}
    }
}

fn handle_message_start(json: &Value, state: &mut StreamState) {
    let message = &json["message"];
    state.model = message["model"].as_str().unwrap_or("unknown").to_string();
    let u = &message["usage"];
    state.usage.input_tokens = u["input_tokens"].as_u64().unwrap_or(0);
}

fn handle_block_start(json: &Value, state: &mut StreamState) {
    let idx = json["index"].as_u64().unwrap_or(0) as usize;
    let block = &json["content_block"];
    let acc = match block["type"].as_str().unwrap_or("") {
        "tool_use" => BlockAcc::Tool {
            id: block["id"].as_str().unwrap_or("").to_string(),
            name: block["name"].as_str().unwrap_or("").to_string(),
            input: String::new(),
        },
        "thinking" => BlockAcc::Thinking {
            thinking: String::new(),
            signature: String::new(),
        },
        "redacted_thinking" => BlockAcc::Redacted {
            data: block["data"].as_str().unwrap_or("").to_string(),
        },
        _ => BlockAcc::Text(String::new()),
    };
    *state.slot(idx) = Some(acc);
}

fn handle_block_delta(
    json: &Value,
    state: &mut StreamState,
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    let idx = json["index"].as_u64().unwrap_or(0) as usize;
    let delta = &json["delta"];
    match delta["type"].as_str().unwrap_or("") {
        "text_delta" => {
            let text = delta["text"].as_str().unwrap_or("").to_string();
            if let Some(Some(BlockAcc::Text(buf))) = state.blocks.get_mut(idx) {
                buf.push_str(&text);
            }
            on_event(StreamEvent::TextDelta { index: idx, text });
        }
        "input_json_delta" => {
            let partial = delta["partial_json"].as_str().unwrap_or("");
            if let Some(Some(BlockAcc::Tool { input, .. })) = state.blocks.get_mut(idx) {
                input.push_str(partial);
            }
            on_event(StreamEvent::InputJsonDelta {
                index: idx,
                partial_json: partial.to_string(),
            });
        }
        "thinking_delta" => {
            if let Some(Some(BlockAcc::Thinking { thinking, .. })) = state.blocks.get_mut(idx) {
                thinking.push_str(delta["thinking"].as_str().unwrap_or(""));
            }
        }
        "signature_delta" => {
            if let Some(Some(BlockAcc::Thinking { signature, .. })) = state.blocks.get_mut(idx) {
                signature.push_str(delta["signature"].as_str().unwrap_or(""));
            }
        }
        _ => {}
    }
}

fn handle_block_stop(
    json: &Value,
    state: &mut StreamState,
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    let idx = json["index"].as_u64().unwrap_or(0) as usize;
    match state.blocks.get_mut(idx).and_then(Option::take) {
        Some(BlockAcc::Text(text)) => state.content_blocks.push(ContentBlock::Text { text }),
        Some(BlockAcc::Tool { id, name, input }) => {
            let input = serde_json::from_str(&input).unwrap_or(Value::Object(Default::default()));
            state
                .content_blocks
                .push(ContentBlock::ToolUse { id, name, input });
        }
        Some(BlockAcc::Thinking {
            thinking,
            signature,
        }) => state.content_blocks.push(ContentBlock::Thinking {
            thinking,
            signature,
        }),
        Some(BlockAcc::Redacted { data }) => state
            .content_blocks
            .push(ContentBlock::RedactedThinking { data }),
        None => {}
    }
    on_event(StreamEvent::ContentBlockStop { index: idx });
}

fn handle_message_delta(
    json: &Value,
    state: &mut StreamState,
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    state.status = parse_status_str(json["delta"]["stop_reason"].as_str().unwrap_or("end_turn"));
    state.usage.output_tokens = json["usage"]["output_tokens"].as_u64().unwrap_or(0);
    on_event(StreamEvent::MessageDelta {
        status: state.status.clone(),
        usage: state.usage.clone(),
    });
}

fn serialize_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|msg| {
            let (role, content) = match msg {
                Message::System { .. } => return None,
                Message::User { content } => ("user", content),
                Message::Assistant { content } => ("assistant", content),
            };
            Some(serde_json::json!({
                "role": role,
                "content": serialize_content_blocks(content),
            }))
        })
        .collect()
}

fn serialize_content_blocks(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks.iter().filter_map(serialize_content_block).collect()
}

fn serialize_content_block(block: &ContentBlock) -> Option<Value> {
    let value = match block {
        ContentBlock::Text { text } => {
            serde_json::json!({"type": "text", "text": text})
        }
        ContentBlock::ToolUse { id, name, input } => {
            serde_json::json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            succeeded,
        } => {
            serde_json::json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": !succeeded})
        }
        // A thinking block replays only with its signature; without one (an
        // aborted stream) the API rejects it, so omit it. It stays in the
        // transcript regardless.
        ContentBlock::Thinking { signature, .. } if signature.is_empty() => return None,
        ContentBlock::Thinking {
            thinking,
            signature,
        } => serde_json::json!({"type": "thinking", "thinking": thinking, "signature": signature}),
        ContentBlock::RedactedThinking { data } => {
            serde_json::json!({"type": "redacted_thinking", "data": data})
        }
    };
    Some(value)
}

fn serialize_tool_definition(tool: &ProviderToolDefinition) -> Value {
    serde_json::json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
    })
}

fn serialize_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => serde_json::json!({"type": "auto"}),
        ToolChoice::Specific { name } => serde_json::json!({"type": "tool", "name": name}),
    }
}

/// True for the Anthropic generation that takes `thinking:{type:"adaptive"}`
/// plus `output_config.effort` (Opus/Sonnet 4.6 and later, Fable, Mythos). The
/// `[1m]` context suffix rides on the base name, so `opus-4-8[1m]` matches.
fn supports_adaptive_thinking(model: &str) -> bool {
    const ADAPTIVE: &[&str] = &[
        "opus-4-6",
        "opus-4-7",
        "opus-4-8",
        "sonnet-4-6",
        "sonnet-5",
        "fable",
        "mythos",
    ];
    ADAPTIVE.iter().any(|family| model.contains(family))
}

fn parse_status_str(raw: &str) -> ResponseStatus {
    match raw {
        "end_turn" => ResponseStatus::EndTurn,
        "stop_sequence" => ResponseStatus::StopSequence,
        "tool_use" => ResponseStatus::ToolUse,
        "max_tokens" => ResponseStatus::OutputTruncated,
        "model_context_window_exceeded" => ResponseStatus::ContextWindowExceeded,
        "refusal" => ResponseStatus::Refused,
        "pause_turn" => ResponseStatus::PauseTurn,
        _ => ResponseStatus::EndTurn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ReasoningEffort;

    fn noop_events() -> Arc<dyn Fn(StreamEvent) + Send + Sync> {
        Arc::new(|_| {})
    }

    #[test]
    fn parses_thinking_and_signature_into_one_block() {
        let mut state = StreamState::default();
        let sink = noop_events();
        handle_stream_event(
            &serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            &mut state,
            &sink,
        );
        handle_stream_event(
            &serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weigh it"}}),
            &mut state,
            &sink,
        );
        handle_stream_event(
            &serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-abc"}}),
            &mut state,
            &sink,
        );
        handle_stream_event(
            &serde_json::json!({"type":"content_block_stop","index":0}),
            &mut state,
            &sink,
        );
        assert!(matches!(
            &state.into_response().content[..],
            [ContentBlock::Thinking { thinking, signature }]
                if thinking == "weigh it" && signature == "sig-abc"
        ));
    }

    #[test]
    fn parses_redacted_thinking_block() {
        let mut state = StreamState::default();
        let sink = noop_events();
        handle_stream_event(
            &serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"enc-xyz"}}),
            &mut state,
            &sink,
        );
        handle_stream_event(
            &serde_json::json!({"type":"content_block_stop","index":0}),
            &mut state,
            &sink,
        );
        assert!(matches!(
            &state.into_response().content[..],
            [ContentBlock::RedactedThinking { data }] if data == "enc-xyz"
        ));
    }

    #[test]
    fn serializes_thinking_block_with_signature() {
        let value = serialize_content_block(&ContentBlock::Thinking {
            thinking: "why".into(),
            signature: "sig".into(),
        })
        .unwrap();
        assert_eq!(value["type"], "thinking");
        assert_eq!(value["thinking"], "why");
        assert_eq!(value["signature"], "sig");
    }

    #[test]
    fn omits_thinking_block_without_signature() {
        assert!(serialize_content_block(&ContentBlock::Thinking {
            thinking: "why".into(),
            signature: String::new(),
        })
        .is_none());
    }

    #[test]
    fn serializes_redacted_thinking_block() {
        let value = serialize_content_block(&ContentBlock::RedactedThinking { data: "enc".into() })
            .unwrap();
        assert_eq!(value["type"], "redacted_thinking");
        assert_eq!(value["data"], "enc");
    }

    #[test]
    fn serialize_request_adds_adaptive_thinking_for_current_model() {
        let mut req = simple_request();
        req.model = "claude-opus-4-8".into();
        req.reasoning_effort = ReasoningEffort::High;
        let body = provider().serialize_request(&req);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
    }

    #[test]
    fn serialize_request_omits_thinking_when_off() {
        let body = provider().serialize_request(&simple_request());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn serialize_request_omits_thinking_for_legacy_model() {
        let mut req = simple_request();
        req.model = "claude-sonnet-4-20250514".into();
        req.reasoning_effort = ReasoningEffort::High;
        let body = provider().serialize_request(&req);
        assert!(body.get("thinking").is_none());
    }

    fn simple_request() -> ModelRequest {
        ModelRequest {
            model: "test-model".into(),
            system_prompt: "You are helpful.".into(),
            messages: vec![Message::User {
                content: vec![ContentBlock::Text { text: "Hi".into() }],
            }],
            tools: vec![],
            max_request_tokens: Some(1024),
            tool_choice: None,
            reasoning_effort: Default::default(),
        }
    }

    fn provider() -> AnthropicProvider {
        AnthropicProvider::new("test-key")
    }

    #[test]
    fn timeout_builder_stores_duration() {
        let p = provider();
        assert_eq!(p.request_timeout(), DEFAULT_REQUEST_TIMEOUT);
        let p = p.timeout(Duration::from_secs(42));
        assert_eq!(p.request_timeout(), Duration::from_secs(42));
    }

    #[test]
    fn serialize_request_sets_model_and_system() {
        let body = provider().serialize_request(&simple_request());
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["system"], "You are helpful.");
        assert_eq!(body["max_tokens"], 1024);
    }

    #[test]
    fn serialize_request_omits_max_tokens_when_none() {
        let mut req = simple_request();
        req.max_request_tokens = None;
        let body = provider().serialize_request(&req);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn serialize_request_excludes_system_from_messages() {
        let body = provider().serialize_request(&simple_request());
        let messages = body["messages"].as_array().unwrap();
        for msg in messages {
            assert_ne!(msg["role"], "system");
        }
    }

    #[test]
    fn serialize_request_includes_tool_choice() {
        let mut req = simple_request();
        req.tool_choice = Some(ToolChoice::Specific {
            name: "read_file".into(),
        });
        let body = provider().serialize_request(&req);
        assert_eq!(body["tool_choice"]["type"], "tool");
        assert_eq!(body["tool_choice"]["name"], "read_file");
    }

    fn invalid_request(message: &str) -> String {
        serde_json::json!({
            "error": { "type": "invalid_request_error", "message": message }
        })
        .to_string()
    }

    #[test]
    fn context_window_exceeded_when_prompt_is_too_long() {
        let body = invalid_request("prompt is too long: 205000 > 200000");
        assert!(matches!(
            classify_error(400, &body),
            Some(ProviderError::ContextWindowExceeded { .. })
        ));
    }

    #[test]
    fn maps_400_not_found_error_to_model_not_found() {
        let body = serde_json::json!({
            "error": { "type": "not_found_error", "message": "model opus-9 not found" }
        })
        .to_string();
        assert!(matches!(
            classify_error(400, &body),
            Some(ProviderError::ModelNotFound { .. })
        ));
    }

    #[test]
    fn maps_http_401_to_authentication_failed() {
        assert!(matches!(
            classify_error(401, "boom"),
            Some(ProviderError::AuthenticationFailed { .. })
        ));
    }

    #[test]
    fn maps_http_403_to_permission_denied() {
        assert!(matches!(
            classify_error(403, "boom"),
            Some(ProviderError::PermissionDenied { .. })
        ));
    }

    #[test]
    fn maps_http_404_to_model_not_found() {
        assert!(matches!(
            classify_error(404, "boom"),
            Some(ProviderError::ModelNotFound { .. })
        ));
    }

    #[test]
    fn unrelated_400_falls_through() {
        let body = invalid_request("max_tokens must be a positive integer");
        assert!(classify_error(400, &body).is_none());
    }

    #[test]
    fn preserves_provider_message() {
        let Some(ProviderError::AuthenticationFailed { message }) =
            classify_error(401, "your api key is revoked")
        else {
            panic!("expected AuthenticationFailed");
        };
        assert_eq!(message, "your api key is revoked");
    }

    #[test]
    fn lookup_claude_4_family_returns_200k() {
        let lookup = AnthropicProvider::lookup_context_window_size;
        assert_eq!(lookup("claude-sonnet-4-20250514"), Some(200_000));
        assert_eq!(lookup("claude-opus-4-20250101"), Some(200_000));
        assert_eq!(lookup("claude-haiku-4-5-20251001"), Some(200_000));
    }

    #[test]
    fn lookup_claude_3_family_returns_200k() {
        let lookup = AnthropicProvider::lookup_context_window_size;
        assert_eq!(lookup("claude-3-5-sonnet-20241022"), Some(200_000));
        assert_eq!(lookup("claude-3-opus-20240229"), Some(200_000));
    }

    #[test]
    fn lookup_one_million_suffix_overrides_base_family() {
        let lookup = AnthropicProvider::lookup_context_window_size;
        assert_eq!(
            lookup("claude-opus-4-7[1m]"),
            Some(1_000_000),
            "explicit [1m] opt-in promotes to 1M"
        );
        assert_eq!(lookup("claude-sonnet-4-20250514[1m]"), Some(1_000_000));
    }

    #[test]
    fn lookup_unknown_models_return_none() {
        let lookup = AnthropicProvider::lookup_context_window_size;
        assert_eq!(lookup("gpt-4"), None);
        assert_eq!(lookup("mistral-large-2411"), None);
        assert_eq!(lookup("some-future-model"), None);
    }
}
