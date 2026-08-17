//! Lets an agent write the result for its ticket and mark it finished, handing
//! the work on to another agent in the same call when it needs to.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use crate::agents::tickets::Ticket;
use crate::providers::ProviderResult;

use super::super::tool::{ToolContext, ToolLike, ToolResult};
use super::super::tool_file::ToolFile;
use super::{resolve_current_key, write_result};

/// Write a ticket's result and mark it finished, optionally handing
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

/// Reserved placeholders substituted into the child ticket's `task`
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

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("finish.tool.md")))
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

impl ToolLike for FinishTool {
    fn name(&self) -> &str {
        &tool_file().name
    }

    fn description(&self) -> &str {
        description()
    }

    fn input_schema(&self) -> Value {
        tool_file().input_schema.clone()
    }

    fn is_concurrent(&self) -> bool {
        tool_file().concurrent
    }

    fn call<'a>(
        &'a self,
        input: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>> {
        Box::pin(async move {
            let Some(ticket_queue) = ctx.ticket_queue_handle().cloned() else {
                return Ok(ToolResult::error(
                    "Ticket queue unavailable in this context",
                ));
            };

            // A present, non-empty `handover` selects the chaining path.
            let handover = match input.get("handover") {
                Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
                Some(Value::String(_)) => {
                    return Ok(ToolResult::error("`handover` must not be an empty string"))
                }
                Some(Value::Null) | None => None,
                Some(_) => return Ok(ToolResult::error("`handover` must be a string")),
            };

            let parent_key = match resolve_current_key(&ticket_queue, ctx) {
                Ok(k) => k,
                Err(e) => return Ok(e),
            };
            let schema = ticket_queue.get_ticket(&parent_key).and_then(|t| t.schema);
            let agent = ctx.agent_id_str().unwrap_or_default().to_string();
            // The ticket's own schema decides whether the result rode in as
            // the top-level arguments (object schema) or under `result`.
            let result = super::result_shape::parse_result("finish", schema.as_ref(), &input);

            let Some(handover) = handover else {
                return Ok(write_result(&ticket_queue, &parent_key, result, &agent));
            };

            // An omitted `task` defaults to the parent result below: the
            // common handoff forwards the finding verbatim.
            let task = match input.get("task") {
                Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
                Some(Value::String(_)) => {
                    return Ok(ToolResult::error("`task` must not be an empty string"))
                }
                Some(Value::Null) | None => None,
                Some(_) => return Ok(ToolResult::error("`task` must be a string")),
            };

            // null and an empty string are rejected: a handoff needs a real result.
            match &result {
                Value::String(s) if s.is_empty() => {
                    return Ok(ToolResult::error("`result` must not be an empty string"))
                }
                Value::Null => return Ok(ToolResult::error("Missing required parameter: result")),
                _ => {}
            }

            // Validate, log, and attach the parent result. set_result does
            // not finish the ticket, so the child is inserted and the
            // parent finished below. A schema failure returns here before
            // any child exists.
            let (validated_result, _repairs) = match ticket_queue.set_result(&parent_key, result) {
                Ok(kept) => kept,
                Err(violations) => return Ok(ToolResult::schema_error(violations.to_string())),
            };

            // `{parent_result}` needs a string: a plain string substitutes
            // verbatim, anything structured renders as compact JSON.
            let parent_result_str = match &validated_result {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            let result_path = ticket_queue.result_path(&parent_key);
            let result_path = result_path.display().to_string();

            let body = match task {
                Some(task) => {
                    apply_handover_templates(&task, &parent_key, &result_path, &parent_result_str)
                }
                None => parent_result_str.clone(),
            };
            let body = append_parent_reference(&body, &parent_key, &result_path);
            let child = Ticket::new(body).label(&handover).parent(&parent_key);

            // Insert the child BEFORE finishing the parent: the child is
            // already `Todo` when the parent leaves the queue, so a
            // concurrent `work_left` check never reads false and `finish`
            // cannot end the chain mid-handover. `parent_key` is resolved
            // and `InProgress`, so `set_finished_by` cannot miss it and leave
            // the inserted child orphaned.
            let child_key = ticket_queue.insert(child, agent.clone());
            if let Err(e) = ticket_queue.set_finished_by(&parent_key, &agent) {
                return Ok(ToolResult::error(super::ticket_error_message(e)));
            }

            Ok(ToolResult::success(format!(
                "Ticket {parent_key} marked finished; handed off to {child_key} (handover: {handover})"
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::agents::tickets::{Status, Ticket, TicketQueue};
    use crate::schemas::Schema;

    fn ctx_with(ticket_queue: Arc<TicketQueue>, agent: &str, dir: PathBuf) -> ToolContext {
        ToolContext::new(dir)
            .ticket_queue(ticket_queue)
            .agent_id(agent.to_string())
    }

    fn one_ticket(agent: &str) -> (Arc<TicketQueue>, String) {
        let queue = TicketQueue::new();
        queue.dir(shared_test_dir().to_path_buf());
        queue.insert(Ticket::new("body").label(agent), "tester".into());
        let key = queue
            .claim(|t| t.status == Status::Todo, agent)
            .expect("claim must succeed");
        (queue, key)
    }

    /// Read a ticket's result back from `tickets/<key>/result.json`, or `None`
    /// when it wrote none.
    fn read_result(dir: &std::path::Path, key: &str) -> Option<serde_json::Value> {
        let path = dir.join("tickets").join(key).join("result.json");
        let body = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&body).ok()
    }

    /// Process-lifetime tempdir used as the default `TicketQueue` root
    /// for tests in this module. Tests that need an isolated workspace
    /// still call `queue.dir(...)` explicitly to override.
    fn shared_test_dir() -> &'static std::path::Path {
        use std::sync::OnceLock;
        static DIR: OnceLock<crate::test_util::TempDir> = OnceLock::new();
        DIR.get_or_init(|| crate::test_util::TempDir::new().unwrap())
            .path()
    }

    #[tokio::test]
    async fn writes_string_result_and_marks_finished() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, key) = one_ticket("alice");
        queue.dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = FinishTool
            .call(serde_json::json!({"result": "the answer"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));
        let t = queue.get_ticket(&key).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(
            t.result.as_ref().and_then(|v| v.as_str()),
            Some("the answer")
        );

        assert_eq!(read_result(dir.path(), &key), Some("the answer".into()));
    }

    #[tokio::test]
    async fn a_result_is_written_to_the_ticket_folder() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, key) = one_ticket("alice");
        queue.dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        FinishTool
            .call(serde_json::json!({"result": {"x": 1}}), &ctx)
            .await
            .unwrap();

        let path = dir.path().join("tickets").join(&key).join("result.json");
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed, serde_json::json!({"x": 1}));

        // One home on disk: the ticket record no longer carries the result.
        let record =
            std::fs::read_to_string(dir.path().join("tickets").join(&key).join("ticket.json"))
                .unwrap();
        let record: serde_json::Value = serde_json::from_str(&record).unwrap();
        assert!(record.get("result").is_none(), "{record}");
    }

    #[tokio::test]
    async fn a_reloaded_queue_reads_the_result_back() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, key) = one_ticket("alice");
        queue.dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        FinishTool
            .call(serde_json::json!({"result": "the answer"}), &ctx)
            .await
            .unwrap();

        let reloaded = TicketQueue::load(dir.path()).unwrap();
        let ticket = reloaded.get_ticket(&key).unwrap();
        assert_eq!(
            ticket.result.as_ref().and_then(|v| v.as_str()),
            Some("the answer")
        );
        assert_eq!(ticket.status, Status::Finished);
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
            let (queue, key) = one_ticket("alice");
            queue.dir(dir.path().to_path_buf());
            let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
            let outcome = FinishTool
                .call(serde_json::json!({"result": value}), &ctx)
                .await
                .unwrap();
            assert!(
                matches!(outcome, ToolResult::Success(_)),
                "expected success for {value:?}"
            );
            let t = queue.get_ticket(&key).unwrap();
            assert_eq!(t.status, Status::Finished);
        }
    }

    #[tokio::test]
    async fn accepts_structured_value_when_no_schema() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, key) = one_ticket("alice");
        queue.dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = FinishTool
            .call(serde_json::json!({"result": {"x": 1, "y": [2, 3]}}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));
        let t = queue.get_ticket(&key).unwrap();
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
        let queue = TicketQueue::new();
        queue.dir(shared_test_dir().to_path_buf());
        queue.dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        }))
        .unwrap();
        queue.insert(
            Ticket::new("hi").schema(schema).label("alice"),
            "tester".into(),
        );
        let key = queue
            .claim(|t| t.status == Status::Todo, "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        // wrong shape
        let outcome = FinishTool
            .call(serde_json::json!({"result": {"x": 7}}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::SchemaError(_)));
        let t = queue.get_ticket(&key).unwrap();
        assert_eq!(t.status, Status::InProgress);

        // valid shape
        let outcome = FinishTool
            .call(serde_json::json!({"result": {"x": "ok"}}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));
        let t = queue.get_ticket(&key).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref().unwrap()["x"], "ok");
    }

    #[tokio::test]
    async fn object_schema_takes_flat_arguments_as_the_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(shared_test_dir().to_path_buf());
        queue.dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        }))
        .unwrap();
        queue.insert(
            Ticket::new("hi").schema(schema).label("alice"),
            "tester".into(),
        );
        let key = queue
            .claim(|t| t.status == Status::Todo, "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        // Fields at the top level, no `result` wrapper.
        let outcome = FinishTool
            .call(serde_json::json!({"x": "ok"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));
        let t = queue.get_ticket(&key).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref().unwrap(), &serde_json::json!({"x": "ok"}));
    }

    #[tokio::test]
    async fn stores_a_string_encoded_result_as_the_decoded_object() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(shared_test_dir().to_path_buf());
        queue.dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        }))
        .unwrap();
        queue.insert(
            Ticket::new("hi").schema(schema).label("alice"),
            "tester".into(),
        );
        let key = queue
            .claim(|t| t.status == Status::Todo, "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        // The agent double-encoded the conforming object as a JSON string.
        let outcome = FinishTool
            .call(serde_json::json!({"result": "{\"x\": \"ok\"}"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));

        let t = queue.get_ticket(&key).unwrap();
        assert_eq!(t.status, Status::Finished);
        // Stored as the decoded object, not the raw string.
        assert!(t.result.as_ref().unwrap().is_object());
        assert_eq!(t.result.as_ref().unwrap()["x"], "ok");
        assert!(read_result(dir.path(), &key).unwrap().is_object());
    }

    #[tokio::test]
    async fn errors_when_no_current_ticket() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(shared_test_dir().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = FinishTool
            .call(serde_json::json!({"result": "x"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn appends_one_line_per_completed_ticket() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(shared_test_dir().to_path_buf());
        queue.dir(dir.path().to_path_buf());

        queue.insert(Ticket::new("a").label("alice"), "tester".into());
        let key1 = queue
            .claim(|t| t.key == "TICKET-1", "alice")
            .expect("claim must succeed");
        let ctx_alice = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        FinishTool
            .call(serde_json::json!({"result": "from alice"}), &ctx_alice)
            .await
            .unwrap();

        queue.insert(Ticket::new("b").label("bob"), "tester".into());
        let key2 = queue
            .claim(|t| t.key == "TICKET-2", "bob")
            .expect("claim must succeed");
        let ctx_bob = ctx_with(Arc::clone(&queue), "bob", dir.path().to_path_buf());
        FinishTool
            .call(serde_json::json!({"result": "from bob"}), &ctx_bob)
            .await
            .unwrap();

        assert_eq!(read_result(dir.path(), &key1), Some("from alice".into()));
        assert_eq!(read_result(dir.path(), &key2), Some("from bob".into()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writes_produce_one_intact_line_per_ticket() {
        const N: usize = 32;
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(shared_test_dir().to_path_buf());
        queue.dir(dir.path().to_path_buf());

        let mut expected = Vec::with_capacity(N);
        for i in 0..N {
            let agent = format!("agent_{i}");
            queue.insert(
                Ticket::new(format!("body_{i}")).label(&agent),
                "tester".into(),
            );
            let key = queue
                .claim(|t| t.status == Status::Todo && t.has_label(&agent), &agent)
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
                FinishTool
                    .call(serde_json::json!({"result": format!("payload_{i}")}), &ctx)
                    .await
                    .unwrap()
            }));
        }
        for h in handles {
            assert!(matches!(h.await.unwrap(), ToolResult::Success(_)));
        }

        // Every finish appends to the one shared log, so a torn line here is
        // two agents writing over each other.
        let log = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let mut finished = std::collections::HashSet::new();
        for line in log.lines() {
            let parsed: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("corrupt line {line:?}: {e}"));
            if parsed["event"] == "ticket_finished" {
                let ticket = parsed["ticket_key"].as_str().unwrap().to_string();
                assert!(finished.insert(ticket), "duplicate ticket in log");
            }
        }
        let expected_keys: std::collections::HashSet<String> =
            expected.iter().map(|(_, k)| k.clone()).collect();
        assert_eq!(finished, expected_keys);
    }

    // Handover

    fn one_ticket_in(agent: &str, dir: PathBuf) -> (Arc<TicketQueue>, String) {
        let queue = TicketQueue::new();
        queue.dir(dir);
        queue.insert(Ticket::new("parent body").label(agent), "tester".into());
        let key = queue
            .claim(|t| t.status == Status::Todo, agent)
            .expect("claim must succeed");
        (queue, key)
    }

    #[tokio::test]
    async fn handover_finishes_parent_creates_child_with_parent_link() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = FinishTool
            .call(
                serde_json::json!({
                    "handover": "bob",
                    "task": "continue with X",
                    "result": "summary of alice's work"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));

        let parent = queue.get_ticket(&parent_key).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref().and_then(|v| v.as_str()),
            Some("summary of alice's work")
        );

        let child = queue.get_ticket("TICKET-2").unwrap();
        assert_eq!(child.status, Status::Todo);
        assert_eq!(child.parent.as_deref(), Some(parent_key.as_str()));
        assert!(child.has_label("bob"));
        assert_eq!(child.reporter, "alice");
    }

    #[tokio::test]
    async fn handover_child_takes_the_schema_bound_to_its_label() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, _parent_key) = one_ticket_in("alice", dir.path().to_path_buf());
        let schemas = crate::schemas::SchemaStore::new();
        schemas
            .label(
                "bob",
                serde_json::json!({"type": "object", "title": "verdict"}),
            )
            .unwrap();
        queue.schemas(&schemas);
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        FinishTool
            .call(
                serde_json::json!({"handover": "bob", "result": "a lead worth tracing"}),
                &ctx,
            )
            .await
            .unwrap();

        // `finish` cannot attach a schema to the child it inserts, so the
        // child is born without one and picks the label's up at claim.
        assert!(queue.get_ticket("TICKET-2").unwrap().schema.is_none());

        queue.claim(|t| t.has_label("bob"), "bob");
        let bound = queue.get_ticket("TICKET-2").unwrap().schema.unwrap();
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
        let (queue, parent_key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        FinishTool
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": "done part 1"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(
            read_result(dir.path(), &parent_key),
            Some("done part 1".into())
        );
        assert_eq!(
            read_result(dir.path(), "TICKET-2"),
            None,
            "only the parent finish writes a result"
        );
    }

    #[tokio::test]
    async fn handover_schema_violation_aborts_atomically() {
        // Parent demands a string of at least 50 characters; we pass
        // a short string, which violates the schema but passes the
        // type check, so we exercise the schema-validation abort path.
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        let schema = Schema::new(serde_json::json!({
            "type": "string",
            "minLength": 50
        }))
        .unwrap();
        queue.insert(
            Ticket::new("strict parent").schema(schema).label("alice"),
            "tester".into(),
        );
        let parent_key = queue
            .claim(|t| t.status == Status::Todo, "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = FinishTool
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": "too short"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::SchemaError(_)));

        let parent = queue.get_ticket(&parent_key).unwrap();
        assert_eq!(parent.status, Status::InProgress);
        assert!(parent.result.is_none());
        assert!(
            queue.get_ticket("TICKET-2").is_none(),
            "no child created on schema failure"
        );
        assert_eq!(read_result(dir.path(), &parent_key), None);
    }

    /// Build a claimed parent whose own schema requires an object with a
    /// `status` field, so the handover result is validated structurally.
    fn one_ticket_with_object_schema(agent: &str, dir: PathBuf) -> (Arc<TicketQueue>, String) {
        let queue = TicketQueue::new();
        queue.dir(dir);
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "required": ["status"]
        }))
        .unwrap();
        queue.insert(
            Ticket::new("strict parent").schema(schema).label(agent),
            "tester".into(),
        );
        let key = queue
            .claim(|t| t.status == Status::Todo, agent)
            .expect("claim must succeed");
        (queue, key)
    }

    #[tokio::test]
    async fn handover_structured_result_validated_against_parent_schema_is_stored_as_object() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_ticket_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = FinishTool
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": {"status": "done"}}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));

        let parent = queue.get_ticket(&parent_key).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
        assert!(queue.get_ticket("TICKET-2").is_some());

        assert_eq!(
            read_result(dir.path(), &parent_key),
            Some(serde_json::json!({"status": "done"}))
        );
    }

    #[tokio::test]
    async fn handover_object_schema_takes_result_fields_flat_alongside_control_keys() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_ticket_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        // `status` sits at the top level next to `handover`/`task`, no `result` wrapper.
        let outcome = FinishTool
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "status": "done"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));

        let parent = queue.get_ticket(&parent_key).unwrap();
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
        let (queue, parent_key) = one_ticket_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        FinishTool
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": "{\"status\":\"done\"}"}),
                &ctx,
            )
            .await
            .unwrap();

        let parent = queue.get_ticket(&parent_key).unwrap();
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
    }

    #[tokio::test]
    async fn handover_object_schema_violation_aborts_atomically() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_ticket_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        let outcome = FinishTool
            .call(
                serde_json::json!({"handover": "bob", "task": "next", "result": {"wrong": 1}}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::SchemaError(_)));

        let parent = queue.get_ticket(&parent_key).unwrap();
        assert_eq!(parent.status, Status::InProgress);
        assert!(parent.result.is_none());
        assert!(
            queue.get_ticket("TICKET-2").is_none(),
            "no child created on schema failure"
        );
        assert_eq!(read_result(dir.path(), &parent_key), None);
    }

    #[tokio::test]
    async fn rejects_empty_handover() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, _key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = FinishTool
            .call(
                serde_json::json!({"handover": "  ", "task": "x", "result": "y"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn omitted_handover_finishes_without_a_child() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = FinishTool
            .call(serde_json::json!({"result": "done"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));
        assert_eq!(
            queue.get_ticket(&parent_key).unwrap().status,
            Status::Finished
        );
        assert!(queue.get_ticket("TICKET-2").is_none());
    }

    #[tokio::test]
    async fn omitted_task_defaults_child_body_to_the_parent_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, _key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = FinishTool
            .call(
                serde_json::json!({"handover": "bob", "result": "alice's findings"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)), "{outcome:?}");

        let child = queue.get_ticket("TICKET-2").unwrap();
        assert!(child_body(&child).starts_with("alice's findings"));
    }

    #[tokio::test]
    async fn handover_ends_the_child_body_with_the_parent_key_and_result_file() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        FinishTool
            .call(
                serde_json::json!({"handover": "bob", "result": "alice's findings"}),
                &ctx,
            )
            .await
            .unwrap();

        let child = queue.get_ticket("TICKET-2").unwrap();
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
        let (queue, parent_key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        FinishTool
            .call(
                serde_json::json!({
                    "handover": "bob",
                    "task": "Read {parent_result_path} and continue",
                    "result": "alice's findings"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let child = queue.get_ticket("TICKET-2").unwrap();
        let path = queue.result_path(&parent_key);
        assert!(
            child_body(&child).starts_with(&format!("Read {} and continue", path.display())),
            "{}",
            child_body(&child)
        );
    }

    /// The child's task as text, which every handover writes as a string.
    fn child_body(child: &Ticket) -> String {
        child.task.as_str().expect("a task string").to_string()
    }

    #[tokio::test]
    async fn handover_rejects_missing_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, _key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = FinishTool
            .call(serde_json::json!({"handover": "bob", "task": "x"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn handover_rejects_null_or_empty_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, _key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        for body in [
            serde_json::json!({"handover": "bob", "task": "x", "result": null}),
            serde_json::json!({"handover": "bob", "task": "x", "result": ""}),
        ] {
            let outcome = FinishTool.call(body, &ctx).await.unwrap();
            assert!(matches!(outcome, ToolResult::Error(_)));
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
            let (queue, parent_key) = one_ticket_in("alice", dir.path().to_path_buf());
            let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

            let outcome = FinishTool
                .call(
                    serde_json::json!({"handover": "bob", "task": "next", "result": result_value}),
                    &ctx,
                )
                .await
                .unwrap();
            assert!(matches!(outcome, ToolResult::Success(_)));

            let parent = queue.get_ticket(&parent_key).unwrap();
            assert_eq!(parent.status, Status::Finished);
            assert_eq!(parent.result.as_ref(), Some(&result_value));
            assert!(queue.get_ticket("TICKET-2").is_some());
        }
    }

    #[tokio::test]
    async fn handover_rejects_non_string_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, _key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = FinishTool
            .call(
                serde_json::json!({"handover": "bob", "task": 42, "result": "ok"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn handover_errors_when_no_current_ticket() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());
        let outcome = FinishTool
            .call(
                serde_json::json!({"handover": "bob", "task": "x", "result": "y"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn substitutes_parent_key_and_result_in_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        FinishTool
            .call(
                serde_json::json!({
                    "handover": "bob",
                    "task": "Continue {parent_key}: {parent_result}",
                    "result": "alice's findings"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let child = queue.get_ticket("TICKET-2").unwrap();
        assert!(child_body(&child).starts_with(&format!("Continue {parent_key}: alice's findings")));
    }

    #[tokio::test]
    async fn unknown_placeholders_pass_through() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        FinishTool
            .call(
                serde_json::json!({
                    "handover": "bob",
                    "task": "See {parent_key} and {unknown}",
                    "result": "ok"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let child = queue.get_ticket("TICKET-2").unwrap();
        assert!(child_body(&child).starts_with(&format!("See {parent_key} and {{unknown}}")));
    }

    #[tokio::test]
    async fn substitution_is_single_pass() {
        // A `result` that itself contains the literal text `{parent_key}`
        // must NOT be re-expanded: the substitution pass runs once
        // per placeholder, not recursively.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (queue, parent_key) = one_ticket_in("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&queue), "alice", dir.path().to_path_buf());

        FinishTool
            .call(
                serde_json::json!({
                    "handover": "bob",
                    "task": "[{parent_result}]",
                    "result": "{parent_key}"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let child = queue.get_ticket("TICKET-2").unwrap();
        assert!(
            child_body(&child).starts_with("[{parent_key}]"),
            "result containing `{{parent_key}}` should be inserted literally, \
             not recursively expanded (parent_key was {parent_key})",
        );
    }
}
