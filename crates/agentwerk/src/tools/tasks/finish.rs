//! Lets an agent write the result for its task and mark it finished, handing
//! the work on to another agent in the same call when it needs to.

use serde_json::Value;

use crate::agents::tasks::{Queue, Task};
use crate::event::ToolFailureKind;
use crate::prompts::directives::{
    DirectiveStore, FINISH_ARGUMENT_BLANK, HANDOVER_RESULT_MISSING, QUEUE_UNAVAILABLE,
};
use crate::schemas::Schema;

use super::super::tool::{retype_message, Tool, ToolContext, ToolResult};
use super::resolve_current_key;

/// The two files the tool is described by.
const DEFINITION: &str = include_str!("finish.tool.md");
const SCHEMA: &str = include_str!("finish.schema.json");

/// Write a task's result and mark it finished, optionally handing
/// follow-up work to another agent.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::FinishTool;
///
/// Agent::new().tool(FinishTool);
/// ```
pub struct FinishTool;

impl From<FinishTool> for Tool {
    /// Unbound: the registered arguments, reading the result out of `result`.
    /// The loop rebinds it to the task's schema at claim.
    fn from(_: FinishTool) -> Tool {
        FinishTool::from_schema(None)
    }
}

impl FinishTool {
    /// The name the model calls, and the name the loop looks the tool up under.
    pub(crate) const NAME: &str = "finish";

    /// The finish tool declaring `schema` as its `result` argument, so
    /// dispatch validates the result before the handler runs and the shape the
    /// model is shown is the shape a call is checked against.
    pub(crate) fn from_schema(schema: Option<Schema>) -> Tool {
        let arguments = schema.as_ref().map(|task| {
            let mut document: Value =
                serde_json::from_str(SCHEMA).expect("finish.schema.json is valid JSON");
            document["properties"]["result"] = task.get_raw_schema().clone();
            document["required"] = serde_json::json!(["result"]);
            document
        });
        let run = move |input: Value, ctx: ToolContext| {
            // `Fn`, not `FnOnce`: the closure cannot move `schema` out.
            let schema = schema.clone();
            async move { finish(&input, &ctx, schema.as_ref()).unwrap_or_else(|failure| failure) }
        };
        let tool = Tool::new(Self::NAME).description(DEFINITION).handler(run);
        match arguments {
            Some(document) => tool.schema(document).build(),
            None => tool.schema(SCHEMA).build(),
        }
    }
}

/// The whole flow behind [`FinishTool::call`], so every argument or queue
/// failure can surface through `?` as the `ToolResult` it reads back as. A
/// success carries the repair notes its result took, for the loop to report.
fn finish(
    input: &Value,
    ctx: &ToolContext,
    schema: Option<&Schema>,
) -> Result<ToolResult, ToolResult> {
    let queue = ctx
        .queue
        .clone()
        .ok_or_else(|| ToolResult::error(ctx.directives.render(QUEUE_UNAVAILABLE, &[])))?;
    let parent_key = resolve_current_key(&queue, ctx)?;
    let agent = ctx.agent_id.clone().unwrap_or_default();

    let result = input.get("result").cloned().unwrap_or(Value::Null);

    let Some(handover) = control_string(input, "handover", &ctx.directives)? else {
        let (_, repaired) = attach_result(&queue, &parent_key, result, schema, &ctx.directives)?;
        mark_finished(&queue, &parent_key, &agent, &ctx.directives)?;
        return Ok(ToolResult::Success {
            content: format!("Task {parent_key} marked finished"),
            offloaded: None,
            repaired,
        });
    };
    hand_over(
        &queue,
        input,
        &parent_key,
        &agent,
        result,
        schema,
        handover.trim().to_string(),
        &ctx.directives,
    )
}

/// The chaining path: attach the parent's result, file the child task under
/// the `handover` label, then finish the parent.
fn hand_over(
    queue: &Queue,
    input: &Value,
    parent_key: &str,
    agent: &str,
    result: Value,
    schema: Option<&Schema>,
    handover: String,
    directives: &DirectiveStore,
) -> Result<ToolResult, ToolResult> {
    // An omitted `task` defaults to the parent result below: the
    // common handoff forwards the finding verbatim.
    let task = control_string(input, "task", directives)?;

    // A task carrying no schema declares no `required`, so nothing else
    // stops a handover that passes on an empty result.
    if matches!(&result, Value::Null) || result.as_str().is_some_and(str::is_empty) {
        return Err(ToolResult::error(
            directives.render(HANDOVER_RESULT_MISSING, &[]),
        ));
    }

    // A schema failure returns here, before any child exists.
    let (validated_result, repaired) =
        attach_result(queue, parent_key, result, schema, directives)?;

    // `{parent_result}` needs a string.
    let parent_result = match validated_result {
        Value::String(s) => s,
        other => serde_json::to_string(&other).unwrap_or_default(),
    };
    let result_path = queue.result_path(parent_key);
    let result_path = result_path.display().to_string();

    let body = match task {
        Some(task) => apply_handover_templates(&task, parent_key, &result_path, &parent_result),
        None => parent_result,
    };
    let body = append_parent_reference(&body, parent_key, &result_path);
    let child = Task::new(body).label(&handover).parent(parent_key);

    // Insert the child BEFORE finishing the parent: the child is already
    // `Todo` when the parent leaves the queue, so a concurrent `pending`
    // check never reads false and `finish` cannot end the chain mid-handover.
    // `parent_key` is resolved and `InProgress`, so `set_finished_by` cannot
    // miss it and leave the inserted child orphaned.
    let child_key = queue.insert(child, agent.to_string());
    mark_finished(queue, parent_key, agent, directives)?;

    Ok(ToolResult::Success {
        content: format!(
            "Task {parent_key} marked finished; handed off to {child_key} (handover: {handover})"
        ),
        offloaded: None,
        repaired,
    })
}

/// Read an optional control argument. Absent and null both mean "not given".
/// A value that is present but blank or not a string is refused rather than
/// read as absent: a finish asked to hand over must not quietly finish
/// without doing so. The one place the rule lives, for the model and for a
/// host calling the tool directly.
fn control_string(
    input: &Value,
    key: &str,
    directives: &DirectiveStore,
) -> Result<Option<String>, ToolResult> {
    let Some(value) = input.get(key).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    match value.as_str() {
        Some(text) if !text.trim().is_empty() => Ok(Some(text.to_string())),
        _ => Err(ToolResult::error(directives.render(
            FINISH_ARGUMENT_BLANK,
            &[("argument", key), ("value", &value.to_string())],
        ))),
    }
}

fn mark_finished(
    queue: &Queue,
    key: &str,
    agent: &str,
    directives: &DirectiveStore,
) -> Result<(), ToolResult> {
    queue
        .set_finished_by(key, agent)
        .map_err(|error| ToolResult::error(super::task_error_message(error, directives)))
}

/// Reserved placeholders substituted into the child task's `task`
/// string at handover time: `{parent_key}`, `{parent_result_path}`, and
/// `{parent_result}`. Single-pass `str::replace` over each in turn, the
/// result last so text it carries is never expanded again; unknown
/// `{name}` placeholders pass through verbatim.
fn apply_handover_templates(
    task: &str,
    parent_key: &str,
    result_path: &str,
    result: &str,
) -> String {
    task.replace("{parent_key}", parent_key)
        .replace("{parent_result_path}", result_path)
        .replace("{parent_result}", result)
}

/// End the child's body with where the work came from, so the receiving
/// agent can read the whole result even when the body carries a summary
/// of it or something else entirely.
fn append_parent_reference(body: &str, parent_key: &str, result_path: &str) -> String {
    format!("{body}\n\nHanded over from {parent_key}, result file: {result_path}")
}

/// Validate the result against the task's schema, attach it, and give back
/// a note for every repair it took to get there, so a prompt or a schema that
/// keeps causing one stays discoverable once the loop reports them. A result
/// that failed carries no notes: the violations already say what was wrong.
/// The task is not finished here, since a handover inserts its child first.
fn attach_result(
    queue: &Queue,
    key: &str,
    result: Value,
    schema: Option<&Schema>,
    directives: &DirectiveStore,
) -> Result<(Value, Vec<String>), ToolResult> {
    let (validated, repaired) = queue.set_result(key, result).map_err(|violations| {
        // Composed here, where the task's schema is known: the loop
        // passes the content to the model as-is.
        ToolResult::Error {
            content: crate::prompts::arguments_retry_detail(
                FinishTool::NAME,
                &violations.to_string(),
                schema.map(Schema::get_raw_schema),
                directives,
            ),
            kind: ToolFailureKind::SchemaValidationFailed,
        }
    })?;
    let notes = repaired.iter().map(|pointer| retype_message(pointer));
    Ok((validated, notes.collect()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::agents::tasks::{Queue, Status, Task};
    use crate::agents::Query;
    use crate::schemas::Schema;

    fn ctx_with(queue: Arc<Queue>, agent: &str, dir: PathBuf) -> ToolContext {
        ToolContext::new(dir)
            .queue(queue)
            .agent_id(agent.to_string())
    }

    fn one_task(agent: &str) -> (Arc<Queue>, String) {
        let queue = Queue::new();
        queue.set_dir(shared_test_dir().to_path_buf());
        queue.insert(Task::new("body").label(agent), "tester".into());
        let key = queue
            .claim(&Query::from("status = Todo"), agent)
            .expect("claim must succeed");
        (queue, key)
    }

    /// Read a task's result back from `tasks/<key>/result.json`, or `None`
    /// when it wrote none.
    fn read_result(dir: &std::path::Path, key: &str) -> Option<serde_json::Value> {
        let path = dir.join("tasks").join(key).join("result.json");
        let body = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&body).ok()
    }

    /// Process-lifetime tempdir used as the default `Queue` root
    /// for tests in this module. Tests that need an isolated workspace
    /// still call `queue.set_dir(...)` explicitly to override.
    fn shared_test_dir() -> &'static std::path::Path {
        use std::sync::OnceLock;
        static DIR: OnceLock<crate::test_util::TempDir> = OnceLock::new();
        DIR.get_or_init(|| crate::test_util::TempDir::new().unwrap())
            .path()
    }

    fn line_schema() -> Schema {
        Schema::new(serde_json::json!({
            "type": "object",
            "properties": { "line": { "type": "integer" } },
            "required": ["line"],
        }))
        .unwrap()
    }

    /// Claim a task carrying an integer-typed `line`.
    fn line_task(dir: &std::path::Path) -> Arc<Queue> {
        let queue = Queue::new();
        queue.set_dir(dir.to_path_buf());
        queue.insert(
            Task::new("body").schema(line_schema()).label("alice"),
            "tester".into(),
        );
        queue
            .claim(&Query::from("status = Todo"), "alice")
            .expect("claim must succeed");
        queue
    }

    /// The finish tool as the loop binds it at claim.
    fn finish_for(schema: Schema) -> Tool {
        FinishTool::from_schema(Some(schema))
    }

    #[tokio::test]
    async fn finish_notes_every_repair_its_result_needed() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = line_task(dir.path());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = finish_for(line_schema())
            .call(serde_json::json!({"result": {"line": "42"}}), &ctx)
            .await;

        assert!(
            matches!(
                &outcome,
                ToolResult::Success { repaired, .. } if repaired == &vec!["/line retyped".to_string()]
            ),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn finish_notes_no_repair_for_a_result_it_rejected() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = line_task(dir.path());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        // `line` is beyond repair.
        let outcome = finish_for(line_schema())
            .call(serde_json::json!({"result": {"line": "about 42"}}), &ctx)
            .await;

        assert!(matches!(
            outcome,
            ToolResult::Error {
                kind: ToolFailureKind::SchemaValidationFailed,
                ..
            }
        ));
    }

    // Argument shape

    fn object_schema() -> Schema {
        Schema::new(serde_json::json!({
            "type": "object",
            "properties": { "status": { "type": "string" }, "note": { "type": "string" } },
            "required": ["status"],
        }))
        .unwrap()
    }

    fn string_schema() -> Schema {
        Schema::new(serde_json::json!({ "type": "string" })).unwrap()
    }

    #[test]
    fn a_bound_task_schema_is_the_result_argument() {
        let declared = finish_for(object_schema())
            .input_schema()
            .get_raw_schema()
            .clone();
        assert_eq!(
            declared["properties"]["result"],
            *object_schema().get_raw_schema()
        );
        assert_eq!(declared["properties"]["handover"]["type"], "string");
        assert_eq!(declared["required"], serde_json::json!(["result"]));
        // A task field never reaches the top level, whatever it is called.
        assert!(declared["properties"].get("status").is_none(), "{declared}");
    }

    #[test]
    fn a_scalar_task_schema_is_the_result_argument_too() {
        let declared = finish_for(string_schema())
            .input_schema()
            .get_raw_schema()
            .clone();
        assert_eq!(
            declared["properties"]["result"],
            *string_schema().get_raw_schema()
        );
    }

    /// An object schema declaring a property `finish` also declares as a
    /// control key. Nesting keeps the two apart.
    fn colliding_schema() -> Schema {
        Schema::new(serde_json::json!({
            "type": "object",
            "properties": { "handover": { "type": "string" } },
            "required": ["handover"],
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn a_result_field_named_like_a_control_key_survives_the_finish() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(dir.path().to_path_buf());
        queue.insert(
            Task::new("body").schema(colliding_schema()).label("alice"),
            "tester".into(),
        );
        let key = queue
            .claim(&Query::from("status = Todo"), "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": {"handover": "x"}}), &ctx)
            .await;

        assert!(matches!(outcome, ToolResult::Success { .. }), "{outcome:?}");
        let task = queue.get_task(&key).unwrap();
        assert_eq!(task.status, Status::Finished);
        assert_eq!(
            task.result.as_ref(),
            Some(&serde_json::json!({"handover": "x"})),
        );
        assert!(queue.get_task("t-2").is_none(), "no child filed");
    }

    #[tokio::test]
    async fn a_double_encoded_result_decodes_through_the_bound_schema() {
        // The whole reason the flat shape existed: a model that writes the
        // object as JSON text. Dispatch decodes it against the nested schema.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, key) = one_task("alice");
        queue.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let mut registry = crate::tools::ToolRegistry::default();
        registry.register(FinishTool::from_schema(Some(object_schema())));

        let calls = vec![crate::tools::ToolCall {
            id: "call-1".to_string(),
            name: "finish".to_string(),
            input: serde_json::json!({ "result": "{\"status\": \"malicious\"}" }),
        }];
        let outcome = registry.execute(&calls, &ctx).await.remove(0);

        assert!(matches!(outcome, ToolResult::Success { .. }), "{outcome:?}");
        assert_eq!(
            queue.get_task(&key).unwrap().result.as_ref(),
            Some(&serde_json::json!({ "status": "malicious" }))
        );
    }

    #[test]
    fn an_unbound_finish_declares_the_registered_arguments() {
        let unbound = Tool::from(FinishTool);
        assert!(unbound.input_schema().get_raw_schema()["properties"]["result"].is_object());
        assert_eq!(
            FinishTool::from_schema(None)
                .input_schema()
                .get_raw_schema(),
            unbound.input_schema().get_raw_schema()
        );
    }

    #[tokio::test]
    async fn writes_string_result_and_marks_finished() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, key) = one_task("alice");
        queue.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": "the answer"}), &ctx)
            .await;
        assert!(matches!(outcome, ToolResult::Success { .. }));
        let t = queue.get_task(&key).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(
            t.result.as_ref().and_then(|v| v.as_str()),
            Some("the answer")
        );

        assert_eq!(read_result(dir.path(), &key), Some("the answer".into()));
    }

    #[tokio::test]
    async fn a_result_is_written_to_the_task_folder() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, key) = one_task("alice");
        queue.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        Tool::from(FinishTool)
            .call(serde_json::json!({"result": {"x": 1}}), &ctx)
            .await;

        let path = dir.path().join("tasks").join(&key).join("result.json");
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed, serde_json::json!({"x": 1}));

        // One home on disk: the task record no longer carries the result.
        let record =
            std::fs::read_to_string(dir.path().join("tasks").join(&key).join("task.json")).unwrap();
        let record: serde_json::Value = serde_json::from_str(&record).unwrap();
        assert!(record.get("result").is_none(), "{record}");
    }

    #[tokio::test]
    async fn a_reloaded_queue_reads_the_result_back() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, key) = one_task("alice");
        queue.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        Tool::from(FinishTool)
            .call(serde_json::json!({"result": "the answer"}), &ctx)
            .await;

        let reloaded = Queue::load(dir.path()).unwrap();
        let task = reloaded.get_task(&key).unwrap();
        assert_eq!(
            task.result.as_ref().and_then(|v| v.as_str()),
            Some("the answer")
        );
        assert_eq!(task.status, Status::Finished);
    }

    #[tokio::test]
    async fn accepts_any_value_when_no_schema() {
        for value in [
            serde_json::json!(""),
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!([]),
        ] {
            let dir = crate::test_util::TempDir::new().unwrap();
            let (queue, key) = one_task("alice");
            queue.set_dir(dir.path().to_path_buf());
            let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
            let outcome = Tool::from(FinishTool)
                .call(serde_json::json!({"result": value}), &ctx)
                .await;
            assert!(
                matches!(outcome, ToolResult::Success { .. }),
                "expected success for {value:?}"
            );
            let t = queue.get_task(&key).unwrap();
            assert_eq!(t.status, Status::Finished);
        }
    }

    #[tokio::test]
    async fn accepts_structured_value_when_no_schema() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, key) = one_task("alice");
        queue.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": {"x": 1, "y": [2, 3]}}), &ctx)
            .await;
        assert!(matches!(outcome, ToolResult::Success { .. }));
        let t = queue.get_task(&key).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref().unwrap()["x"], 1);

        // The stored result is a JSON object, not an escaped string of JSON.
        let result = read_result(dir.path(), &key).unwrap();
        assert!(result.is_object(), "expected raw object, got {result}");
        assert_eq!(result["x"], 1);
    }

    #[tokio::test]
    async fn validates_against_schema() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(shared_test_dir().to_path_buf());
        queue.set_dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        }))
        .unwrap();
        queue.insert(
            Task::new("hi").schema(schema).label("alice"),
            "tester".into(),
        );
        let key = queue
            .claim(&Query::from("status = Todo"), "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        // An object where a string belongs: no retype recovers it, unlike a
        // quoted scalar.
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": {"x": {}}}), &ctx)
            .await;
        assert!(matches!(
            outcome,
            ToolResult::Error {
                kind: ToolFailureKind::SchemaValidationFailed,
                ..
            }
        ));
        let t = queue.get_task(&key).unwrap();
        assert_eq!(t.status, Status::InProgress);

        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": {"x": "ok"}}), &ctx)
            .await;
        assert!(matches!(outcome, ToolResult::Success { .. }));
        let t = queue.get_task(&key).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref().unwrap()["x"], "ok");
    }

    #[tokio::test]
    async fn an_object_result_is_stored_as_the_object() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(shared_test_dir().to_path_buf());
        queue.set_dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        }))
        .unwrap();
        queue.insert(
            Task::new("hi").schema(schema.clone()).label("alice"),
            "tester".into(),
        );
        let key = queue
            .claim(&Query::from("status = Todo"), "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = finish_for(schema)
            .call(serde_json::json!({"result": {"x": "ok"}}), &ctx)
            .await;
        assert!(matches!(outcome, ToolResult::Success { .. }));
        let t = queue.get_task(&key).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref().unwrap(), &serde_json::json!({"x": "ok"}));
    }

    #[tokio::test]
    async fn stores_a_string_encoded_result_as_the_decoded_object() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(shared_test_dir().to_path_buf());
        queue.set_dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        }))
        .unwrap();
        queue.insert(
            Task::new("hi").schema(schema).label("alice"),
            "tester".into(),
        );
        let key = queue
            .claim(&Query::from("status = Todo"), "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        // The agent double-encoded the conforming object as a JSON string.
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": "{\"x\": \"ok\"}"}), &ctx)
            .await;
        assert!(matches!(outcome, ToolResult::Success { .. }));

        let t = queue.get_task(&key).unwrap();
        assert_eq!(t.status, Status::Finished);
        // Stored as the decoded object, not the raw string.
        assert!(t.result.as_ref().unwrap().is_object());
        assert_eq!(t.result.as_ref().unwrap()["x"], "ok");
        assert!(read_result(dir.path(), &key).unwrap().is_object());
    }

    #[tokio::test]
    async fn errors_when_no_current_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(shared_test_dir().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": "x"}), &ctx)
            .await;
        assert!(matches!(outcome, ToolResult::Error { .. }));
    }

    #[tokio::test]
    async fn appends_one_line_per_completed_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(shared_test_dir().to_path_buf());
        queue.set_dir(dir.path().to_path_buf());

        queue.insert(Task::new("a").label("alice"), "tester".into());
        let key1 = queue
            .claim(&Query::from("t-1"), "alice")
            .expect("claim must succeed");
        let ctx_alice = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        Tool::from(FinishTool)
            .call(serde_json::json!({"result": "from alice"}), &ctx_alice)
            .await;

        queue.insert(Task::new("b").label("bob"), "tester".into());
        let key2 = queue
            .claim(&Query::from("t-2"), "bob")
            .expect("claim must succeed");
        let ctx_bob = ctx_with(Arc::clone(&queue), "bob", dir.path().to_path_buf());
        Tool::from(FinishTool)
            .call(serde_json::json!({"result": "from bob"}), &ctx_bob)
            .await;

        assert_eq!(read_result(dir.path(), &key1), Some("from alice".into()));
        assert_eq!(read_result(dir.path(), &key2), Some("from bob".into()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writes_produce_one_intact_line_per_task() {
        const N: usize = 32;
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(shared_test_dir().to_path_buf());
        queue.set_dir(dir.path().to_path_buf());

        let mut expected = Vec::with_capacity(N);
        for i in 0..N {
            let agent = format!("agent_{i}");
            queue.insert(
                Task::new(format!("body_{i}")).label(&agent),
                "tester".into(),
            );
            let key = queue
                .claim(
                    &Query::from(format!("status = Todo AND label = {agent}")),
                    &agent,
                )
                .expect("claim must succeed");
            expected.push((agent, key));
        }

        let mut handles = Vec::with_capacity(N);
        for (i, (agent, _)) in expected.iter().enumerate() {
            let queue = Arc::clone(&queue);
            let dir_path = dir.path().to_path_buf();
            let agent = agent.clone();
            handles.push(tokio::spawn(async move {
                let ctx = ctx_with(queue, &agent, dir_path);
                Tool::from(FinishTool)
                    .call(serde_json::json!({"result": format!("payload_{i}")}), &ctx)
                    .await
            }));
        }
        for h in handles {
            assert!(matches!(h.await.unwrap(), ToolResult::Success { .. }));
        }

        // Every finish appends to the one shared log, so a torn line here is
        // two agents writing over each other.
        let log = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let mut finished = std::collections::HashSet::new();
        for line in log.lines() {
            let parsed: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("corrupt line {line:?}: {e}"));
            if parsed["event"] == "task_finished" {
                let task = parsed["task_key"].as_str().unwrap().to_string();
                assert!(finished.insert(task), "duplicate task in log");
            }
        }
        let expected_keys: std::collections::HashSet<String> =
            expected.iter().map(|(_, k)| k.clone()).collect();
        assert_eq!(finished, expected_keys);
    }

    // Handover

    fn one_task_in(agent: &str, dir: PathBuf) -> (Arc<Queue>, String) {
        let queue = Queue::new();
        queue.set_dir(dir);
        queue.insert(Task::new("parent body").label(agent), "tester".into());
        let key = queue
            .claim(&Query::from("status = Todo"), agent)
            .expect("claim must succeed");
        (queue, key)
    }

    #[tokio::test]
    async fn handover_finishes_parent_creates_child_with_parent_link() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": "bob",
                    "task": "continue with X",
                    "result": "summary of alice's work"
                }),
                &ctx,
            )
            .await;
        assert!(matches!(outcome, ToolResult::Success { .. }));

        let parent = queue.get_task(&parent_key).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref().and_then(|v| v.as_str()),
            Some("summary of alice's work")
        );

        let child = queue.get_task("t-2").unwrap();
        assert_eq!(child.status, Status::Todo);
        assert_eq!(child.parent.as_deref(), Some(parent_key.as_str()));
        assert_eq!(child.label.as_deref(), Some("bob"));
        assert_eq!(child.reporter, "alice");
    }

    #[tokio::test]
    async fn handover_child_takes_the_schema_bound_to_its_label() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, _parent_key) = one_task_in("alice", dir.path().to_path_buf());
        let schemas = crate::schemas::SchemaStore::new();
        schemas
            .label(
                "bob",
                serde_json::json!({"type": "object", "title": "verdict"}),
            )
            .unwrap();
        queue.set_schemas(&schemas);
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({"handover": "bob", "result": "a lead worth tracing"}),
                &ctx,
            )
            .await;

        // `finish` cannot attach a schema to the child it inserts, so the
        // child is born without one and picks the label's up at claim.
        assert!(queue.get_task("t-2").unwrap().schema.is_none());

        queue.claim(&Query::from("bob"), "bob");
        let bound = queue.get_task("t-2").unwrap().schema.unwrap();
        assert_eq!(title_of(&bound), "verdict");
    }

    /// The `title` a schema was built with, which names it in an assertion.
    fn title_of(schema: &Schema) -> String {
        serde_json::to_value(schema).unwrap()["title"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test]
    async fn handover_appends_one_ndjson_line_for_parent_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": "done part 1"}),
                &ctx,
            )
            .await;

        assert_eq!(
            read_result(dir.path(), &parent_key),
            Some("done part 1".into())
        );
        assert_eq!(
            read_result(dir.path(), "t-2"),
            None,
            "only the parent finish writes a result"
        );
    }

    #[tokio::test]
    async fn handover_schema_violation_aborts_atomically() {
        // A short string passes the type check and fails `minLength`, which is
        // the abort path this exercises.
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "string",
            "minLength": 50
        }))
        .unwrap();
        queue.insert(
            Task::new("strict parent").schema(schema).label("alice"),
            "tester".into(),
        );
        let parent_key = queue
            .claim(&Query::from("status = Todo"), "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": "too short"}),
                &ctx,
            )
            .await;
        assert!(matches!(
            outcome,
            ToolResult::Error {
                kind: ToolFailureKind::SchemaValidationFailed,
                ..
            }
        ));

        let parent = queue.get_task(&parent_key).unwrap();
        assert_eq!(parent.status, Status::InProgress);
        assert!(parent.result.is_none());
        assert!(
            queue.get_task("t-2").is_none(),
            "no child created on schema failure"
        );
        assert_eq!(read_result(dir.path(), &parent_key), None);
    }

    /// Build a claimed parent whose own schema requires an object with a
    /// `status` field, so the handover result is validated structurally.
    fn strict_object_schema() -> Schema {
        Schema::new(serde_json::json!({
            "type": "object",
            "required": ["status"]
        }))
        .unwrap()
    }

    fn one_task_with_object_schema(agent: &str, dir: PathBuf) -> (Arc<Queue>, String) {
        let queue = Queue::new();
        queue.set_dir(dir);
        queue.insert(
            Task::new("strict parent")
                .schema(strict_object_schema())
                .label(agent),
            "tester".into(),
        );
        let key = queue
            .claim(&Query::from("status = Todo"), agent)
            .expect("claim must succeed");
        (queue, key)
    }

    #[tokio::test]
    async fn handover_structured_result_validated_against_parent_schema_is_stored_as_object() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": {"status": "done"}}),
                &ctx,
            )
            .await
            ;
        assert!(matches!(outcome, ToolResult::Success { .. }));

        let parent = queue.get_task(&parent_key).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
        assert!(queue.get_task("t-2").is_some());

        assert_eq!(
            read_result(dir.path(), &parent_key),
            Some(serde_json::json!({"status": "done"}))
        );
    }

    #[tokio::test]
    async fn handover_takes_an_object_result_alongside_the_control_keys() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = finish_for(strict_object_schema())
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": {"status": "done"}}),
                &ctx,
            )
            .await;
        assert!(matches!(outcome, ToolResult::Success { .. }));

        let parent = queue.get_task(&parent_key).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
    }

    #[tokio::test]
    async fn handover_double_encoded_structured_result_is_decoded_to_object() {
        // The agent double-encodes the object as a JSON string; the parent
        // schema's validation decodes it so the stored value is the object.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": "{\"status\":\"done\"}"}),
                &ctx,
            )
            .await
            ;

        let parent = queue.get_task(&parent_key).unwrap();
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
    }

    #[tokio::test]
    async fn handover_object_schema_violation_aborts_atomically() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": {"wrong": 1}}),
                &ctx,
            )
            .await;
        assert!(matches!(
            outcome,
            ToolResult::Error {
                kind: ToolFailureKind::SchemaValidationFailed,
                ..
            }
        ));

        let parent = queue.get_task(&parent_key).unwrap();
        assert_eq!(parent.status, Status::InProgress);
        assert!(parent.result.is_none());
        assert!(
            queue.get_task("t-2").is_none(),
            "no child created on schema failure"
        );
        assert_eq!(read_result(dir.path(), &parent_key), None);
    }

    #[tokio::test]
    async fn omitted_handover_finishes_without_a_child() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": "done"}), &ctx)
            .await;
        assert!(matches!(outcome, ToolResult::Success { .. }));
        assert_eq!(
            queue.get_task(&parent_key).unwrap().status,
            Status::Finished
        );
        assert!(queue.get_task("t-2").is_none());
    }

    #[tokio::test]
    async fn omitted_task_defaults_child_body_to_the_parent_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, _key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({"handover": "bob", "result": "alice's findings"}),
                &ctx,
            )
            .await;
        assert!(matches!(outcome, ToolResult::Success { .. }), "{outcome:?}");

        let child = queue.get_task("t-2").unwrap();
        assert!(child_body(&child).starts_with("alice's findings"));
    }

    #[tokio::test]
    async fn handover_ends_the_child_body_with_the_parent_key_and_result_file() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({"handover": "bob", "result": "alice's findings"}),
                &ctx,
            )
            .await;

        let child = queue.get_task("t-2").unwrap();
        let body = child_body(&child);
        assert!(body.contains(&parent_key), "{body}");
        assert!(
            body.contains(&queue.result_path(&parent_key).display().to_string()),
            "{body}"
        );
    }

    #[tokio::test]
    async fn handover_substitutes_the_parent_result_file_in_the_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": "bob",
                    "task": "Read {parent_result_path} and continue",
                    "result": "alice's findings"
                }),
                &ctx,
            )
            .await;

        let child = queue.get_task("t-2").unwrap();
        let path = queue.result_path(&parent_key);
        assert!(
            child_body(&child).starts_with(&format!("Read {} and continue", path.display())),
            "{}",
            child_body(&child)
        );
    }

    /// The child's task as text, which every handover writes as a string.
    fn child_body(child: &Task) -> String {
        child.task.as_str().expect("a task string").to_string()
    }

    #[tokio::test]
    async fn a_handover_with_nothing_to_pass_on_is_rejected() {
        // Absent, null, and empty all leave the receiving agent a body that
        // says nothing, and this task carries no schema to require one.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, _key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        for body in [
            serde_json::json!({"handover": "bob", "task": "x"}),
            serde_json::json!({"handover": "bob", "task": "x", "result": null}),
            serde_json::json!({"handover": "bob", "task": "x", "result": ""}),
        ] {
            let outcome = Tool::from(FinishTool).call(body, &ctx).await;
            assert!(
                matches!(&outcome, ToolResult::Error { content: message, .. } if message.contains("needs a result")),
                "{outcome:?}",
            );
        }
    }

    #[tokio::test]
    async fn handover_accepts_structured_result_without_schema() {
        // With no parent schema, any JSON value is a valid handoff result
        // and is stored verbatim; only `null`/empty string are rejected.
        for result_value in [
            serde_json::json!(42),
            serde_json::json!([1, 2, 3]),
            serde_json::json!({"k": "v"}),
        ] {
            let dir = crate::test_util::TempDir::new().unwrap();
            let (queue, parent_key) = one_task_in("alice", dir.path().to_path_buf());
            let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

            let outcome = Tool::from(FinishTool)
                .call(
                    serde_json::json!({"handover": "bob", "task": "next", "result": result_value}),
                    &ctx,
                )
                .await;
            assert!(matches!(outcome, ToolResult::Success { .. }));

            let parent = queue.get_task(&parent_key).unwrap();
            assert_eq!(parent.status, Status::Finished);
            assert_eq!(parent.result.as_ref(), Some(&result_value));
            assert!(queue.get_task("t-2").is_some());
        }
    }

    #[test]
    fn a_number_where_the_declared_schema_asks_for_a_string_is_rejected() {
        // Stringifying `42` would pass the check with a task the model never
        // wrote; the violation names the field to fix instead.
        let schema = Tool::from(FinishTool).input_schema().clone();
        let violations = schema
            .validate(serde_json::json!({"handover": "bob", "task": 42, "result": "ok"}))
            .unwrap_err();
        assert!(
            violations.iter().any(|v| v.instance_path == "/task"),
            "{violations}"
        );
    }

    #[tokio::test]
    async fn a_blank_handover_from_a_direct_call_is_an_error() {
        // Model or host, a caller asking to hand over must hear a refusal
        // rather than finish without the handover it asked for.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        for body in [
            serde_json::json!({"handover": "  ", "result": "x"}),
            serde_json::json!({"handover": "bob", "task": "  ", "result": "x"}),
            serde_json::json!({"handover": 7, "result": "x"}),
        ] {
            let outcome = Tool::from(FinishTool).call(body, &ctx).await;
            assert!(
                matches!(&outcome, ToolResult::Error { content: message, .. } if message.contains("non-blank")),
                "{outcome:?}",
            );
        }
        assert_eq!(
            queue.get_task(&parent_key).unwrap().status,
            Status::InProgress,
        );
    }

    #[tokio::test]
    async fn handover_errors_when_no_current_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({"handover": "bob", "task": "x", "result": "y"}),
                &ctx,
            )
            .await;
        assert!(matches!(outcome, ToolResult::Error { .. }));
    }

    #[tokio::test]
    async fn substitutes_parent_key_and_result_in_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": "bob",
                    "task": "Continue {parent_key}: {parent_result}",
                    "result": "alice's findings"
                }),
                &ctx,
            )
            .await;

        let child = queue.get_task("t-2").unwrap();
        assert!(child_body(&child).starts_with(&format!("Continue {parent_key}: alice's findings")));
    }

    #[tokio::test]
    async fn unknown_placeholders_pass_through() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": "bob",
                    "task": "See {parent_key} and {unknown}",
                    "result": "ok"
                }),
                &ctx,
            )
            .await;

        let child = queue.get_task("t-2").unwrap();
        assert!(child_body(&child).starts_with(&format!("See {parent_key} and {{unknown}}")));
    }

    #[tokio::test]
    async fn substitution_is_single_pass() {
        // A `result` that itself contains the literal text `{parent_key}`
        // must NOT be re-expanded: the substitution pass runs once
        // per placeholder, not recursively.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": "bob",
                    "task": "[{parent_result}]",
                    "result": "{parent_key}"
                }),
                &ctx,
            )
            .await;

        let child = queue.get_task("t-2").unwrap();
        assert!(
            child_body(&child).starts_with("[{parent_key}]"),
            "result containing `{{parent_key}}` should be inserted literally, \
             not recursively expanded (parent_key was {parent_key})",
        );
    }
}
