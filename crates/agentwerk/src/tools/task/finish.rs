//! The `finish` compatibility tool, backed by `EventTool`'s `task_finished` event.

use serde_json::Value;

use crate::event::Event;
use crate::schemas::Schema;

use super::super::event;
use super::super::tool::{Tool, ToolContext};

const DEFINITION: &str = include_str!("finish.tool.md");

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
        FinishTool::from_schema(None, None)
    }
}

impl FinishTool {
    /// The name the model calls, and the name the loop looks the tool up under.
    pub(crate) const NAME: &str = "finish";

    /// Bind object results directly and preserve the legacy envelope for the
    /// other shapes, using `EventTool`'s completion branch for both.
    pub(crate) fn from_schema(schema: Option<Schema>, handover: Option<crate::Task>) -> Tool {
        let envelope = event::task_finished_schema(schema.as_ref(), handover.as_ref());
        let bound_object = schema.as_ref().is_some_and(declares_object);
        let arguments = arguments_schema(schema.as_ref(), envelope.clone());
        let run = move |input: Value, ctx: ToolContext| {
            let schema = schema.clone();
            let handover = handover.clone();
            let envelope = envelope.clone();
            async move {
                let input = normalize_input(input, &envelope, bound_object);
                let event = serde_json::json!({
                    "name": Event::TASK_FINISHED,
                    "data": input,
                });
                event::dispatch(
                    &event,
                    &ctx,
                    schema.as_ref(),
                    handover.as_ref(),
                    FinishTool::NAME,
                )
                .unwrap_or_else(|event| *event)
            }
        };
        Tool::new(Self::NAME)
            .description(DEFINITION)
            .schema(arguments)
            .handler_with_context(run)
    }
}

/// A bound object is the call itself. Scalars and unbound calls retain the
/// legacy envelope; the conditional keeps repair inside its selected branch.
fn arguments_schema(schema: Option<&Schema>, envelope: Value) -> Value {
    if let Some(schema) = schema.filter(|schema| declares_object(schema)) {
        return schema.get_raw_schema().clone();
    }
    let bare = schema
        .map(|schema| schema.get_raw_schema().clone())
        .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
    let mut envelope_cases = vec![serde_json::json!({ "const": {} })];
    envelope_cases.extend(
        envelope["properties"]
            .as_object()
            .expect("finish schema properties are an object")
            .keys()
            .map(|name| serde_json::json!({ "required": [name] })),
    );
    serde_json::json!({
        "type": "object",
        "if": {
            "anyOf": envelope_cases
        },
        "then": envelope,
        "else": {
            "allOf": [
                { "type": "object" },
                bare
            ]
        },
        "examples": [
            { "result": "..." },
            {
                "result": "...",
                "handover": {
                    "label": "...",
                    "task": "..."
                }
            }
        ]
    })
}

fn declares_object(schema: &Schema) -> bool {
    schema.get_raw_schema()["type"] == "object"
}

/// Put a bound object under the event engine's stable `data.result` key.
/// Legacy calls are bare only when non-empty and free of reserved arguments;
/// an empty call still means finishing without a result.
fn normalize_input(input: Value, envelope: &Value, bound_object: bool) -> Value {
    if bound_object {
        return serde_json::json!({ "result": input });
    }
    let envelope_fields = envelope["properties"]
        .as_object()
        .expect("finish schema properties are an object");
    let is_bare = input.as_object().is_some_and(|arguments| {
        !arguments.is_empty()
            && envelope_fields
                .keys()
                .all(|name| !arguments.contains_key(name))
    });
    match is_bare {
        true => serde_json::json!({ "result": input }),
        false => input,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::agents::tasks::{Status, Task, Werk};
    use crate::agents::Query;
    use crate::schemas::Schema;

    fn ctx_with(werk: Arc<Werk>, agent: &str, dir: PathBuf) -> ToolContext {
        ToolContext::new(dir).werk(werk).agent_id(agent.to_string())
    }

    fn one_task(agent: &str) -> (Arc<Werk>, String) {
        let werk = Werk::new();
        werk.set_dir(shared_test_dir().to_path_buf());
        werk.insert(Task::new("body").label(agent), "tester".into());
        let id = werk
            .claim(&Query::from("task.status = todo"), agent)
            .expect("claim must succeed");
        (werk, id)
    }

    /// Read a task's result back from `tasks/<id>/result.json`, or `None`
    /// when it wrote none.
    fn read_result(dir: &std::path::Path, id: &str) -> Option<serde_json::Value> {
        let path = dir.join("tasks").join(id).join("result.json");
        let body = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&body).ok()
    }

    /// Process-lifetime tempdir used as the default `Werk` root
    /// for tests in this module. Tests that need an isolated workspace
    /// still call `werk.set_dir(...)` explicitly to override.
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
    fn line_task(dir: &std::path::Path) -> Arc<Werk> {
        let werk = Werk::new();
        werk.set_dir(dir.to_path_buf());
        werk.insert(
            Task::new("body").schema(line_schema()).label("alice"),
            "tester".into(),
        );
        werk.claim(&Query::from("task.status = todo"), "alice")
            .expect("claim must succeed");
        werk
    }

    /// The finish tool as the loop binds it at claim.
    fn finish_for(schema: Schema) -> Tool {
        FinishTool::from_schema(Some(schema), None)
    }

    #[tokio::test]
    async fn finish_notes_every_repair_its_result_needed() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = line_task(dir.path());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        let outcome = finish_for(line_schema())
            .call(serde_json::json!({"line": "42"}), &ctx)
            .await;

        assert_eq!(outcome.repairs().collect::<Vec<_>>(), vec!["/line retyped"]);
    }

    #[tokio::test]
    async fn finish_notes_no_repair_for_a_result_it_rejected() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = line_task(dir.path());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        // `line` is beyond repair.
        let outcome = finish_for(line_schema())
            .call(serde_json::json!({"line": "about 42"}), &ctx)
            .await;

        assert_eq!(outcome.get_data()["kind"], "schema_failed");
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
    fn a_bound_object_schema_is_the_finish_arguments() {
        let declared = finish_for(object_schema())
            .get_input_schema()
            .get_raw_schema()
            .clone();
        assert_eq!(declared, *object_schema().get_raw_schema());
        assert_eq!(declared["properties"]["status"]["type"], "string");
        assert!(declared["properties"].get("result").is_none());
    }

    #[test]
    fn a_scalar_task_schema_is_the_result_argument_too() {
        let declared = finish_for(string_schema())
            .get_input_schema()
            .get_raw_schema()
            .clone();
        assert_eq!(
            declared["then"]["properties"]["result"],
            *string_schema().get_raw_schema()
        );
        assert_eq!(
            declared["else"]["allOf"][1],
            *string_schema().get_raw_schema()
        );
    }

    #[tokio::test]
    async fn a_bare_object_is_stored_as_the_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, id) = one_task("alice");
        werk.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({"status": "done", "note": "direct"}),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_name(), Event::TOOL_CALL_FINISHED);
        assert_eq!(
            werk.get_task(&id).unwrap().get_result(),
            Some(&serde_json::json!({"status": "done", "note": "direct"}))
        );
    }

    #[tokio::test]
    async fn a_bare_object_uses_bound_schema_repair_and_rejection() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = line_task(dir.path());
        let id = werk
            .find_task("task.status = in_progress")
            .unwrap()
            .get_id()
            .to_string();
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let finish = finish_for(line_schema());
        let rejected = finish
            .invoke(serde_json::json!({"line": "about 42"}), &ctx)
            .await;

        assert_eq!(rejected.get_name(), Event::TOOL_CALL_FAILED);
        assert_eq!(rejected.get_data()["kind"], "schema_failed");
        assert!(werk.get_task(&id).unwrap().is_in_progress());

        let repaired = finish.invoke(serde_json::json!({"line": "42"}), &ctx).await;

        assert_eq!(repaired.get_name(), Event::TOOL_CALL_FINISHED);
        assert_eq!(
            repaired.repairs().collect::<Vec<_>>(),
            vec!["/line retyped"]
        );
        assert_eq!(
            werk.get_task(&id).unwrap().get_result().unwrap()["line"],
            42
        );
    }

    #[tokio::test]
    async fn a_bound_object_rejects_the_legacy_result_wrapper() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = line_task(dir.path());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let outcome = finish_for(line_schema())
            .invoke(serde_json::json!({"result": {"line": 42}}), &ctx)
            .await;

        assert_eq!(outcome.get_name(), Event::TOOL_CALL_FAILED);
        assert_eq!(outcome.get_data()["kind"], "schema_failed");
    }

    #[tokio::test]
    async fn an_empty_call_keeps_its_legacy_null_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, id) = one_task("alice");
        werk.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({}), &ctx)
            .await;

        assert_eq!(outcome.get_name(), Event::TOOL_CALL_FINISHED);
        assert_eq!(
            werk.get_task(&id).unwrap().get_result(),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            werk.find_event("event.name = task_finished")
                .unwrap()
                .get_data()["result"],
            serde_json::Value::Null
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
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        werk.insert(
            Task::new("body").schema(colliding_schema()).label("alice"),
            "tester".into(),
        );
        let id = werk
            .claim(&Query::from("task.status = todo"), "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        let outcome = FinishTool::from_schema(Some(colliding_schema()), None)
            .call(serde_json::json!({"handover": "x"}), &ctx)
            .await;

        assert!(
            outcome.get_name() == Event::TOOL_CALL_FINISHED,
            "{outcome:?}"
        );
        let task = werk.get_task(&id).unwrap();
        assert_eq!(task.status, Status::Finished);
        assert_eq!(
            task.result.as_ref(),
            Some(&serde_json::json!({"handover": "x"})),
        );
        assert!(werk.get_task("t-2").is_none(), "no child filed");
    }

    #[tokio::test]
    async fn a_whole_object_encoded_as_text_decodes_through_the_bound_schema() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, id) = one_task("alice");
        werk.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let outcome = FinishTool::from_schema(Some(object_schema()), None)
            .invoke(serde_json::json!("{\"status\": \"malicious\"}"), &ctx)
            .await;

        assert!(
            outcome.get_name() == Event::TOOL_CALL_FINISHED,
            "{outcome:?}"
        );
        assert_eq!(
            werk.get_task(&id).unwrap().result.as_ref(),
            Some(&serde_json::json!({ "status": "malicious" }))
        );
    }

    #[test]
    fn an_unbound_finish_declares_the_registered_arguments() {
        let unbound = Tool::from(FinishTool);
        assert!(
            unbound.get_input_schema().get_raw_schema()["then"]["properties"]["result"].is_object()
        );
        assert_eq!(
            unbound.get_input_schema().get_raw_schema()["else"]["allOf"][0]["type"],
            "object"
        );
        assert_eq!(
            FinishTool::from_schema(None, None)
                .get_input_schema()
                .get_raw_schema(),
            unbound.get_input_schema().get_raw_schema()
        );
    }

    #[tokio::test]
    async fn writes_string_result_and_marks_finished() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, id) = one_task("alice");
        werk.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": "the answer"}), &ctx)
            .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FINISHED);
        let t = werk.get_task(&id).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(
            t.result.as_ref().and_then(|v| v.as_str()),
            Some("the answer")
        );
        let event = werk.find_event("event.name = task_finished").unwrap();
        assert_eq!(event.get_data()["result"], "the answer");

        assert_eq!(read_result(dir.path(), &id), Some("the answer".into()));
    }

    #[tokio::test]
    async fn a_result_is_written_to_the_task_folder() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, id) = one_task("alice");
        werk.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        Tool::from(FinishTool)
            .call(serde_json::json!({"result": {"x": 1}}), &ctx)
            .await;

        let path = dir.path().join("tasks").join(&id).join("result.json");
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed, serde_json::json!({"x": 1}));

        // One home on disk: the task record no longer carries the result.
        let record =
            std::fs::read_to_string(dir.path().join("tasks").join(&id).join("task.json")).unwrap();
        let record: serde_json::Value = serde_json::from_str(&record).unwrap();
        assert!(record.get("result").is_none(), "{record}");
    }

    #[tokio::test]
    async fn a_reloaded_werk_reads_the_result_back() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, id) = one_task("alice");
        werk.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        Tool::from(FinishTool)
            .call(serde_json::json!({"result": "the answer"}), &ctx)
            .await;

        let reloaded = Werk::load(dir.path()).unwrap();
        let task = reloaded.get_task(&id).unwrap();
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
            serde_json::json!(42),
            serde_json::json!(true),
            serde_json::json!({}),
            serde_json::json!([]),
        ] {
            let dir = crate::test_util::TempDir::new().unwrap();
            let (werk, id) = one_task("alice");
            werk.set_dir(dir.path().to_path_buf());
            let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
            let outcome = Tool::from(FinishTool)
                .call(serde_json::json!({"result": value}), &ctx)
                .await;
            assert!(
                outcome.get_name() == Event::TOOL_CALL_FINISHED,
                "expected success for {value:?}"
            );
            let t = werk.get_task(&id).unwrap();
            assert_eq!(t.status, Status::Finished);
        }
    }

    #[tokio::test]
    async fn accepts_structured_value_when_no_schema() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, id) = one_task("alice");
        werk.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": {"x": 1, "y": [2, 3]}}), &ctx)
            .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FINISHED);
        let t = werk.get_task(&id).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref().unwrap()["x"], 1);

        // The stored result is a JSON object, not an escaped string of JSON.
        let result = read_result(dir.path(), &id).unwrap();
        assert!(result.is_object(), "expected raw object, got {result}");
        assert_eq!(result["x"], 1);
    }

    #[tokio::test]
    async fn validates_against_schema() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(shared_test_dir().to_path_buf());
        werk.set_dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        }))
        .unwrap();
        werk.insert(
            Task::new("hi").schema(schema).label("alice"),
            "tester".into(),
        );
        let id = werk
            .claim(&Query::from("task.status = todo"), "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        // An object where a string belongs: no retype recovers it, unlike a
        // quoted scalar.
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": {"x": {}}}), &ctx)
            .await;
        assert_eq!(outcome.get_data()["kind"], "schema_failed");
        let t = werk.get_task(&id).unwrap();
        assert_eq!(t.status, Status::InProgress);

        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": {"x": "ok"}}), &ctx)
            .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FINISHED);
        let t = werk.get_task(&id).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref().unwrap()["x"], "ok");
    }

    #[tokio::test]
    async fn an_object_result_is_stored_as_the_object() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(shared_test_dir().to_path_buf());
        werk.set_dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        }))
        .unwrap();
        werk.insert(
            Task::new("hi").schema(schema.clone()).label("alice"),
            "tester".into(),
        );
        let id = werk
            .claim(&Query::from("task.status = todo"), "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        let outcome = finish_for(schema)
            .call(serde_json::json!({"x": "ok"}), &ctx)
            .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FINISHED);
        let t = werk.get_task(&id).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref().unwrap(), &serde_json::json!({"x": "ok"}));
    }

    #[tokio::test]
    async fn stores_a_string_encoded_result_as_the_decoded_object() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(shared_test_dir().to_path_buf());
        werk.set_dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        }))
        .unwrap();
        werk.insert(
            Task::new("hi").schema(schema).label("alice"),
            "tester".into(),
        );
        let id = werk
            .claim(&Query::from("task.status = todo"), "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        // The agent double-encoded the conforming object as a JSON string.
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": "{\"x\": \"ok\"}"}), &ctx)
            .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FINISHED);

        let t = werk.get_task(&id).unwrap();
        assert_eq!(t.status, Status::Finished);
        // Stored as the decoded object, not the raw string.
        assert!(t.result.as_ref().unwrap().is_object());
        assert_eq!(t.result.as_ref().unwrap()["x"], "ok");
        assert!(read_result(dir.path(), &id).unwrap().is_object());
    }

    #[tokio::test]
    async fn errors_when_no_current_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(shared_test_dir().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": "x"}), &ctx)
            .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FAILED);
    }

    #[tokio::test]
    async fn appends_one_line_per_completed_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(shared_test_dir().to_path_buf());
        werk.set_dir(dir.path().to_path_buf());

        werk.insert(Task::new("a").label("alice"), "tester".into());
        let id1 = werk
            .claim(&Query::from("t-1"), "alice")
            .expect("claim must succeed");
        let ctx_alice = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        Tool::from(FinishTool)
            .call(serde_json::json!({"result": "from alice"}), &ctx_alice)
            .await;

        werk.insert(Task::new("b").label("bob"), "tester".into());
        let id2 = werk
            .claim(&Query::from("t-2"), "bob")
            .expect("claim must succeed");
        let ctx_bob = ctx_with(Arc::clone(&werk), "bob", dir.path().to_path_buf());
        Tool::from(FinishTool)
            .call(serde_json::json!({"result": "from bob"}), &ctx_bob)
            .await;

        assert_eq!(read_result(dir.path(), &id1), Some("from alice".into()));
        assert_eq!(read_result(dir.path(), &id2), Some("from bob".into()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writes_produce_one_intact_line_per_task() {
        const N: usize = 32;
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(shared_test_dir().to_path_buf());
        werk.set_dir(dir.path().to_path_buf());

        let mut expected = Vec::with_capacity(N);
        for i in 0..N {
            let agent = format!("agent_{i}");
            werk.insert(
                Task::new(format!("body_{i}")).label(&agent),
                "tester".into(),
            );
            let id = werk
                .claim(
                    &Query::from(format!("task.status = todo AND task.label = {agent}")),
                    &agent,
                )
                .expect("claim must succeed");
            expected.push((agent, id));
        }

        let mut handles = Vec::with_capacity(N);
        for (i, (agent, _)) in expected.iter().enumerate() {
            let werk = Arc::clone(&werk);
            let dir_path = dir.path().to_path_buf();
            let agent = agent.clone();
            handles.push(tokio::spawn(async move {
                let ctx = ctx_with(werk, &agent, dir_path);
                Tool::from(FinishTool)
                    .call(serde_json::json!({"result": format!("payload_{i}")}), &ctx)
                    .await
            }));
        }
        for h in handles {
            assert!(h.await.unwrap().get_name() == Event::TOOL_CALL_FINISHED);
        }

        // Every finish appends to the one shared log, so a torn line here is
        // two agents writing over each other.
        let log = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let mut finished = std::collections::HashSet::new();
        for line in log.lines() {
            let parsed: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("corrupt line {line:?}: {e}"));
            if parsed["name"] == "task_finished" {
                let task = parsed["task_id"].as_str().unwrap().to_string();
                assert!(finished.insert(task), "duplicate task in log");
            }
        }
        let expected_ids: std::collections::HashSet<String> =
            expected.iter().map(|(_, k)| k.clone()).collect();
        assert_eq!(finished, expected_ids);
    }

    // Handover

    fn one_task_in(agent: &str, dir: PathBuf) -> (Arc<Werk>, String) {
        let werk = Werk::new();
        werk.set_dir(dir);
        werk.insert(Task::new("parent body").label(agent), "tester".into());
        let id = werk
            .claim(&Query::from("task.status = todo"), agent)
            .expect("claim must succeed");
        (werk, id)
    }

    #[tokio::test]
    async fn handover_finishes_parent_creates_child_with_parent_link() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {"label": "bob", "task": "continue with X"},
                    "result": "summary of alice's work"
                }),
                &ctx,
            )
            .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FINISHED);

        let parent = werk.get_task(&parent_id).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref().and_then(|v| v.as_str()),
            Some("summary of alice's work")
        );

        let child = werk.get_task("t-2").unwrap();
        assert_eq!(child.status, Status::Todo);
        assert_eq!(child.parent.as_deref(), Some(parent_id.as_str()));
        assert_eq!(child.label.as_deref(), Some("bob"));
        assert_eq!(child.reporter, "alice");
    }

    #[tokio::test]
    async fn inline_handover_attaches_its_schema_to_the_child() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, _parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "result": "a lead worth tracing",
                    "handover": {
                        "label": "bob",
                        "task": "trace the lead",
                        "schema": {"type": "object", "title": "verdict"}
                    }
                }),
                &ctx,
            )
            .await;

        let bound = werk.get_task("t-2").unwrap().schema.unwrap();
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
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {"label": "bob", "task": "next"},
                    "result": "done part 1"
                }),
                &ctx,
            )
            .await;

        assert_eq!(
            read_result(dir.path(), &parent_id),
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
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "string",
            "minLength": 50
        }))
        .unwrap();
        werk.insert(
            Task::new("strict parent").schema(schema).label("alice"),
            "tester".into(),
        );
        let parent_id = werk
            .claim(&Query::from("task.status = todo"), "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {"label": "bob", "task": "next"},
                    "result": "too short"
                }),
                &ctx,
            )
            .await;
        assert_eq!(outcome.get_data()["kind"], "schema_failed");

        let parent = werk.get_task(&parent_id).unwrap();
        assert_eq!(parent.status, Status::InProgress);
        assert!(parent.result.is_none());
        assert!(
            werk.get_task("t-2").is_none(),
            "no child created on schema failure"
        );
        assert_eq!(read_result(dir.path(), &parent_id), None);
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

    fn one_task_with_object_schema(agent: &str, dir: PathBuf) -> (Arc<Werk>, String) {
        let werk = Werk::new();
        werk.set_dir(dir);
        werk.insert(
            Task::new("strict parent")
                .schema(strict_object_schema())
                .label(agent),
            "tester".into(),
        );
        let id = werk
            .claim(&Query::from("task.status = todo"), agent)
            .expect("claim must succeed");
        (werk, id)
    }

    #[tokio::test]
    async fn handover_structured_result_validated_against_parent_schema_is_stored_as_object() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {"label": "bob", "task": "next"},
                    "result": {"status": "done"}
                }),
                &ctx,
            )
            .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FINISHED);

        let parent = werk.get_task(&parent_id).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
        assert!(werk.get_task("t-2").is_some());

        assert_eq!(
            read_result(dir.path(), &parent_id),
            Some(serde_json::json!({"status": "done"}))
        );
    }

    #[tokio::test]
    async fn a_bound_object_uses_its_configured_handover() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        let outcome = FinishTool::from_schema(
            Some(strict_object_schema()),
            Some(Task::labeled("bob", "next")),
        )
        .call(serde_json::json!({"status": "done"}), &ctx)
        .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FINISHED);

        let parent = werk.get_task(&parent_id).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
        assert_eq!(werk.get_task("t-2").unwrap().get_label(), Some("bob"));
    }

    #[tokio::test]
    async fn handover_double_encoded_structured_result_is_decoded_to_object() {
        // The agent double-encodes the object as a JSON string; the parent
        // schema's validation decodes it so the stored value is the object.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {"label": "bob", "task": "next"},
                    "result": "{\"status\":\"done\"}"
                }),
                &ctx,
            )
            .await;

        let parent = werk.get_task(&parent_id).unwrap();
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
    }

    #[tokio::test]
    async fn handover_object_schema_violation_aborts_atomically() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {"label": "bob", "task": "next"},
                    "result": {"wrong": 1}
                }),
                &ctx,
            )
            .await;
        assert_eq!(outcome.get_data()["kind"], "schema_failed");

        let parent = werk.get_task(&parent_id).unwrap();
        assert_eq!(parent.status, Status::InProgress);
        assert!(parent.result.is_none());
        assert!(
            werk.get_task("t-2").is_none(),
            "no child created on schema failure"
        );
        assert_eq!(read_result(dir.path(), &parent_id), None);
    }

    #[tokio::test]
    async fn omitted_handover_finishes_without_a_child() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(serde_json::json!({"result": "done"}), &ctx)
            .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FINISHED);
        assert_eq!(werk.get_task(&parent_id).unwrap().status, Status::Finished);
        assert!(werk.get_task("t-2").is_none());
    }

    #[tokio::test]
    async fn configured_handover_creates_a_child_from_result_only() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, _id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let outcome =
            FinishTool::from_schema(None, Some(Task::labeled("bob", "Review {parent_result}")))
                .call(serde_json::json!({"result": "alice's findings"}), &ctx)
                .await;
        assert!(
            outcome.get_name() == Event::TOOL_CALL_FINISHED,
            "{outcome:?}"
        );

        let child = werk.get_task("t-2").unwrap();
        assert_eq!(child_body(&child), "Review alice's findings");
    }

    #[tokio::test]
    async fn configured_handover_accepts_its_label_and_overrides_task_and_schema() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, _id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let configured_schema = Schema::new(serde_json::json!({
            "type": "string",
            "title": "configured"
        }))
        .unwrap();
        let tool = FinishTool::from_schema(
            None,
            Some(Task::labeled("bob", "configured body").schema(configured_schema)),
        );

        let outcome = tool
            .call(
                serde_json::json!({
                    "result": "done",
                    "handover": {
                        "label": "bob",
                        "task": {"kind": "review", "source": "{parent_id}"},
                        "schema": {"type": "object", "title": "override"}
                    }
                }),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_name(), Event::TOOL_CALL_FINISHED);
        let child = werk.get_task("t-2").unwrap();
        assert_eq!(child.get_label(), Some("bob"));
        assert_eq!(child.get_task()["kind"], "review");
        assert_eq!(child.get_task()["source"], "t-1");
        assert_eq!(title_of(child.get_schema().unwrap()), "override");
    }

    #[tokio::test]
    async fn configured_handover_accepts_an_override_without_repeating_its_label() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, _id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        FinishTool::from_schema(None, Some(Task::labeled("bob", "configured body")))
            .call(
                serde_json::json!({
                    "result": "done",
                    "handover": {"task": "overridden body"}
                }),
                &ctx,
            )
            .await;

        let child = werk.get_task("t-2").unwrap();
        assert_eq!(child.get_label(), Some("bob"));
        assert_eq!(child.get_task(), "overridden body");
    }

    #[tokio::test]
    async fn configured_handover_rejects_a_different_label_atomically() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let outcome = FinishTool::from_schema(None, Some(Task::labeled("bob", "review")))
            .invoke(
                serde_json::json!({
                "result": "done",
                "handover": {"label": "charlie"}
                }),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_data()["kind"], "schema_failed");
        let parent = werk.get_task(&parent_id).unwrap();
        assert_eq!(parent.get_status(), Status::InProgress);
        assert_eq!(parent.get_result(), None);
        assert_eq!(read_result(dir.path(), &parent_id), None);
        assert!(werk.get_task("t-2").is_none());
    }

    #[tokio::test]
    async fn configured_handover_cannot_create_a_second_child_after_finish() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let tool = FinishTool::from_schema(None, Some(Task::labeled("bob", "review")));

        let first = tool
            .call(serde_json::json!({"result": "first"}), &ctx)
            .await;
        let second = tool
            .call(serde_json::json!({"result": "second"}), &ctx)
            .await;

        assert_eq!(first.get_name(), Event::TOOL_CALL_FINISHED);
        assert_eq!(second.get_name(), Event::TOOL_CALL_FAILED);
        assert_eq!(
            werk.get_task(&parent_id).unwrap().get_result(),
            Some(&serde_json::json!("first"))
        );
        assert!(werk.get_task("t-2").is_some());
        assert!(werk.get_task("t-3").is_none());
    }

    #[tokio::test]
    async fn invalid_inline_handover_schema_is_atomic() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .invoke(
                serde_json::json!({
                "result": "done",
                "handover": {
                    "label": "bob",
                    "task": "review",
                    "schema": {"unsupported": true}
                }
                }),
                &ctx,
            )
            .await;

        assert_eq!(outcome.get_directive(), Some("handover_schema_invalid"));
        let parent = werk.get_task(&parent_id).unwrap();
        assert_eq!(parent.get_status(), Status::InProgress);
        assert_eq!(parent.get_result(), None);
        assert_eq!(read_result(dir.path(), &parent_id), None);
        assert!(werk.get_task("t-2").is_none());
    }

    #[tokio::test]
    async fn configured_handover_requires_a_nonempty_result() {
        for result in [serde_json::Value::Null, serde_json::json!("")] {
            let dir = crate::test_util::TempDir::new().unwrap();
            let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
            let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

            let outcome = FinishTool::from_schema(None, Some(Task::labeled("bob", "review")))
                .call(serde_json::json!({"result": result}), &ctx)
                .await;

            assert!(outcome
                .get_content()
                .contains("requires a non-null, non-empty result"));
            assert_eq!(
                werk.get_task(&parent_id).unwrap().get_status(),
                Status::InProgress
            );
            assert!(werk.get_task("t-2").is_none());
        }
    }

    #[tokio::test]
    async fn handover_preserves_structure_and_substitutes_every_string_leaf() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "result": {"verdict": "safe"},
                    "handover": {
                        "label": "bob",
                        "task": {
                            "parent": "{parent_id}",
                            "steps": ["Read {parent_result_path}", 2, {"input": "{parent_result}"}]
                        }
                    }
                }),
                &ctx,
            )
            .await;

        let child = werk.get_task("t-2").unwrap();
        assert_eq!(child.get_task()["parent"], parent_id);
        assert_eq!(child.get_task()["steps"][1], 2);
        assert_eq!(
            child.get_task()["steps"][2]["input"],
            serde_json::json!({"verdict": "safe"}).to_string()
        );
        assert_eq!(child.get_parent(), Some(parent_id.as_str()));
    }

    #[tokio::test]
    async fn configured_handover_does_not_append_parent_metadata() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        FinishTool::from_schema(None, Some(Task::labeled("bob", "Review it")))
            .call(serde_json::json!({"result": "alice's findings"}), &ctx)
            .await;

        let child = werk.get_task("t-2").unwrap();
        let body = child_body(&child);
        assert_eq!(body, "Review it");
        assert_eq!(child.parent.as_deref(), Some(parent_id.as_str()));
    }

    #[tokio::test]
    async fn handover_substitutes_the_parent_result_file_in_the_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {
                        "label": "bob",
                        "task": "Read {parent_result_path} and continue"
                    },
                    "result": "alice's findings"
                }),
                &ctx,
            )
            .await;

        let child = werk.get_task("t-2").unwrap();
        let path = werk.result_path(&parent_id);
        assert!(
            child_body(&child).starts_with(&format!("Read {} and continue", path.display())),
            "{}",
            child_body(&child)
        );
    }

    /// Read a text task from a child used by these string-specific tests.
    fn child_body(child: &Task) -> String {
        child.task.as_str().expect("a task string").to_string()
    }

    #[tokio::test]
    async fn a_handover_with_nothing_to_pass_on_is_rejected() {
        // Absent, null, and empty all leave the receiving agent a body that
        // says nothing, and this task carries no schema to require one.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, _id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        for body in [
            serde_json::json!({"handover": {"label": "bob", "task": "x"}}),
            serde_json::json!({"handover": {"label": "bob", "task": "x"}, "result": null}),
            serde_json::json!({"handover": {"label": "bob", "task": "x"}, "result": ""}),
        ] {
            let outcome = Tool::from(FinishTool).call(body, &ctx).await;
            assert!(
                outcome.get_name() == Event::TOOL_CALL_FAILED
                    && outcome
                        .get_content()
                        .contains("requires a non-null, non-empty result"),
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
            let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
            let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

            let outcome = Tool::from(FinishTool)
                .call(
                    serde_json::json!({
                        "handover": {"label": "bob", "task": "next"},
                        "result": result_value
                    }),
                    &ctx,
                )
                .await;
            assert!(outcome.get_name() == Event::TOOL_CALL_FINISHED);

            let parent = werk.get_task(&parent_id).unwrap();
            assert_eq!(parent.status, Status::Finished);
            assert_eq!(parent.result.as_ref(), Some(&result_value));
            assert!(werk.get_task("t-2").is_some());
        }
    }

    #[test]
    fn a_non_schema_handover_schema_is_rejected() {
        let schema = Tool::from(FinishTool).get_input_schema().clone();
        let violations = schema
            .validate(serde_json::json!({
                "handover": {"label": "bob", "task": 42, "schema": "string"},
                "result": "ok"
            }))
            .unwrap_err();
        assert!(
            violations
                .iter()
                .any(|v| v.instance_path == "/handover/schema"),
            "{violations}"
        );
    }

    #[tokio::test]
    async fn malformed_handover_arguments_are_rejected_atomically() {
        for input in [
            serde_json::json!({"handover": 7, "result": "x"}),
            serde_json::json!({"handover": {"task": "x"}, "result": "x"}),
            serde_json::json!({"handover": {"label": "bob"}, "result": "x"}),
            serde_json::json!({
                "handover": {"label": "  ", "task": "x"},
                "result": "x"
            }),
            serde_json::json!({
                "handover": {"label": "bob", "task": "x", "schema": "string"},
                "result": "x"
            }),
        ] {
            let dir = crate::test_util::TempDir::new().unwrap();
            let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
            let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
            let outcome = Tool::from(FinishTool).invoke(input, &ctx).await;

            assert_eq!(outcome.get_data()["kind"], "schema_failed");
            let parent = werk.get_task(&parent_id).unwrap();
            assert_eq!(parent.get_status(), Status::InProgress);
            assert_eq!(parent.get_result(), None);
            assert_eq!(read_result(dir.path(), &parent_id), None);
            assert!(werk.get_task("t-2").is_none());
        }
    }

    #[tokio::test]
    async fn a_blank_handover_from_a_direct_call_is_an_error() {
        // Hosts that bypass runtime validation still fail without mutating
        // the Werk, but do not receive a model-facing directive.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        for body in [
            serde_json::json!({"handover": {"label": "  ", "task": "x"}, "result": "x"}),
            serde_json::json!({"handover": {"task": "x"}, "result": "x"}),
            serde_json::json!({"handover": 7, "result": "x"}),
        ] {
            let outcome = Tool::from(FinishTool).call(body, &ctx).await;
            assert_eq!(outcome.get_name(), Event::TOOL_CALL_FAILED);
            assert_eq!(outcome.get_directive(), None);
        }
        assert_eq!(
            werk.get_task(&parent_id).unwrap().status,
            Status::InProgress,
        );
    }

    #[tokio::test]
    async fn handover_errors_when_no_current_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());
        let outcome = Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {"label": "bob", "task": "x"},
                    "result": "y"
                }),
                &ctx,
            )
            .await;
        assert!(outcome.get_name() == Event::TOOL_CALL_FAILED);
    }

    #[tokio::test]
    async fn substitutes_parent_id_and_result_in_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {
                        "label": "bob",
                        "task": "Continue {parent_id}: {parent_result}"
                    },
                    "result": "alice's findings"
                }),
                &ctx,
            )
            .await;

        let child = werk.get_task("t-2").unwrap();
        assert!(child_body(&child).starts_with(&format!("Continue {parent_id}: alice's findings")));
    }

    #[tokio::test]
    async fn unknown_placeholders_pass_through() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {
                        "label": "bob",
                        "task": "See {parent_id} and {unknown}"
                    },
                    "result": "ok"
                }),
                &ctx,
            )
            .await;

        let child = werk.get_task("t-2").unwrap();
        assert!(child_body(&child).starts_with(&format!("See {parent_id} and {{unknown}}")));
    }

    #[tokio::test]
    async fn parent_key_is_not_an_alias_for_parent_id() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, _parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {"label": "bob", "task": "See {parent_key}"},
                    "result": "ok"
                }),
                &ctx,
            )
            .await;

        let child = werk.get_task("t-2").unwrap();
        assert!(child_body(&child).starts_with("See {parent_key}"));
    }

    #[tokio::test]
    async fn substitution_is_single_pass() {
        // A `result` that itself contains the literal text `{parent_id}`
        // must NOT be re-expanded: the substitution pass runs once
        // per placeholder, not recursively.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (werk, parent_id) = one_task_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&werk), "alice", dir.path().to_path_buf());

        Tool::from(FinishTool)
            .call(
                serde_json::json!({
                    "handover": {"label": "bob", "task": "[{parent_result}]"},
                    "result": "{parent_id}"
                }),
                &ctx,
            )
            .await;

        let child = werk.get_task("t-2").unwrap();
        assert!(
            child_body(&child).starts_with("[{parent_id}]"),
            "result containing `{{parent_id}}` should be inserted literally, \
             not recursively expanded (parent_id was {parent_id})",
        );
    }
}
