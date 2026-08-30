//! Insights into the lifecycle and activities of an agent's work.

use std::sync::Arc;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// One thing that happened as agents work. Agent and task attribution are
/// optional and independent.
///
/// ```no_run
/// use agentwerk::{Event, Queue};
/// use serde_json::json;
///
/// let tasks = Queue::new();
/// tasks.emit_event(
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
    pub const RUN_STARTED: &'static str = "run_started";
    pub const RUN_FINISHED: &'static str = "run_finished";
    pub const TASK_CREATED: &'static str = "task_created";
    pub const TASK_STARTED: &'static str = "task_started";
    pub const TASK_FINISHED: &'static str = "task_finished";
    pub const TASK_FAILED: &'static str = "task_failed";
    pub const TURN_STARTED: &'static str = "turn_started";
    pub const REQUEST_STARTED: &'static str = "request_started";
    pub const REQUEST_FINISHED: &'static str = "request_finished";
    pub const REQUEST_FAILED: &'static str = "request_failed";
    pub const REQUEST_RETRIED: &'static str = "request_retried";
    pub const TEXT_CHUNK_RECEIVED: &'static str = "text_chunk_received";
    pub const TOOL_CALL_REPAIRED: &'static str = "tool_call_repaired";
    pub const TOOL_CALL_DECLINED: &'static str = "tool_call_declined";
    pub const TOOL_CALL_STARTED: &'static str = "tool_call_started";
    pub const TOOL_CALL_FINISHED: &'static str = "tool_call_finished";
    pub const TOOL_CALL_FAILED: &'static str = "tool_call_failed";
    pub const FILE_OPEN_FINISHED: &'static str = "file_open_finished";
    pub const FILE_OPEN_FAILED: &'static str = "file_open_failed";
    pub const KNOWLEDGE_WRITTEN: &'static str = "knowledge_written";
    pub const KNOWLEDGE_READ: &'static str = "knowledge_read";
    pub const KNOWLEDGE_REMOVED: &'static str = "knowledge_removed";
    pub const KNOWLEDGE_LISTED: &'static str = "knowledge_listed";
    pub const KNOWLEDGE_FAILED: &'static str = "knowledge_failed";
    pub const POLICY_VIOLATED: &'static str = "policy_violated";
    pub const SCHEMA_RETRIED: &'static str = "schema_retried";
    pub const COMPACTION_STARTED: &'static str = "compaction_started";
    pub const COMPACTION_PROGRESS: &'static str = "compaction_progress";
    pub const COMPACTION_FINISHED: &'static str = "compaction_finished";
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
        Self::FILE_OPEN_FINISHED,
        Self::FILE_OPEN_FAILED,
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
/// It prints task lifecycle, tool activity, limit breaches, and failed
/// requests to stderr, and drops the rest.
pub fn default_logger() -> Arc<dyn Fn(&Event) + Send + Sync> {
    Arc::new(|event: &Event| {
        let agent = &event.agent_id;
        match event.name.as_str() {
            Event::RUN_STARTED => eprintln!("run started"),
            Event::RUN_FINISHED => match data_str(event, "outcome") {
                Some(outcome) => eprintln!("run finished: {outcome}"),
                None => eprintln!("run finished"),
            },
            Event::TASK_CREATED => eprintln!("[{agent}] created {}", event.task_id),
            Event::TASK_STARTED => eprintln!("[{agent}] started {}", event.task_id),
            Event::TASK_FINISHED => eprintln!("[{agent}] finished {}", event.task_id),
            Event::TASK_FAILED => eprintln!("[{agent}] failed {}", event.task_id),
            Event::TOOL_CALL_STARTED => {
                if let (Some(tool_name), Some(input)) =
                    (data_str(event, "tool_name"), event.data.get("input"))
                {
                    eprintln!("[{agent}] {tool_name}({})", compact_input(input));
                }
            }
            Event::TOOL_CALL_FAILED => {
                if let (Some(tool_name), Some(reason), Some(message)) = (
                    data_str(event, "tool_name"),
                    data_str(event, "kind"),
                    data_str(event, "message"),
                ) {
                    eprintln!("[{agent}] {tool_name} failed ({reason}): {message}");
                }
            }
            Event::REQUEST_FAILED => {
                if let Some(message) = data_str(event, "message") {
                    eprintln!("[{agent}] request failed: {message}");
                }
            }
            Event::REQUEST_RETRIED | Event::SCHEMA_RETRIED => {
                if let (Some(attempt), Some(max_attempts), Some(message)) = (
                    data_u64(event, "attempt"),
                    data_u64(event, "max_attempts"),
                    data_str(event, "message"),
                ) {
                    let prefix = match event.name.as_str() {
                        Event::REQUEST_RETRIED => "retry",
                        _ => "schema retry",
                    };
                    eprintln!("[{agent}] {prefix} {attempt}/{max_attempts}: {message}");
                }
            }
            Event::POLICY_VIOLATED => {
                if let (Some(policy), Some(limit)) =
                    (data_str(event, "policy"), data_u64(event, "limit"))
                {
                    eprintln!("[{agent}] policy violated: {policy} limit={limit}");
                }
            }
            Event::COMPACTION_STARTED => {
                if let (Some(reason), Some(total)) =
                    (data_str(event, "trigger"), data_u64(event, "total"))
                {
                    eprintln!("[{agent}] compacting context ({reason}): {total} chunks");
                }
            }
            Event::COMPACTION_PROGRESS => {
                if let (Some(reason), Some(completed), Some(total)) = (
                    data_str(event, "trigger"),
                    data_u64(event, "completed"),
                    data_u64(event, "total"),
                ) {
                    eprintln!("[{agent}] compaction progress ({reason}): {completed}/{total}");
                }
            }
            Event::COMPACTION_FINISHED => {
                if let Some(trigger) = data_str(event, "trigger") {
                    eprintln!("[{agent}] context compacted ({trigger})");
                }
            }
            Event::COMPACTION_FAILED => {
                if let (Some(trigger), Some(message)) =
                    (data_str(event, "trigger"), data_str(event, "message"))
                {
                    eprintln!("[{agent}] compaction failed ({trigger}): {message}");
                }
            }
            _ => {}
        }
    })
}

fn data_str<'a>(event: &'a Event, key: &str) -> Option<&'a str> {
    event.data.get(key)?.as_str()
}

fn data_u64(event: &Event, key: &str) -> Option<u64> {
    event.data.get(key)?.as_u64()
}

fn compact_input(input: &serde_json::Value) -> String {
    let one_line = input.to_string().replace('\n', " ");
    const MAX: usize = 80;
    if one_line.chars().count() <= MAX {
        one_line
    } else {
        let cut: String = one_line.chars().take(MAX).collect();
        format!("{cut}…")
    }
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
    fn built_in_event_names_are_unique() {
        let names: BTreeSet<&str> = Event::BUILTIN_NAMES.iter().copied().collect();
        assert_eq!(names.len(), Event::BUILTIN_NAMES.len());
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

        let event = plain.directive("command_timed_out");
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["directive"], "command_timed_out");
        let restored: Event = serde_json::from_value(value).unwrap();
        assert_eq!(restored.get_directive(), Some("command_timed_out"));
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
