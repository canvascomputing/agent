//! An HTTP response in, a `ModelResponse` out.
//!
//! A vendor decodes its own payloads and names which [`ResponseBuilder`] call
//! each one is. Everything else about turning a stream into a reply is decided
//! here, so no two vendors can disagree about it: which block a fragment
//! continues, when a [`StreamEvent`] fires, and what is recovered from text a
//! machine did not intend as data.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use super::error::{ProviderError, ProviderResult};
use super::types::{
    ContentBlock, ModelResponse, ResponseStatus, StreamEvent, TokenUsage, ToolDeclineKind,
};

// ---------- reading one reply ----------

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

// ---------- reading an SSE body ----------

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

// ---------- building the reply ----------

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

// ---------- recovering the calls a model wrote as text ----------
//
// These models are trained to write a call as
// `<tool_call><function=NAME><parameter=KEY>VALUE</parameter>…</function></tool_call>`
// for the endpoint to convert, and do not do it consistently: in one run the
// same model emits some calls properly and writes others as text. One left as
// text costs the turn, since the loop answers "no tool call", or the tool
// answers "missing required parameter" for one delivered empty.

const TOOL_CALL_OPEN: &str = "<tool_call>";
const TOOL_CALL_CLOSE: &str = "</tool_call>";
const FUNCTION_OPEN: &str = "<function=";
const FUNCTION_CLOSE: &str = "</function>";
const PARAMETER_OPEN: &str = "<parameter=";
const PARAMETER_CLOSE: &str = "</parameter>";

/// Nothing is invented: a call runs only if the reply ended of the model's own
/// accord. A name no tool answers to still runs, so the registry's error names
/// the tools that exist rather than the call vanishing into the reply text.
pub(crate) fn recover_framed_calls(
    response: &mut ModelResponse,
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    let framed = find_framed_calls(response);
    if framed.is_empty() {
        return;
    }
    // Read before the syntax leaves the reply: a batch nothing runs keeps its
    // text, which is then all that is left of it.
    if let Some(reason) = decline_reason(&response.status) {
        for call in &framed {
            report_declined(call, reason, on_event);
        }
        return;
    }
    strip_framed_syntax(response);
    apply_framed_calls(response, &framed, on_event);
}

/// Read every framed call the reply carries, leaving the reply as it is.
fn find_framed_calls(response: &ModelResponse) -> Vec<FramedCall> {
    let mut framed = Vec::new();
    for block in &response.content {
        // Thinking is read but never stripped: the endpoint takes the block
        // back verbatim.
        let text = match block {
            ContentBlock::Text { text } => text,
            ContentBlock::Thinking { thinking, .. } => thinking,
            _ => continue,
        };
        framed.extend(split_framed_calls(text).1);
    }
    framed
}

fn strip_framed_syntax(response: &mut ModelResponse) {
    for block in &mut response.content {
        if let ContentBlock::Text { text } = block {
            *text = split_framed_calls(text).0;
        }
    }
    response
        .content
        .retain(|block| !matches!(block, ContentBlock::Text { text } if text.is_empty()));
}

fn report_declined(
    call: &FramedCall,
    reason: ToolDeclineKind,
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    on_event(StreamEvent::ToolCallDeclined {
        tool_name: call.name.clone(),
        reason,
    });
}

fn decline_reason(status: &ResponseStatus) -> Option<ToolDeclineKind> {
    // Any terminal but the model's own can end a reply just past a complete
    // `</function>`, leaving whole-looking syntax it never committed to.
    match status {
        ResponseStatus::EndTurn | ResponseStatus::ToolUse => None,
        ResponseStatus::OutputTruncated => Some(ToolDeclineKind::OutputTruncated),
        ResponseStatus::StopSequence | ResponseStatus::Refused | ResponseStatus::PauseTurn => {
            Some(ToolDeclineKind::ReplyNotFinished)
        }
    }
}

/// Each framed call either fills the call the endpoint delivered empty at the
/// same position among its same-named siblings, or is added under a name the
/// endpoint delivered nothing for. Matching a delivered name by position only is
/// what keeps a call the endpoint delivered from running twice.
fn apply_framed_calls(
    response: &mut ModelResponse,
    framed: &[FramedCall],
    on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    // Read before anything is added, so one added call cannot hide a same-named
    // sibling behind it.
    let delivered: HashSet<String> = response
        .content
        .iter()
        .filter_map(tool_call_name)
        .map(str::to_string)
        .collect();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut added = Vec::new();

    for (offset, call) in framed.iter().enumerate() {
        let position = seen.entry(call.name.as_str()).or_default();
        let at = *position;
        *position += 1;

        let Some(input) = nth_delivered_input(response, &call.name, at) else {
            if delivered.contains(call.name.as_str()) {
                // Delivered under this name already, just not this many times:
                // what the model wrote here has nowhere left to go.
                report_declined(call, ToolDeclineKind::AlreadyDelivered, on_event);
            } else {
                added.push(ContentBlock::ToolUse {
                    id: format!("repaired_{offset}"),
                    name: call.name.clone(),
                    input: arguments_as_object(call),
                });
                report_repaired(call, on_event);
            }
            continue;
        };

        if input.as_object().is_none_or(|fields| fields.is_empty()) {
            *input = arguments_as_object(call);
            report_repaired(call, on_event);
        } else if !same_call(call, input) {
            report_declined(call, ToolDeclineKind::AlreadyDelivered, on_event);
        }
        // A delivered call whose arguments already match is the call the model
        // wrote twice: nothing to repair, and nothing lost.
    }

    if !added.is_empty() {
        response.content.extend(added);
        // Without the flip the reply reads as an answer and nothing added runs.
        response.status = ResponseStatus::ToolUse;
    }
}

fn tool_call_name(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

fn nth_delivered_input<'a>(
    response: &'a mut ModelResponse,
    name: &str,
    at: usize,
) -> Option<&'a mut Value> {
    response
        .content
        .iter_mut()
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                name: delivered,
                input,
                ..
            } if delivered == name => Some(input),
            _ => None,
        })
        .nth(at)
}

fn report_repaired(call: &FramedCall, on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>) {
    on_event(StreamEvent::ToolCallRepaired {
        tool_name: call.name.clone(),
    });
}

/// Every value stays the text the model typed: the registry retypes a call's
/// arguments against the schema the tool advertises, reading the unions,
/// nested properties, and enums a second engine here never would.
fn arguments_as_object(call: &FramedCall) -> Value {
    let fields = call
        .arguments
        .iter()
        .map(|(name, text)| (name.clone(), Value::String(text.clone())));
    Value::Object(Map::from_iter(fields))
}

/// The delivered call, written a second time as text. A framed value is always
/// text, so a delivered one is compared by what it reads as: `offset=100` and
/// the number 100 are one call, not two.
fn same_call(framed: &FramedCall, delivered: &Value) -> bool {
    let Some(fields) = delivered.as_object() else {
        return false;
    };
    fields.len() == framed.arguments.len()
        && framed.arguments.iter().all(|(name, text)| {
            fields.get(name).is_some_and(|value| match value {
                Value::String(delivered) => delivered == text,
                other => serde_json::from_str::<Value>(text).is_ok_and(|read| read == *other),
            })
        })
}

/// A tool call the model wrote as text instead of emitting it; every value is
/// the text the model typed.
struct FramedCall {
    name: String,
    arguments: Vec<(String, String)>,
}

/// The prose and the framed calls in one pass, so what is removed from the reply
/// and what runs can never disagree. The `<tool_call>` frame is required: it is
/// the model's own syntax for making a call, which is what separates one from a
/// call it merely wrote about.
fn split_framed_calls(text: &str) -> (String, Vec<FramedCall>) {
    // Almost no reply carries one, so the common path costs a search rather
    // than a copy of the whole text.
    if !text.contains(TOOL_CALL_OPEN) {
        return (text.to_string(), Vec::new());
    }
    let mut prose = String::with_capacity(text.len());
    let mut calls = Vec::new();
    let mut rest = text;
    while let Some((before, framed)) = rest.split_once(TOOL_CALL_OPEN) {
        let Some((body, remainder)) = framed.split_once(TOOL_CALL_CLOSE) else {
            break;
        };
        prose.push_str(before);
        match read_function_block(body) {
            Some(call) => calls.push(call),
            None => {
                prose.push_str(TOOL_CALL_OPEN);
                prose.push_str(body);
                prose.push_str(TOOL_CALL_CLOSE);
            }
        }
        rest = remainder;
    }
    prose.push_str(rest);
    (prose.trim().to_string(), calls)
}

fn read_function_block(body: &str) -> Option<FramedCall> {
    let (_, after_open) = body.split_once(FUNCTION_OPEN)?;
    let (name, inner) = after_open.split_once('>')?;
    let (parameters, _) = inner.split_once(FUNCTION_CLOSE)?;
    if !is_tool_name(name) {
        return None;
    }
    Some(FramedCall {
        name: name.to_string(),
        arguments: read_parameters(parameters),
    })
}

fn read_parameters(body: &str) -> Vec<(String, String)> {
    let mut arguments = Vec::new();
    let mut rest = body;
    while let Some((_, after_open)) = rest.split_once(PARAMETER_OPEN) {
        let Some((key, after_key)) = after_open.split_once('>') else {
            break;
        };
        let Some((value, remainder)) = after_key.split_once(PARAMETER_CLOSE) else {
            break;
        };
        rest = remainder;
        if !key.is_empty() {
            // The newlines around a value are the format, not what was typed.
            arguments.push((key.to_string(), value.trim().to_string()));
        }
    }
    arguments
}

fn is_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// ---------- recovering an error a proxy wrapped ----------
//
// Errors arrive wrapped: a proxy returns an HTTP 400 whose body carries another
// provider's error verbatim, with its own status and code on the outside. The
// vendor classifier sees `code = "400"` and gives up, and the wrapped signal is
// then lost: the loop treats the response as terminal instead of compacting.
// This runs after the vendor returns None and before the generic fallback.

/// Matched case-insensitively against the full body, so they survive arbitrary
/// JSON wrapping.
const OVERFLOW_PATTERNS: &[&str] = &[
    // Anthropic
    "prompt is too long",
    "request_too_large",
    // OpenAI
    "this model's maximum context length",
    "exceeds the context window",
    "context_length_exceeded",
    // Mistral
    "too large for model with",
    // Vertex / Gemini, observed through LiteLLM-passthrough: its 1 M-token limit
    // phrases overflow this way and nothing else does.
    "input token count",
    "exceeds the maximum number of tokens",
    // LiteLLM prepends `litellm.ContextWindowExceededError:` before forwarding,
    // catching an upstream overflow whose own wording none of the above knows.
    "contextwindowexceedederror",
    // Deliberately broad, for upstreams not enumerated above: a false positive
    // costs one compaction attempt, a false negative costs the ticket.
    "context window exceeded",
    "maximum context length",
];

/// Throttling signals, several of which also match [`OVERFLOW_PATTERNS`].
const RATE_LIMIT_PATTERNS: &[&str] = &[
    "rate limit",
    "too many requests",
    "throttling",
    // Lowercased, a class name like `litellm.RateLimitError` has no space, so
    // the prose form above misses it.
    "ratelimit",
    // Google Vertex's RPC status for quota exhaustion.
    "resource_exhausted",
];

/// A rate-limited body classifies as such whatever the outer HTTP status:
/// LiteLLM occasionally wraps a 429 inside a different outer code, so the status
/// alone cannot be trusted to mean "this was a rate limit". `None` falls through
/// to `fallback_http_error`, preserving the handling of unrecognised errors.
pub(crate) fn recover_wrapped_error(
    status: u16,
    body: &str,
    retry_delay: Option<Duration>,
) -> Option<ProviderError> {
    // One lowercase per call: a wrapped body carries the full upstream payload.
    let lower = body.to_lowercase();

    // Rate-limit exclusion runs FIRST. A body that says both "throttling" and
    // "maximum tokens" is a rate limit, not an overflow: the inverse would burn
    // a compaction round-trip and still hit the same throttle on the next request.
    let looks_like_rate_limit = RATE_LIMIT_PATTERNS.iter().any(|p| lower.contains(p));

    if !looks_like_rate_limit && OVERFLOW_PATTERNS.iter().any(|p| lower.contains(p)) {
        return Some(ProviderError::ContextWindowExceeded {
            message: body.to_string(),
        });
    }

    if looks_like_rate_limit {
        return Some(ProviderError::RateLimited {
            status,
            message: body.to_string(),
            retry_delay,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::providers::TokenUsage;

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

    fn is_tool_use(block: &ContentBlock) -> bool {
        matches!(block, ContentBlock::ToolUse { .. })
    }

    /// Verbatim from a cortecs.ai reply: the model wrote the call as text
    /// instead of emitting it.
    const FRAMED_GREP: &str = "<tool_call>\n<function=grep>\n<parameter=-n>\ntrue\n</parameter>\n<parameter=output_mode>\ncontent\n</parameter>\n<parameter=pattern>\nplugin.Open(...)\n</parameter>\n</function>\n</tool_call>";

    /// Verbatim from a cortecs.ai reply: the model wrote the call into its
    /// reasoning instead of emitting it.
    const FRAMED_READ: &str = "<tool_call>\n<function=read_file>\n<parameter=path>\n/Users/mav/dev/lambda/README.md\n</parameter>\n</function>\n</tool_call>";

    /// `FRAMED_READ` naming another file, for the tests that need two blocks.
    fn framed_read(path: &str) -> String {
        FRAMED_READ.replace("/Users/mav/dev/lambda/README.md", path)
    }

    fn thinking(text: &str) -> ContentBlock {
        ContentBlock::Thinking {
            thinking: text.into(),
            signature: String::new(),
        }
    }

    fn recover(
        content: Vec<ContentBlock>,
        status: ResponseStatus,
    ) -> (Vec<ContentBlock>, Vec<StreamEvent>) {
        let (response, events) = recover_response(content, status);
        (response.content, events)
    }

    /// The same, for the tests that read the reply's status.
    fn recover_response(
        content: Vec<ContentBlock>,
        status: ResponseStatus,
    ) -> (ModelResponse, Vec<StreamEvent>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let collected = Arc::clone(&seen);
        let sink: Arc<dyn Fn(StreamEvent) + Send + Sync> =
            Arc::new(move |event| collected.lock().unwrap().push(event));
        let mut response = ModelResponse {
            content,
            status,
            usage: TokenUsage::default(),
            model: "test".into(),
        };
        recover_framed_calls(&mut response, &sink);
        let events = seen.lock().unwrap().clone();
        (response, events)
    }

    /// The calls `text` carried, for the tests that do not read the prose.
    fn framed_in(text: &str) -> Vec<FramedCall> {
        split_framed_calls(text).1
    }

    #[test]
    fn framed_block_parses_into_a_call() {
        let calls = framed_in(FRAMED_GREP);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "grep");
        assert_eq!(
            calls[0].arguments,
            vec![
                ("-n".to_string(), "true".to_string()),
                ("output_mode".to_string(), "content".to_string()),
                ("pattern".to_string(), "plugin.Open(...)".to_string()),
            ]
        );
    }

    #[test]
    fn bare_function_block_is_not_a_call() {
        // Without the frame the same text is a call the model wrote about, not
        // one it emitted, so promoting it would run what it only considered.
        let bare = FRAMED_GREP
            .replace(TOOL_CALL_OPEN, "")
            .replace(TOOL_CALL_CLOSE, "");
        assert!(framed_in(&bare).is_empty());
    }

    #[test]
    fn a_block_cut_off_before_its_close_stays_in_the_prose() {
        // Half a call is not a call, and the text is then all that is left of it.
        let truncated = "<tool_call>\n<function=read_file>\n<parameter=path>\n/Users/m";
        let (prose, calls) = split_framed_calls(truncated);
        assert!(calls.is_empty());
        assert_eq!(prose, truncated);
    }

    #[test]
    fn a_complete_block_parses_alongside_a_truncated_one() {
        let text = format!("{FRAMED_GREP}\n<tool_call>\n<function=grep>\n<parameter=pattern>\nFunction(...)\n</parameter>");
        assert_eq!(framed_in(&text).len(), 1);
    }

    #[test]
    fn a_framed_block_leaves_the_prose_around_it() {
        let text = format!("Let me just run 5 searches:\n{FRAMED_GREP}\nDone.");
        assert_eq!(
            split_framed_calls(&text).0,
            "Let me just run 5 searches:\n\nDone."
        );
    }

    #[test]
    fn a_name_that_is_not_an_identifier_is_not_a_call() {
        // Removing it too would lose what the model wrote, with no call to
        // show for it.
        let spaced = FRAMED_READ.replace("read_file", "read file");
        let (prose, calls) = split_framed_calls(&spaced);
        assert!(calls.is_empty());
        assert_eq!(prose, spaced);
    }

    #[test]
    fn framed_call_promotes_when_the_reply_carries_none() {
        let (content, events) = recover(vec![thinking(FRAMED_READ)], ResponseStatus::EndTurn);
        assert!(matches!(
            content.last(),
            Some(ContentBlock::ToolUse { name, input, .. })
                if name == "read_file"
                    && *input == serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"})
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name }] if tool_name == "read_file"
        ));
    }

    #[test]
    fn two_framed_calls_are_promoted_under_distinct_ids() {
        let both = format!(
            "{}\n{}",
            framed_read("/first.md"),
            framed_read("/second.md")
        );
        let (content, events) = recover(vec![thinking(&both)], ResponseStatus::EndTurn);
        let ids: Vec<&str> = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["repaired_0", "repaired_1"]);
        assert!(matches!(
            &events[..],
            [
                StreamEvent::ToolCallRepaired { tool_name: first },
                StreamEvent::ToolCallRepaired { tool_name: second },
            ] if first == "read_file" && second == "read_file"
        ));
    }

    #[test]
    fn promoting_turns_the_reply_into_a_tool_use() {
        let (response, _) = recover_response(vec![thinking(FRAMED_READ)], ResponseStatus::EndTurn);
        assert_eq!(response.status, ResponseStatus::ToolUse);
    }

    #[test]
    fn a_framed_call_beside_a_delivered_one_is_still_promoted() {
        // The endpoint delivered `read_file` and converted nothing for `grep`,
        // whose block is the only record of it.
        let (content, events) = recover(
            vec![
                thinking(FRAMED_GREP),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/a.md"}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert!(matches!(
            content.last(),
            Some(ContentBlock::ToolUse { id, name, .. })
                if id == "repaired_0" && name == "grep"
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name }] if tool_name == "grep"
        ));
    }

    #[test]
    fn a_framed_call_the_endpoint_answered_differently_is_declined() {
        // The endpoint delivered `read_file` for one path while the model wrote
        // a block for another. Nothing runs the block, and the prose no longer
        // shows it, so the event is all that is left of it.
        let (content, events) = recover(
            vec![
                ContentBlock::Text {
                    text: framed_read("/wanted.md"),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/delivered.md"}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert_eq!(content.iter().filter(|b| is_tool_use(b)).count(), 1);
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { tool_name, reason }]
                if tool_name == "read_file" && *reason == ToolDeclineKind::AlreadyDelivered
        ));
    }

    #[test]
    fn a_call_the_endpoint_delivered_is_never_promoted_beside_itself() {
        // The model wrote the call it also emitted. Promoting would run it twice.
        let (content, events) = recover(
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert_eq!(content.iter().filter(|b| is_tool_use(b)).count(), 1);
        assert!(events.is_empty());
    }

    #[test]
    fn a_call_delivered_empty_is_filled_from_its_framed_block() {
        let (content, events) = recover(
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert!(matches!(
            &content[1],
            ContentBlock::ToolUse { input, .. }
                if *input == serde_json::json!({"path": "/Users/mav/dev/lambda/README.md"})
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name }] if tool_name == "read_file"
        ));
    }

    #[test]
    fn each_empty_call_is_filled_from_its_own_framed_block() {
        // Its own block is the one at its position among its same-named siblings.
        let both = format!(
            "{}\n{}",
            framed_read("/first.md"),
            framed_read("/second.md")
        );
        let (content, _) = recover(
            vec![
                thinking(&both),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/first.md"}),
                },
                ContentBlock::ToolUse {
                    id: "call_2".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert!(matches!(
            &content[2],
            ContentBlock::ToolUse { input, .. } if *input == serde_json::json!({"path": "/second.md"})
        ));
    }

    #[test]
    fn a_second_framed_call_with_nowhere_to_go_is_declined() {
        // Two blocks written for one delivered call: the first fills it, and
        // the second has no call of its own left to reach.
        let both = format!(
            "{}\n{}",
            framed_read("/first.md"),
            framed_read("/second.md")
        );
        let (content, events) = recover(
            vec![
                thinking(&both),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert_eq!(content.iter().filter(|block| is_tool_use(block)).count(), 1);
        assert!(matches!(
            &events[..],
            [
                StreamEvent::ToolCallRepaired { .. },
                StreamEvent::ToolCallDeclined { reason, .. },
            ] if *reason == ToolDeclineKind::AlreadyDelivered
        ));
    }

    #[test]
    fn a_reply_carrying_no_framed_call_keeps_its_own_whitespace() {
        // Every reply on this path is scanned, so one with nothing to repair
        // must come back exactly as the model wrote it.
        let written = "  an answer, indented on purpose  ";
        let (content, events) = recover(
            vec![ContentBlock::Text {
                text: written.into(),
            }],
            ResponseStatus::EndTurn,
        );
        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }] if text == written
        ));
        assert!(events.is_empty());
    }

    #[test]
    fn a_delivered_call_with_arguments_is_left_alone() {
        // Overwriting one would run the endpoint's call with the framed block's
        // arguments; adding one would run the same call twice.
        let delivered = serde_json::json!({"path": "/other.md"});
        let (content, events) = recover(
            vec![
                thinking(FRAMED_READ),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: delivered.clone(),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert_eq!(content.len(), 2);
        assert!(matches!(&content[1], ContentBlock::ToolUse { input, .. } if *input == delivered));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { reason, .. }]
                if *reason == ToolDeclineKind::AlreadyDelivered
        ));
    }

    #[test]
    fn a_delivered_call_the_model_also_framed_is_one_call() {
        // A framed value is text and a delivered one is typed, so comparing
        // them as they stand would report the same call as two.
        let framed = "<tool_call>\n<function=read_file>\n<parameter=offset>\n100\n</parameter>\n</function>\n</tool_call>";
        let (_, events) = recover(
            vec![
                thinking(framed),
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"offset": 100}),
                },
            ],
            ResponseStatus::ToolUse,
        );
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_truncated_reply_declines_instead_of_promoting() {
        let (content, events) =
            recover(vec![thinking(FRAMED_READ)], ResponseStatus::OutputTruncated);
        assert!(!content.iter().any(is_tool_use));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { reason, .. }]
                if *reason == ToolDeclineKind::OutputTruncated
        ));
    }

    #[test]
    fn a_refused_reply_declines_as_not_finished() {
        // Apart from a truncated one: nothing was cut off, the model stopped
        // before committing to the call it had already written.
        let (content, events) = recover(vec![thinking(FRAMED_READ)], ResponseStatus::Refused);
        assert!(!content.iter().any(is_tool_use));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallDeclined { reason, .. }]
                if *reason == ToolDeclineKind::ReplyNotFinished
        ));
    }

    #[test]
    fn a_promoted_call_carries_the_text_the_model_wrote() {
        // Retyping them is the registry's, against the schema of the tool that
        // will run.
        let (content, events) = recover(vec![thinking(FRAMED_GREP)], ResponseStatus::EndTurn);
        assert!(matches!(
            content.last(),
            Some(ContentBlock::ToolUse { name, input, .. })
                if name == "grep"
                    && *input == serde_json::json!({
                        "-n": "true", "output_mode": "content", "pattern": "plugin.Open(...)"
                    })
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name }] if tool_name == "grep"
        ));
    }

    #[test]
    fn a_declined_call_keeps_the_text_the_model_wrote() {
        // Nothing ran, so nothing is taken away: stripping the syntax off a
        // call that will never run would leave no record of it anywhere.
        let written = format!("Reading it now.\n{FRAMED_READ}");
        let (content, _) = recover(
            vec![ContentBlock::Text {
                text: written.clone(),
            }],
            ResponseStatus::OutputTruncated,
        );
        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }] if *text == written
        ));
    }

    #[test]
    fn a_call_written_in_the_reply_runs_and_leaves_its_prose() {
        // The commonest shape: the model wrote the call where it writes to be
        // read, so the call has to run and its syntax has to go.
        let (content, events) = recover(
            vec![ContentBlock::Text {
                text: format!("Reading it now.\n{FRAMED_READ}"),
            }],
            ResponseStatus::EndTurn,
        );
        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }, ContentBlock::ToolUse { name, .. }]
                if text == "Reading it now." && name == "read_file"
        ));
        assert!(matches!(
            &events[..],
            [StreamEvent::ToolCallRepaired { tool_name }] if tool_name == "read_file"
        ));
    }

    #[test]
    fn a_reply_without_framed_calls_is_untouched() {
        let answer = "Just an answer.";
        let (content, events) = recover(
            vec![ContentBlock::Text {
                text: answer.into(),
            }],
            ResponseStatus::EndTurn,
        );
        assert!(matches!(
            &content[..],
            [ContentBlock::Text { text }] if text == answer
        ));
        assert!(events.is_empty());
    }

    /// The exact body that triggered the original failure: LiteLLM wrapping a
    /// Vertex/Gemini 400 INVALID_ARGUMENT with a `code = "400"` outer field. The
    /// OpenAI vendor classifier returns None on this.
    #[test]
    fn litellm_vertex_overflow_classifies_as_context_window() {
        let body = r#"{"error":{"message":"litellm.ContextWindowExceededError: litellm.BadRequestError: ContextWindowExceededError: Vertex_ai_betaException - b'{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"The input token count exceeds the maximum number of tokens allowed 1048576.\",\n    \"status\": \"INVALID_ARGUMENT\"\n  }\n}\n'","type":null,"param":null,"code":"400"}}"#;
        assert!(matches!(
            recover_wrapped_error(400, body, None),
            Some(ProviderError::ContextWindowExceeded { .. })
        ));
    }

    /// LiteLLM wrapping a Vertex RESOURCE_EXHAUSTED behind its own
    /// `MidStreamFallbackError`. The outer status IS 429 so the fallback path
    /// would handle it, but a wrapped 429 returned with any other outer status
    /// must classify correctly too, and the `retry_delay` must propagate.
    #[test]
    fn litellm_rate_limit_wrap_classifies_as_rate_limited() {
        let body = r#"{"error":{"message":"litellm.MidStreamFallbackError: litellm.RateLimitError: litellm.RateLimitError: vertex_ai_betaException - b'{\n  \"error\": {\n    \"code\": 429,\n    \"message\": \"Resource exhausted. Please try again later.\",\n    \"status\": \"RESOURCE_EXHAUSTED\"\n  }\n}\n'. Received Model Group=gemini-3-flash-preview","type":null,"param":null,"code":"429"}}"#;
        let delay = Some(Duration::from_secs(2));
        match recover_wrapped_error(429, body, delay) {
            Some(ProviderError::RateLimited {
                status,
                retry_delay,
                ..
            }) => {
                assert_eq!(status, 429);
                assert_eq!(retry_delay, delay);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn throttling_with_overflow_wording_classifies_as_rate_limited() {
        // Compacting would not help and the next request would hit the same throttle.
        let body = r#"{"message":"Throttling error: maximum context length per minute reached"}"#;
        match recover_wrapped_error(400, body, None) {
            Some(ProviderError::RateLimited { .. }) => {}
            other => panic!("expected RateLimited (throttling wins), got {other:?}"),
        }
    }

    /// First-party Anthropic phrasing, which its own classifier normally
    /// catches: the shared bank is the backstop if that one ever misses a variant.
    #[test]
    fn anthropic_prompt_too_long_classifies_as_context_window() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#;
        assert!(matches!(
            recover_wrapped_error(400, body, None),
            Some(ProviderError::ContextWindowExceeded { .. })
        ));
    }

    #[test]
    fn unrelated_400_returns_none() {
        // An auth error has nothing in common with overflow or rate-limit
        // wording, so `fallback_http_error` handles the classification.
        let body = r#"{"error":{"message":"invalid_api_key: incorrect API key provided","code":"invalid_api_key"}}"#;
        assert!(recover_wrapped_error(400, body, None).is_none());
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
        // The endpoint supplies the number, so it routes without sizing anything.
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
