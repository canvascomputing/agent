//! One unit of work an agent picks up, and how it is stored.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::persistence::Persist;
use crate::providers::{AsUserMessage, Message};

use super::reply::{Author, Reply, ReplyContent};

/// A `Ticket` is a task plus what assigns and validates it.
///
/// You set `task`, `labels`, `schema`, and `parent`. The rest is set for you at
/// insertion time and as the agent works.
///
/// ```no_run
/// use agentwerk::Ticket;
/// use agentwerk::schemas::Schema;
/// use serde_json::json;
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let schema = Schema::new(json!({"type": "object"}))?;
/// let ticket = Ticket::new("Summarize this URL.")
///     .label("research")
///     .schema(schema);
/// # let _ = ticket;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Ticket {
    /// The work the agent is asked to do.
    pub task: serde_json::Value,
    /// Labels carried by the ticket.
    pub labels: Vec<String>,
    /// Optional schema the result must satisfy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<crate::schemas::Schema>,
    /// Ticket key, of the form `TICKET-N`.
    pub key: String,
    /// The ticket lifecycle status.
    pub status: Status,
    /// Identifier of the agent that created the ticket.
    pub reporter: String,
    /// Identifier of the agent that claimed the ticket.
    ///
    /// A label makes the ticket eligible for every agent serving it; this names
    /// the one that actually took it, and is what brings a resumed ticket back
    /// to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// Creation time, in milliseconds.
    pub created_at: u64,
    /// Claim time, in milliseconds.
    pub started_at: Option<u64>,
    /// Finish time, in milliseconds. Never set together with `failed_at`.
    pub finished_at: Option<u64>,
    /// Failure time, in milliseconds. Never set together with `finished_at`.
    pub failed_at: Option<u64>,
    /// The result the agent produced.
    pub result: Option<serde_json::Value>,
    /// The parent ticket if a handover was performed.
    pub parent: Option<String>,
    /// Messages exchanged with the model.
    #[serde(skip)]
    pub replies: Vec<Reply>,
}

impl Ticket {
    /// Create a ticket carrying `task`.
    ///
    /// Add labels and a schema with the chainable methods. Everything else is
    /// filled in when the ticket is submitted.
    pub fn new<T: Serialize>(task: T) -> Self {
        let value = serde_json::to_value(task).expect("Ticket::new: value must serialize to JSON");
        Self {
            task: value,
            labels: Vec::new(),
            schema: None,
            key: String::new(),
            status: Status::Todo,
            reporter: String::new(),
            assignee: None,
            created_at: 0,
            started_at: None,
            finished_at: None,
            failed_at: None,
            result: None,
            parent: None,
            replies: Vec::new(),
        }
    }

    /// Add one label.
    pub fn label(mut self, l: impl Into<String>) -> Self {
        self.labels.push(l.into());
        self
    }

    /// Add several labels.
    pub fn labels<I, S>(mut self, iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.labels.extend(iter.into_iter().map(Into::into));
        self
    }

    /// Constrain the result to a schema.
    pub fn schema(mut self, schema: crate::schemas::Schema) -> Self {
        self.schema = Some(schema);
        self
    }

    /// Name the ticket this one came from.
    ///
    /// What the relationship means is up to you. A `finish` handover uses it to
    /// chain a child to the ticket that handed off.
    pub fn parent(mut self, key: impl Into<String>) -> Self {
        self.parent = Some(key.into());
        self
    }

    /// Check whether the ticket carries a label.
    pub fn has_label(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label)
    }

    /// Check whether the ticket is waiting to be claimed.
    pub fn is_todo(&self) -> bool {
        self.status == Status::Todo
    }

    /// Check whether the ticket finished.
    pub fn is_finished(&self) -> bool {
        self.status == Status::Finished
    }

    /// Check whether the ticket failed.
    pub fn is_failed(&self) -> bool {
        self.status == Status::Failed
    }

    /// Check whether an agent is working on the ticket.
    pub fn is_in_progress(&self) -> bool {
        self.status == Status::InProgress
    }

    /// Check whether the ticket is still todo or in progress.
    pub fn is_pending(&self) -> bool {
        matches!(self.status, Status::Todo | Status::InProgress)
    }

    /// False once the model has spoken. The agent then waits for the next
    /// reply, whether a tool result or one you add with
    /// [`TicketQueue::reply`].
    pub(crate) fn is_waiting_for_response(&self) -> bool {
        self.replies
            .last()
            .is_none_or(|r| r.author != Author::Assistant)
    }

    /// True while the ticket waits on the caller: the model has spoken and
    /// called no tool. Stricter than the negation of
    /// [`Self::is_waiting_for_response`], which also holds in the window
    /// between a tool-calling reply and its results, where the agent is still
    /// working.
    pub(crate) fn is_paused(&self) -> bool {
        self.replies.last().is_some_and(|r| {
            r.author == Author::Assistant
                && !r
                    .content
                    .iter()
                    .any(|c| matches!(c, ReplyContent::ToolUse { .. }))
        })
    }

    /// Turn this ticket's replies into the messages sent to the model.
    ///
    /// System-author replies are left out: the system prompt travels in its own
    /// field, and a compaction marker is there for the record only.
    pub(crate) fn to_messages(&self) -> Vec<Message> {
        self.replies.iter().filter_map(Reply::as_message).collect()
    }

    /// Record the timestamp for the status a ticket is about to reach.
    pub(crate) fn stamp_transition(&mut self, next: Status, now: u64) {
        if self.status == Status::Todo && next == Status::InProgress {
            self.started_at = Some(now);
        }
        match next {
            Status::Finished => {
                self.finished_at = Some(now);
            }
            Status::Failed => {
                self.failed_at = Some(now);
            }
            _ => {}
        }
    }

    /// The time from creation to resolution, and the time an agent held the
    /// ticket. Either is zero when its timestamps are not both set.
    pub(crate) fn terminal_durations(&self) -> (Duration, Duration) {
        let terminal = self.finished_at.or(self.failed_at);
        let ticket_duration = match terminal {
            Some(end) => Duration::from_millis(end.saturating_sub(self.created_at)),
            None => Duration::ZERO,
        };
        let work_duration = match (self.started_at, terminal) {
            (Some(start), Some(end)) => Duration::from_millis(end.saturating_sub(start)),
            _ => Duration::ZERO,
        };
        (ticket_duration, work_duration)
    }
}

impl crate::persistence::Persist for Ticket {
    type Key = String;

    fn save(&self, dir: &Path) -> io::Result<()> {
        let path = ticket_record_path(dir, &self.key);
        let body = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        crate::persistence::write_atomic(&path, &body)
    }

    fn load(dir: &Path, key: &Self::Key) -> io::Result<Self> {
        let bytes = std::fs::read(ticket_record_path(dir, key))?;
        let mut ticket: Ticket = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        ticket.replies = Replies::load(dir, key)?.entries;
        Ok(ticket)
    }
}

/// A ticket's replies on disk, one JSON reply per line in
/// `tickets/<key>/replies.jsonl`.
///
/// [`Persist::save`] rewrites the whole file, and [`Replies::append`] adds one
/// line without reading it.
pub(crate) struct Replies {
    pub(crate) key: String,
    pub(crate) entries: Vec<Reply>,
}

impl Replies {
    /// Add one reply as a single line, without reading the file.
    ///
    /// It is keyed by ticket, so it fits neither [`Persist::save`] nor
    /// [`Append`](crate::persistence::Append).
    pub(crate) fn append(dir: &Path, key: &str, reply: &Reply) -> io::Result<()> {
        let line = serde_json::to_string(reply).map_err(io::Error::other)?;
        crate::persistence::append_line(&replies_path(dir, key), &line)
    }
}

impl Persist for Replies {
    type Key = String;

    /// Rewrite `replies.jsonl` whole, so a dropped or redacted reply leaves
    /// nothing behind.
    fn save(&self, dir: &Path) -> io::Result<()> {
        let mut body = String::new();
        for reply in &self.entries {
            body.push_str(&serde_json::to_string(reply).map_err(io::Error::other)?);
            body.push('\n');
        }
        crate::persistence::write_atomic(&replies_path(dir, &self.key), body.as_bytes())
    }

    fn load(dir: &Path, key: &String) -> io::Result<Self> {
        let path = replies_path(dir, key);
        let entries = if path.exists() {
            std::fs::read_to_string(&path)?
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| serde_json::from_str::<Reply>(l).map_err(io::Error::other))
                .collect::<io::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        Ok(Replies {
            key: key.clone(),
            entries,
        })
    }
}

/// Where `key`'s ticket is stored: `tickets/<key>/ticket.json`.
pub(super) fn ticket_record_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("tickets").join(key).join("ticket.json")
}

/// Where `key`'s replies are stored: `tickets/<key>/replies.jsonl`.
fn replies_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("tickets").join(key).join("replies.jsonl")
}

impl AsUserMessage for Ticket {
    fn as_user_message(&self) -> Message {
        let mut body = match &self.task {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        // Show the result shape up front: the finish tool validates against it, and
        // the role prompt alone is a thin thread for the model to hold. The
        // directive names an object schema's fields as top-level arguments.
        if let Some(schema) = &self.schema {
            if let Ok(document) = serde_json::to_value(schema) {
                body.push_str(&crate::prompts::schema_directive(&document));
            }
        }
        Message::user(body)
    }
}

/// Where a ticket is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Status {
    /// Created, waiting for an agent.
    Todo,
    /// Claimed by an agent and under way.
    InProgress,
    /// Finished, carrying a result.
    Finished,
    /// Failed after exhausted schema retries, exhausted missing-`finish`
    /// retries, or a limit being breached.
    Failed,
}

impl fmt::Display for Status {
    /// The lowercase form: `"todo"`, `"in_progress"`, `"finished"`, `"failed"`.
    ///
    /// It is the one source for that spelling, used by the event log and by
    /// anyone printing a status.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Status::Todo => "todo",
            Status::InProgress => "in_progress",
            Status::Finished => "finished",
            Status::Failed => "failed",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ContentBlock;

    fn assistant(content: Vec<ReplyContent>) -> Ticket {
        let mut ticket = Ticket::new("chat");
        ticket.replies.push(Reply {
            author: Author::Assistant,
            content,
            created_at: 0,
        });
        ticket
    }

    #[test]
    fn paused_when_a_reasoning_reply_carries_thinking_then_text() {
        // Regression: reasoning models add a Thinking block, which a
        // text-only check rejects, hanging an interactive turn forever.
        let ticket = assistant(vec![
            ReplyContent::Thinking {
                thinking: "hmm".into(),
                signature: "s".into(),
            },
            ReplyContent::Text { text: "Hi.".into() },
        ]);
        assert!(ticket.is_paused());
    }

    #[test]
    fn not_paused_while_the_reply_still_carries_a_tool_call() {
        let ticket = assistant(vec![ReplyContent::ToolUse {
            id: "c1".into(),
            name: "read_file".into(),
            input: serde_json::json!({}),
        }]);
        assert!(!ticket.is_paused());
    }

    #[test]
    fn not_paused_when_the_last_reply_is_the_caller() {
        let mut ticket = Ticket::new("chat");
        ticket.replies.push(Reply {
            author: Author::User,
            content: vec![ReplyContent::Text { text: "hey".into() }],
            created_at: 0,
        });
        assert!(!ticket.is_paused());
    }

    #[test]
    fn ticket_label_helpers_compose() {
        let t = Ticket::new("body").label("research").label("urgent");
        assert_eq!(t.labels, vec!["research".to_string(), "urgent".to_string()]);
    }

    #[test]
    fn as_user_message_appends_the_result_schema_when_set() {
        let schema = crate::schemas::Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"summary": {"type": "string"}},
            "required": ["summary"],
        }))
        .unwrap();
        let ticket = Ticket::new("describe the project").schema(schema);
        let Message::User { content } = ticket.as_user_message() else {
            panic!("as_user_message must return Message::User");
        };
        let [ContentBlock::Text { text }] = content.as_slice() else {
            panic!("expected a single text block, got {content:?}");
        };
        assert!(text.starts_with("describe the project"));
        assert!(text.contains("matching this schema"));
        assert!(text.contains("summary"));
    }

    #[test]
    fn is_waiting_for_response_true_for_empty_transcript() {
        let ticket = Ticket::new("x");
        assert!(ticket.is_waiting_for_response());
    }

    #[test]
    fn is_waiting_for_response_true_when_last_reply_is_user() {
        let mut ticket = Ticket::new("x");
        ticket.replies.push(Reply::user_text("hello"));
        assert!(ticket.is_waiting_for_response());
    }

    #[test]
    fn is_waiting_for_response_false_when_last_reply_is_text_assistant() {
        let mut ticket = Ticket::new("x");
        ticket.replies.push(Reply::user_text("go"));
        ticket.replies.push(Reply::assistant(&[ContentBlock::Text {
            text: "hi".into(),
        }]));
        assert!(!ticket.is_waiting_for_response());
    }

    #[test]
    fn is_waiting_for_response_false_when_assistant_reply_has_empty_content() {
        let mut ticket = Ticket::new("x");
        ticket.replies.push(Reply::user_text("go"));
        ticket.replies.push(Reply::assistant(&[]));
        assert!(!ticket.is_waiting_for_response());
    }

    #[test]
    fn is_waiting_for_response_false_when_assistant_reply_carries_tool_use() {
        let mut ticket = Ticket::new("x");
        ticket.replies.push(Reply::user_text("go"));
        ticket
            .replies
            .push(Reply::assistant(&[ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "do_thing".into(),
                input: serde_json::json!({}),
            }]));
        assert!(!ticket.is_waiting_for_response());
    }

    #[test]
    fn has_label_true_when_label_present() {
        let t = Ticket::new("x").label("research").label("urgent");
        assert!(t.has_label("research"));
        assert!(t.has_label("urgent"));
    }

    #[test]
    fn has_label_false_when_label_missing() {
        let t = Ticket::new("x").label("research");
        assert!(!t.has_label("urgent"));
    }

    #[test]
    fn has_label_false_on_empty_labels() {
        let t = Ticket::new("x");
        assert!(!t.has_label("anything"));
    }

    #[test]
    fn is_todo_true_only_while_unclaimed() {
        let mut t = Ticket::new("x");
        assert!(t.is_todo());
        for status in [Status::InProgress, Status::Finished, Status::Failed] {
            t.status = status;
            assert!(!t.is_todo(), "expected !is_todo for {status:?}");
        }
    }

    #[test]
    fn is_finished_true_for_finished_status() {
        let mut t = Ticket::new("x");
        t.status = Status::Finished;
        assert!(t.is_finished());
    }

    #[test]
    fn is_failed_true_only_for_failed_status() {
        let mut t = Ticket::new("x");
        t.status = Status::Failed;
        assert!(t.is_failed());
        for status in [Status::Todo, Status::InProgress, Status::Finished] {
            t.status = status;
            assert!(!t.is_failed(), "expected !is_failed for {status:?}");
        }
    }

    #[test]
    fn is_finished_false_for_todo_in_progress_failed() {
        let mut t = Ticket::new("x");
        for status in [Status::Todo, Status::InProgress, Status::Failed] {
            t.status = status;
            assert!(!t.is_finished(), "expected !is_finished for {status:?}");
        }
    }

    #[test]
    fn is_in_progress_true_only_while_claimed() {
        let mut t = Ticket::new("x");
        t.status = Status::InProgress;
        assert!(t.is_in_progress());
        for status in [Status::Todo, Status::Finished, Status::Failed] {
            t.status = status;
            assert!(
                !t.is_in_progress(),
                "expected !is_in_progress for {status:?}"
            );
        }
    }

    #[test]
    fn is_pending_true_for_todo_and_in_progress() {
        let mut t = Ticket::new("x");
        for status in [Status::Todo, Status::InProgress] {
            t.status = status;
            assert!(t.is_pending(), "expected is_pending for {status:?}");
        }
        for status in [Status::Finished, Status::Failed] {
            t.status = status;
            assert!(!t.is_pending(), "expected !is_pending for {status:?}");
        }
    }
}
