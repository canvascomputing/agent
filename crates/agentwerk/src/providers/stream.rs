//! An SSE body in, one assembled `ModelResponse` out.
//!
//! A vendor decodes its own payloads and names which [`ResponseBuilder`] call
//! each one is. Everything else is decided here, so no two vendors can disagree
//! about which block a fragment continues or when a [`StreamEvent`] fires.

use std::sync::Arc;

use serde_json::Value;

use super::error::{ProviderError, ProviderResult};
use super::types::{ContentBlock, ModelResponse, ResponseStatus, StreamEvent, TokenUsage};

/// Read one reply to its end through the vendor's `decode`.
pub(crate) async fn read_reply(
    response: reqwest::Response,
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
    mut decode: impl FnMut(&Value, &mut ResponseBuilder),
) -> ProviderResult<ModelResponse> {
    let mut reply = ResponseBuilder::new(on_event);
    read_stream(response, |payload| decode(payload, &mut reply)).await?;
    reply.into_response()
}

/// Read an SSE response to its end, handing every `data:` JSON payload to `ingest`.
pub(crate) async fn read_stream(
    mut response: reqwest::Response,
    mut ingest: impl FnMut(&Value),
) -> ProviderResult<()> {
    let mut lines = LineBuffer::new();
    while let Some(chunk) =
        response
            .chunk()
            .await
            .map_err(|error| ProviderError::StreamInterrupted {
                message: error.to_string(),
            })?
    {
        for payload in lines.push(&chunk) {
            ingest(&payload);
        }
    }
    Ok(())
}

/// Holds bytes rather than text: a chunk may end partway through a multi-byte
/// character.
struct LineBuffer {
    buffer: Vec<u8>,
}

impl LineBuffer {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<Value> {
        self.buffer.extend_from_slice(chunk);
        let Some(last_newline) = self.buffer.iter().rposition(|&byte| byte == b'\n') else {
            return Vec::new();
        };
        let payloads = self.buffer[..last_newline]
            .split(|&byte| byte == b'\n')
            .filter_map(read_data_line)
            .collect();
        self.buffer.drain(..=last_newline);
        payloads
    }
}

/// The space after the colon is optional, and an endpoint that omits it would
/// otherwise stream a reply that parses to nothing at all. The `[DONE]` sentinel
/// says only that the stream is over, which the response ending says too.
fn read_data_line(line: &[u8]) -> Option<Value> {
    let line = String::from_utf8_lossy(line);
    let data = line.trim().strip_prefix("data:")?.trim_start();
    if data == "[DONE]" {
        return None;
    }
    serde_json::from_str(data).ok()
}

/// Which tool call a fragment continues. An endpoint that numbers its calls
/// supplies one; for the rest the builder assigns one, and the two can never
/// collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolCallKey {
    Numbered(usize),
    Unnumbered(usize),
}

pub(crate) struct ResponseBuilder {
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    model: String,
    status: ResponseStatus,
    overflowed: bool,
    usage: TokenUsage,
    blocks: Vec<Block>,
}

impl ResponseBuilder {
    pub(crate) fn new(on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>) -> Self {
        Self {
            on_event: Arc::clone(on_event),
            model: String::new(),
            status: ResponseStatus::default(),
            overflowed: false,
            usage: TokenUsage::default(),
            blocks: Vec::new(),
        }
    }

    pub(crate) fn set_model(&mut self, name: &str) {
        self.model = name.to_string();
    }

    pub(crate) fn set_status(&mut self, status: ResponseStatus) {
        self.status = status;
    }

    /// The reply is then a failed request rather than an answer, so
    /// [`into_response`](Self::into_response) gives back the error to summarize on.
    pub(crate) fn set_context_window_exceeded(&mut self) {
        self.overflowed = true;
    }

    /// Set separately from the output tokens, since an endpoint may report the
    /// two in different payloads.
    pub(crate) fn set_input_tokens(&mut self, tokens: u64) {
        self.usage.input_tokens = tokens;
    }

    pub(crate) fn set_output_tokens(&mut self, tokens: u64) {
        self.usage.output_tokens = tokens;
    }

    pub(crate) fn add_text(&mut self, fragment: &str) {
        if fragment.is_empty() {
            return;
        }
        match self.blocks.last_mut() {
            Some(Block::Text(text)) => text.push_str(fragment),
            _ => self.blocks.push(Block::Text(fragment.to_string())),
        }
        self.emit(StreamEvent::TextDelta {
            text: fragment.to_string(),
        });
    }

    pub(crate) fn add_thinking(&mut self, fragment: &str) {
        if fragment.is_empty() {
            return;
        }
        if let Block::Thinking { thinking, .. } = self.thinking_block() {
            thinking.push_str(fragment);
        }
    }

    pub(crate) fn add_signature(&mut self, fragment: &str) {
        if fragment.is_empty() {
            return;
        }
        if let Block::Thinking { signature, .. } = self.thinking_block() {
            signature.push_str(fragment);
        }
    }

    /// The reasoning block a fragment continues, opening one when the last block
    /// is something else.
    fn thinking_block(&mut self) -> &mut Block {
        if !matches!(self.blocks.last(), Some(Block::Thinking { .. })) {
            self.blocks.push(Block::Thinking {
                thinking: String::new(),
                signature: String::new(),
            });
        }
        self.blocks
            .last_mut()
            .expect("a block was just pushed if none matched")
    }

    /// Redacted reasoning arrives whole rather than in fragments, so it fills a
    /// block of its own in one call.
    pub(crate) fn add_redacted_thinking(&mut self, data: &str) {
        self.blocks.push(Block::RedactedThinking {
            data: data.to_string(),
        });
    }

    /// Gives back the key later fragments reach the call by. Pass the number the
    /// endpoint gave it, or `None` when it numbers none.
    pub(crate) fn open_tool_call(
        &mut self,
        numbered: Option<usize>,
        id: &str,
        name: &str,
    ) -> ToolCallKey {
        let key = numbered.map_or_else(|| self.key_for(id), ToolCallKey::Numbered);
        let position = self.tool_call_at(key);
        if let Block::ToolCall {
            id: held_id,
            name: held_name,
            ..
        } = &mut self.blocks[position]
        {
            if !id.is_empty() {
                *held_id = id.to_string();
            }
            if !name.is_empty() {
                *held_name = name.to_string();
            }
        }
        key
    }

    /// Append what `fragment` adds beyond what the tool call already holds. Some
    /// endpoints re-send the whole payload after streaming a prefix of it, and
    /// appending would leave `prefix + full`, parsing as neither.
    pub(crate) fn add_arguments(&mut self, key: ToolCallKey, fragment: &str) {
        let position = self.tool_call_at(key);
        if let Block::ToolCall { arguments, .. } = &mut self.blocks[position] {
            let unseen = fragment
                .strip_prefix(arguments.as_str())
                .unwrap_or(fragment);
            arguments.push_str(unseen);
        }
    }

    pub(crate) fn into_response(self) -> ProviderResult<ModelResponse> {
        if self.overflowed {
            return Err(ProviderError::ContextWindowExceeded {
                message: "the model reported the request exceeds its context window".into(),
            });
        }
        Ok(ModelResponse {
            content: self.blocks.into_iter().map(Block::into_content).collect(),
            status: self.status,
            usage: self.usage,
            model: model_or_unknown(self.model),
        })
    }

    /// The call an endpoint that numbers none of them means: the one already
    /// holding `id`, the one most recently opened when it names no id at all,
    /// or a call of its own.
    fn key_for(&self, id: &str) -> ToolCallKey {
        if id.is_empty() {
            let in_flight = self.blocks.iter().rev().find_map(Block::tool_call_key);
            return in_flight.unwrap_or(ToolCallKey::Unnumbered(0));
        }
        let held = self
            .blocks
            .iter()
            .find(|block| block.holds_tool_call_id(id))
            .and_then(Block::tool_call_key);
        held.unwrap_or_else(|| ToolCallKey::Unnumbered(self.tool_call_count()))
    }

    /// Where the call `key` names sits, opening one at the end when the stream
    /// has not mentioned it yet.
    fn tool_call_at(&mut self, key: ToolCallKey) -> usize {
        let held = self
            .blocks
            .iter()
            .position(|block| block.tool_call_key() == Some(key));
        held.unwrap_or_else(|| {
            self.blocks.push(Block::ToolCall {
                key,
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
            self.blocks.len() - 1
        })
    }

    fn tool_call_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| block.tool_call_key().is_some())
            .count()
    }

    fn emit(&self, event: StreamEvent) {
        (self.on_event)(event);
    }
}

/// One content block under construction, in the order the stream reached it.
enum Block {
    Text(String),
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolCall {
        key: ToolCallKey,
        id: String,
        name: String,
        arguments: String,
    },
}

impl Block {
    fn tool_call_key(&self) -> Option<ToolCallKey> {
        match self {
            Block::ToolCall { key, .. } => Some(*key),
            _ => None,
        }
    }

    fn holds_tool_call_id(&self, wanted: &str) -> bool {
        matches!(self, Block::ToolCall { id, .. } if id == wanted)
    }

    fn into_content(self) -> ContentBlock {
        match self {
            Block::Text(text) => ContentBlock::Text { text },
            Block::Thinking {
                thinking,
                signature,
            } => ContentBlock::Thinking {
                thinking,
                signature,
            },
            Block::RedactedThinking { data } => ContentBlock::RedactedThinking { data },
            Block::ToolCall {
                id,
                name,
                arguments,
                ..
            } => ContentBlock::ToolUse {
                id,
                name,
                input: read_arguments(arguments),
            },
        }
    }
}

/// A non-empty but unparseable string is kept verbatim, so the schema check
/// reports the real problem rather than a fabricated `{}`.
pub(crate) fn read_arguments(arguments: String) -> Value {
    if arguments.trim().is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(&arguments).unwrap_or(Value::String(arguments))
}

/// The model name a stream reported, or `"unknown"` for an endpoint that never
/// named one.
fn model_or_unknown(model: String) -> String {
    if model.is_empty() {
        "unknown".into()
    } else {
        model
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    #[test]
    fn a_data_line_yields_its_json_payload() {
        let mut lines = LineBuffer::new();
        let payloads = lines.push(b"data: {\"type\":\"ping\"}\n\n");
        assert_eq!(payloads, [json!({"type": "ping"})]);
    }

    #[test]
    fn a_data_line_without_a_space_yields_its_json_payload() {
        let mut lines = LineBuffer::new();
        let payloads = lines.push(b"data:{\"type\":\"ping\"}\n\n");
        assert_eq!(payloads, [json!({"type": "ping"})]);
    }

    #[test]
    fn the_done_sentinel_yields_no_payload() {
        let mut lines = LineBuffer::new();
        assert!(lines.push(b"data: [DONE]\n\n").is_empty());
    }

    #[test]
    fn non_data_lines_are_ignored() {
        let mut lines = LineBuffer::new();
        assert!(lines
            .push(b"event: message_start\n: comment\n\n")
            .is_empty());
    }

    #[test]
    fn a_line_split_across_chunks_is_reassembled() {
        let mut lines = LineBuffer::new();

        assert!(lines.push(b"data: {\"type\":\"pi").is_empty());

        let payloads = lines.push(b"ng\"}\n\n");
        assert_eq!(payloads, [json!({"type": "ping"})]);
    }

    #[test]
    fn a_character_split_across_chunks_is_reassembled() {
        let mut lines = LineBuffer::new();
        let line = "data: {\"text\":\"é\"}\n".as_bytes();
        let mid_character = line.len() - 4;

        assert!(lines.push(&line[..mid_character]).is_empty());

        let payloads = lines.push(&line[mid_character..]);
        assert_eq!(payloads, [json!({"text": "é"})]);
    }

    #[test]
    fn one_chunk_can_hold_many_payloads() {
        let mut lines = LineBuffer::new();
        let chunk = b"data: {\"a\":1}\n\ndata: {\"a\":2}\n\ndata: [DONE]\n\n";
        assert_eq!(lines.push(chunk), [json!({"a": 1}), json!({"a": 2})]);
    }

    #[test]
    fn malformed_json_is_skipped() {
        let mut lines = LineBuffer::new();
        let payloads = lines.push(b"data: not-json\ndata: {\"ok\":true}\n\n");
        assert_eq!(payloads, [json!({"ok": true})]);
    }

    /// A builder and the events it emits, so a test can read both.
    fn builder() -> (ResponseBuilder, Arc<Mutex<Vec<StreamEvent>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let collected = Arc::clone(&seen);
        let sink: Arc<dyn Fn(StreamEvent) + Send + Sync> =
            Arc::new(move |event| collected.lock().unwrap().push(event));
        (ResponseBuilder::new(&sink), seen)
    }

    /// Every text fragment the builder reported, in order.
    fn text_deltas(events: &[StreamEvent]) -> String {
        events
            .iter()
            .map(|event| match event {
                StreamEvent::TextDelta { text } => text.as_str(),
                _ => "",
            })
            .collect()
    }

    fn content(reply: ResponseBuilder) -> Vec<ContentBlock> {
        reply
            .into_response()
            .expect("a reply that did not overflow")
            .content
    }

    /// Verbatim from a cortecs.ai reply: a streamed prefix, then the whole
    /// payload again in the next chunk.
    const RESENT_PREFIX: &str = r#"{"pattern": "__import__""#;
    const RESENT_FULL: &str =
        r#"{"pattern": "__import__", "output_mode": "content", "type": "py"}"#;

    #[test]
    fn arguments_parse_as_a_json_object() {
        let (mut reply, _) = builder();
        let call = reply.open_tool_call(Some(0), "call_1", "grep");
        reply.add_arguments(call, r#"{"pattern":"foo"}"#);
        assert!(matches!(
            &content(reply)[..],
            [ContentBlock::ToolUse { name, input, .. }]
                if name == "grep" && *input == serde_json::json!({"pattern": "foo"})
        ));
    }

    #[test]
    fn a_tool_call_without_argument_fragments_takes_no_arguments() {
        let (mut reply, _) = builder();
        reply.open_tool_call(Some(0), "call_1", "grep");
        assert!(matches!(
            &content(reply)[..],
            [ContentBlock::ToolUse { input, .. }] if *input == serde_json::json!({})
        ));
    }

    #[test]
    fn malformed_arguments_stay_verbatim() {
        // Blanking to `{}` would drop the call and fabricate the problem reported.
        let raw = r#"{"pattern": "exec""#;
        let (mut reply, _) = builder();
        let call = reply.open_tool_call(Some(0), "call_1", "grep");
        reply.add_arguments(call, raw);
        assert!(matches!(
            &content(reply)[..],
            [ContentBlock::ToolUse { input, .. }] if *input == Value::String(raw.into())
        ));
    }

    #[test]
    fn argument_fragments_accumulate_across_deltas() {
        let (mut reply, _) = builder();
        let call = reply.open_tool_call(Some(0), "call_1", "grep");
        reply.add_arguments(call, r#"{"pattern":"#);
        reply.add_arguments(call, r#" "exec"}"#);
        assert!(matches!(
            &content(reply)[..],
            [ContentBlock::ToolUse { input, .. }]
                if *input == serde_json::json!({"pattern": "exec"})
        ));
    }

    #[test]
    fn a_resent_argument_payload_replaces_the_streamed_prefix() {
        let (mut reply, _) = builder();
        let call = reply.open_tool_call(Some(0), "call_1", "grep");
        reply.add_arguments(call, RESENT_PREFIX);
        reply.add_arguments(call, RESENT_FULL);
        assert!(matches!(
            &content(reply)[..],
            [ContentBlock::ToolUse { input, .. }]
                if *input == serde_json::json!({
                    "pattern": "__import__",
                    "output_mode": "content",
                    "type": "py",
                })
        ));
    }

    #[test]
    fn tool_calls_the_endpoint_numbered_keep_their_own_arguments() {
        let (mut reply, _) = builder();
        let first = reply.open_tool_call(Some(0), "call_1", "grep");
        reply.add_arguments(first, r#"{"a": 1}"#);
        let second = reply.open_tool_call(Some(1), "call_2", "grep");
        reply.add_arguments(second, r#"{"a": 2}"#);
        assert!(matches!(
            &content(reply)[..],
            [
                ContentBlock::ToolUse { input: first, .. },
                ContentBlock::ToolUse { input: second, .. },
            ] if *first == serde_json::json!({"a": 1}) && *second == serde_json::json!({"a": 2})
        ));
    }

    #[test]
    fn a_high_number_from_the_endpoint_adds_only_the_one_call() {
        // The endpoint supplies the number, so it selects the call without sizing anything.
        let (mut reply, _) = builder();
        reply.open_tool_call(Some(100_000), "call_1", "grep");
        assert_eq!(content(reply).len(), 1);
    }

    #[test]
    fn an_unnumbered_fragment_with_a_new_id_opens_its_own_tool_call() {
        let (mut reply, _) = builder();
        reply.open_tool_call(None, "call_1", "grep");
        reply.open_tool_call(None, "call_2", "grep");
        assert_eq!(content(reply).len(), 2);
    }

    #[test]
    fn an_unnumbered_fragment_without_an_id_continues_the_tool_call_in_flight() {
        let (mut reply, _) = builder();
        let opened = reply.open_tool_call(None, "call_1", "grep");
        assert_eq!(reply.open_tool_call(None, "", ""), opened);
    }

    #[test]
    fn an_unnumbered_call_never_lands_on_a_numbered_one() {
        let (mut reply, _) = builder();
        reply.open_tool_call(Some(0), "call_1", "grep");
        reply.open_tool_call(None, "call_2", "grep");
        assert_eq!(content(reply).len(), 2);
    }

    #[test]
    fn text_fragments_join_into_one_block() {
        let (mut reply, events) = builder();
        reply.add_text("an ");
        reply.add_text("answer");
        assert!(matches!(
            &content(reply)[..],
            [ContentBlock::Text { text }] if text == "an answer"
        ));
        assert_eq!(text_deltas(&events.lock().unwrap()), "an answer");
    }

    #[test]
    fn reasoning_and_text_alternating_keep_the_order_they_arrived_in() {
        let (mut reply, _) = builder();
        reply.add_thinking("weighing it");
        reply.add_text("first");
        reply.add_thinking("reconsidering");
        reply.add_text("second");
        assert!(matches!(
            &content(reply)[..],
            [
                ContentBlock::Thinking { .. },
                ContentBlock::Text { .. },
                ContentBlock::Thinking { .. },
                ContentBlock::Text { .. },
            ]
        ));
    }

    #[test]
    fn a_signature_lands_on_the_reasoning_it_signs() {
        let (mut reply, _) = builder();
        reply.add_thinking("weighing it");
        reply.add_signature("sig-1");
        assert!(matches!(
            &content(reply)[..],
            [ContentBlock::Thinking { thinking, signature }]
                if thinking == "weighing it" && signature == "sig-1"
        ));
    }

    #[test]
    fn an_empty_text_fragment_opens_no_block() {
        let (mut reply, events) = builder();
        reply.add_text("");
        assert!(content(reply).is_empty());
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn text_after_a_tool_call_opens_its_own_block() {
        // Appending to the call would lose the arguments the endpoint streamed.
        let (mut reply, _) = builder();
        reply.open_tool_call(Some(0), "call_1", "grep");
        reply.add_text("and an answer");
        assert!(matches!(
            &content(reply)[..],
            [ContentBlock::ToolUse { name, .. }, ContentBlock::Text { text }]
                if name == "grep" && text == "and an answer"
        ));
    }

    #[test]
    fn a_reply_whose_endpoint_named_no_model_reports_unknown() {
        let (reply, _) = builder();
        assert_eq!(reply.into_response().unwrap().model, "unknown");
    }

    #[test]
    fn the_input_and_output_tokens_are_reported_independently() {
        // Anthropic names the input tokens as the message opens and the output
        // tokens as it ends, so one must not blank the other.
        let (mut reply, _) = builder();
        reply.set_input_tokens(100);
        reply.set_output_tokens(50);
        let usage = reply.into_response().unwrap().usage;
        assert_eq!((usage.input_tokens, usage.output_tokens), (100, 50));
    }

    #[test]
    fn an_overflowed_reply_becomes_the_context_window_error() {
        let (mut reply, _) = builder();
        reply.add_text("a partial answer");
        reply.set_context_window_exceeded();
        assert!(matches!(
            reply.into_response(),
            Err(ProviderError::ContextWindowExceeded { .. })
        ));
    }
}
