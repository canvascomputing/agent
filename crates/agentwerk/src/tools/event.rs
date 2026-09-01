//! Lets an agent publish an event, with `task_finished` additionally recording
//! its result and completing the current task.

use serde_json::Value;

use crate::agents::tasks::{Queue, Status, Task, TaskError};
use crate::event::Event;
use crate::prompts::directives::{
    DirectiveStore, HANDOVER_RESULT_MISSING, HANDOVER_SCHEMA_INVALID, QUEUE_UNAVAILABLE,
};
use crate::schemas::Schema;

use super::tasks::{resolve_current_id, task_error_message};
use super::tool::{retype_message, Tool, ToolContext};

const DEFINITION: &str = include_str!("event.tool.md");
const SCHEMA: &str = include_str!("event.schema.json");
const FINISH_SCHEMA: &str = include_str!("tasks/finish.schema.json");

/// Publish an event for the current task and agent. A `task_finished` event
/// records the task's result and completes it.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::EventTool;
///
/// Agent::new().tool(EventTool);
/// ```
pub struct EventTool;

impl From<EventTool> for Tool {
    fn from(_: EventTool) -> Tool {
        EventTool::from_schema(None, None)
    }
}

impl EventTool {
    pub(crate) const NAME: &str = "event";

    /// Bind the current task's result schema inside `data.result` while
    /// leaving every non-terminal event's data unconstrained.
    pub(crate) fn from_schema(schema: Option<Schema>, handover: Option<Task>) -> Tool {
        let mut document: Value =
            serde_json::from_str(SCHEMA).expect("event.schema.json is valid JSON");
        document["allOf"][0]["then"]["properties"]["data"] =
            task_finished_schema(schema.as_ref(), handover.as_ref());

        let run = move |input: Value, ctx: ToolContext| {
            let schema = schema.clone();
            let handover = handover.clone();
            async move {
                dispatch(
                    &input,
                    &ctx,
                    schema.as_ref(),
                    handover.as_ref(),
                    EventTool::NAME,
                )
                .unwrap_or_else(|failure| failure)
            }
        };
        Tool::new(Self::NAME)
            .description(DEFINITION)
            .schema(document)
            .handler_with_context(run)
    }
}

/// The `finish` arguments, also used as `task_finished` event data.
pub(super) fn task_finished_schema(schema: Option<&Schema>, handover: Option<&Task>) -> Value {
    let mut document: Value =
        serde_json::from_str(FINISH_SCHEMA).expect("finish.schema.json is valid JSON");
    if let Some(task) = schema {
        document["properties"]["result"] = task.get_raw_schema().clone();
    }
    if schema.is_some() || handover.is_some() {
        document["required"] = serde_json::json!(["result"]);
    }
    let handover_schema = &mut document["properties"]["handover"];
    if let Some(task) = handover {
        handover_schema["properties"]["label"]["const"] =
            serde_json::json!(task.get_label().expect("configured handover has a label"));
    } else {
        handover_schema["required"] = serde_json::json!(["label", "task"]);
    }
    document
}

/// Publish one event. `task_finished` routes through the task transition so
/// validation, persistence, handover ordering, and observers stay in one path.
pub(super) fn dispatch(
    input: &Value,
    ctx: &ToolContext,
    schema: Option<&Schema>,
    handover: Option<&Task>,
    tool_name: &str,
) -> Result<Event, Event> {
    let queue = ctx.queue.clone().ok_or_else(|| {
        Event::error(ctx.directives.render(QUEUE_UNAVAILABLE, &[])).directive(QUEUE_UNAVAILABLE)
    })?;
    let name = input["name"].as_str().unwrap_or_default();
    let data = input
        .get("data")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    if name == Event::TASK_FINISHED {
        return finish(&queue, &data, ctx, schema, handover, tool_name);
    }

    let task_id = ctx.task_id.as_deref().unwrap_or_default();
    let agent_id = ctx.agent_id.as_deref().unwrap_or_default();
    queue.emit_event(
        Event::new(name)
            .data(data)
            .task_id(task_id)
            .agent_id(agent_id),
    );
    Ok(Event::success(format!("Event {name} published")))
}

fn finish(
    queue: &Queue,
    input: &Value,
    ctx: &ToolContext,
    schema: Option<&Schema>,
    configured_handover: Option<&Task>,
    tool_name: &str,
) -> Result<Event, Event> {
    let parent_id = resolve_current_id(queue, ctx)?;
    let agent = ctx.agent_id.clone().unwrap_or_default();
    let result = input.get("result").cloned().unwrap_or(Value::Null);

    let handover = resolve_handover(input, configured_handover, &ctx.directives)?;
    let parent = queue.get_task(&parent_id).ok_or_else(|| {
        let error = TaskError::TaskMissing {
            id: parent_id.clone(),
        };
        Event::error(task_error_message(error, &ctx.directives))
    })?;
    if handover.is_some() && !parent.is_in_progress() {
        let error = TaskError::TransitionRejected {
            from: parent.get_status(),
            to: Status::Finished,
        };
        return Err(Event::error(task_error_message(error, &ctx.directives)));
    }
    let Some(mut child) = handover else {
        let (_, repaired) = attach_result(
            queue,
            &parent_id,
            result,
            schema,
            tool_name,
            &ctx.directives,
        )?;
        mark_finished(queue, &parent_id, &agent, &ctx.directives)?;
        let mut event = Event::success(format!("Task {parent_id} marked finished"));
        event.prepend_repairs(repaired);
        return Ok(event);
    };
    hand_over(
        queue,
        &parent_id,
        &agent,
        result,
        schema,
        tool_name,
        &mut child,
        &ctx.directives,
    )
}

fn hand_over(
    queue: &Queue,
    parent_id: &str,
    agent: &str,
    result: Value,
    schema: Option<&Schema>,
    tool_name: &str,
    child: &mut Task,
    directives: &DirectiveStore,
) -> Result<Event, Event> {
    if matches!(&result, Value::Null) || result.as_str().is_some_and(str::is_empty) {
        return Err(Event::error(
            directives.render(HANDOVER_RESULT_MISSING, &[]),
        ));
    }

    let (validated_result, repaired) =
        attach_result(queue, parent_id, result, schema, tool_name, directives)?;
    let parent_result = match validated_result {
        Value::String(s) => s,
        other => serde_json::to_string(&other).unwrap_or_default(),
    };
    let result_path = queue.result_path(parent_id).display().to_string();
    apply_handover_templates(&mut child.task, parent_id, &result_path, &parent_result);
    child.parent = Some(parent_id.to_string());
    let handover = child.label.clone().expect("resolved handover has a label");

    let child_id = queue.insert(child.clone(), agent.to_string());
    mark_finished(queue, parent_id, agent, directives)?;

    let mut event = Event::success(format!(
        "Task {parent_id} marked finished; handed off to {child_id} (handover: {handover})"
    ));
    event.prepend_repairs(repaired);
    Ok(event)
}

fn resolve_handover(
    input: &Value,
    configured: Option<&Task>,
    directives: &DirectiveStore,
) -> Result<Option<Task>, Event> {
    let Some(value) = input.get("handover") else {
        return Ok(configured.cloned());
    };
    let Some(fields) = value.as_object() else {
        return Err(Event::error("`handover` must be an object"));
    };

    let mut child = match configured {
        Some(task) => task.clone(),
        None => {
            let label = required_label(fields)?;
            let task = fields
                .get("task")
                .cloned()
                .ok_or_else(|| Event::error("`task` is required"))?;
            Task::new(task).label(label)
        }
    };

    if let Some(label) = fields.get("label") {
        let label = label
            .as_str()
            .filter(|label| !label.trim().is_empty())
            .ok_or_else(|| Event::error("`label` must be a non-blank string"))?;
        if child.get_label() != Some(label) {
            return Err(Event::error(
                "`label` cannot replace the configured handover label",
            ));
        }
    }
    if let Some(task) = fields.get("task") {
        child.task = task.clone();
    }
    if let Some(document) = fields.get("schema") {
        child.schema = Some(
            Schema::new(document.clone())
                .map_err(|error| invalid_handover_schema(&error.to_string(), directives))?,
        );
    }
    Ok(Some(child))
}

fn required_label(fields: &serde_json::Map<String, Value>) -> Result<String, Event> {
    fields
        .get("label")
        .and_then(Value::as_str)
        .filter(|label| !label.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| Event::error("`label` is required"))
}

fn invalid_handover_schema(error: &str, directives: &DirectiveStore) -> Event {
    Event::error(directives.render(HANDOVER_SCHEMA_INVALID, &[("error", error)]))
        .directive(HANDOVER_SCHEMA_INVALID)
}

fn mark_finished(
    queue: &Queue,
    id: &str,
    agent: &str,
    directives: &DirectiveStore,
) -> Result<(), Event> {
    queue
        .set_finished_by(id, agent)
        .map_err(|error| Event::error(task_error_message(error, directives)))
}

fn apply_handover_templates(task: &mut Value, parent_id: &str, result_path: &str, result: &str) {
    match task {
        Value::String(text) => {
            *text = substitute_handover_text(text, parent_id, result_path, result);
        }
        Value::Array(values) => {
            for value in values {
                apply_handover_templates(value, parent_id, result_path, result);
            }
        }
        Value::Object(fields) => {
            for value in fields.values_mut() {
                apply_handover_templates(value, parent_id, result_path, result);
            }
        }
        _ => {}
    }
}

fn substitute_handover_text(
    text: &str,
    parent_id: &str,
    result_path: &str,
    result: &str,
) -> String {
    let replacements = [
        ("{parent_id}", parent_id),
        ("{parent_result_path}", result_path),
        ("{parent_result}", result),
    ];
    let mut output = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(start) = remaining.find('{') {
        output.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        match replacements
            .iter()
            .find(|(placeholder, _)| remaining.starts_with(placeholder))
        {
            Some((placeholder, value)) => {
                output.push_str(value);
                remaining = &remaining[placeholder.len()..];
            }
            None => {
                output.push('{');
                remaining = &remaining[1..];
            }
        }
    }
    output.push_str(remaining);
    output
}

fn attach_result(
    queue: &Queue,
    id: &str,
    result: Value,
    schema: Option<&Schema>,
    tool_name: &str,
    directives: &DirectiveStore,
) -> Result<(Value, Vec<String>), Event> {
    let (validated, repaired) = queue.set_result(id, result).map_err(|violations| {
        Event::tool_failure(
            crate::prompts::arguments_retry_detail(
                tool_name,
                &violations.to_string(),
                schema.map(Schema::get_raw_schema),
                directives,
            ),
            "schema_failed",
        )
    })?;
    let notes = repaired.iter().map(|pointer| retype_message(pointer));
    Ok((validated, notes.collect()))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::agents::tasks::Task;
    use crate::agents::Query;

    fn claimed_task() -> (crate::test_util::TempDir, Arc<Queue>, String, ToolContext) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        let queue = Queue::new();
        queue.set_dir(path.clone());
        queue.insert(Task::new("work").label("alice"), "tester".into());
        let id = queue
            .claim(&Query::from("status = Todo"), "alice")
            .expect("claim must succeed");
        let ctx = ToolContext::new(path)
            .queue(Arc::clone(&queue))
            .task_id(id.clone())
            .agent_id("alice".into());
        (dir, queue, id, ctx)
    }

    #[tokio::test]
    async fn a_custom_event_reaches_observers_with_call_context() {
        let (_dir, queue, id, ctx) = claimed_task();
        let seen = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&seen);
        queue.on_event(move |_, event| {
            if event.get_name() == "candidate_found" {
                *captured.lock().unwrap() = Some(event.clone());
            }
        });

        let outcome = Tool::from(EventTool)
            .call(
                serde_json::json!({
                    "name": "candidate_found",
                    "data": { "path": "src/auth.rs", "line": 42 }
                }),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_name(), Event::TOOL_CALL_FINISHED);
        let event = seen.lock().unwrap().clone().expect("event observed");
        assert_eq!(event.get_task_id(), id);
        assert_eq!(event.get_agent_id(), "alice");
        assert_eq!(event.get_label(), Some("alice"));
        assert_eq!(event.get_data()["line"], 42);
        assert_eq!(queue.stats.event_count("candidate_found"), 1);
        let persisted = queue
            .find_event(r#"event = "candidate_found""#)
            .expect("event persisted to the session log");
        assert_eq!(persisted.get_task_id(), id);
        assert_eq!(persisted.get_data()["path"], "src/auth.rs");
        assert!(queue.get_task(&id).unwrap().is_in_progress());
    }

    #[tokio::test]
    async fn a_nonterminal_builtin_event_does_not_change_task_state() {
        let (_dir, queue, id, ctx) = claimed_task();

        Tool::from(EventTool)
            .call(
                serde_json::json!({ "name": Event::TASK_FAILED, "data": { "reason": "reported" } }),
                &ctx,
            )
            .await;

        assert!(queue.get_task(&id).unwrap().is_in_progress());
        assert_eq!(
            queue.find_event(Event::TASK_FAILED).unwrap().get_data()["reason"],
            "reported"
        );
    }

    #[tokio::test]
    async fn task_finished_records_the_result_and_transitions_once() {
        let (_dir, queue, id, ctx) = claimed_task();
        let tool = Tool::from(EventTool);

        let outcome = tool
            .call(
                serde_json::json!({
                    "name": Event::TASK_FINISHED,
                    "data": { "result": { "verdict": "safe" } }
                }),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_name(), Event::TOOL_CALL_FINISHED);
        let task = queue.get_task(&id).unwrap();
        assert!(task.is_finished());
        assert_eq!(
            task.get_result(),
            Some(&serde_json::json!({ "verdict": "safe" }))
        );

        let second = tool
            .call(
                serde_json::json!({
                    "name": Event::TASK_FINISHED,
                    "data": { "result": { "verdict": "unsafe" } }
                }),
                &ctx,
            )
            .await;

        assert_eq!(second.get_name(), Event::TOOL_CALL_FINISHED);
        assert_eq!(
            queue.get_task(&id).unwrap().get_result(),
            Some(&serde_json::json!({ "verdict": "safe" }))
        );
        assert_eq!(queue.find_events(Event::TASK_FINISHED).len(), 1);
    }

    #[tokio::test]
    async fn task_finished_hands_work_over_through_its_data() {
        let (_dir, queue, id, ctx) = claimed_task();

        let outcome = Tool::from(EventTool)
            .call(
                serde_json::json!({
                    "name": Event::TASK_FINISHED,
                    "data": {
                        "result": "a lead",
                        "handover": {
                            "label": "review",
                            "task": "Check {parent_result_path}."
                        }
                    }
                }),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_name(), Event::TOOL_CALL_FINISHED);
        assert!(queue.get_task(&id).unwrap().is_finished());
        let child = queue.get_task("t-2").expect("handover child created");
        assert_eq!(child.get_parent(), Some(id.as_str()));
        assert_eq!(child.get_label(), Some("review"));
        assert!(child
            .get_task()
            .as_str()
            .unwrap_or_default()
            .contains(&queue.result_path(&id).display().to_string()));
    }

    #[tokio::test]
    async fn task_finished_uses_the_configured_handover_from_result_only() {
        let (_dir, queue, id, ctx) = claimed_task();
        let child_schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"verdict": {"type": "string"}}
        }))
        .unwrap();
        let tool = EventTool::from_schema(
            None,
            Some(
                Task::labeled("review", serde_json::json!({"source": "{parent_id}"}))
                    .schema(child_schema),
            ),
        );

        let outcome = tool
            .call(
                serde_json::json!({
                    "name": Event::TASK_FINISHED,
                    "data": {"result": "a lead"}
                }),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_name(), Event::TOOL_CALL_FINISHED);
        assert!(queue.get_task(&id).unwrap().is_finished());
        let child = queue.get_task("t-2").expect("handover child created");
        assert_eq!(child.get_label(), Some("review"));
        assert_eq!(child.get_task()["source"], id);
        assert!(child.get_schema().is_some());
    }

    #[tokio::test]
    async fn task_finished_repairs_its_bound_result_schema() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": { "line": { "type": "integer" } },
            "required": ["line"]
        }))
        .unwrap();
        queue.insert(
            Task::new("work").schema(schema.clone()).label("alice"),
            "tester".into(),
        );
        let id = queue
            .claim(&Query::from("status = Todo"), "alice")
            .expect("claim must succeed");
        let ctx = ToolContext::new(dir.path().to_path_buf())
            .queue(Arc::clone(&queue))
            .task_id(id.clone())
            .agent_id("alice".into());
        let tool = EventTool::from_schema(Some(schema), None);

        let rejected = tool
            .call(
                serde_json::json!({
                    "name": Event::TASK_FINISHED,
                    "data": { "result": { "line": "forty-two" } }
                }),
                &ctx,
            )
            .await;

        assert_eq!(rejected.get_name(), Event::TOOL_CALL_FAILED);
        assert!(queue.get_task(&id).unwrap().is_in_progress());

        let outcome = tool
            .call(
                serde_json::json!({
                    "name": Event::TASK_FINISHED,
                    "data": { "result": { "line": "42" } }
                }),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_name(), Event::TOOL_CALL_FINISHED);
        assert_eq!(
            queue.get_task(&id).unwrap().get_result().unwrap()["line"],
            42
        );
        assert_eq!(outcome.repairs().collect::<Vec<_>>(), vec!["/line retyped"]);
    }

    #[test]
    fn a_bound_schema_lives_at_task_finished_data_result_only() {
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": { "verdict": { "type": "string" } },
            "required": ["verdict"]
        }))
        .unwrap();
        let tool = EventTool::from_schema(Some(schema), None);
        let declared = tool.get_input_schema().get_raw_schema().clone();

        assert!(declared["properties"]["data"].get("properties").is_none());
        assert_eq!(
            declared["allOf"][0]["then"]["properties"]["data"]["required"],
            serde_json::json!(["result"])
        );
        assert!(
            declared["allOf"][0]["then"]["properties"]["data"]["properties"]["result"]
                ["properties"]["verdict"]
                .is_object()
        );
        assert!(tool
            .get_input_schema()
            .validate(serde_json::json!({
                "name": Event::TASK_FINISHED,
                "data": { "verdict": "safe" }
            }))
            .is_err());
    }

    #[test]
    fn handover_template_replacements_are_not_expanded_again() {
        assert_eq!(
            substitute_handover_text(
                "{parent_id}|{parent_result_path}|{parent_result}",
                "{parent_result}",
                "/tmp/{parent_id}",
                "{parent_result_path}",
            ),
            "{parent_result}|/tmp/{parent_id}|{parent_result_path}"
        );
    }
}
