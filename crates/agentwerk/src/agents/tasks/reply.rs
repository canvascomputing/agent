//! One task's replies and how they become
//! provider [`Message`] values. [`ReplyContent`] mirrors
//! [`ContentBlock`] so the task surface stays free of provider types.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::providers::{ContentBlock, Message};

use super::now_millis;

/// Who wrote a reply. The agent loop writes `System`
/// entries for the system prompt and for compaction boundaries; those
/// are filtered when projecting replies back into `Message` values for
/// the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    System,
    User,
    Assistant,
}

/// One entry in a task's replies.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Reply {
    pub(crate) author: Author,
    pub(crate) content: Vec<ReplyContent>,
    /// Millis since epoch.
    pub(crate) created_at: u64,
}

/// Task-side mirror of [`ContentBlock`]. Keeps the public task
/// surface free of provider types while still recording every payload
/// shape the agent loop sends. Carries the same tags as `ContentBlock`
/// so both serialize alike; `ToolResult::path` is the one task-side
/// field the provider block has no counterpart for.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplyContent {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        succeeded: bool,
        /// Absolute path of the offloaded full payload when the inline
        /// `content` carries only a preview. `None` when the full output
        /// fit inline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
}

impl Reply {
    /// Build a reply from its author and content blocks.
    pub fn new(author: Author, content: Vec<ReplyContent>) -> Self {
        Self {
            author,
            content,
            created_at: now_millis(),
        }
    }

    /// Who wrote this reply.
    pub fn get_author(&self) -> Author {
        self.author
    }

    /// The blocks carried by this reply.
    pub fn get_content(&self) -> &[ReplyContent] {
        &self.content
    }

    /// The blocks carried by this reply, for reply editors.
    pub fn get_content_mut(&mut self) -> &mut [ReplyContent] {
        &mut self.content
    }

    /// Milliseconds since the epoch.
    pub fn get_created_at(&self) -> u64 {
        self.created_at
    }

    /// Build a user reply from the provider blocks the loop sent.
    /// `paths` maps `tool_use_id → absolute path` for tool results whose
    /// full output was offloaded to disk; empty when nothing was offloaded.
    pub(crate) fn user(blocks: &[ContentBlock], paths: &HashMap<String, PathBuf>) -> Self {
        Self {
            author: Author::User,
            content: blocks
                .iter()
                .map(|b| ReplyContent::from_block(b, paths))
                .collect(),
            created_at: now_millis(),
        }
    }

    /// Build a user reply carrying a single text payload.
    pub fn user_text(text: impl Into<String>) -> Self {
        Self::new(Author::User, vec![ReplyContent::Text { text: text.into() }])
    }

    /// Build an assistant reply from the model's response content.
    /// Assistant content never carries tool-result blocks, so no paths
    /// map is needed.
    pub(crate) fn assistant(blocks: &[ContentBlock]) -> Self {
        let empty = HashMap::new();
        Self {
            author: Author::Assistant,
            content: blocks
                .iter()
                .map(|b| ReplyContent::from_block(b, &empty))
                .collect(),
            created_at: now_millis(),
        }
    }

    /// Build a system reply carrying a single text payload. Used
    /// for the leading system-prompt entry and compaction boundaries.
    pub(crate) fn system_text(text: impl Into<String>) -> Self {
        Self {
            author: Author::System,
            content: vec![ReplyContent::Text { text: text.into() }],
            created_at: now_millis(),
        }
    }

    /// Project this reply back into a provider [`Message`]. Returns
    /// `None` for `System` entries: the system prompt is passed via
    /// `request.system_prompt`, and compaction-boundary replies are
    /// audit markers only.
    pub(crate) fn as_message(&self) -> Option<Message> {
        let content = self.content.iter().map(ReplyContent::to_block).collect();
        match self.author {
            Author::User => Some(Message::User { content }),
            Author::Assistant => Some(Message::Assistant { content }),
            Author::System => None,
        }
    }
}

impl ReplyContent {
    /// The stable snake_case tag for this block.
    pub fn get_kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::ToolUse { .. } => "tool_use",
            Self::ToolResult { .. } => "tool_result",
            Self::Thinking { .. } => "thinking",
            Self::RedactedThinking { .. } => "redacted_thinking",
        }
    }

    pub fn get_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }

    pub fn get_id(&self) -> Option<&str> {
        match self {
            Self::ToolUse { id, .. } => Some(id),
            _ => None,
        }
    }

    pub fn get_name(&self) -> Option<&str> {
        match self {
            Self::ToolUse { name, .. } => Some(name),
            _ => None,
        }
    }

    pub fn get_input(&self) -> Option<&serde_json::Value> {
        match self {
            Self::ToolUse { input, .. } => Some(input),
            _ => None,
        }
    }

    pub fn get_tool_use_id(&self) -> Option<&str> {
        match self {
            Self::ToolResult { tool_use_id, .. } => Some(tool_use_id),
            _ => None,
        }
    }

    pub fn get_content(&self) -> Option<&str> {
        match self {
            Self::ToolResult { content, .. } => Some(content),
            _ => None,
        }
    }

    pub fn get_succeeded(&self) -> Option<bool> {
        match self {
            Self::ToolResult { succeeded, .. } => Some(*succeeded),
            _ => None,
        }
    }

    pub fn get_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::ToolResult { path, .. } => path.as_deref(),
            _ => None,
        }
    }

    pub fn get_thinking(&self) -> Option<&str> {
        match self {
            Self::Thinking { thinking, .. } => Some(thinking),
            _ => None,
        }
    }

    pub fn get_signature(&self) -> Option<&str> {
        match self {
            Self::Thinking { signature, .. } => Some(signature),
            _ => None,
        }
    }

    pub fn get_data(&self) -> Option<&str> {
        match self {
            Self::RedactedThinking { data } => Some(data),
            _ => None,
        }
    }

    fn from_block(b: &ContentBlock, paths: &HashMap<String, PathBuf>) -> Self {
        match b {
            ContentBlock::Text { text } => ReplyContent::Text { text: text.clone() },
            ContentBlock::ToolUse { id, name, input } => ReplyContent::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                succeeded,
            } => ReplyContent::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                succeeded: *succeeded,
                path: paths.get(tool_use_id).cloned(),
            },
            ContentBlock::Thinking {
                thinking,
                signature,
            } => ReplyContent::Thinking {
                thinking: thinking.clone(),
                signature: signature.clone(),
            },
            ContentBlock::RedactedThinking { data } => {
                ReplyContent::RedactedThinking { data: data.clone() }
            }
        }
    }

    fn to_block(&self) -> ContentBlock {
        match self {
            ReplyContent::Text { text } => ContentBlock::Text { text: text.clone() },
            ReplyContent::ToolUse { id, name, input } => ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            ReplyContent::ToolResult {
                tool_use_id,
                content,
                succeeded,
                path: _,
            } => ContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                succeeded: *succeeded,
            },
            ReplyContent::Thinking {
                thinking,
                signature,
            } => ContentBlock::Thinking {
                thinking: thinking.clone(),
                signature: signature.clone(),
            },
            ReplyContent::RedactedThinking { data } => {
                ContentBlock::RedactedThinking { data: data.clone() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_and_content_getters_expose_their_values() {
        let reply = Reply::new(
            Author::Assistant,
            vec![ReplyContent::ToolUse {
                id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "README.md"}),
            }],
        );
        assert_eq!(reply.get_author(), Author::Assistant);
        assert!(reply.get_created_at() > 0);
        let content = &reply.get_content()[0];
        assert_eq!(content.get_kind(), "tool_use");
        assert_eq!(content.get_id(), Some("c1"));
        assert_eq!(content.get_name(), Some("read_file"));
        assert_eq!(content.get_input().unwrap()["path"], "README.md");
    }

    #[test]
    fn thinking_block_round_trips_through_reply_content() {
        let block = ContentBlock::Thinking {
            thinking: "reason".into(),
            signature: "sig".into(),
        };
        let reply = ReplyContent::from_block(&block, &HashMap::new());
        assert!(matches!(
            reply.to_block(),
            ContentBlock::Thinking { thinking, signature } if thinking == "reason" && signature == "sig"
        ));
    }

    #[test]
    fn redacted_thinking_round_trips_through_reply_content() {
        let block = ContentBlock::RedactedThinking { data: "enc".into() };
        let reply = ReplyContent::from_block(&block, &HashMap::new());
        assert!(matches!(
            reply.to_block(),
            ContentBlock::RedactedThinking { data } if data == "enc"
        ));
    }

    #[test]
    fn reply_with_thinking_survives_serde_round_trip() {
        let reply = Reply {
            author: Author::Assistant,
            content: vec![ReplyContent::Thinking {
                thinking: "r".into(),
                signature: "s".into(),
            }],
            created_at: 0,
        };
        let back: Reply = serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert!(matches!(
            &back.content[..],
            [ReplyContent::Thinking { thinking, signature }] if thinking == "r" && signature == "s"
        ));
    }

    /// Every variant must serialize exactly like the `ContentBlock` it
    /// mirrors, so one shape reaches callers whether they read a task's
    /// replies or a provider message. `Text` is the trap: as a newtype
    /// variant it cannot carry the `type` tag at all, and serde only
    /// discovers that at run time.
    #[test]
    fn every_reply_content_serializes_like_its_content_block() {
        let paths = HashMap::new();
        for block in [
            ContentBlock::Text { text: "hi".into() },
            ContentBlock::ToolUse {
                id: "c1".into(),
                name: "bash".into(),
                input: serde_json::json!({"cmd": "ls"}),
            },
            ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: "out".into(),
                succeeded: true,
            },
            ContentBlock::Thinking {
                thinking: "r".into(),
                signature: "s".into(),
            },
            ContentBlock::RedactedThinking { data: "enc".into() },
        ] {
            let content = ReplyContent::from_block(&block, &paths);
            assert_eq!(
                serde_json::to_value(&content).unwrap(),
                serde_json::to_value(&block).unwrap(),
                "{content:?} must serialize like the block it mirrors",
            );
        }
    }
}
