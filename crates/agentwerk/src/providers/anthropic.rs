//! Anthropic Messages API provider, with SSE streaming and cache-aware token accounting.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::endpoint::Endpoint;
use super::error::{ProviderError, ProviderResult};
use super::provider::{self, Protocol, ProviderLike};
use super::stream::{ResponseBuilder, ToolCallKey};
use super::types::{
    ContentBlock, Message, ModelRequest, ModelResponse, ReasoningEffort, ResponseStatus,
    StreamEvent,
};
use crate::tools::Tool;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// LLM provider for the Anthropic Messages API.
///
/// Reads `ANTHROPIC_API_KEY` (and optional `ANTHROPIC_BASE_URL`) when built
/// via [`Provider::from_env`]. Override the endpoint with [`base_url`] and
/// the per-request timeout with [`timeout`].
///
/// # Examples
///
/// Direct construction with an API key:
///
/// ```no_run
/// use agentwerk::providers::Anthropic;
///
/// let _provider = Anthropic::new("sk-ant-...");
/// ```
///
/// Read the API key from the environment:
///
/// ```no_run
/// use agentwerk::providers::Provider;
///
/// let _provider = Provider::from_env().expect("LLM provider required");
/// ```
///
/// [`Provider::from_env`]: crate::providers::Provider::from_env
/// [`base_url`]: Anthropic::base_url
/// [`timeout`]: Anthropic::timeout
pub struct Anthropic(Endpoint);

impl Anthropic {
    /// Create an endpoint using the API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self(Endpoint::new(api_key, DEFAULT_BASE_URL))
    }

    /// Replace the provider API base URL.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.0 = self.0.base_url(url);
        self
    }

    /// Set the request timeout.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.0 = self.0.timeout(duration);
        self
    }

    pub(crate) fn from_env() -> ProviderResult<Self> {
        use super::environment::{env_or, env_required};
        Ok(Self::new(env_required("ANTHROPIC_API_KEY")?)
            .base_url(env_or("ANTHROPIC_BASE_URL", DEFAULT_BASE_URL)))
    }
}

impl ProviderLike for Anthropic {
    fn respond(
        &self,
        request: ModelRequest,
        on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
        provider::respond::<AnthropicMessages>(&self.0, request, on_event)
    }
}

/// The Anthropic Messages API.
pub(crate) struct AnthropicMessages;

impl Protocol for AnthropicMessages {
    const PATH: &'static str = "/v1/messages";

    fn authenticate(posted: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
        posted
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
    }

    /// The one type only this vendor names. Its overflow wordings are read by
    /// `error::recover_wrapped_error`, and anything unrecognised falls through
    /// to [`ProviderError::StatusUnclassified`] (or
    /// [`ProviderError::RateLimited`] for 429/529).
    fn classify_error(status: u16, body: &str) -> Option<ProviderError> {
        if status != 400 {
            return None;
        }
        let json: Value = serde_json::from_str(body).ok()?;
        let error = &json["error"];
        let message = error["message"].as_str().unwrap_or("").to_string();
        match error["type"].as_str().unwrap_or("") {
            "not_found_error" => Some(ProviderError::ModelNotFound { message }),
            _ => None,
        }
    }

    fn serialize(request: &ModelRequest) -> Value {
        let mut body = serde_json::json!({
            "model": request.model,
            "stream": true,
            "system": request.system_prompt,
            "messages": serialize_messages(&request.messages),
        });
        if let Some(n) = request.max_request_tokens {
            body["max_tokens"] = Value::from(n);
        }
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request.tools.iter().map(serialize_tool).collect();
            body["tools"] = Value::Array(tools);
        }
        // Request thinking only when an effort is set and the model accepts the
        // adaptive form. Older Anthropic models used a different request field we
        // no longer send, so they get none; parsing still keeps the thinking of
        // any model that returns it.
        if request.reasoning_effort != ReasoningEffort::Off {
            let effort = request.reasoning_effort.get_name();
            if supports_adaptive_thinking(&request.model) {
                body["thinking"] = serde_json::json!({"type": "adaptive", "display": "summarized"});
                body["output_config"] = serde_json::json!({"effort": effort});
            }
        }
        body
    }

    fn decode(payload: &Value, reply: &mut ResponseBuilder) {
        match payload["type"].as_str().unwrap_or("") {
            "message_start" => decode_message_start(payload, reply),
            "content_block_start" => decode_block_start(payload, reply),
            "content_block_delta" => decode_block_delta(payload, reply),
            "message_delta" => decode_message_delta(payload, reply),
            _ => {}
        }
    }
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
        // This API types `input` as an object, so a payload `read_arguments`
        // kept as text has to go: sending it back 400s every later request.
        ContentBlock::ToolUse { id, name, input } => {
            let input = input.as_object().cloned().unwrap_or_default();
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
        // replies regardless.
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

fn serialize_tool(tool: &Tool) -> Value {
    serde_json::json!({
        "name": tool.get_name(),
        "description": tool.get_description(),
        "input_schema": tool.get_input_schema().get_raw_schema(),
    })
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

fn block_number(json: &Value) -> usize {
    json["index"].as_u64().unwrap_or(0) as usize
}

fn decode_message_start(json: &Value, reply: &mut ResponseBuilder) {
    let message = &json["message"];
    reply.set_model(message["model"].as_str().unwrap_or("unknown"));
    reply.set_input_tokens(message["usage"]["input_tokens"].as_u64().unwrap_or(0));
}

fn decode_block_start(json: &Value, reply: &mut ResponseBuilder) {
    let block = &json["content_block"];
    match block["type"].as_str().unwrap_or("") {
        "tool_use" => {
            reply.open_tool_call(
                Some(block_number(json)),
                block["id"].as_str().unwrap_or(""),
                block["name"].as_str().unwrap_or(""),
            );
        }
        "redacted_thinking" => reply.add_redacted_thinking(block["data"].as_str().unwrap_or("")),
        // A text or thinking block is opened by the first delta that reaches it.
        _ => {}
    }
}

fn decode_block_delta(json: &Value, reply: &mut ResponseBuilder) {
    let delta = &json["delta"];
    match delta["type"].as_str().unwrap_or("") {
        "text_delta" => reply.add_text(delta["text"].as_str().unwrap_or("")),
        "input_json_delta" => reply.add_arguments(
            ToolCallKey::Numbered(block_number(json)),
            delta["partial_json"].as_str().unwrap_or(""),
        ),
        "thinking_delta" => reply.add_thinking(delta["thinking"].as_str().unwrap_or("")),
        "signature_delta" => reply.add_signature(delta["signature"].as_str().unwrap_or("")),
        _ => {}
    }
}

fn decode_message_delta(json: &Value, reply: &mut ResponseBuilder) {
    let stop_reason = json["delta"]["stop_reason"].as_str().unwrap_or("end_turn");
    // The one stop reason that reports a failed request, not a finished reply.
    if stop_reason == "model_context_window_exceeded" {
        reply.set_context_window_exceeded();
    } else {
        reply.set_status(status_from_stop_reason(stop_reason));
    }
    reply.set_output_tokens(json["usage"]["output_tokens"].as_u64().unwrap_or(0));
}

fn status_from_stop_reason(raw: &str) -> ResponseStatus {
    match raw {
        "end_turn" => ResponseStatus::EndTurn,
        "stop_sequence" => ResponseStatus::StopSequence,
        "tool_use" => ResponseStatus::ToolUse,
        "max_tokens" => ResponseStatus::OutputTruncated,
        "refusal" => ResponseStatus::Refused,
        "pause_turn" => ResponseStatus::PauseTurn,
        _ => ResponseStatus::EndTurn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::endpoint::DEFAULT_REQUEST_TIMEOUT;
    use crate::providers::error::recover_wrapped_error;
    use crate::providers::ReasoningEffort;

    /// Feed `payloads` through the decoder, returning the blocks they assemble.
    fn decode_blocks(payloads: &[Value]) -> Vec<ContentBlock> {
        decode(payloads)
            .expect("a reply that did not overflow")
            .content
    }

    fn decode(payloads: &[Value]) -> ProviderResult<ModelResponse> {
        let sink: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(|_| {});
        let mut reply = ResponseBuilder::new(&sink);
        for payload in payloads {
            AnthropicMessages::decode(payload, &mut reply);
        }
        reply.into_response()
    }

    #[test]
    fn parses_thinking_and_signature_into_one_block() {
        let content = decode_blocks(&[
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weigh it"}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-abc"}}),
            serde_json::json!({"type":"content_block_stop","index":0}),
        ]);
        assert!(matches!(
            &content[..],
            [ContentBlock::Thinking { thinking, signature }]
                if thinking == "weigh it" && signature == "sig-abc"
        ));
    }

    #[test]
    fn parses_tool_input_from_json_fragments() {
        let content = decode_blocks(&[
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"grep"}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":r#"{"pattern":"#}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":r#" "exec"}"#}}),
            serde_json::json!({"type":"content_block_stop","index":0}),
        ]);
        assert!(matches!(
            &content[..],
            [ContentBlock::ToolUse { id, name, input }]
                if id == "toolu_1" && name == "grep"
                    && *input == serde_json::json!({"pattern": "exec"})
        ));
    }

    #[test]
    fn parses_two_numbered_tool_calls_into_their_own_blocks() {
        let content = decode_blocks(&[
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"grep"}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":r#"{"a":1}"#}}),
            serde_json::json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_2","name":"read_file"}}),
            serde_json::json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":r#"{"a":2}"#}}),
        ]);
        assert!(matches!(
            &content[..],
            [
                ContentBlock::ToolUse { name: first, input: one, .. },
                ContentBlock::ToolUse { name: second, input: two, .. },
            ] if first == "grep" && second == "read_file"
                && *one == serde_json::json!({"a": 1})
                && *two == serde_json::json!({"a": 2})
        ));
    }

    #[test]
    fn parses_redacted_thinking_block() {
        let content = decode_blocks(&[
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"enc-xyz"}}),
            serde_json::json!({"type":"content_block_stop","index":0}),
        ]);
        assert!(matches!(
            &content[..],
            [ContentBlock::RedactedThinking { data }] if data == "enc-xyz"
        ));
    }

    #[test]
    fn message_start_and_message_delta_together_report_the_usage() {
        let response = decode(&[
            serde_json::json!({"type":"message_start","message":{"model":"claude-opus-5","usage":{"input_tokens":120}}}),
            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":34}}),
        ])
        .unwrap();
        assert_eq!(response.model, "claude-opus-5");
        assert_eq!(response.status, ResponseStatus::ToolUse);
        assert_eq!(response.usage.input_tokens, 120);
        assert_eq!(response.usage.output_tokens, 34);
    }

    #[test]
    fn an_overflow_stop_reason_becomes_the_context_window_error() {
        let outcome = decode(&[
            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"model_context_window_exceeded"},"usage":{"output_tokens":0}}),
        ]);
        assert!(matches!(
            outcome,
            Err(ProviderError::ContextWindowExceeded { .. })
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
    fn serializes_tool_use_input_as_an_object() {
        let value = serialize_content_block(&ContentBlock::ToolUse {
            id: "toolu_1".into(),
            name: "grep".into(),
            input: serde_json::json!({"pattern": "exec"}),
        })
        .unwrap();
        assert_eq!(value["input"], serde_json::json!({"pattern": "exec"}));
    }

    #[test]
    fn replays_malformed_tool_input_as_an_empty_object() {
        // The API rejects a non-object `input`, which would fail every later
        // request in the task rather than just this call.
        let value = serialize_content_block(&ContentBlock::ToolUse {
            id: "toolu_1".into(),
            name: "grep".into(),
            input: Value::String(r#"{"pattern": "exec""#.into()),
        })
        .unwrap();
        assert_eq!(value["input"], serde_json::json!({}));
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
        let mut request = simple_request();
        request.model = "claude-opus-4-8".into();
        request.reasoning_effort = ReasoningEffort::High;
        let body = AnthropicMessages::serialize(&request);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
    }

    #[test]
    fn serialize_request_omits_thinking_when_off() {
        let body = AnthropicMessages::serialize(&simple_request());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn serialize_request_omits_thinking_for_legacy_model() {
        let mut request = simple_request();
        request.model = "claude-sonnet-4-20250514".into();
        request.reasoning_effort = ReasoningEffort::High;
        let body = AnthropicMessages::serialize(&request);
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
            reasoning_effort: Default::default(),
        }
    }

    #[test]
    fn the_timeout_builder_reaches_the_endpoint() {
        let provider = Anthropic::new("test-key");
        assert_eq!(provider.0.get_timeout(), DEFAULT_REQUEST_TIMEOUT);
        let provider = provider.timeout(Duration::from_secs(42));
        assert_eq!(provider.0.get_timeout(), Duration::from_secs(42));
    }

    #[test]
    fn serialize_request_sets_model_and_system() {
        let body = AnthropicMessages::serialize(&simple_request());
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["system"], "You are helpful.");
        assert_eq!(body["max_tokens"], 1024);
    }

    #[test]
    fn serialize_request_omits_max_tokens_when_none() {
        let mut request = simple_request();
        request.max_request_tokens = None;
        let body = AnthropicMessages::serialize(&request);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn serialize_request_excludes_system_from_messages() {
        let body = AnthropicMessages::serialize(&simple_request());
        for message in body["messages"].as_array().unwrap() {
            assert_ne!(message["role"], "system");
        }
    }

    fn invalid_request(message: &str) -> String {
        serde_json::json!({
            "error": { "type": "invalid_request_error", "message": message }
        })
        .to_string()
    }

    #[test]
    fn a_prompt_too_long_falls_through_to_the_shared_bank() {
        let body = invalid_request("prompt is too long: 205000 > 200000");
        assert!(AnthropicMessages::classify_error(400, &body).is_none());
        assert!(matches!(
            recover_wrapped_error(400, &body, None),
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
            AnthropicMessages::classify_error(400, &body),
            Some(ProviderError::ModelNotFound { .. })
        ));
    }

    #[test]
    fn an_unrelated_anthropic_400_falls_through() {
        let body = invalid_request("max_tokens must be a positive integer");
        assert!(AnthropicMessages::classify_error(400, &body).is_none());
    }

    #[test]
    fn an_anthropic_400_keeps_the_message_it_carried() {
        let body = serde_json::json!({
            "error": { "type": "not_found_error", "message": "model opus-9 not found" }
        })
        .to_string();
        let Some(ProviderError::ModelNotFound { message }) =
            AnthropicMessages::classify_error(400, &body)
        else {
            panic!("expected ModelNotFound");
        };
        assert_eq!(message, "model opus-9 not found");
    }
}
