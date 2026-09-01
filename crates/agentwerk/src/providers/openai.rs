//! OpenAI's Chat Completions API. The `mistral` and `litellm` providers send
//! the same shape to a different base URL.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::endpoint::Endpoint;
use super::error::{ProviderError, ProviderResult};
use super::frames;
use super::provider::{self, Protocol, ProviderLike};
use super::stream::ResponseBuilder;
use super::types::{
    ContentBlock, Message, ModelRequest, ModelResponse, ReasoningEffort, ResponseStatus,
    StreamEvent,
};
use crate::tools::Tool;

const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// LLM provider for the OpenAI Chat Completions API and any compatible
/// endpoint that accepts the same request shape.
///
/// Build it through [`Provider::from_env`] to read `OPENAI_API_KEY` and the optional `OPENAI_BASE_URL`. Override the endpoint with [`base_url`] and the per-request timeout with [`timeout`].
///
/// # Examples
///
/// Direct construction with an API key:
///
/// ```no_run
/// use agentwerk::providers::OpenAi;
///
/// let _provider = OpenAi::new("sk-...");
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
/// [`base_url`]: OpenAi::base_url
/// [`timeout`]: OpenAi::timeout
pub struct OpenAi(Endpoint);

impl OpenAi {
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
        Ok(Self::new(env_required("OPENAI_API_KEY")?)
            .base_url(env_or("OPENAI_BASE_URL", DEFAULT_BASE_URL)))
    }
}

impl ProviderLike for OpenAi {
    fn respond(
        &self,
        request: ModelRequest,
        on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
        provider::respond::<OpenAiChat>(&self.0, request, on_event)
    }
}

/// OpenAI's chat completions, which `Mistral` and `LiteLlm` speak too against
/// their own base URL.
pub(crate) struct OpenAiChat;

impl Protocol for OpenAiChat {
    const PATH: &'static str = "/v1/chat/completions";

    fn authenticate(posted: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
        posted.bearer_auth(api_key)
    }

    /// The codes only this vendor names. Its overflow wordings are read by
    /// `error::recover_wrapped_error`, which every body reaches: reading them
    /// here too would answer ahead of the rate-limit signal that outranks them.
    fn classify_error(status: u16, body: &str) -> Option<ProviderError> {
        if status != 400 {
            return None;
        }
        let json: Value = serde_json::from_str(body).ok()?;
        let error = &json["error"];
        let message = error["message"].as_str().unwrap_or("").to_string();
        match error["code"].as_str().unwrap_or("") {
            "model_not_found" => Some(ProviderError::ModelNotFound { message }),
            "content_filter" => Some(ProviderError::SafetyFilterTriggered { message }),
            _ => None,
        }
    }

    fn serialize(request: &ModelRequest) -> Value {
        let mut body = serde_json::json!({
            "model": request.model,
            "stream": true,
            "stream_options": {"include_usage": true},
            "messages": serialize_messages(request),
        });
        if let Some(n) = request.max_request_tokens {
            body["max_tokens"] = Value::from(n);
        }
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request.tools.iter().map(serialize_tool).collect();
            body["tools"] = Value::Array(tools);
        }
        // The OpenAI-standard reasoning knob; the litellm proxy maps it to the
        // backend's own thinking switch.
        if request.reasoning_effort != ReasoningEffort::Off {
            let effort = request.reasoning_effort.get_name();
            body["reasoning_effort"] = Value::from(effort);
        }
        body
    }

    fn decode(payload: &Value, reply: &mut ResponseBuilder) {
        if let Some(model) = payload["model"].as_str() {
            reply.set_model(model);
        }

        let choice = &payload["choices"][0];
        let delta = &choice["delta"];
        decode_reasoning(delta, reply);
        if let Some(text) = delta["content"].as_str() {
            reply.add_text(text);
        }
        decode_tool_calls(delta, reply);

        if let Some(reason) = choice["finish_reason"].as_str() {
            reply.set_status(status_from_finish_reason(reason));
        }
        if let Some(usage) = payload.get("usage").filter(|usage| !usage.is_null()) {
            reply.set_input_tokens(usage["prompt_tokens"].as_u64().unwrap_or(0));
            reply.set_output_tokens(usage["completion_tokens"].as_u64().unwrap_or(0));
        }
    }

    fn recover(reply: &mut ModelResponse, on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>) {
        frames::recover_framed_calls(reply, on_event);
    }
}

fn serialize_messages(request: &ModelRequest) -> Vec<Value> {
    let mut messages = Vec::new();

    if !request.system_prompt.is_empty() {
        messages.push(serde_json::json!({
            "role": "system",
            "content": request.system_prompt,
        }));
    }

    for message in &request.messages {
        match message {
            Message::System { content } => {
                messages.push(serde_json::json!({"role": "system", "content": content}));
            }
            Message::User { content } => messages.extend(serialize_user_blocks(content)),
            Message::Assistant { content } => messages.push(serialize_assistant_message(content)),
        }
    }

    messages
}

fn serialize_user_blocks(blocks: &[ContentBlock]) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut text_parts = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text { text } => text_parts.push(text.clone()),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": content,
                }));
            }
            _ => {}
        }
    }

    if !text_parts.is_empty() {
        messages.push(serde_json::json!({
            "role": "user",
            "content": text_parts.join("\n"),
        }));
    }

    messages
}

fn serialize_assistant_message(blocks: &[ContentBlock]) -> Value {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text { text } => text_parts.push(text.clone()),
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(serde_json::json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": input.to_string()},
                }));
            }
            // Reasoning is regenerated each turn on these endpoints, so it is
            // kept in the replies but not sent back in the assistant message.
            ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {}
            ContentBlock::ToolResult { .. } => {}
        }
    }

    let content = text_parts.join("\n");
    let mut message = serde_json::json!({
        "role": "assistant",
        "content": if content.is_empty() { Value::Null } else { Value::String(content) },
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    message
}

fn serialize_tool(tool: &Tool) -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.get_name(),
            "description": tool.get_description(),
            "parameters": tool.get_input_schema().get_raw_schema(),
        }
    })
}

/// Endpoints disagree on the field name: `reasoning_content` (deepseek, qwen,
/// llama.cpp) and `reasoning` (other OpenAI-compatible proxies) both appear, so
/// whichever is present wins.
fn decode_reasoning(delta: &Value, reply: &mut ResponseBuilder) {
    let fragment = ["reasoning_content", "reasoning"]
        .into_iter()
        .find_map(|field| delta[field].as_str().filter(|text| !text.is_empty()));
    if let Some(fragment) = fragment {
        reply.add_thinking(fragment);
    }
}

fn decode_tool_calls(delta: &Value, reply: &mut ResponseBuilder) {
    let Some(calls) = delta["tool_calls"].as_array() else {
        return;
    };
    for call in calls {
        let numbered = call["index"].as_u64().map(|index| index as usize);
        let function = &call["function"];
        let key = reply.open_tool_call(
            numbered,
            call["id"].as_str().unwrap_or(""),
            function["name"].as_str().unwrap_or(""),
        );
        reply.add_arguments(key, function["arguments"].as_str().unwrap_or(""));
    }
}

fn status_from_finish_reason(raw: &str) -> ResponseStatus {
    match raw {
        "stop" => ResponseStatus::EndTurn,
        "tool_calls" => ResponseStatus::ToolUse,
        "length" => ResponseStatus::OutputTruncated,
        "content_filter" => ResponseStatus::Refused,
        _ => ResponseStatus::EndTurn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::error::recover_wrapped_error;
    use crate::providers::ReasoningEffort;

    /// Feed `chunks` through the decoder, returning the blocks they assemble.
    fn decode_blocks(chunks: &[Value]) -> Vec<ContentBlock> {
        decode(chunks).content
    }

    fn decode(chunks: &[Value]) -> ModelResponse {
        let sink: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(|_| {});
        let mut reply = ResponseBuilder::new(&sink);
        for chunk in chunks {
            OpenAiChat::decode(chunk, &mut reply);
        }
        reply
            .into_response()
            .expect("a reply that did not overflow")
    }

    #[test]
    fn parses_reasoning_content_into_thinking_before_text() {
        let content = decode_blocks(&[
            serde_json::json!({"choices":[{"delta":{"reasoning_content":"let me think"}}]}),
            serde_json::json!({"choices":[{"delta":{"content":"the answer"}}]}),
        ]);
        // The signature is empty: these endpoints stream no replay token, and
        // the reasoning is not echoed back on the next turn.
        assert!(matches!(
            &content[..],
            [ContentBlock::Thinking { thinking, signature }, ContentBlock::Text { text }]
                if thinking == "let me think" && signature.is_empty() && text == "the answer"
        ));
    }

    #[test]
    fn parses_reasoning_field_alias() {
        let content =
            decode_blocks(&[serde_json::json!({"choices":[{"delta":{"reasoning":"alt field"}}]})]);
        assert!(matches!(
            &content[..],
            [ContentBlock::Thinking { thinking, .. }] if thinking == "alt field"
        ));
    }

    #[test]
    fn an_empty_reasoning_field_falls_through_to_the_other_name() {
        let content = decode_blocks(&[serde_json::json!({"choices":[{"delta":{
            "reasoning_content": "", "reasoning": "weighing it up",
        }}]})]);
        assert!(matches!(
            &content[..],
            [ContentBlock::Thinking { thinking, .. }] if thinking == "weighing it up"
        ));
    }

    #[test]
    fn reasoning_returning_after_text_opens_a_second_thinking_block() {
        // The blocks record the order the chunks arrived in, so a proxy that
        // interleaves the two streams is reported as it answered.
        let content = decode_blocks(&[
            serde_json::json!({"choices":[{"delta":{"reasoning_content":"weighing it"}}]}),
            serde_json::json!({"choices":[{"delta":{"content":"first"}}]}),
            serde_json::json!({"choices":[{"delta":{"reasoning_content":"reconsidering"}}]}),
            serde_json::json!({"choices":[{"delta":{"content":"second"}}]}),
        ]);
        assert!(matches!(
            &content[..],
            [
                ContentBlock::Thinking { .. },
                ContentBlock::Text { .. },
                ContentBlock::Thinking { .. },
                ContentBlock::Text { .. },
            ]
        ));
    }

    #[test]
    fn parses_tool_call_arguments_from_chunk_fragments() {
        let content = decode_blocks(&[
            serde_json::json!({"choices":[{"delta":{"tool_calls":[
                {"index": 0, "id": "call_1", "function": {"name": "grep", "arguments": r#"{"pattern":"#}}
            ]}}]}),
            serde_json::json!({"choices":[{"delta":{"tool_calls":[
                {"index": 0, "function": {"arguments": r#" "exec"}"#}}
            ]}}]}),
        ]);
        assert!(matches!(
            &content[..],
            [ContentBlock::ToolUse { id, name, input }]
                if id == "call_1" && name == "grep"
                    && *input == serde_json::json!({"pattern": "exec"})
        ));
    }

    #[test]
    fn tool_calls_the_chunks_index_apart_keep_their_own_arguments() {
        let content = decode_blocks(&[
            serde_json::json!({"choices":[{"delta":{"tool_calls":[
                {"index": 0, "id": "call_1", "function": {"name": "grep", "arguments": r#"{"pattern": "a"}"#}}
            ]}}]}),
            serde_json::json!({"choices":[{"delta":{"tool_calls":[
                {"index": 1, "id": "call_2", "function": {"name": "grep", "arguments": r#"{"pattern": "b"}"#}}
            ]}}]}),
        ]);
        assert!(matches!(
            &content[..],
            [
                ContentBlock::ToolUse { input: first, .. },
                ContentBlock::ToolUse { input: second, .. },
            ] if *first == serde_json::json!({"pattern": "a"})
                && *second == serde_json::json!({"pattern": "b"})
        ));
    }

    #[test]
    fn a_high_tool_call_index_adds_only_the_one_call() {
        // The endpoint supplies the index, so it selects the call without sizing anything.
        let content = decode_blocks(&[serde_json::json!({"choices":[{"delta":{"tool_calls":[
            {"index": 100000, "id": "call_1", "function": {"name": "grep", "arguments": "{}"}}
        ]}}]})]);
        assert_eq!(content.len(), 1);
    }

    #[test]
    fn index_less_fragments_with_new_ids_stay_separate_tool_calls() {
        let content = decode_blocks(&[
            serde_json::json!({"choices":[{"delta":{"tool_calls":[
                {"id": "call_1", "function": {"name": "grep", "arguments": r#"{"pattern": "a"}"#}}
            ]}}]}),
            serde_json::json!({"choices":[{"delta":{"tool_calls":[
                {"id": "call_2", "function": {"name": "grep", "arguments": r#"{"pattern": "b"}"#}}
            ]}}]}),
        ]);
        assert!(matches!(
            &content[..],
            [
                ContentBlock::ToolUse { input: first, .. },
                ContentBlock::ToolUse { input: second, .. },
            ] if *first == serde_json::json!({"pattern": "a"})
                && *second == serde_json::json!({"pattern": "b"})
        ));
    }

    #[test]
    fn index_less_fragments_without_an_id_continue_the_tool_call_in_flight() {
        let content = decode_blocks(&[
            serde_json::json!({"choices":[{"delta":{"tool_calls":[
                {"id": "call_1", "function": {"name": "grep", "arguments": r#"{"pattern":"#}}
            ]}}]}),
            serde_json::json!({"choices":[{"delta":{"tool_calls":[
                {"function": {"arguments": r#" "a"}"#}}
            ]}}]}),
        ]);
        assert!(matches!(
            &content[..],
            [ContentBlock::ToolUse { input, .. }]
                if *input == serde_json::json!({"pattern": "a"})
        ));
    }

    #[test]
    fn a_chunk_reports_the_finish_reason_and_the_usage() {
        let response = decode(&[serde_json::json!({
            "model": "gpt-5",
            "choices": [{"delta": {}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 120, "completion_tokens": 34},
        })]);
        assert_eq!(response.model, "gpt-5");
        assert_eq!(response.status, ResponseStatus::ToolUse);
        assert_eq!(response.usage.input_tokens, 120);
        assert_eq!(response.usage.output_tokens, 34);
    }

    #[test]
    fn serialize_request_sets_reasoning_effort() {
        let mut request = dummy_request();
        request.reasoning_effort = ReasoningEffort::High;
        assert_eq!(OpenAiChat::serialize(&request)["reasoning_effort"], "high");
    }

    #[test]
    fn serialize_request_omits_reasoning_effort_when_off() {
        assert!(OpenAiChat::serialize(&dummy_request())
            .get("reasoning_effort")
            .is_none());
    }

    #[test]
    fn assistant_message_drops_thinking() {
        let message = serialize_assistant_message(&[
            ContentBlock::Thinking {
                thinking: "hidden".into(),
                signature: String::new(),
            },
            ContentBlock::Text {
                text: "shown".into(),
            },
        ]);
        assert_eq!(message["content"], "shown");
        assert!(message.get("reasoning_content").is_none());
    }

    fn dummy_request() -> ModelRequest {
        ModelRequest {
            model: "test-model".into(),
            system_prompt: "You are helpful.".into(),
            messages: vec![Message::User {
                content: vec![ContentBlock::Text {
                    text: "Hello".into(),
                }],
            }],
            tools: vec![],
            max_request_tokens: Some(1024),
            reasoning_effort: Default::default(),
        }
    }

    #[test]
    fn serialize_system_prompt_as_message() {
        let body = OpenAiChat::serialize(&dummy_request());
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
    }

    #[test]
    fn serialize_basic_structure() {
        let body = OpenAiChat::serialize(&dummy_request());
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["max_tokens"], 1024);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn serialize_request_omits_max_tokens_when_none() {
        let mut request = dummy_request();
        request.max_request_tokens = None;
        let body = OpenAiChat::serialize(&request);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn serialize_tools() {
        let request = ModelRequest {
            model: "test".into(),
            system_prompt: String::new(),
            messages: vec![],
            tools: vec![Tool::new("get_weather")
                .description("Get weather")
                .schema(
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                )
                .handler(|_: Value| async { crate::Event::success("") })],
            max_request_tokens: Some(1024),
            reasoning_effort: Default::default(),
        };
        let body = OpenAiChat::serialize(&request);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }

    fn body_400(code: &str, message: &str) -> String {
        serde_json::json!({
            "error": {
                "type": "invalid_request_error",
                "code": code,
                "message": message,
            }
        })
        .to_string()
    }

    #[test]
    fn an_overflow_code_falls_through_to_the_shared_bank() {
        let body = body_400(
            "context_length_exceeded",
            "This model's maximum context length is 128000 tokens.",
        );
        assert!(OpenAiChat::classify_error(400, &body).is_none());
        assert!(matches!(
            recover_wrapped_error(400, &body, None),
            Some(ProviderError::ContextWindowExceeded { .. })
        ));
    }

    #[test]
    fn an_overflow_wording_without_a_code_falls_through_to_the_shared_bank() {
        // Mistral / LiteLLM path: `code` absent, so the message text is all
        // there is to read.
        let body = serde_json::json!({
            "error": {
                "type": "invalid_request_error",
                "message":
                    "This model's maximum context length is 32000 tokens, however you requested 40000.",
            }
        })
        .to_string();
        assert!(OpenAiChat::classify_error(400, &body).is_none());
        assert!(matches!(
            recover_wrapped_error(400, &body, None),
            Some(ProviderError::ContextWindowExceeded { .. })
        ));
    }

    #[test]
    fn maps_400_content_filter_to_safety_filter_triggered() {
        let body = body_400("content_filter", "request blocked by policy");
        assert!(matches!(
            OpenAiChat::classify_error(400, &body),
            Some(ProviderError::SafetyFilterTriggered { .. })
        ));
    }

    #[test]
    fn maps_400_model_not_found_to_model_not_found() {
        let body = body_400("model_not_found", "the model `gpt-x` does not exist");
        assert!(matches!(
            OpenAiChat::classify_error(400, &body),
            Some(ProviderError::ModelNotFound { .. })
        ));
    }

    #[test]
    fn an_unrelated_openai_400_falls_through() {
        let body = body_400("invalid_api_key", "incorrect API key provided");
        assert!(OpenAiChat::classify_error(400, &body).is_none());
    }

    #[test]
    fn an_openai_400_keeps_the_message_it_carried() {
        let body = body_400("model_not_found", "the model `gpt-x` does not exist");
        let Some(ProviderError::ModelNotFound { message }) = OpenAiChat::classify_error(400, &body)
        else {
            panic!("expected ModelNotFound");
        };
        assert_eq!(message, "the model `gpt-x` does not exist");
    }
}
