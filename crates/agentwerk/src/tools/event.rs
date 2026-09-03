//! Lets an agent publish an event, with `task_finished` additionally recording
//! its result and completing the current task.

use serde_json::Value;

use crate::agents::tasks::{Status, Task, TaskError, Werk};
use crate::event::Event;
use crate::prompts::directives::{
    DirectiveStore, HANDOVER_RESULT_MISSING, HANDOVER_SCHEMA_INVALID, WERK_UNAVAILABLE,
};
use crate::schemas::Schema;

use super::task::{resolve_current_id, task_error_message};
use super::tool::{retype_message, Tool, ToolContext};

const DEFINITION: &str = include_str!("event.tool.md");
const SCHEMA: &str = include_str!("event.schema.json");
const FINISH_SCHEMA: &str = include_str!("task/finish.schema.json");

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
                .unwrap_or_else(|event| *event)
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

/// Publish one event. `task_finished` uses the task transition so
/// validation, persistence, handover ordering, and observers stay in one path.
pub(super) fn dispatch(
    input: &Value,
    ctx: &ToolContext,
    schema: Option<&Schema>,
    handover: Option<&Task>,
    tool_name: &str,
) -> Result<Event, Box<Event>> {
    let werk = ctx.werk.clone().ok_or_else(|| {
        Event::error(ctx.directives.render(WERK_UNAVAILABLE, &[])).directive(WERK_UNAVAILABLE)
    })?;
    let name = input["name"].as_str().unwrap_or_default();
    let data = input
        .get("data")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    if name == Event::TASK_FINISHED {
        return finish(&werk, &data, ctx, schema, handover, tool_name);
    }

    let task_id = ctx.task_id.as_deref().unwrap_or_default();
    let agent_id = ctx.agent_id.as_deref().unwrap_or_default();
    let acknowledgement = event_directive(name, &data, &ctx.directives);
    let mut event = Event::new(name)
        .data(data)
        .task_id(task_id)
        .agent_id(agent_id);
    if acknowledgement.is_some() {
        event = event.directive(name);
    }
    werk.emit_event(event);

    Ok(match acknowledgement {
        Some(content) => Event::success(content).directive(name),
        None => Event::success(format!("Event {name} published")),
    })
}

/// Render an explicit application-event override from its JSON payload. The
/// complete payload is `{data}`; top-level object fields are variables of
/// their own.
fn event_directive(name: &str, data: &Value, directives: &DirectiveStore) -> Option<String> {
    let mut owned = vec![("data".to_string(), json_template_value(data))];
    if let Some(fields) = data.as_object() {
        owned.extend(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), json_template_value(value))),
        );
    }
    let values: Vec<(&str, &str)> = owned
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    directives.render_override(name, &values)
}

fn json_template_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        value => value.to_string(),
    }
}

fn finish(
    werk: &Werk,
    input: &Value,
    ctx: &ToolContext,
    schema: Option<&Schema>,
    configured_handover: Option<&Task>,
    tool_name: &str,
) -> Result<Event, Box<Event>> {
    let parent_id = resolve_current_id(werk, ctx)?;
    let agent = ctx.agent_id.clone().unwrap_or_default();
    let result = input.get("result").cloned().unwrap_or(Value::Null);

    let handover = resolve_handover(input, configured_handover, &ctx.directives)?;
    let parent = werk.get_task(&parent_id).ok_or_else(|| {
        let error = TaskError::TaskNotFound {
            id: parent_id.clone(),
        };
        Event::error(task_error_message(error, &ctx.directives))
    })?;
    if handover.is_some() && !parent.is_in_progress() {
        let error = TaskError::TransitionRejected {
            from: parent.get_status(),
            to: Status::Finished,
        };
        return Err(Event::error(task_error_message(error, &ctx.directives)).into());
    }
    let completion = CompletionContext {
        werk,
        schema,
        tool_name,
        directives: &ctx.directives,
    };
    let Some(mut child) = handover else {
        let (_, repaired) = completion.attach_result(&parent_id, result)?;
        completion.mark_finished(&parent_id, &agent)?;
        let mut event = Event::success(format!("Task {parent_id} marked finished"));
        event.prepend_repairs(repaired);
        return Ok(event);
    };
    completion.hand_over(&parent_id, &agent, result, &mut child)
}

struct CompletionContext<'a> {
    werk: &'a Werk,
    schema: Option<&'a Schema>,
    tool_name: &'a str,
    directives: &'a DirectiveStore,
}

impl CompletionContext<'_> {
    fn hand_over(
        &self,
        parent_id: &str,
        agent: &str,
        result: Value,
        child: &mut Task,
    ) -> Result<Event, Box<Event>> {
        if matches!(&result, Value::Null) || result.as_str().is_some_and(str::is_empty) {
            return Err(Event::error(self.directives.render(HANDOVER_RESULT_MISSING, &[])).into());
        }

        let (validated_result, repaired) = self.attach_result(parent_id, result)?;
        let parent_result = match validated_result {
            Value::String(s) => s,
            other => serde_json::to_string(&other).unwrap_or_default(),
        };
        let result_path = self.werk.result_path(parent_id).display().to_string();
        apply_handover_templates(&mut child.task, parent_id, &result_path, &parent_result);
        child.parent = Some(parent_id.to_string());
        let handover = child.label.clone().expect("resolved handover has a label");

        let child_id = self.werk.insert(child.clone(), agent.to_string());
        self.mark_finished(parent_id, agent)?;

        let mut event = Event::success(format!(
            "Task {parent_id} marked finished; handed off to {child_id} (handover: {handover})"
        ));
        event.prepend_repairs(repaired);
        Ok(event)
    }

    fn mark_finished(&self, id: &str, agent: &str) -> Result<(), Box<Event>> {
        self.werk
            .set_finished_by(id, agent)
            .map_err(|error| Event::error(task_error_message(error, self.directives)).into())
    }

    fn attach_result(&self, id: &str, result: Value) -> Result<(Value, Vec<String>), Box<Event>> {
        let (validated, repaired) = self.werk.set_result(id, result).map_err(|violations| {
            Event::tool_failure(
                crate::prompts::arguments_retry_detail(
                    self.tool_name,
                    &violations.to_string(),
                    self.schema.map(Schema::get_raw_schema),
                    self.directives,
                ),
                "schema_failed",
            )
        })?;
        let notes = repaired.iter().map(|pointer| retype_message(pointer));
        Ok((validated, notes.collect()))
    }
}

fn resolve_handover(
    input: &Value,
    configured: Option<&Task>,
    directives: &DirectiveStore,
) -> Result<Option<Task>, Box<Event>> {
    let Some(value) = input.get("handover") else {
        return Ok(configured.cloned());
    };
    let Some(fields) = value.as_object() else {
        return Err(Event::error("`handover` must be an object").into());
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
            return Err(
                Event::error("`label` cannot replace the configured handover label").into(),
            );
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

fn required_label(fields: &serde_json::Map<String, Value>) -> Result<String, Box<Event>> {
    fields
        .get("label")
        .and_then(Value::as_str)
        .filter(|label| !label.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| Event::error("`label` is required").into())
}

fn invalid_handover_schema(error: &str, directives: &DirectiveStore) -> Event {
    Event::error(directives.render(HANDOVER_SCHEMA_INVALID, &[("error", error)]))
        .directive(HANDOVER_SCHEMA_INVALID)
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::agents::tasks::Task;
    use crate::agents::Query;

    fn claimed_task() -> (crate::test_util::TempDir, Arc<Werk>, String, ToolContext) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        let werk = Werk::new();
        werk.set_dir(path.clone());
        werk.insert(Task::new("work").label("alice"), "tester".into());
        let id = werk
            .claim(&Query::from("task.status = todo"), "alice")
            .expect("claim must succeed");
        let ctx = ToolContext::new(path)
            .werk(Arc::clone(&werk))
            .task_id(id.clone())
            .agent_id("alice".into());
        (dir, werk, id, ctx)
    }

    #[tokio::test]
    async fn a_custom_event_reaches_observers_with_call_context() {
        let (_dir, werk, id, ctx) = claimed_task();
        let seen = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&seen);
        werk.on_event(move |_, event| {
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
        assert_eq!(outcome.get_content(), "Event candidate_found published");
        assert_eq!(outcome.get_directive(), None);
        let event = seen.lock().unwrap().clone().expect("event observed");
        assert_eq!(event.get_task_id(), id);
        assert_eq!(event.get_agent_id(), "alice");
        assert_eq!(event.get_label(), Some("alice"));
        assert_eq!(event.get_data()["line"], 42);
        assert_eq!(werk.stats.event_count("candidate_found"), 1);
        let persisted = werk
            .find_event(r#"event.name = "candidate_found""#)
            .expect("event persisted to the session log");
        assert_eq!(persisted.get_task_id(), id);
        assert_eq!(persisted.get_data()["path"], "src/auth.rs");
        assert!(werk.get_task(&id).unwrap().is_in_progress());
    }

    #[tokio::test]
    async fn a_custom_event_override_binds_its_data_and_marks_the_events() {
        let (_dir, werk, _id, ctx) = claimed_task();
        let mut directives = DirectiveStore::default();
        directives.insert(
            "candidate_found",
            "Found {path} at {line} with {meta}; keep {missing}. Payload: {data}",
        );
        let ctx = ctx.directives(Arc::new(directives));

        let outcome = Tool::from(EventTool)
            .call(
                serde_json::json!({
                    "name": "candidate_found",
                    "data": {
                        "data": "shadow",
                        "path": "src/auth.rs",
                        "line": 42,
                        "meta": {"reviewed": true}
                    }
                }),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_directive(), Some("candidate_found"));
        assert!(outcome
            .get_content()
            .starts_with("Found src/auth.rs at 42 with {\"reviewed\":true}; keep {missing}."));
        assert!(outcome.get_content().contains("\"path\":\"src/auth.rs\""));
        assert!(outcome.get_content().contains("\"data\":\"shadow\""));
        assert_eq!(
            werk.find_event(r#"event.name = "candidate_found""#)
                .unwrap()
                .get_directive(),
            Some("candidate_found"),
        );
    }

    #[tokio::test]
    async fn a_catalogue_key_used_as_an_event_name_keeps_the_generic_acknowledgement() {
        let (_dir, _werk, _id, ctx) = claimed_task();

        let outcome = Tool::from(EventTool)
            .call(
                serde_json::json!({ "name": "grep_failed", "data": { "path": "src" } }),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_content(), "Event grep_failed published");
        assert_eq!(outcome.get_directive(), None);
    }

    #[tokio::test]
    async fn a_nonterminal_builtin_event_does_not_change_task_state() {
        let (_dir, werk, id, ctx) = claimed_task();

        Tool::from(EventTool)
            .call(
                serde_json::json!({ "name": Event::TASK_FAILED, "data": { "reason": "reported" } }),
                &ctx,
            )
            .await;

        assert!(werk.get_task(&id).unwrap().is_in_progress());
        assert_eq!(
            werk.find_event("event.name = task_failed")
                .unwrap()
                .get_data()["reason"],
            "reported"
        );
    }

    #[tokio::test]
    async fn task_finished_records_the_result_and_transitions_once() {
        let (_dir, werk, id, ctx) = claimed_task();
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
        let task = werk.get_task(&id).unwrap();
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
            werk.get_task(&id).unwrap().get_result(),
            Some(&serde_json::json!({ "verdict": "safe" }))
        );
        assert_eq!(werk.find_events("event.name = task_finished").len(), 1);
    }

    #[tokio::test]
    async fn task_finished_does_not_use_an_event_name_override() {
        let (_dir, werk, id, ctx) = claimed_task();
        let mut directives = DirectiveStore::default();
        directives.insert(Event::TASK_FINISHED, "keep working");
        let ctx = ctx.directives(Arc::new(directives));

        let outcome = Tool::from(EventTool)
            .call(
                serde_json::json!({
                    "name": Event::TASK_FINISHED,
                    "data": {"result": "done"}
                }),
                &ctx,
            )
            .await;

        assert!(werk.get_task(&id).unwrap().is_finished());
        assert!(!outcome.get_content().contains("keep working"));
    }

    #[tokio::test]
    async fn task_finished_hands_work_over_through_its_data() {
        let (_dir, werk, id, ctx) = claimed_task();

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
        assert!(werk.get_task(&id).unwrap().is_finished());
        let child = werk.get_task("t-2").expect("handover child created");
        assert_eq!(child.get_parent(), Some(id.as_str()));
        assert_eq!(child.get_label(), Some("review"));
        assert!(child
            .get_task()
            .as_str()
            .unwrap_or_default()
            .contains(&werk.result_path(&id).display().to_string()));
    }

    #[tokio::test]
    async fn task_finished_uses_the_configured_handover_from_result_only() {
        let (_dir, werk, id, ctx) = claimed_task();
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
        assert!(werk.get_task(&id).unwrap().is_finished());
        let child = werk.get_task("t-2").expect("handover child created");
        assert_eq!(child.get_label(), Some("review"));
        assert_eq!(child.get_task()["source"], id);
        assert!(child.get_schema().is_some());
    }

    #[tokio::test]
    async fn task_finished_repairs_its_bound_result_schema() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": { "line": { "type": "integer" } },
            "required": ["line"]
        }))
        .unwrap();
        werk.insert(
            Task::new("work").schema(schema.clone()).label("alice"),
            "tester".into(),
        );
        let id = werk
            .claim(&Query::from("task.status = todo"), "alice")
            .expect("claim must succeed");
        let ctx = ToolContext::new(dir.path().to_path_buf())
            .werk(Arc::clone(&werk))
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
        assert!(werk.get_task(&id).unwrap().is_in_progress());

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
            werk.get_task(&id).unwrap().get_result().unwrap()["line"],
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
