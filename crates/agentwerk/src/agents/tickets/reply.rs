//! Per-ticket transcript entries and their projection back into
//! provider [`Message`] values. [`ReplyContent`] mirrors
//! [`ContentBlock`] so the ticket surface stays free of provider types.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::providers::{ContentBlock, Message};

use super::now_millis;

/// Originator of a transcript entry. The agent loop writes `System`
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

/// One entry in a ticket's transcript.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Reply {
    pub author: Author,
    pub content: Vec<ReplyContent>,
    /// Millis since epoch.
    pub created_at: u64,
}

/// Ticket-side mirror of [`ContentBlock`]. Keeps the public ticket
/// surface free of provider types while still recording every payload
/// shape the agent loop sends.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ReplyContent {
    Text(String),
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
}

impl Reply {
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
    pub(crate) fn user_text(text: impl Into<String>) -> Self {
        Self {
            author: Author::User,
            content: vec![ReplyContent::Text(text.into())],
            created_at: now_millis(),
        }
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
            content: vec![ReplyContent::Text(text.into())],
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
    fn from_block(b: &ContentBlock, paths: &HashMap<String, PathBuf>) -> Self {
        match b {
            ContentBlock::Text { text } => ReplyContent::Text(text.clone()),
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
        }
    }

    fn to_block(&self) -> ContentBlock {
        match self {
            ReplyContent::Text(text) => ContentBlock::Text { text: text.clone() },
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
        }
    }
}
