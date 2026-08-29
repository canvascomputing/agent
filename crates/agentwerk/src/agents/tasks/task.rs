//! One unit of work an agent picks up, and how it is stored.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::event::Event;
use crate::persistence::Persist;
use crate::prompts::Text;
use crate::providers::{AsUserMessage, Message};

use super::reply::{Author, Reply, ReplyContent};

/// A `Task` is a task plus what assigns and validates it.
///
/// You set the task with [`Task::new`] and optionally use the `label`, `schema`,
/// and `parent` builders. The rest is set for you at insertion time and as the
/// agent works.
///
/// ```no_run
/// use agentwerk::Task;
/// use agentwerk::schemas::Schema;
/// use serde_json::json;
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let schema = Schema::new(json!({"type": "object"}))?;
/// let task = Task::new("Summarize this URL.")
///     .label("research")
///     .schema(schema);
/// # let _ = task;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Task {
    /// The work the agent is asked to do.
    pub(crate) task: serde_json::Value,
    /// Label carried by the task, naming the pool of agents that may claim
    /// it. `None` is the default scope, which only an unlabelled agent serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    /// Optional schema the result must satisfy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) schema: Option<crate::schemas::Schema>,
    /// Task key, of the form `t-N`.
    pub(crate) key: String,
    /// The task lifecycle status.
    pub(crate) status: Status,
    /// Whether the current run has taken this task off the queue.
    ///
    /// Cancellation is run-local rather than a persisted lifecycle status:
    /// [`Queue::start`] clears it so unfinished work may resume.
    #[serde(skip)]
    pub(crate) cancelled: bool,
    /// Identifier of the agent that created the task.
    pub(crate) reporter: String,
    /// Identifier of the agent that claimed the task.
    ///
    /// A label makes the task eligible for every agent serving it; this names
    /// the one that actually took it, and is what brings a resumed task back
    /// to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) assignee: Option<String>,
    /// Creation time, in milliseconds.
    pub(crate) created_at: u64,
    /// Claim time, in milliseconds.
    pub(crate) started_at: Option<u64>,
    /// Finish time, in milliseconds. Never set together with `failed_at`.
    pub(crate) finished_at: Option<u64>,
    /// Failure time, in milliseconds. Never set together with `finished_at`.
    pub(crate) failed_at: Option<u64>,
    /// The result the agent produced. Stored in its own file, so it is not part
    /// of the task record.
    #[serde(skip)]
    pub(crate) result: Option<serde_json::Value>,
    /// Failures recorded against the task, appended as they happen. A tool
    /// call or request that failed does not fail the task, so this can carry
    /// entries on a task that finished. Read back out of the session log by
    /// `Queue::load`, so it is not part of the task record.
    #[serde(skip)]
    pub(crate) errors: Vec<Event>,
    /// The parent task if a handover was performed.
    pub(crate) parent: Option<String>,
    /// Messages exchanged with the model.
    #[serde(skip)]
    pub(crate) replies: Vec<Reply>,
}

impl Task {
    /// Create a task carrying `task`.
    ///
    /// Add a label and a schema with the chainable methods. Everything else is
    /// filled in when the task is submitted.
    pub fn new<T: Serialize>(task: T) -> Self {
        let value = serde_json::to_value(task).expect("Task::new: value must serialize to JSON");
        Self {
            task: value,
            label: None,
            schema: None,
            key: String::new(),
            status: Status::Todo,
            cancelled: false,
            reporter: String::new(),
            assignee: None,
            created_at: 0,
            started_at: None,
            finished_at: None,
            failed_at: None,
            result: None,
            errors: Vec::new(),
            parent: None,
            replies: Vec::new(),
        }
    }

    /// Create a task carrying `task` under `label`, the pair most tasks set.
    pub fn labeled<T: Serialize>(label: impl Into<String>, task: T) -> Self {
        Self::new(task).label(label)
    }

    /// Set the label, replacing any label already set.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Constrain the result to a schema.
    pub fn schema(mut self, schema: crate::schemas::Schema) -> Self {
        self.schema = Some(schema);
        self
    }

    /// Name the task this one came from.
    ///
    /// What the relationship means is up to you. A `finish` handover uses it to
    /// chain a child to the task that handed off.
    pub fn parent(mut self, key: impl Into<String>) -> Self {
        self.parent = Some(key.into());
        self
    }

    /// The work the agent was asked to do.
    pub fn get_task(&self) -> &serde_json::Value {
        &self.task
    }

    /// The task's agent-pool label, if it has one.
    pub fn get_label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The optional schema the result must satisfy.
    pub fn get_schema(&self) -> Option<&crate::schemas::Schema> {
        self.schema.as_ref()
    }

    /// The task key, of the form `t-N`.
    pub fn get_key(&self) -> &str {
        &self.key
    }

    /// The task's lifecycle status.
    pub fn get_status(&self) -> Status {
        self.status
    }

    /// The agent that created the task.
    pub fn get_reporter(&self) -> &str {
        &self.reporter
    }

    /// The agent that claimed the task, if any.
    pub fn get_assignee(&self) -> Option<&str> {
        self.assignee.as_deref()
    }

    /// Creation time, in milliseconds.
    pub fn get_created_at(&self) -> u64 {
        self.created_at
    }

    /// Claim time, in milliseconds.
    pub fn get_started_at(&self) -> Option<u64> {
        self.started_at
    }

    /// Finish time, in milliseconds.
    pub fn get_finished_at(&self) -> Option<u64> {
        self.finished_at
    }

    /// Failure time, in milliseconds.
    pub fn get_failed_at(&self) -> Option<u64> {
        self.failed_at
    }

    /// The result the agent produced, if it finished with one.
    pub fn get_result(&self) -> Option<&serde_json::Value> {
        self.result.as_ref()
    }

    /// Failures recorded against the task.
    pub fn get_errors(&self) -> &[Event] {
        &self.errors
    }

    /// The parent task key, if any.
    pub fn get_parent(&self) -> Option<&str> {
        self.parent.as_deref()
    }

    /// Messages exchanged with the model.
    pub fn get_replies(&self) -> &[Reply] {
        &self.replies
    }

    /// Check whether the task is waiting to be claimed.
    pub fn is_todo(&self) -> bool {
        self.status == Status::Todo
    }

    /// Check whether the task finished.
    pub fn is_finished(&self) -> bool {
        self.status == Status::Finished
    }

    /// Check whether the task failed.
    pub fn is_failed(&self) -> bool {
        self.status == Status::Failed
    }

    /// Check whether an agent is working on the task.
    pub fn is_in_progress(&self) -> bool {
        self.status == Status::InProgress
    }

    /// Check whether the task still has work for an agent in this run.
    pub fn is_pending(&self) -> bool {
        matches!(self.status, Status::Todo | Status::InProgress) && !self.cancelled
    }

    /// Check whether this run has taken the task off the queue.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// False once the model has spoken. The agent then waits for the next
    /// reply, whether a tool result or one you add with
    /// [`Queue::add_reply`].
    pub(crate) fn is_waiting_for_response(&self) -> bool {
        self.replies
            .last()
            .is_none_or(|r| r.author != Author::Assistant)
    }

    /// True while the task waits on the caller: the model has spoken and
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

    /// Turn this task's replies into the messages sent to the model.
    ///
    /// System-author replies are left out: the system prompt travels in its own
    /// field, and a compaction marker is there for the record only.
    pub(crate) fn to_messages(&self) -> Vec<Message> {
        self.replies.iter().filter_map(Reply::as_message).collect()
    }

    /// Record the timestamp for the status a task is about to reach.
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
}

// A blanket impl over `Serialize` would collide with the reflexive
// `From<Task>`, so the task types callers actually pass are listed.
impl From<&str> for Task {
    fn from(task: &str) -> Self {
        Task::new(task)
    }
}

impl From<String> for Task {
    fn from(task: String) -> Self {
        Task::new(task)
    }
}

impl From<serde_json::Value> for Task {
    fn from(task: serde_json::Value) -> Self {
        Task::new(task)
    }
}

/// The file holds the task. `Task::new(file)` stores the path itself, since
/// there the value passed is the task.
impl From<&Path> for Task {
    fn from(file: &Path) -> Self {
        Task::new(Text::from(file).into_string())
    }
}

impl From<PathBuf> for Task {
    fn from(file: PathBuf) -> Self {
        Self::from(file.as_path())
    }
}

impl From<&PathBuf> for Task {
    fn from(file: &PathBuf) -> Self {
        Self::from(file.as_path())
    }
}

impl crate::persistence::Persist for Task {
    type Key = String;

    fn save(&self, dir: &Path) -> io::Result<()> {
        let path = task_record_path(dir, &self.key);
        let body = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        crate::persistence::write_atomic(&path, &body)
    }

    /// `errors` stays empty: the failures live in the session log, and
    /// `Queue::load` fills them in the pass it makes over it.
    fn load(dir: &Path, key: &Self::Key) -> io::Result<Self> {
        let bytes = std::fs::read(task_record_path(dir, key))?;
        let mut task: Task = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        task.replies = Replies::load(dir, key)?.entries;
        task.result = TaskResult::load(dir, key)?.value;
        Ok(task)
    }
}

/// A task's result on disk: the bare JSON value in
/// `tasks/<key>/result.json`, so reading the file gives the result and
/// nothing around it.
pub(crate) struct TaskResult {
    pub(crate) key: String,
    pub(crate) value: Option<serde_json::Value>,
}

impl Persist for TaskResult {
    type Key = String;

    fn save(&self, dir: &Path) -> io::Result<()> {
        let Some(value) = &self.value else {
            return Ok(());
        };
        let body = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
        crate::persistence::write_atomic(&result_path(dir, &self.key), &body)
    }

    fn load(dir: &Path, key: &String) -> io::Result<Self> {
        let path = result_path(dir, key);
        let value = if path.exists() {
            let bytes = std::fs::read(&path)?;
            Some(serde_json::from_slice(&bytes).map_err(io::Error::other)?)
        } else {
            None
        };
        Ok(TaskResult {
            key: key.clone(),
            value,
        })
    }
}

/// A task's replies on disk, one JSON reply per line in
/// `tasks/<key>/replies.jsonl`.
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
    /// It is keyed by task, so it fits neither [`Persist::save`] nor
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

/// Where `key`'s task is stored: `tasks/<key>/task.json`.
pub(super) fn task_record_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("tasks").join(key).join("task.json")
}

/// Where `key`'s replies are stored: `tasks/<key>/replies.jsonl`.
fn replies_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("tasks").join(key).join("replies.jsonl")
}

/// Where `key`'s result is stored: `tasks/<key>/result.json`.
pub(super) fn result_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("tasks").join(key).join("result.json")
}

impl AsUserMessage for Task {
    fn as_user_message(&self) -> Message {
        let mut body = match &self.task {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        // Show the result shape up front: the finish tool validates against it, and
        // the role prompt alone is a thin thread for the model to hold.
        if let Some(schema) = &self.schema {
            body.push_str(&crate::prompts::schema_directive(schema));
        }
        Message::user(body)
    }
}

/// Where a task is in its lifecycle.
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

    fn assistant(content: Vec<ReplyContent>) -> Task {
        let mut task = Task::new("chat");
        task.replies.push(Reply {
            author: Author::Assistant,
            content,
            created_at: 0,
        });
        task
    }

    #[test]
    fn labeled_carries_both_the_label_and_the_task() {
        let task = Task::labeled("analysis", "Audit src/db.");
        assert_eq!(task.get_label(), Some("analysis"));
        assert_eq!(task.get_task(), &serde_json::json!("Audit src/db."));
    }

    #[test]
    fn new_exposes_runtime_state_through_getters() {
        let task = Task::new("Audit src/db.");
        assert_eq!(task.get_label(), None);
        assert!(task.get_schema().is_none());
        assert_eq!(task.get_key(), "");
        assert_eq!(task.get_status(), Status::Todo);
        assert_eq!(task.get_reporter(), "");
        assert_eq!(task.get_assignee(), None);
        assert_eq!(task.get_created_at(), 0);
        assert_eq!(task.get_started_at(), None);
        assert_eq!(task.get_finished_at(), None);
        assert_eq!(task.get_failed_at(), None);
        assert_eq!(task.get_result(), None);
        assert!(task.get_errors().is_empty());
        assert_eq!(task.get_parent(), None);
        assert!(task.get_replies().is_empty());
    }

    #[test]
    fn a_string_converts_into_a_task_carrying_it_as_the_task() {
        assert_eq!(
            Task::from("Audit src/db.").task,
            serde_json::json!("Audit src/db.")
        );
        assert_eq!(
            Task::from("Audit src/db.".to_string()).task,
            serde_json::json!("Audit src/db.")
        );
    }

    #[test]
    fn a_path_converts_into_a_task_carrying_the_file_as_the_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let file = dir.path().join("task.md");
        std::fs::write(&file, "Audit src/db.\n").unwrap();
        assert_eq!(
            Task::from(file.as_path()).task,
            serde_json::json!("Audit src/db.")
        );
    }

    #[test]
    fn a_json_value_converts_into_a_task_carrying_it_as_the_task() {
        let task = serde_json::json!({ "file": "src/db.rs" });
        assert_eq!(Task::from(task.clone()).task, task);
    }

    #[test]
    fn paused_when_a_reasoning_reply_carries_thinking_then_text() {
        // Regression: reasoning models add a Thinking block, which a
        // text-only check rejects, hanging an interactive turn forever.
        let task = assistant(vec![
            ReplyContent::Thinking {
                thinking: "hmm".into(),
                signature: "s".into(),
            },
            ReplyContent::Text { text: "Hi.".into() },
        ]);
        assert!(task.is_paused());
    }

    #[test]
    fn not_paused_while_the_reply_still_carries_a_tool_call() {
        let task = assistant(vec![ReplyContent::ToolUse {
            id: "c1".into(),
            name: "read_file".into(),
            input: serde_json::json!({}),
        }]);
        assert!(!task.is_paused());
    }

    #[test]
    fn not_paused_when_the_last_reply_is_the_caller() {
        let mut task = Task::new("chat");
        task.replies.push(Reply {
            author: Author::User,
            content: vec![ReplyContent::Text { text: "hey".into() }],
            created_at: 0,
        });
        assert!(!task.is_paused());
    }

    #[test]
    fn as_user_message_appends_the_result_schema_when_set() {
        let schema = crate::schemas::Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"summary": {"type": "string"}},
            "required": ["summary"],
        }))
        .unwrap();
        let task = Task::new("describe the project").schema(schema);
        let Message::User { content } = task.as_user_message() else {
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
        let task = Task::new("x");
        assert!(task.is_waiting_for_response());
    }

    #[test]
    fn is_waiting_for_response_true_when_last_reply_is_user() {
        let mut task = Task::new("x");
        task.replies.push(Reply::user_text("hello"));
        assert!(task.is_waiting_for_response());
    }

    #[test]
    fn is_waiting_for_response_false_when_last_reply_is_text_assistant() {
        let mut task = Task::new("x");
        task.replies.push(Reply::user_text("go"));
        task.replies.push(Reply::assistant(&[ContentBlock::Text {
            text: "hi".into(),
        }]));
        assert!(!task.is_waiting_for_response());
    }

    #[test]
    fn is_waiting_for_response_false_when_assistant_reply_has_empty_content() {
        let mut task = Task::new("x");
        task.replies.push(Reply::user_text("go"));
        task.replies.push(Reply::assistant(&[]));
        assert!(!task.is_waiting_for_response());
    }

    #[test]
    fn is_waiting_for_response_false_when_assistant_reply_carries_tool_use() {
        let mut task = Task::new("x");
        task.replies.push(Reply::user_text("go"));
        task.replies.push(Reply::assistant(&[ContentBlock::ToolUse {
            id: "call-1".into(),
            name: "do_thing".into(),
            input: serde_json::json!({}),
        }]));
        assert!(!task.is_waiting_for_response());
    }

    #[test]
    fn label_replaces_the_previous_one() {
        let t = Task::new("body").label("research").label("urgent");
        assert_eq!(t.label.as_deref(), Some("urgent"));
    }

    #[test]
    fn public_predicates_follow_label_status_and_cancellation() {
        let mut task = Task::new("body").label("scan");
        assert!(task.get_label() == Some("scan"));
        assert!(task.get_label() != Some("report"));
        assert!(task.is_todo());
        assert!(task.is_pending());
        assert!(!task.is_in_progress());
        assert!(!task.is_finished());
        assert!(!task.is_failed());
        assert!(!task.is_cancelled());

        task.status = Status::InProgress;
        assert!(task.is_in_progress());
        task.cancelled = true;
        assert!(task.is_cancelled());
        assert!(!task.is_pending());

        task.status = Status::Finished;
        assert!(task.is_finished());
        task.status = Status::Failed;
        assert!(task.is_failed());
    }

    #[test]
    fn is_pending_means_unfinished_and_not_cancelled() {
        let mut t = Task::new("x");
        for status in [Status::Todo, Status::InProgress] {
            t.status = status;
            assert!(t.is_pending(), "expected is_pending for {status:?}");
        }
        t.cancelled = true;
        assert!(!t.is_pending());
        t.cancelled = false;
        for status in [Status::Finished, Status::Failed] {
            t.status = status;
            assert!(!t.is_pending(), "expected !is_pending for {status:?}");
        }
    }
}
