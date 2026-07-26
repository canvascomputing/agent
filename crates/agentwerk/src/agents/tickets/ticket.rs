//! The [`Ticket`] value type, its disk persistence, and the
//! [`Replies`] transcript-log helper.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::providers::{AsUserMessage, Message};

use super::reply::{Author, Reply};

/// A task plus the metadata that assigns it. Caller-settable fields:
/// `task`, `labels`, `schema`, `parent`. System-managed fields (`key`,
/// `status`, `reporter`, the lifecycle timestamps, `result`, `replies`)
/// are set by the ticket system and the agent loop.
///
/// ```no_run
/// use agentwerk::Ticket;
/// use agentwerk::schemas::Schema;
/// use serde_json::json;
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let schema = Schema::parse(json!({"type": "object"}))?;
/// let ticket = Ticket::new("Summarize this URL.")
///     .label("research")
///     .schema(schema);
/// # let _ = ticket;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Ticket {
    /// Task body the agent is asked to perform, as arbitrary JSON.
    pub task: serde_json::Value,
    /// Labels carried by the ticket.
    pub labels: Vec<String>,
    /// Schema the result must conform to, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<crate::schemas::Schema>,
    /// Stable identifier (`TICKET-N`).
    pub key: String,
    /// Lifecycle status (`Todo`, `InProgress`, `Finished`, `Failed`).
    pub status: Status,
    /// Name of the agent that opened the ticket.
    pub reporter: String,
    /// Millisecond timestamp at which the ticket was created.
    pub created_at: u64,
    /// Millisecond timestamp at which an agent claimed the ticket.
    pub started_at: Option<u64>,
    /// Millisecond timestamp at which the ticket was marked finished.
    /// Mutually exclusive with `failed_at`.
    pub finished_at: Option<u64>,
    /// Millisecond timestamp at which the ticket failed. Mutually
    /// exclusive with `finished_at`.
    pub failed_at: Option<u64>,
    /// Raw JSON result payload.
    pub result: Option<serde_json::Value>,
    /// Back-reference to another ticket, or `None` when the ticket
    /// has no parent. Caller-settable via [`Ticket::parent`].
    pub parent: Option<String>,
    /// Transcript of messages exchanged with the model.
    #[serde(skip)]
    pub replies: Vec<Reply>,
}

impl Ticket {
    /// New ticket carrying `task` as its body. Use the chainable helpers
    /// (`label`, `labels`, `schema`) to populate caller-settable fields.
    /// System-managed fields are set by the ticket system at insertion
    /// time; the placeholders set here are overwritten.
    pub fn new<T: Serialize>(task: T) -> Self {
        let value = serde_json::to_value(task).expect("Ticket::new: value must serialize to JSON");
        Self {
            task: value,
            labels: Vec::new(),
            schema: None,
            key: String::new(),
            status: Status::Todo,
            reporter: String::new(),
            created_at: 0,
            started_at: None,
            finished_at: None,
            failed_at: None,
            result: None,
            parent: None,
            replies: Vec::new(),
        }
    }

    /// Add a single label. Use [`Self::labels`] to add several at once.
    pub fn label(mut self, l: impl Into<String>) -> Self {
        self.labels.push(l.into());
        self
    }

    /// Add many labels at once.
    pub fn labels<I, S>(mut self, iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.labels.extend(iter.into_iter().map(Into::into));
        self
    }

    pub fn schema(mut self, schema: crate::schemas::Schema) -> Self {
        self.schema = Some(schema);
        self
    }

    /// Record a back-reference to another ticket. The meaning is
    /// caller-defined: a `finish` handover uses it to chain a
    /// child to the ticket that handed off, but any code building a
    /// ticket may set it to express a parent relationship.
    pub fn parent(mut self, key: impl Into<String>) -> Self {
        self.parent = Some(key.into());
        self
    }

    /// True when `label` is present on this ticket.
    pub fn has_label(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label)
    }

    /// True while the ticket sits unclaimed in the queue.
    pub fn is_todo(&self) -> bool {
        self.status == Status::Todo
    }

    /// True when this ticket has reached `Status::Finished`.
    pub fn is_finished(&self) -> bool {
        self.status == Status::Finished
    }

    /// True when this ticket has reached `Status::Failed`.
    pub fn is_failed(&self) -> bool {
        self.status == Status::Failed
    }

    /// True while an agent holds the ticket.
    pub fn is_in_progress(&self) -> bool {
        self.status == Status::InProgress
    }

    /// True while the ticket is still in flight: `Todo` or `InProgress`.
    pub fn is_pending(&self) -> bool {
        matches!(self.status, Status::Todo | Status::InProgress)
    }

    /// True once the ticket reached a terminal state: `Finished` or `Failed`.
    pub fn is_resolved(&self) -> bool {
        matches!(self.status, Status::Finished | Status::Failed)
    }

    /// Reduce the transcript to just `summary_text`: every non-system
    /// reply is dropped and a single `user` reply carrying
    /// `summary_text` is appended. System-author replies (the system
    /// prompt) survive unchanged. `task` is rewritten to the summary
    /// too, so the persisted ticket header reflects the post-compaction
    /// state alongside the paired `replies.<at>.jsonl`. Safe because
    /// `task` is read for the seed user message only when `replies` is
    /// empty, which is never true after compaction. Used by the loop
    /// after a successful compaction.
    pub(crate) fn summarize(&mut self, summary_text: String) {
        self.replies.retain(|r| r.author == Author::System);
        self.replies.push(Reply::user_text(summary_text.clone()));
        self.task = serde_json::Value::String(summary_text);
    }

    /// False once the assistant has spoken: the loop pauses until the
    /// next non-assistant reply lands (a tool-result append or an
    /// external caller reply via [`TicketSystem::reply`]).
    pub(crate) fn is_waiting_for_response(&self) -> bool {
        self.replies
            .last()
            .is_none_or(|r| r.author != Author::Assistant)
    }

    /// Project this ticket's transcript into the provider's
    /// [`Message`] values. Skips `system`-author replies: the system
    /// prompt is passed via `request.system_prompt`, and
    /// compaction-boundary replies are audit markers only.
    pub(crate) fn to_messages(&self) -> Vec<Message> {
        self.replies.iter().filter_map(Reply::as_message).collect()
    }

    /// Stamp `started_at` / `finished_at` / `failed_at` on a ticket
    /// whose status is about to flip to `next`. Called inside the
    /// locked critical section before `self.status` is overwritten.
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

    /// Compute (ticket_duration, work_duration) for a ticket that just
    /// reached a terminal status. `ticket_duration` is creation→terminal;
    /// `work_duration` is started→terminal. Both default to zero if the
    /// relevant timestamps aren't both set.
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
        let path = ticket_header_path(dir, &self.key);
        let body = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        crate::persistence::write_atomic(&path, &body)
    }

    fn load(dir: &Path, key: &Self::Key) -> io::Result<Self> {
        let bytes = std::fs::read(ticket_header_path(dir, key))?;
        let mut ticket: Ticket = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        ticket.replies = Replies::load(dir, key)?;
        Ok(ticket)
    }
}

/// Per-ticket transcript files on disk. `tickets/<key>/replies.jsonl`
/// is the initial replies file: pre-first-compaction appends land
/// there. Each compaction event creates an immutable pair
/// `(ticket.<ts>.json, replies.<ts>.jsonl)`; subsequent appends grow
/// `replies.<ts>.jsonl` rather than the initial file. On load the
/// newest committed pair's replies file becomes the base and any
/// initial-file entries with a strictly greater `created_at` are
/// merged on top (legacy data recovery).
pub(crate) struct Replies;

impl Replies {
    /// Append one reply as a single JSON line. Writes to the latest
    /// committed compaction's `replies.<at>.jsonl` when one exists, so
    /// post-compaction replies grow that file rather than the initial
    /// `replies.jsonl`. Falls back to `replies.jsonl` before the first
    /// compaction.
    pub(crate) fn append(dir: &Path, key: &str, reply: &Reply) -> io::Result<()> {
        let line = serde_json::to_string(reply).map_err(io::Error::other)?;
        crate::persistence::append_line(&current_replies_path(dir, key), &line)
    }

    /// Reconstruct the transcript for `key` per the rule on the type doc.
    pub(crate) fn load(dir: &Path, key: &str) -> io::Result<Vec<Reply>> {
        fn read_jsonl(path: &Path) -> io::Result<Vec<Reply>> {
            let body = std::fs::read_to_string(path)?;
            body.lines()
                .filter(|l| !l.is_empty())
                .map(|l| serde_json::from_str::<Reply>(l).map_err(io::Error::other))
                .collect()
        }

        let ticket_dir = dir.join("tickets").join(key);
        let restart_at = last_committed_compaction_at(&ticket_dir);
        let mut replies = match restart_at {
            Some(at) => read_jsonl(&ticket_dir.join(format!("replies.{at}.jsonl")))?,
            None => Vec::new(),
        };
        let initial = initial_replies_path(dir, key);
        if initial.exists() {
            let mut tail = read_jsonl(&initial)?;
            tail.retain(|c| restart_at.is_none_or(|at| c.created_at > at));
            replies.extend(tail);
        }
        Ok(replies)
    }
}

/// Path of the active header file for `key`: `tickets/<key>/ticket.json`.
pub(super) fn ticket_header_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("tickets").join(key).join("ticket.json")
}

/// Path of the initial replies file for `key`: `tickets/<key>/replies.jsonl`.
/// Receives appends before the first compaction; also holds pre-fix
/// legacy data for old tickets.
fn initial_replies_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("tickets").join(key).join("replies.jsonl")
}

/// Path of the replies file new replies must be appended to: the
/// `replies.<at>.jsonl` of the latest committed compaction when one
/// exists, else the initial replies file. `Replies::append` and
/// `Replies::load` use the same paired-check rule via
/// [`last_committed_compaction_at`], so the writer's destination and
/// the reader's base file always agree.
fn current_replies_path(dir: &Path, key: &str) -> PathBuf {
    let ticket_dir = dir.join("tickets").join(key);
    match last_committed_compaction_at(&ticket_dir) {
        Some(at) => ticket_dir.join(format!("replies.{at}.jsonl")),
        None => initial_replies_path(dir, key),
    }
}

/// Most recent compaction timestamp whose pair is committed:
/// `replies.<at>.jsonl` exists AND its sibling `ticket.<at>.json`
/// commit marker exists. An orphan replies file from a mid-compaction
/// crash is ignored. `None` when no committed compaction has happened.
fn last_committed_compaction_at(ticket_dir: &Path) -> Option<u64> {
    std::fs::read_dir(ticket_dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_prefix("ticket.")?
                .strip_suffix(".json")?
                .parse::<u64>()
                .ok()
        })
        .filter(|at| ticket_dir.join(format!("replies.{at}.jsonl")).is_file())
        .max()
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

/// Lifecycle status of a ticket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Status {
    /// Created but not yet claimed by an agent.
    Todo,
    /// Claimed by an agent and running.
    InProgress,
    /// Finished with a result.
    Finished,
    /// Failed after a schema-retry trip, missing-finish-tool exhaustion,
    /// or policy violation.
    Failed,
}

impl fmt::Display for Status {
    /// Lowercase wire form: `"todo"`, `"in_progress"`, `"finished"`, `"failed"`.
    /// Single source of truth for the string rendering used by the
    /// `tickets.jsonl` event log and any caller that prints a status.
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

    #[test]
    fn ticket_label_helpers_compose() {
        let t = Ticket::new("body").label("research").label("urgent");
        assert_eq!(t.labels, vec!["research".to_string(), "urgent".to_string()]);
    }

    #[test]
    fn as_user_message_appends_the_result_schema_when_set() {
        let schema = crate::schemas::Schema::parse(serde_json::json!({
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

    #[test]
    fn is_resolved_true_for_finished_and_failed() {
        let mut t = Ticket::new("x");
        for status in [Status::Finished, Status::Failed] {
            t.status = status;
            assert!(t.is_resolved(), "expected is_resolved for {status:?}");
        }
        for status in [Status::Todo, Status::InProgress] {
            t.status = status;
            assert!(!t.is_resolved(), "expected !is_resolved for {status:?}");
        }
    }
}
