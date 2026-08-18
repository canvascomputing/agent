//! Every value agentwerk and an LLM provider exchange, in the order a turn
//! happens: the request and its parts, the messages and content blocks both
//! sides use, then the reply, its token usage, its stop reason, and the pieces
//! of it as they arrive.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::tools::Tool;

/// How much reasoning to ask the model for.
///
/// Each LLM provider has its own field for it. `Off` sends none, leaving the
/// model's own default. This shapes only the request: whatever reasoning comes
/// back is always kept as a `Thinking` [`ContentBlock`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningEffort {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    /// The value sent with the request, `"low"`, `"medium"`, or `"high"`, or
    /// `None` when off. Every supported LLM provider takes these same words.
    pub(crate) fn label(self) -> Option<&'static str> {
        match self {
            ReasoningEffort::Off => None,
            ReasoningEffort::Low => Some("low"),
            ReasoningEffort::Medium => Some("medium"),
            ReasoningEffort::High => Some("high"),
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label().unwrap_or("off"))
    }
}

/// One request to an LLM provider, assembled from the agent's configuration and
/// the conversation so far.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    /// Which model to ask, such as `claude-sonnet-4-20250514`.
    pub model: String,
    /// The system prompt, assembled from the role, the behavior, and the
    /// facts of the moment.
    pub system_prompt: String,
    /// Everything said so far, ending with the latest input.
    pub messages: Vec<Message>,
    /// The tools the model may call this turn, each carrying the name,
    /// description, and schema to send. Read the schema document with
    /// [`Schema::get_raw_schema`](crate::schemas::Schema::get_raw_schema).
    pub tools: Vec<Tool>,
    /// Limit on this request's output tokens, or `None` for the LLM provider's
    /// own default.
    pub max_request_tokens: Option<u32>,
    /// How much reasoning to ask for, taken from the [`Model`](super::Model).
    pub reasoning_effort: ReasoningEffort,
}

/// One message in the conversation passed to a provider, tagged by role.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    /// System-role message: the prompt frame the provider sees first.
    #[serde(rename = "system")]
    System { content: String },
    /// User-role message: input from the caller or tool results.
    #[serde(rename = "user")]
    User { content: Vec<ContentBlock> },
    /// Assistant-role message: model output.
    #[serde(rename = "assistant")]
    Assistant { content: Vec<ContentBlock> },
}

impl Message {
    /// User-role message wrapping one text block.
    pub fn user(text: impl Into<String>) -> Self {
        Self::User {
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// System-role message carrying `text` verbatim.
    pub fn system(text: impl Into<String>) -> Self {
        Self::System {
            content: text.into(),
        }
    }

    /// Assistant-role message wrapping one text block.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::Assistant {
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

/// Render this value as a user-role `Message`. Implemented by anything
/// that becomes one turn's input, such as `Ticket`, whose task agentwerk sends
/// on the first turn.
pub trait AsUserMessage {
    fn as_user_message(&self) -> Message;
}

/// Content block carried inside a [`Message`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// Plain text block.
    #[serde(rename = "text")]
    Text { text: String },
    /// Tool invocation the model requested.
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Outcome of a tool invocation sent back to the model.
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default = "default_true")]
        succeeded: bool,
    },
    /// Extended-thinking the model produced before its answer. `signature`
    /// is the provider's opaque replay token, echoed back on the next turn so
    /// the provider will accept the block; empty for the OpenAI-compatible
    /// endpoints, which carry no such token and regenerate reasoning instead.
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    /// Extended-thinking the provider returned encrypted, when Anthropic's
    /// safety systems redact the reasoning. `data` is opaque and echoed back
    /// unchanged to replay the turn.
    #[serde(rename = "redacted_thinking")]
    RedactedThinking { data: String },
}

fn default_true() -> bool {
    true
}

/// What the LLM API reported about why token generation ended.
///
/// Provider-agnostic superset of stop/finish reasons across LLM APIs.
///
/// References:
/// - <https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons>
/// - <https://github.com/BerriAI/litellm/issues/21348>
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    /// Model finished generating naturally.
    /// Anthropic: `end_turn` | OpenAI: `stop` | Mistral: `stop`
    #[default]
    EndTurn,

    /// Model hit a caller-provided stop sequence.
    /// Anthropic: `stop_sequence` | OpenAI: `stop` (with stop param)
    StopSequence,

    /// Model emitted tool_use blocks and expects execution.
    /// Anthropic: `tool_use` | OpenAI: `tool_calls` | Mistral: `tool_calls`
    ToolUse,

    /// Output was truncated by the max_tokens limit.
    /// Anthropic: `max_tokens` | OpenAI: `length` | Mistral: `length`
    OutputTruncated,

    /// Model refused to respond due to safety policy.
    /// Anthropic: `refusal` | OpenAI: `content_filter`
    Refused,

    /// Server-side tool loop hit its iteration limit.
    /// Anthropic: `pause_turn`
    PauseTurn,
}

/// Token counts the provider reported for one response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input tokens consumed by the request.
    pub input_tokens: u64,
    /// Output tokens generated by the response.
    pub output_tokens: u64,
}

impl std::ops::AddAssign<&TokenUsage> for TokenUsage {
    fn add_assign(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
    }
}

/// One assembled response from a provider: content blocks the model
/// produced, why generation stopped, token counts, and the model name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    /// Content blocks the model produced this turn.
    pub content: Vec<ContentBlock>,
    /// Why generation stopped (natural finish, tool call, truncated, ...).
    pub status: ResponseStatus,
    /// Token counts the provider reported for this response.
    pub usage: TokenUsage,
    /// Model name the provider used to generate this response.
    pub model: String,
}

/// Why a tool call a model wrote as text was not promoted to a real call,
/// carried by `EventKind::ToolCallDeclined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ToolDeclineKind {
    /// The reply hit a length limit, so the block may be cut short.
    #[serde(rename = "output_truncated")]
    OutputTruncated,
    /// The model refused, paused, or hit a stop sequence, so a whole-looking
    /// block may be one it never committed to.
    #[serde(rename = "not_finished")]
    ReplyNotFinished,
    /// The endpoint already delivered a call under that name, so the block was
    /// left alone rather than run a second time.
    #[serde(rename = "already_delivered")]
    AlreadyDelivered,
}

impl ToolDeclineKind {
    /// The stable snake_case spelling, the one `Event.data["reason"]` carries.
    pub fn name(&self) -> &'static str {
        match self {
            ToolDeclineKind::OutputTruncated => "output_truncated",
            ToolDeclineKind::ReplyNotFinished => "not_finished",
            ToolDeclineKind::AlreadyDelivered => "already_delivered",
        }
    }
}

impl std::fmt::Display for ToolDeclineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Something worth reporting while a reply is still arriving, in the order it
/// happened.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Text the model just produced, to be shown as it arrives.
    TextDelta { text: String },

    /// A tool call the endpoint did not deliver usably, written as text or
    /// delivered without its arguments, was rebuilt from the text the model
    /// wrote; it will run.
    ToolCallRepaired { tool_name: String },

    /// A framed tool call was found in the reply and declined, with the
    /// reason it was not promoted.
    ToolCallDeclined {
        tool_name: String,
        reason: ToolDeclineKind,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_serde_round_trip() {
        let msg = Message::User {
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        match deserialized {
            Message::User { content } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentBlock::Text { text } => assert_eq!(text, "hello"),
                    other => panic!("Expected Text, got {other:?}"),
                }
            }
            other => panic!("Expected User, got {other:?}"),
        }
    }

    #[test]
    fn tool_use_block_serde() {
        let block = ContentBlock::ToolUse {
            id: "call_123".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "/tmp/test.txt"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_use\""));

        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        match deserialized {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_123");
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "/tmp/test.txt");
            }
            other => panic!("Expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_succeeded_defaults_true() {
        let json = r#"{"type":"tool_result","tool_use_id":"id1","content":"ok"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::ToolResult { succeeded, .. } => assert!(succeeded),
            other => panic!("Expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn usage_add_accumulates() {
        let mut usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
        };
        let other = TokenUsage {
            input_tokens: 200,
            output_tokens: 100,
        };
        usage += &other;
        assert_eq!(usage.input_tokens, 300);
        assert_eq!(usage.output_tokens, 150);
    }
}
