//! Records task, agent, tool, provider, and execution activity.

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// One thing that happened as agents work. Agent and task attribution are
/// optional and independent.
///
/// ```no_run
/// use agentwerk::{Event, Werk};
/// use serde_json::json;
///
/// let werk = Werk::new();
/// werk.emit_event(
///     Event::new("document_indexed")
///         .data(json!({ "documents": 42 }))
///         .task_id("t-1")
///         .agent_id("indexer-1"),
/// );
/// ```
#[derive(Debug, Clone)]
pub struct Event {
    /// The event name.
    pub(crate) name: String,
    /// The model-facing directive that explains this event, when one applies.
    pub(crate) directive: Option<String>,
    /// The JSON value carried by the event.
    pub(crate) data: Value,
    /// ID of the task this event concerns, or empty when it has no task
    /// context.
    pub(crate) task_id: String,
    /// Agent that produced the event, or empty when it has no agent context.
    pub(crate) agent_id: String,
    /// Label the task carries, so a handler counting per label reads it
    /// without looking the task up. `None` when the event names no known task,
    /// and when the task carries no label.
    pub(crate) label: Option<String>,
    /// When this event happened, in milliseconds since the epoch.
    pub(crate) created_at: u64,
}

impl Event {
    /// Event name emitted when a run begins.
    pub const RUN_STARTED: &'static str = "run_started";
    /// Event name emitted when a run ends.
    pub const RUN_FINISHED: &'static str = "run_finished";
    /// Event name emitted when a task is created.
    pub const TASK_CREATED: &'static str = "task_created";
    /// Event name emitted when a task is claimed.
    pub const TASK_STARTED: &'static str = "task_started";
    /// Event name emitted when a task finishes.
    pub const TASK_FINISHED: &'static str = "task_finished";
    /// Event name emitted when a task fails.
    pub const TASK_FAILED: &'static str = "task_failed";
    /// Event name emitted when an agent turn begins.
    pub const TURN_STARTED: &'static str = "turn_started";
    /// Event name emitted before a provider request.
    pub const REQUEST_STARTED: &'static str = "request_started";
    /// Event name emitted after a provider request succeeds.
    pub const REQUEST_FINISHED: &'static str = "request_finished";
    /// Event name emitted after a provider request fails.
    pub const REQUEST_FAILED: &'static str = "request_failed";
    /// Event name emitted before retrying a provider request.
    pub const REQUEST_RETRIED: &'static str = "request_retried";
    /// Event name emitted for a streamed text fragment.
    pub const TEXT_CHUNK_RECEIVED: &'static str = "text_chunk_received";
    /// Event name emitted when malformed tool arguments are repaired.
    pub const TOOL_CALL_REPAIRED: &'static str = "tool_call_repaired";
    /// Event name emitted when a textual tool call is not executed.
    pub const TOOL_CALL_DECLINED: &'static str = "tool_call_declined";
    /// Event name emitted before a tool call runs.
    pub const TOOL_CALL_STARTED: &'static str = "tool_call_started";
    /// Event name emitted after a tool call succeeds.
    pub const TOOL_CALL_FINISHED: &'static str = "tool_call_finished";
    /// Event name emitted after a tool call fails.
    pub const TOOL_CALL_FAILED: &'static str = "tool_call_failed";
    /// Event name emitted when a knowledge page is written.
    pub const KNOWLEDGE_WRITTEN: &'static str = "knowledge_written";
    /// Event name emitted when a knowledge page is read.
    pub const KNOWLEDGE_READ: &'static str = "knowledge_read";
    /// Event name emitted when a knowledge page is removed.
    pub const KNOWLEDGE_REMOVED: &'static str = "knowledge_removed";
    /// Event name emitted when knowledge pages are listed.
    pub const KNOWLEDGE_LISTED: &'static str = "knowledge_listed";
    /// Event name emitted when a knowledge operation fails.
    pub const KNOWLEDGE_FAILED: &'static str = "knowledge_failed";
    /// Event name emitted when a run exceeds policy.
    pub const POLICY_VIOLATED: &'static str = "policy_violated";
    /// Event name emitted before retrying result-schema validation.
    pub const SCHEMA_RETRIED: &'static str = "schema_retried";
    /// Event name emitted when context compaction starts.
    pub const COMPACTION_STARTED: &'static str = "compaction_started";
    /// Event name emitted as context compaction advances.
    pub const COMPACTION_PROGRESS: &'static str = "compaction_progress";
    /// Event name emitted when context compaction succeeds.
    pub const COMPACTION_FINISHED: &'static str = "compaction_finished";
    /// Event name emitted when context compaction fails.
    pub const COMPACTION_FAILED: &'static str = "compaction_failed";

    pub(crate) const BUILTIN_NAMES: &'static [&'static str] = &[
        Self::RUN_STARTED,
        Self::RUN_FINISHED,
        Self::TASK_CREATED,
        Self::TASK_STARTED,
        Self::TASK_FINISHED,
        Self::TASK_FAILED,
        Self::TURN_STARTED,
        Self::REQUEST_STARTED,
        Self::REQUEST_FINISHED,
        Self::REQUEST_FAILED,
        Self::REQUEST_RETRIED,
        Self::TEXT_CHUNK_RECEIVED,
        Self::TOOL_CALL_REPAIRED,
        Self::TOOL_CALL_DECLINED,
        Self::TOOL_CALL_STARTED,
        Self::TOOL_CALL_FINISHED,
        Self::TOOL_CALL_FAILED,
        Self::KNOWLEDGE_WRITTEN,
        Self::KNOWLEDGE_READ,
        Self::KNOWLEDGE_REMOVED,
        Self::KNOWLEDGE_LISTED,
        Self::KNOWLEDGE_FAILED,
        Self::POLICY_VIOLATED,
        Self::SCHEMA_RETRIED,
        Self::COMPACTION_STARTED,
        Self::COMPACTION_PROGRESS,
        Self::COMPACTION_FINISHED,
        Self::COMPACTION_FAILED,
    ];

    /// Create an event with empty JSON-object data.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            directive: None,
            data: Value::Object(Map::new()),
            task_id: String::new(),
            agent_id: String::new(),
            label: None,
            created_at: 0,
        }
    }

    /// Create a run-started event.
    pub fn run_started() -> Self {
        Self::new(Self::RUN_STARTED)
    }

    /// Create a run-finished event.
    pub fn run_finished(outcome: crate::agents::tasks::FinishReason) -> Self {
        Self::new(Self::RUN_FINISHED).data(serde_json::json!({ "outcome": outcome }))
    }

    /// Create a task-created event.
    pub fn task_created() -> Self {
        Self::new(Self::TASK_CREATED)
    }

    /// Create a task-started event.
    pub fn task_started() -> Self {
        Self::new(Self::TASK_STARTED)
    }

    /// Create a task-finished event with no result payload.
    ///
    /// Events emitted by a Werk transition carry the stored result under
    /// `data.result` when the task has one.
    pub fn task_finished() -> Self {
        Self::new(Self::TASK_FINISHED)
    }

    /// Create a task-failed event.
    pub fn task_failed() -> Self {
        Self::new(Self::TASK_FAILED)
    }

    /// Create a turn-started event.
    pub fn turn_started() -> Self {
        Self::new(Self::TURN_STARTED)
    }

    /// Create a request-started event.
    pub fn request_started(model: impl Into<String>) -> Self {
        Self::new(Self::REQUEST_STARTED).data(serde_json::json!({ "model": model.into() }))
    }

    /// Create a request-finished event.
    pub fn request_finished(model: impl Into<String>, usage: crate::providers::TokenUsage) -> Self {
        Self::new(Self::REQUEST_FINISHED)
            .data(serde_json::json!({ "model": model.into(), "usage": usage }))
    }

    /// Create a request-failed event.
    pub fn request_failed(
        model: impl Into<String>,
        kind: crate::providers::RequestErrorKind,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Self::REQUEST_FAILED).data(serde_json::json!({
            "model": model.into(),
            "kind": kind,
            "message": message.into(),
        }))
    }

    /// Create a request-retried event.
    pub fn request_retried(
        model: impl Into<String>,
        attempt: u32,
        max_attempts: u32,
        kind: crate::providers::RequestErrorKind,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Self::REQUEST_RETRIED).data(serde_json::json!({
            "model": model.into(),
            "attempt": attempt,
            "max_attempts": max_attempts,
            "kind": kind,
            "message": message.into(),
        }))
    }

    /// Create a text-chunk-received event.
    pub fn text_chunk_received(content: impl Into<String>) -> Self {
        Self::new(Self::TEXT_CHUNK_RECEIVED).data(serde_json::json!({ "content": content.into() }))
    }

    /// Create a tool-call-repaired event.
    pub fn tool_call_repaired(
        tool_name: impl Into<String>,
        call_id: impl Into<String>,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Self::TOOL_CALL_REPAIRED).data(serde_json::json!({
            "tool_name": tool_name.into(),
            "call_id": call_id.into(),
            "kind": kind.into(),
            "message": message.into(),
        }))
    }

    /// Create a tool-call-declined event.
    pub fn tool_call_declined(
        tool_name: impl Into<String>,
        kind: crate::providers::ToolDeclineKind,
    ) -> Self {
        Self::new(Self::TOOL_CALL_DECLINED)
            .data(serde_json::json!({ "tool_name": tool_name.into(), "kind": kind }))
    }

    /// Create a tool-call-started event.
    pub fn tool_call_started(
        tool_name: impl Into<String>,
        call_id: impl Into<String>,
        input: Value,
    ) -> Self {
        Self::new(Self::TOOL_CALL_STARTED).data(serde_json::json!({
            "tool_name": tool_name.into(),
            "call_id": call_id.into(),
            "input": input,
        }))
    }

    /// Create a successful terminal tool-call event.
    pub fn tool_call_finished(output: impl Into<String>) -> Self {
        Self::new(Self::TOOL_CALL_FINISHED).data(serde_json::json!({ "output": output.into() }))
    }

    /// Create a failed terminal tool-call event.
    pub fn tool_call_failed(message: impl Into<String>) -> Self {
        Self::new(Self::TOOL_CALL_FAILED).data(serde_json::json!({
            "kind": "execution_failed",
            "message": message.into(),
        }))
    }

    /// Create a knowledge-written event.
    pub fn knowledge_written(slug: impl Into<String>) -> Self {
        Self::new(Self::KNOWLEDGE_WRITTEN).data(serde_json::json!({ "slug": slug.into() }))
    }

    /// Create a knowledge-read event.
    pub fn knowledge_read(slug: impl Into<String>) -> Self {
        Self::new(Self::KNOWLEDGE_READ).data(serde_json::json!({ "slug": slug.into() }))
    }

    /// Create a knowledge-removed event.
    pub fn knowledge_removed(slug: impl Into<String>) -> Self {
        Self::new(Self::KNOWLEDGE_REMOVED).data(serde_json::json!({ "slug": slug.into() }))
    }

    /// Create a knowledge-listed event.
    pub fn knowledge_listed() -> Self {
        Self::new(Self::KNOWLEDGE_LISTED)
    }

    /// Create a knowledge-failed event.
    pub fn knowledge_failed(
        action: impl Into<String>,
        slug: impl Into<String>,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Self::KNOWLEDGE_FAILED).data(serde_json::json!({
            "action": action.into(),
            "slug": slug.into(),
            "kind": kind.into(),
            "message": message.into(),
        }))
    }

    /// Create a policy-violated event.
    pub fn policy_violated(policy: crate::agents::PolicyViolation, limit: u64) -> Self {
        Self::new(Self::POLICY_VIOLATED)
            .data(serde_json::json!({ "policy": policy, "limit": limit }))
    }

    /// Create a schema-retried event.
    pub fn schema_retried(
        attempt: u32,
        max_attempts: u32,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Self::SCHEMA_RETRIED).data(serde_json::json!({
            "attempt": attempt,
            "max_attempts": max_attempts,
            "kind": kind.into(),
            "message": message.into(),
        }))
    }

    /// Create a compaction-started event.
    pub fn compaction_started(trigger: impl Into<String>, total: u32) -> Self {
        Self::new(Self::COMPACTION_STARTED)
            .data(serde_json::json!({ "trigger": trigger.into(), "total": total }))
    }

    /// Create a compaction-progress event.
    pub fn compaction_progress(trigger: impl Into<String>, completed: u32, total: u32) -> Self {
        Self::new(Self::COMPACTION_PROGRESS).data(serde_json::json!({
            "trigger": trigger.into(),
            "completed": completed,
            "total": total,
        }))
    }

    /// Create a compaction-finished event.
    pub fn compaction_finished(trigger: impl Into<String>) -> Self {
        Self::new(Self::COMPACTION_FINISHED).data(serde_json::json!({ "trigger": trigger.into() }))
    }

    /// Create a compaction-failed event.
    pub fn compaction_failed(
        trigger: impl Into<String>,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Self::COMPACTION_FAILED).data(serde_json::json!({
            "trigger": trigger.into(),
            "kind": kind.into(),
            "message": message.into(),
        }))
    }

    /// Associate the event with the directive used to explain it to the model.
    pub fn directive(mut self, directive: impl Into<String>) -> Self {
        self.directive = Some(directive.into());
        self
    }

    /// Set the JSON value carried by this event.
    pub fn data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }

    /// Associate this event with a task ID.
    pub fn task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = task_id.into();
        self
    }

    /// Attribute this event to an agent id.
    pub fn agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = agent_id.into();
        self
    }

    /// The event name.
    pub fn get_name(&self) -> &str {
        &self.name
    }

    /// The directive used to explain this event to the model, if any.
    pub fn get_directive(&self) -> Option<&str> {
        self.directive.as_deref()
    }

    /// The JSON value carried by this event.
    pub fn get_data(&self) -> &Value {
        &self.data
    }

    /// The task this event concerns, or an empty string when omitted.
    pub fn get_task_id(&self) -> &str {
        &self.task_id
    }

    /// The agent that produced this event, or an empty string when omitted.
    pub fn get_agent_id(&self) -> &str {
        &self.agent_id
    }

    /// The task label captured by this event, if any.
    pub fn get_label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// When this event happened, in milliseconds since the epoch.
    pub fn get_created_at(&self) -> u64 {
        self.created_at
    }
}

impl Serialize for Event {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = Map::new();
        object.insert("name".into(), self.name.clone().into());
        if let Some(directive) = &self.directive {
            object.insert("directive".into(), directive.clone().into());
        }
        object.insert("data".into(), self.data.clone());
        object.insert("task_id".into(), self.task_id.clone().into());
        object.insert("agent_id".into(), self.agent_id.clone().into());
        if let Some(label) = &self.label {
            object.insert("label".into(), label.clone().into());
        }
        object.insert("created_at".into(), self.created_at.into());
        object.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut object = Map::<String, Value>::deserialize(deserializer)?;
        let created_at = take_or(&mut object, "created_at", 0)?;
        let directive = match object.remove("directive") {
            Some(value) => serde_json::from_value(value).map_err(D::Error::custom)?,
            None => None,
        };
        let agent_id = take_or(&mut object, "agent_id", String::new())?;
        let task_id = take_or(&mut object, "task_id", String::new())?;
        let label = match object.remove("label") {
            Some(value) => serde_json::from_value(value).map_err(D::Error::custom)?,
            None => None,
        };
        let (name, data) = match object.remove("name") {
            Some(value) => {
                let name: String = serde_json::from_value(value).map_err(D::Error::custom)?;
                let data = object
                    .remove("data")
                    .unwrap_or_else(|| Value::Object(Map::new()));
                (name, data)
            }
            None => {
                let name: String = take(&mut object, "event")?;
                let data = match object.remove("data") {
                    Some(data) if object.is_empty() => data,
                    Some(data) => {
                        object.insert("data".into(), data);
                        Value::Object(object)
                    }
                    None => Value::Object(object),
                };
                (name, data)
            }
        };
        Ok(Self {
            name,
            directive,
            data,
            task_id,
            agent_id,
            label,
            created_at,
        })
    }
}

fn take<T, E>(object: &mut Map<String, Value>, field: &'static str) -> Result<T, E>
where
    T: serde::de::DeserializeOwned,
    E: serde::de::Error,
{
    let value = object
        .remove(field)
        .ok_or_else(|| E::missing_field(field))?;
    serde_json::from_value(value).map_err(E::custom)
}

fn take_or<T, E>(object: &mut Map<String, Value>, field: &'static str, default: T) -> Result<T, E>
where
    T: serde::de::DeserializeOwned,
    E: serde::de::Error,
{
    match object.remove(field) {
        Some(value) => serde_json::from_value(value).map_err(E::custom),
        None => Ok(default),
    }
}

/// The handler that runs when you install none of your own.
///
/// Print concise activity and failure lines to stderr with agent/task context
/// and tool-specific action summaries. Symbols are colored only in terminals
/// without `NO_COLOR`. Streamed text and successful tool outputs are omitted.
/// Output errors never interrupt execution.
pub fn default_logger() -> Arc<dyn Fn(&Event) + Send + Sync> {
    Arc::new(|event: &Event| {
        let stderr = io::stderr();
        let color = stderr.is_terminal() && std::env::var_os("NO_COLOR").is_none();
        if let Some(line) = default_log_line(event, color) {
            // A closed output stream must not take down an agent task.
            let _ = writeln!(stderr.lock(), "{line}");
        }
    })
}

fn default_log_line(event: &Event, color: bool) -> Option<String> {
    let (symbol, message) = default_log_message(event)?;
    let context = match (event.agent_id.as_str(), event.task_id.as_str()) {
        ("", "") => "run".to_string(),
        (agent, "") => agent.to_string(),
        ("", task) => task.to_string(),
        (agent, task) => format!("{agent} / {task}"),
    };
    let context = compact_log_text(&context, usize::MAX);
    let max = if matches!(symbol, '!' | '✗' | '↻') {
        240
    } else {
        120
    };
    let message = compact_log_text(&message, max);
    let marker = if color && symbol != ' ' {
        let code = match symbol {
            '✗' => 31,
            '!' | '↻' => 33,
            '✓' => 32,
            '→' => 36,
            _ => 90,
        };
        format!("\x1b[{code}m{symbol}\x1b[0m")
    } else {
        symbol.to_string()
    };
    let separator = if context == "run" { " " } else { "  " };
    Some(format!("{marker} {context}{separator}{message}"))
}

fn default_log_message(event: &Event) -> Option<(char, String)> {
    let data = &event.data;
    let text = |key| log_field(data, key, "?");
    let count = |key| log_count(data, key);
    let detail = || log_error_detail(data);
    let message = match event.name.as_str() {
        Event::RUN_STARTED => ('·', "started".to_string()),
        Event::RUN_FINISHED => {
            let outcome = data.get("outcome").cloned().unwrap_or(Value::Null);
            match serde_json::from_value::<crate::FinishReason>(outcome) {
                Ok(crate::FinishReason::Drained) => ('·', "drained".to_string()),
                Ok(crate::FinishReason::Cancelled) => ('·', "cancelled".to_string()),
                Ok(crate::FinishReason::PolicyViolated(policy)) => {
                    ('✗', format!("stopped: {policy} limit exceeded"))
                }
                Err(_) => ('·', format!("finished: {}", text("outcome"))),
            }
        }
        Event::TASK_CREATED => ('·', "created".to_string()),
        Event::TASK_STARTED => ('→', "started".to_string()),
        Event::TASK_FINISHED => ('✓', "finished".to_string()),
        Event::TASK_FAILED => ('✗', format!("failed{}", detail())),
        Event::TOOL_CALL_STARTED => {
            let input = data.get("input").unwrap_or(&Value::Null);
            (' ', log_tool_action(text("tool_name"), input)?)
        }
        Event::TOOL_CALL_FAILED => ('!', format!("{} failed{}", text("tool_name"), detail())),
        Event::REQUEST_FAILED => ('✗', format!("request failed{}", detail())),
        Event::REQUEST_RETRIED | Event::SCHEMA_RETRIED => {
            let action = if event.name == Event::REQUEST_RETRIED {
                "request retry"
            } else {
                "schema retry"
            };
            (
                '↻',
                format!(
                    "{action} {}/{}{}",
                    count("attempt"),
                    count("max_attempts"),
                    detail()
                ),
            )
        }
        Event::KNOWLEDGE_FAILED => (
            '!',
            format!(
                "knowledge {} {} failed{}",
                text("action"),
                text("slug"),
                detail()
            ),
        ),
        Event::POLICY_VIOLATED => {
            let unit = if text("policy") == "time" { "ms" } else { "" };
            (
                '✗',
                format!(
                    "{} limit exceeded: {}{unit}{}",
                    text("policy"),
                    count("limit"),
                    detail()
                ),
            )
        }
        Event::COMPACTION_STARTED => (
            '·',
            format!(
                "compacting context ({}): {} chunks",
                text("trigger"),
                count("total")
            ),
        ),
        Event::COMPACTION_PROGRESS => (
            '·',
            format!(
                "compacting context ({}): {}/{} chunks",
                text("trigger"),
                count("completed"),
                count("total")
            ),
        ),
        Event::COMPACTION_FINISHED => ('·', format!("context compacted ({})", text("trigger"))),
        Event::COMPACTION_FAILED => (
            '✗',
            format!("compaction failed ({}){}", text("trigger"), detail()),
        ),
        _ => return None,
    };
    Some(message)
}

fn log_tool_action(name: &str, input: &Value) -> Option<String> {
    let text = |key| log_field(input, key, "?");
    let path = || log_field(input, "path", ".");
    let action = text("action");
    let id = || log_field(input, "id", "current");
    let summary = match name {
        "read_file" => {
            let mut summary = format!("read_file {}", text("path"));
            if input.get("offset").is_some() || input.get("limit").is_some() {
                let start = input.get("offset").unwrap_or(&Value::from(1)).as_u64();
                let from = start.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
                let end = match input.get("limit") {
                    Some(limit) => start
                        .zip(limit.as_u64())
                        .and_then(|(start, limit)| limit.checked_sub(1)?.checked_add(start))
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "?".into()),
                    None => String::new(),
                };
                summary.push_str(&format!(":{from}–{end}"));
            }
            for key in ["column", "length"] {
                if input.get(key).is_some() {
                    summary.push_str(&format!(" {key}={}", log_count(input, key)));
                }
            }
            summary
        }
        "write_file" => format!("write_file {}", text("path")),
        "edit_file" => {
            let suffix = if input["replace_all"] == true {
                " (all occurrences)"
            } else {
                ""
            };
            format!("edit_file {}{suffix}", text("path"))
        }
        "list_directory" => {
            let suffix = if input["recursive"] == true {
                " (recursive)"
            } else {
                ""
            };
            format!("list_directory {}{suffix}", path())
        }
        "glob" => format!("glob {:?} in {}", text("pattern"), path()),
        "grep" => {
            let mut summary = format!("grep {:?} in {}", text("pattern"), path());
            for (key, label) in [
                ("glob", "glob"),
                ("type", "type"),
                ("output_mode", "mode"),
                ("syntax", "syntax"),
            ] {
                if input.get(key).is_some() {
                    summary.push_str(&format!(" {label}={:?}", text(key)));
                }
            }
            summary
        }
        "fetch" => format!("fetch {}", text("url")),
        "task" => {
            let mut summary = match action {
                "task" | "result" | "edit" => format!("task {action} {}", id()),
                "list" => format!("task list {}", log_field(input, "aql", "all")),
                _ => format!("task {action}"),
            };
            if matches!(action, "create" | "edit") && input.get("label").is_some() {
                summary.push_str(&format!(" label={:?}", text("label")));
            }
            summary
        }
        "knowledge" if action == "list" => "knowledge list".to_string(),
        "knowledge" => format!("knowledge {action} {}", text("slug")),
        "event" if text("name") == Event::TASK_FINISHED => return None,
        "event" => format!("event {}", text("name")),
        "finish" => return None,
        _ => match input.get("command").and_then(Value::as_str) {
            Some(command) => format!("{name} $ {command}"),
            None => format!("{name} {input}"),
        },
    };
    Some(summary)
}

fn log_field<'a>(data: &'a Value, key: &str, default: &'a str) -> &'a str {
    match data.get(key) {
        Some(value) => value.as_str().unwrap_or("?"),
        None => default,
    }
}

fn log_count(data: &Value, key: &str) -> String {
    data.get(key)
        .and_then(Value::as_u64)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn log_error_detail(data: &Value) -> String {
    let mut detail = String::new();
    if let Some(kind) = data.get("kind").and_then(Value::as_str) {
        detail.push_str(&format!(" ({kind})"));
    }
    if let Some(message) = data.get("message").and_then(Value::as_str) {
        detail.push_str(&format!(": {message}"));
    }
    detail
}

fn compact_log_text(text: &str, max: usize) -> String {
    let mut result = String::new();
    let mut count = 0;
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            space = !result.is_empty();
            continue;
        }
        if space {
            if count == max {
                result.push('…');
                break;
            }
            result.push(' ');
            count += 1;
            space = false;
        }
        if count == max {
            result.push('…');
            break;
        }
        result.push(c);
        count += 1;
    }
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeSet;

    pub(crate) fn all_events() -> Vec<Event> {
        Event::BUILTIN_NAMES
            .iter()
            .map(|name| Event::new(*name))
            .collect()
    }

    #[test]
    fn default_logger_handles_every_name_and_malformed_data() {
        let logger = default_logger();
        for name in Event::BUILTIN_NAMES {
            logger(&Event::new(*name).task_id("T-1").agent_id("agent"));
        }
    }

    #[test]
    fn tool_logs_show_actions_without_content_or_results() {
        use serde_json::json;

        let cases = [
            ("read_file", json!({"path": "src/main.rs"}), "read_file src/main.rs"),
            ("read_file", json!({"path": "src/main.rs", "offset": 120, "limit": 40}), "read_file src/main.rs:120–159"),
            ("read_file", json!({"path": "src/main.rs", "limit": 10}), "read_file src/main.rs:1–10"),
            ("read_file", json!({"path": "src/main.rs", "offset": 120}), "read_file src/main.rs:120–"),
            ("read_file", json!({"path": "src/main.rs", "offset": 120, "limit": 1, "column": 30, "length": 80}), "read_file src/main.rs:120–120 column=30 length=80"),
            ("write_file", json!({"path": "report.md", "content": "PRIVATE CONTENT"}), "write_file report.md"),
            ("edit_file", json!({"path": "main.rs", "old_string": "PRIVATE OLD", "new_string": "PRIVATE NEW"}), "edit_file main.rs"),
            ("edit_file", json!({"path": "main.rs", "replace_all": true}), "edit_file main.rs (all occurrences)"),
            ("list_directory", json!({}), "list_directory ."),
            ("list_directory", json!({"path": "src", "recursive": true}), "list_directory src (recursive)"),
            ("glob", json!({"pattern": "**/*.rs"}), "glob \"**/*.rs\" in ."),
            ("glob", json!({"pattern": "*.rs", "path": "src"}), "glob \"*.rs\" in src"),
            ("grep", json!({"pattern": "TODO"}), "grep \"TODO\" in ."),
            ("grep", json!({"pattern": "$F(...)", "path": "src", "glob": "*.rs", "type": "rust", "output_mode": "count", "syntax": "code"}), "grep \"$F(...)\" in src glob=\"*.rs\" type=\"rust\" mode=\"count\" syntax=\"code\""),
            ("fetch", json!({"url": "https://example.com"}), "fetch https://example.com"),
            ("git", json!({"command": "git diff --stat", "timeout_ms": 1000}), "git $ git diff --stat"),
            ("shell", json!({"command": "cargo test"}), "shell $ cargo test"),
            ("task", json!({"action": "task"}), "task task current"),
            ("task", json!({"action": "result", "id": "t-2"}), "task result t-2"),
            ("task", json!({"action": "list"}), "task list all"),
            ("task", json!({"action": "list", "aql": "task.label = scan"}), "task list task.label = scan"),
            ("task", json!({"action": "create", "task": "PRIVATE TASK", "label": "scan"}), "task create label=\"scan\""),
            ("task", json!({"action": "create", "task": "PRIVATE TASK"}), "task create"),
            ("task", json!({"action": "edit", "task": "PRIVATE TASK"}), "task edit current"),
            ("task", json!({"action": "edit", "id": "t-2", "label": "review"}), "task edit t-2 label=\"review\""),
            ("knowledge", json!({"action": "write", "slug": "notes", "content": "PRIVATE PAGE"}), "knowledge write notes"),
            ("knowledge", json!({"action": "read", "slug": "notes"}), "knowledge read notes"),
            ("knowledge", json!({"action": "remove", "slug": "notes"}), "knowledge remove notes"),
            ("knowledge", json!({"action": "list"}), "knowledge list"),
            ("event", json!({"name": "document_indexed", "data": "PRIVATE DATA"}), "event document_indexed"),
            ("custom", json!({"query": "hello"}), "custom {\"query\":\"hello\"}"),
        ];
        for (name, input, expected) in cases {
            let event = Event::tool_call_started(name, "c-1", input)
                .agent_id("researcher-1")
                .task_id("t-1");
            assert_eq!(
                default_log_line(&event, false).unwrap(),
                format!("  researcher-1 / t-1  {expected}")
            );
        }
    }

    #[test]
    fn malformed_tool_inputs_still_identify_the_requested_action() {
        use serde_json::json;

        for name in [
            "read_file",
            "write_file",
            "edit_file",
            "list_directory",
            "glob",
            "grep",
            "fetch",
            "task",
            "knowledge",
            "event",
            "git",
            "custom",
        ] {
            for input in [Value::Null, json!([]), json!(42), json!("text"), json!({})] {
                let event = Event::tool_call_started(name, "c-1", input);
                assert!(default_log_line(&event, false).unwrap().contains(name));
            }
        }
        for (name, input, expected) in [
            ("read_file", json!({"path": 42}), "read_file ?"),
            (
                "read_file",
                json!({"path": "x", "offset": "bad", "limit": 2}),
                "read_file x:?–?",
            ),
            (
                "read_file",
                json!({"path": "x", "offset": u64::MAX, "limit": 2}),
                "read_file x:18446744073709551615–?",
            ),
            (
                "read_file",
                json!({"path": "x", "limit": 0}),
                "read_file x:1–?",
            ),
            ("list_directory", json!({"path": 42}), "list_directory ?"),
            (
                "fetch",
                json!({"url": "https://example.com", "command": "ignored"}),
                "fetch https://example.com",
            ),
        ] {
            let event = Event::tool_call_started(name, "c-1", input);
            assert_eq!(
                default_log_line(&event, false).unwrap(),
                format!("  run {expected}")
            );
        }
    }

    #[test]
    fn failures_and_retries_keep_their_category_and_available_message() {
        use crate::providers::RequestErrorKind;
        use crate::PolicyViolation;

        let cases = [
            (Event::tool_call_failed("missing file"), "! t-1  ? failed (execution_failed): missing file"),
            (Event::tool_call_failed("timed out").data(serde_json::json!({"tool_name": "fetch", "message": "timed out"})), "! t-1  fetch failed: timed out"),
            (Event::knowledge_failed("read", "notes", "not_found", "missing page"), "! t-1  knowledge read notes failed (not_found): missing page"),
            (Event::request_retried("model", 1, 3, RequestErrorKind::RateLimited, "try later"), "↻ t-1  request retry 1/3 (rate_limited): try later"),
            (Event::new(Event::SCHEMA_RETRIED).data(serde_json::json!({"attempt": 1, "max_attempts": 2, "message": "invalid result"})), "↻ t-1  schema retry 1/2: invalid result"),
            (Event::request_failed("model", RequestErrorKind::AuthenticationFailed, "invalid key"), "✗ t-1  request failed (authentication_failed): invalid key"),
            (Event::compaction_failed("reactive", "summarization_failed", "invalid reply"), "✗ t-1  compaction failed (reactive) (summarization_failed): invalid reply"),
            (Event::task_failed(), "✗ t-1  failed"),
            (Event::policy_violated(PolicyViolation::Time, 1000), "✗ t-1  time limit exceeded: 1000ms"),
        ];
        for (event, expected) in cases {
            assert_eq!(
                default_log_line(&event.task_id("t-1"), false).unwrap(),
                expected
            );
        }
        for name in [
            Event::TASK_FAILED,
            Event::TOOL_CALL_FAILED,
            Event::REQUEST_FAILED,
            Event::KNOWLEDGE_FAILED,
            Event::COMPACTION_FAILED,
            Event::POLICY_VIOLATED,
        ] {
            for data in [
                Value::Null,
                serde_json::json!({"message": "something broke"}),
            ] {
                let has_message = data.is_object();
                let event = Event::new(name).data(data);
                let line = default_log_line(&event, false).unwrap();
                assert!(line.starts_with('!') || line.starts_with('✗'));
                if has_message {
                    assert!(line.contains("something broke"), "{line}");
                }
            }
        }
    }

    #[test]
    fn lifecycle_lines_distinguish_task_success_from_run_drain() {
        use crate::{FinishReason, PolicyViolation};

        for (event, expected) in [
            (
                Event::task_started().agent_id("worker").task_id("t-1"),
                "→ worker / t-1  started",
            ),
            (
                Event::task_finished().agent_id("worker"),
                "✓ worker  finished",
            ),
            (Event::task_created().task_id("t-1"), "· t-1  created"),
            (Event::run_started(), "· run started"),
            (Event::run_finished(FinishReason::Drained), "· run drained"),
            (
                Event::run_finished(FinishReason::Cancelled),
                "· run cancelled",
            ),
            (
                Event::run_finished(FinishReason::PolicyViolated(PolicyViolation::Turns)),
                "✗ run stopped: turns limit exceeded",
            ),
        ] {
            assert_eq!(default_log_line(&event, false).unwrap(), expected);
        }
    }

    #[test]
    fn completion_calls_and_successful_outputs_do_not_duplicate_lifecycle_lines() {
        use serde_json::json;

        for event in [
            Event::tool_call_started("finish", "c-1", json!({"result": "PRIVATE RESULT"})),
            Event::tool_call_started("finish", "c-1", json!({"verdict": "PRIVATE RESULT"})),
            Event::tool_call_started(
                "event",
                "c-1",
                json!({"name": "task_finished", "data": {"result": "PRIVATE RESULT"}}),
            ),
            Event::tool_call_finished("PRIVATE OUTPUT"),
            Event::text_chunk_received("PRIVATE TEXT"),
        ] {
            assert!(default_log_line(&event, false).is_none());
        }
        let failed_finish = Event::tool_call_failed("invalid result")
            .data(json!({"tool_name": "finish", "message": "invalid result"}));
        assert_eq!(
            default_log_line(&failed_finish, false).unwrap(),
            "! run finish failed: invalid result"
        );
    }

    #[test]
    fn summaries_collapse_whitespace_and_truncate_at_unicode_boundaries() {
        let event = Event::tool_call_started(
            "shell",
            "c-1",
            serde_json::json!({"command": "echo\n  hello\tworld\u{1b}"}),
        )
        .agent_id("worker\r\n one");
        assert_eq!(
            default_log_line(&event, false).unwrap(),
            "  worker one  shell $ echo hello world"
        );

        for (event, max) in [
            (
                Event::tool_call_started(
                    "fetch",
                    "c-1",
                    serde_json::json!({"url": "界".repeat(200)}),
                ),
                120,
            ),
            (
                Event::request_failed(
                    "model",
                    crate::providers::RequestErrorKind::ConnectionFailed,
                    "界".repeat(300),
                ),
                240,
            ),
        ] {
            let line = default_log_line(&event, false).unwrap();
            let summary = line.split_once("run ").unwrap().1;
            assert_eq!(summary.chars().count(), max + 1);
            assert!(summary.ends_with('…'));
            assert!(!summary.chars().any(char::is_control));
        }
        assert_eq!(compact_log_text("界界界", 3), "界界界");
        assert_eq!(compact_log_text("界界界界", 3), "界界界…");
        assert_eq!(compact_log_text("  one \t two \n ", 7), "one two");
    }

    #[test]
    fn color_changes_only_symbols_and_plain_logs_keep_the_symbols() {
        for (event, code, symbol) in [
            (Event::task_started(), 36, '→'),
            (Event::task_finished(), 32, '✓'),
            (Event::task_failed(), 31, '✗'),
            (Event::tool_call_failed("oops"), 33, '!'),
        ] {
            let plain = default_log_line(&event, false).unwrap();
            let colored = default_log_line(&event, true).unwrap();
            assert!(!plain.contains('\u{1b}'));
            assert!(plain.starts_with(symbol));
            assert_eq!(
                colored,
                format!("\x1b[{code}m{symbol}\x1b[0m{}", &plain[symbol.len_utf8()..])
            );
        }
        let event = Event::tool_call_started(
            "fetch",
            "c-1",
            serde_json::json!({"url": "https://example.com"}),
        );
        assert_eq!(
            default_log_line(&event, true),
            default_log_line(&event, false)
        );
    }

    #[test]
    fn built_in_event_names_are_unique() {
        let names: BTreeSet<&str> = Event::BUILTIN_NAMES.iter().copied().collect();
        assert_eq!(names.len(), Event::BUILTIN_NAMES.len());
    }

    #[test]
    fn named_constructors_cover_every_built_in_payload() {
        use crate::agents::tasks::FinishReason;
        use crate::agents::PolicyViolation;
        use crate::providers::{RequestErrorKind, TokenUsage, ToolDeclineKind};

        let empty = serde_json::json!({});
        let cases = vec![
            (Event::run_started(), empty.clone()),
            (
                Event::run_finished(FinishReason::Drained),
                serde_json::json!({ "outcome": "drained" }),
            ),
            (Event::task_created(), empty.clone()),
            (Event::task_started(), empty.clone()),
            (Event::task_finished(), empty.clone()),
            (Event::task_failed(), empty.clone()),
            (Event::turn_started(), empty.clone()),
            (
                Event::request_started("model"),
                serde_json::json!({ "model": "model" }),
            ),
            (
                Event::request_finished(
                    "model",
                    TokenUsage {
                        input_tokens: 3,
                        output_tokens: 5,
                    },
                ),
                serde_json::json!({
                    "model": "model",
                    "usage": { "input_tokens": 3, "output_tokens": 5 },
                }),
            ),
            (
                Event::request_failed("model", RequestErrorKind::ConnectionFailed, "offline"),
                serde_json::json!({
                    "model": "model",
                    "kind": "connection_failed",
                    "message": "offline",
                }),
            ),
            (
                Event::request_retried("model", 2, 4, RequestErrorKind::RateLimited, "later"),
                serde_json::json!({
                    "model": "model",
                    "attempt": 2,
                    "max_attempts": 4,
                    "kind": "rate_limited",
                    "message": "later",
                }),
            ),
            (
                Event::text_chunk_received("hello"),
                serde_json::json!({ "content": "hello" }),
            ),
            (
                Event::tool_call_repaired("grep", "c-1", "value_mistyped", "fixed"),
                serde_json::json!({
                    "tool_name": "grep",
                    "call_id": "c-1",
                    "kind": "value_mistyped",
                    "message": "fixed",
                }),
            ),
            (
                Event::tool_call_declined("grep", ToolDeclineKind::AlreadyDelivered),
                serde_json::json!({ "tool_name": "grep", "kind": "already_delivered" }),
            ),
            (
                Event::tool_call_started("grep", "c-1", serde_json::json!({ "q": "x" })),
                serde_json::json!({
                    "tool_name": "grep",
                    "call_id": "c-1",
                    "input": { "q": "x" },
                }),
            ),
            (
                Event::tool_call_finished("done"),
                serde_json::json!({ "output": "done" }),
            ),
            (
                Event::tool_call_failed("nope"),
                serde_json::json!({ "kind": "execution_failed", "message": "nope" }),
            ),
            (
                Event::knowledge_written("notes"),
                serde_json::json!({ "slug": "notes" }),
            ),
            (
                Event::knowledge_read("notes"),
                serde_json::json!({ "slug": "notes" }),
            ),
            (
                Event::knowledge_removed("notes"),
                serde_json::json!({ "slug": "notes" }),
            ),
            (Event::knowledge_listed(), empty),
            (
                Event::knowledge_failed("read", "notes", "not_found", "missing"),
                serde_json::json!({
                    "action": "read",
                    "slug": "notes",
                    "kind": "not_found",
                    "message": "missing",
                }),
            ),
            (
                Event::policy_violated(PolicyViolation::Turns, 10),
                serde_json::json!({ "policy": "turns", "limit": 10 }),
            ),
            (
                Event::schema_retried(2, 4, "schema_failed", "invalid"),
                serde_json::json!({
                    "attempt": 2,
                    "max_attempts": 4,
                    "kind": "schema_failed",
                    "message": "invalid",
                }),
            ),
            (
                Event::compaction_started("proactive", 5),
                serde_json::json!({ "trigger": "proactive", "total": 5 }),
            ),
            (
                Event::compaction_progress("proactive", 2, 5),
                serde_json::json!({ "trigger": "proactive", "completed": 2, "total": 5 }),
            ),
            (
                Event::compaction_finished("proactive"),
                serde_json::json!({ "trigger": "proactive" }),
            ),
            (
                Event::compaction_failed("reactive", "summarization_failed", "bad reply"),
                serde_json::json!({
                    "trigger": "reactive",
                    "kind": "summarization_failed",
                    "message": "bad reply",
                }),
            ),
        ];

        assert_eq!(cases.len(), Event::BUILTIN_NAMES.len());
        for ((event, expected_data), expected_name) in cases.into_iter().zip(Event::BUILTIN_NAMES) {
            assert_eq!(event.get_name(), *expected_name);
            assert_eq!(event.get_data(), &expected_data, "{}", event.get_name());
        }
    }

    #[test]
    fn a_logged_event_keeps_its_name() {
        for name in Event::BUILTIN_NAMES {
            let event = Event::new(*name).task_id("t-1").agent_id("agent");
            let line = serde_json::to_value(&event).unwrap();
            assert_eq!(line["name"].as_str(), Some(*name));
        }
    }

    #[test]
    fn event_builders_and_readers_follow_record_names() {
        let event = Event::new("document_indexed")
            .data(serde_json::json!({ "documents": 42 }))
            .task_id("t-1")
            .agent_id("indexer-1");
        assert_eq!(event.get_name(), "document_indexed");
        assert_eq!(event.get_data(), &serde_json::json!({ "documents": 42 }));
        assert_eq!(event.get_task_id(), "t-1");
        assert_eq!(event.get_agent_id(), "indexer-1");
        assert_eq!(event.get_label(), None);
        assert_eq!(event.get_created_at(), 0);
    }

    #[test]
    fn new_event_records_serialize_with_name_and_data() {
        let event = Event::new("document_indexed")
            .data(serde_json::json!({ "documents": 42 }))
            .task_id("t-1")
            .agent_id("indexer-1");
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "name": "document_indexed",
                "data": { "documents": 42 },
                "task_id": "t-1",
                "agent_id": "indexer-1",
                "created_at": 0,
            })
        );
    }

    #[test]
    fn directive_is_optional_and_round_trips_at_the_top_level() {
        let plain = Event::new(Event::TOOL_CALL_FAILED);
        assert_eq!(plain.get_directive(), None);
        assert!(serde_json::to_value(&plain)
            .unwrap()
            .get("directive")
            .is_none());

        let event = plain.directive("tool_timed_out");
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["directive"], "tool_timed_out");
        let restored: Event = serde_json::from_value(value).unwrap();
        assert_eq!(restored.get_directive(), Some("tool_timed_out"));
    }

    #[test]
    fn terminal_tool_details_round_trip_inside_data() {
        let event = Event::new(Event::TOOL_CALL_FINISHED).data(serde_json::json!({
            "output": "saved",
            "output_path": "tasks/t-1/outputs/c-1.txt",
            "repairs": ["/count retyped"],
        }));

        let value = serde_json::to_value(&event).unwrap();
        let restored: Event = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(value["data"], event.get_data().clone());
        assert_eq!(restored.get_data(), event.get_data());
    }

    #[test]
    fn task_key_is_not_a_deserialization_alias() {
        let event: Event = serde_json::from_value(serde_json::json!({
            "name": "document_indexed",
            "task_key": "t-1",
        }))
        .unwrap();

        assert_eq!(event.get_task_id(), "");
    }

    #[test]
    fn legacy_flattened_built_in_events_still_load() {
        let event: Event = serde_json::from_value(serde_json::json!({
            "event": "request_finished",
            "model": "small",
            "usage": { "input_tokens": 12, "output_tokens": 3 },
            "task_id": "t-1",
            "agent_id": "worker-1",
            "created_at": 100,
        }))
        .unwrap();
        assert_eq!(event.get_name(), Event::REQUEST_FINISHED);
        assert_eq!(event.get_data()["model"], "small");
        assert_eq!(event.get_data()["usage"]["input_tokens"], 12);
    }

    #[test]
    fn legacy_named_events_still_load() {
        let event: Event = serde_json::from_value(serde_json::json!({
            "event": "document_indexed",
            "data": [1, 2, 3],
            "created_at": 100,
        }))
        .unwrap();
        assert_eq!(event.get_name(), "document_indexed");
        assert_eq!(event.get_data(), &serde_json::json!([1, 2, 3]));
    }
}
