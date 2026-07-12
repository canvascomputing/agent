//! Atomic finish + spawner: record the agent's `result` on the current
//! ticket (validate against its schema, log, attach) via
//! `TicketSystem::set_result`, insert a child pinned to `to` (with the
//! current ticket as its `parent`), then finish the current ticket.
//! Inserting the child before the parent finishes keeps the queue
//! non-empty across the handover, so `finish()` cannot drain the chain
//! early. Sister tool to `FinishTicketTool`: both finish the current
//! ticket; this one also chains a follow-up.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use crate::agents::tickets::Ticket;
use crate::providers::ProviderResult;

use super::super::tool::{ToolContext, ToolLike, ToolResult};
use super::super::tool_file::ToolFile;
use super::resolve_current_key;

/// Write a ticket's result, mark it finished, and hand follow-up work
/// to another agent.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::HandoverTicketTool;
///
/// Agent::new().tool(HandoverTicketTool);
/// ```
pub struct HandoverTicketTool;

/// Reserved placeholders substituted into the child ticket's `task`
/// string at handover time: `{parent_key}` and `{parent_result}`.
/// Single-pass `str::replace` over each in turn; unknown `{name}`
/// placeholders pass through verbatim. The non-string arm is
/// defensive: input validation already rejects non-string `task`.
fn apply_handover_templates(task: Value, parent_key: &str, parent_result: &str) -> Value {
    match task {
        Value::String(s) => Value::String(
            s.replace("{parent_key}", parent_key)
                .replace("{parent_result}", parent_result),
        ),
        other => other,
    }
}

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("handover_ticket.tool.md")))
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

impl ToolLike for HandoverTicketTool {
    fn name(&self) -> &str {
        &tool_file().name
    }

    fn description(&self) -> &str {
        description()
    }

    fn input_schema(&self) -> Value {
        tool_file().input_schema.clone()
    }

    fn is_read_only(&self) -> bool {
        tool_file().read_only
    }

    fn call<'a>(
        &'a self,
        input: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>> {
        Box::pin(async move {
            let Some(ticket_system) = ctx.ticket_system_handle().cloned() else {
                return Ok(ToolResult::error(
                    "Ticket system unavailable in this context",
                ));
            };

            let to = match input.get("to").and_then(|v| v.as_str()) {
                Some(s) if !s.trim().is_empty() => s.trim().to_string(),
                _ => return Ok(ToolResult::error("Missing required parameter: to")),
            };
            // An omitted `task` defaults to the parent result below: the
            // common handoff forwards the finding verbatim.
            let task = match input.get("task") {
                Some(Value::String(s)) if !s.is_empty() => Some(Value::String(s.clone())),
                Some(Value::String(_)) => {
                    return Ok(ToolResult::error("`task` must not be an empty string"))
                }
                Some(Value::Null) | None => None,
                Some(_) => return Ok(ToolResult::error("`task` must be a string")),
            };
            let parent_key = match resolve_current_key(&ticket_system, ctx) {
                Ok(k) => k,
                Err(e) => return Ok(e),
            };

            let agent = ctx
                .agent_name_str()
                .expect("agent_name on ToolContext")
                .to_string();

            // The parent's own schema decides whether the result rode in as
            // the top-level arguments (object schema) or under `result`. null
            // and an empty string are rejected: a handoff needs a real result.
            let schema = ticket_system.get_ticket(&parent_key).and_then(|t| t.schema);
            let result =
                super::result_shape::parse_result("handover_ticket", schema.as_ref(), &input);
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
            let validated_result = match ticket_system.set_result(&parent_key, result) {
                Ok(value) => value,
                Err(violations) => return Ok(ToolResult::schema_error(violations.to_string())),
            };

            // `{parent_result}` needs a string: a plain string substitutes
            // verbatim, anything structured renders as compact JSON.
            let parent_result_str = match &validated_result {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };

            let task = match task {
                Some(task) => apply_handover_templates(task, &parent_key, &parent_result_str),
                None => Value::String(parent_result_str.clone()),
            };
            let child = Ticket::new(task).label(&to).parent(&parent_key);

            // Insert the child BEFORE finishing the parent: the child is
            // already `Todo` when the parent leaves the queue, so a
            // concurrent `pending_count()` poll never reads 0 and `finish()`
            // cannot drain the chain mid-handover. `parent_key` is resolved
            // and `InProgress`, so `set_finished` cannot miss it and leave
            // the inserted child orphaned.
            let child_key = ticket_system.insert(child, agent.clone());
            if let Err(e) = ticket_system.set_finished(&parent_key, &agent) {
                return Ok(ToolResult::error(super::ticket_error_message(e)));
            }

            Ok(ToolResult::success(format!(
                "Ticket {parent_key} marked finished; handed off to {child_key} (to: {to})"
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::agents::tickets::{Status, Ticket, TicketSystem};
    use crate::schemas::Schema;

    fn ctx_with(ticket_system: Arc<TicketSystem>, agent: &str, dir: PathBuf) -> ToolContext {
        ToolContext::new(dir)
            .ticket_system(ticket_system)
            .agent_name(agent.to_string())
    }

    fn one_ticket(agent: &str, dir: PathBuf) -> (Arc<TicketSystem>, String) {
        let sys = TicketSystem::new();
        sys.dir(dir);
        sys.insert(Ticket::new("parent body").label(agent), "tester".into());
        let key = sys
            .claim(|t| t.status == Status::Todo, agent)
            .expect("claim must succeed");
        (sys, key)
    }

    #[tokio::test]
    async fn happy_path_finishes_parent_creates_child_with_parent_link() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, parent_key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

        let outcome = HandoverTicketTool
            .call(
                serde_json::json!({
                    "to": "bob",
                    "task": "continue with X",
                    "result": "summary of alice's work"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));

        let parent = sys.get_ticket(&parent_key).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref().and_then(|v| v.as_str()),
            Some("summary of alice's work")
        );

        let child = sys.get_ticket("TICKET-2").unwrap();
        assert_eq!(child.status, Status::Todo);
        assert_eq!(child.parent.as_deref(), Some(parent_key.as_str()));
        assert_eq!(child.labels, vec!["bob".to_string()]);
        assert_eq!(child.reporter, "alice");
    }

    #[tokio::test]
    async fn appends_one_ndjson_line_for_parent_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, parent_key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

        HandoverTicketTool
            .call(
                serde_json::json!({"to": "bob", "task": "next", "result": "done part 1"}),
                &ctx,
            )
            .await
            .unwrap();

        let log = std::fs::read_to_string(dir.path().join("results.jsonl")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "only the parent finish writes a result line"
        );
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["ticket"], parent_key.as_str());
        assert_eq!(parsed["result"], "done part 1");
    }

    #[tokio::test]
    async fn schema_violation_aborts_atomically() {
        // Parent demands a string of at least 50 characters; we pass
        // a short string, which violates the schema but passes the
        // type check, so we exercise the schema-validation abort path.
        let dir = crate::test_util::TempDir::new().unwrap();
        let sys = TicketSystem::new();
        sys.dir(dir.path().to_path_buf());
        let schema = Schema::parse(serde_json::json!({
            "type": "string",
            "minLength": 50
        }))
        .unwrap();
        sys.insert(
            Ticket::new("strict parent").schema(schema).label("alice"),
            "tester".into(),
        );
        let parent_key = sys
            .claim(|t| t.status == Status::Todo, "alice")
            .expect("claim must succeed");
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

        let outcome = HandoverTicketTool
            .call(
                serde_json::json!({"to": "bob", "task": "next", "result": "too short"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::SchemaError(_)));

        let parent = sys.get_ticket(&parent_key).unwrap();
        assert_eq!(parent.status, Status::InProgress);
        assert!(parent.result.is_none());
        assert!(
            sys.get_ticket("TICKET-2").is_none(),
            "no child created on schema failure"
        );
        assert!(!dir.path().join("results.jsonl").exists());
    }

    /// Build a claimed parent whose own schema requires an object with a
    /// `status` field, so the handover result is validated structurally.
    fn one_ticket_with_object_schema(agent: &str, dir: PathBuf) -> (Arc<TicketSystem>, String) {
        let sys = TicketSystem::new();
        sys.dir(dir);
        let schema = Schema::parse(serde_json::json!({
            "type": "object",
            "required": ["status"]
        }))
        .unwrap();
        sys.insert(
            Ticket::new("strict parent").schema(schema).label(agent),
            "tester".into(),
        );
        let key = sys
            .claim(|t| t.status == Status::Todo, agent)
            .expect("claim must succeed");
        (sys, key)
    }

    #[tokio::test]
    async fn structured_result_validated_against_parent_schema_is_stored_as_object() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, parent_key) = one_ticket_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

        let outcome = HandoverTicketTool
            .call(
                serde_json::json!({"to": "bob", "task": "next", "result": {"status": "done"}}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));

        let parent = sys.get_ticket(&parent_key).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
        assert!(sys.get_ticket("TICKET-2").is_some());

        let log = std::fs::read_to_string(dir.path().join("results.jsonl")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(parsed["result"], serde_json::json!({"status": "done"}));
    }

    #[tokio::test]
    async fn object_schema_takes_result_fields_flat_alongside_control_keys() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, parent_key) = one_ticket_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

        // `status` sits at the top level next to `to`/`task`, no `result` wrapper.
        let outcome = HandoverTicketTool
            .call(
                serde_json::json!({"to": "bob", "task": "next", "status": "done"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)));

        let parent = sys.get_ticket(&parent_key).unwrap();
        assert_eq!(parent.status, Status::Finished);
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
    }

    #[tokio::test]
    async fn double_encoded_structured_result_is_decoded_to_object() {
        // The agent double-encodes the object as a JSON string; the parent
        // schema's validation decodes it so the stored value is the object.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, parent_key) = one_ticket_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

        HandoverTicketTool
            .call(
                serde_json::json!({"to": "bob", "task": "next", "result": "{\"status\":\"done\"}"}),
                &ctx,
            )
            .await
            .unwrap();

        let parent = sys.get_ticket(&parent_key).unwrap();
        assert_eq!(
            parent.result.as_ref(),
            Some(&serde_json::json!({"status": "done"}))
        );
    }

    #[tokio::test]
    async fn object_schema_violation_aborts_atomically() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, parent_key) = one_ticket_with_object_schema("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

        let outcome = HandoverTicketTool
            .call(
                serde_json::json!({"to": "bob", "task": "next", "result": {"wrong": 1}}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::SchemaError(_)));

        let parent = sys.get_ticket(&parent_key).unwrap();
        assert_eq!(parent.status, Status::InProgress);
        assert!(parent.result.is_none());
        assert!(
            sys.get_ticket("TICKET-2").is_none(),
            "no child created on schema failure"
        );
        assert!(!dir.path().join("results.jsonl").exists());
    }

    #[tokio::test]
    async fn rejects_missing_to() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, _key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());
        let outcome = HandoverTicketTool
            .call(serde_json::json!({"task": "x", "result": "y"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn rejects_empty_to() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, _key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());
        let outcome = HandoverTicketTool
            .call(
                serde_json::json!({"to": "  ", "task": "x", "result": "y"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn omitted_task_defaults_child_body_to_the_parent_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, _key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());
        let outcome = HandoverTicketTool
            .call(
                serde_json::json!({"to": "bob", "result": "alice's findings"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Success(_)), "{outcome:?}");

        let child = sys.get_ticket("TICKET-2").unwrap();
        assert_eq!(
            child.task,
            serde_json::Value::String("alice's findings".to_string()),
        );
    }

    #[tokio::test]
    async fn rejects_missing_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, _key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());
        let outcome = HandoverTicketTool
            .call(serde_json::json!({"to": "bob", "task": "x"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn rejects_null_or_empty_result() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, _key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());
        for body in [
            serde_json::json!({"to": "bob", "task": "x", "result": null}),
            serde_json::json!({"to": "bob", "task": "x", "result": ""}),
        ] {
            let outcome = HandoverTicketTool.call(body, &ctx).await.unwrap();
            assert!(matches!(outcome, ToolResult::Error(_)));
        }
    }

    #[tokio::test]
    async fn accepts_structured_result_without_schema() {
        // With no parent schema, any JSON value is a valid handoff result
        // and is stored verbatim; only `null`/empty string are rejected.
        for result_value in [
            serde_json::json!(42),
            serde_json::json!([1, 2, 3]),
            serde_json::json!({"k": "v"}),
        ] {
            let dir = crate::test_util::TempDir::new().unwrap();
            let (sys, parent_key) = one_ticket("alice", dir.path().to_path_buf());
            let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

            let outcome = HandoverTicketTool
                .call(
                    serde_json::json!({"to": "bob", "task": "next", "result": result_value}),
                    &ctx,
                )
                .await
                .unwrap();
            assert!(matches!(outcome, ToolResult::Success(_)));

            let parent = sys.get_ticket(&parent_key).unwrap();
            assert_eq!(parent.status, Status::Finished);
            assert_eq!(parent.result.as_ref(), Some(&result_value));
            assert!(sys.get_ticket("TICKET-2").is_some());
        }
    }

    #[tokio::test]
    async fn rejects_non_string_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, _key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());
        let outcome = HandoverTicketTool
            .call(
                serde_json::json!({"to": "bob", "task": 42, "result": "ok"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn errors_when_no_current_ticket() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let sys = TicketSystem::new();
        sys.dir(dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());
        let outcome = HandoverTicketTool
            .call(
                serde_json::json!({"to": "bob", "task": "x", "result": "y"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, ToolResult::Error(_)));
    }

    #[tokio::test]
    async fn substitutes_parent_key_and_result_in_task() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, parent_key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

        HandoverTicketTool
            .call(
                serde_json::json!({
                    "to": "bob",
                    "task": "Continue {parent_key}: {parent_result}",
                    "result": "alice's findings"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let child = sys.get_ticket("TICKET-2").unwrap();
        assert_eq!(
            child.task,
            serde_json::Value::String(format!("Continue {parent_key}: alice's findings")),
        );
    }

    #[tokio::test]
    async fn unknown_placeholders_pass_through() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, parent_key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

        HandoverTicketTool
            .call(
                serde_json::json!({
                    "to": "bob",
                    "task": "See {parent_key} and {unknown}",
                    "result": "ok"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let child = sys.get_ticket("TICKET-2").unwrap();
        assert_eq!(
            child.task,
            serde_json::Value::String(format!("See {parent_key} and {{unknown}}")),
        );
    }

    #[tokio::test]
    async fn substitution_is_single_pass() {
        // A `result` that itself contains the literal text `{parent_key}`
        // must NOT be re-expanded — the substitution pass runs once
        // per placeholder, not recursively.
        let dir = crate::test_util::TempDir::new().unwrap();
        let (sys, parent_key) = one_ticket("alice", dir.path().to_path_buf());
        let ctx = ctx_with(Arc::clone(&sys), "alice", dir.path().to_path_buf());

        HandoverTicketTool
            .call(
                serde_json::json!({
                    "to": "bob",
                    "task": "[{parent_result}]",
                    "result": "{parent_key}"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let child = sys.get_ticket("TICKET-2").unwrap();
        assert_eq!(
            child.task,
            serde_json::Value::String("[{parent_key}]".to_string()),
            "result containing `{{parent_key}}` should be inserted literally, \
             not recursively expanded (parent_key was {parent_key})",
        );
    }
}
