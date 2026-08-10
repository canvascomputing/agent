//! The tools an agent reaches its own ticket queue through: reading it, adding
//! to it, and finishing the ticket it holds.

use serde_json::Value;

use crate::agents::tickets::{Status, Ticket, TicketError, TicketQueue};

use super::tool::{ToolContext, ToolResult};

mod finish;
mod manage_tickets;
mod read_tickets;
mod result_shape;

pub use finish::FinishTool;
pub use manage_tickets::ManageTicketsTool;
pub use read_tickets::ReadTicketsTool;
pub(crate) use result_shape::finish_tool_input_schema;

/// Action sets each multi-action tool exposes. Keeps the dispatch logic
/// in one place and lets each tool reject actions outside its
/// allow-list with a uniform error message.
pub(super) const READ_ACTIONS: &[&str] = &["get", "list", "search"];
pub(super) const WRITE_ACTIONS: &[&str] = &["create", "edit"];

/// Name of the tool that finishes a ticket. The request builder matches tool
/// definitions against it to advertise the ticket's schema on its arguments.
pub(crate) const TICKET_FINISH_TOOL: &str = "finish";

pub(super) fn dispatch(input: Value, ctx: &ToolContext, allowed: &[&str]) -> ToolResult {
    let action = match input["action"].as_str() {
        Some(a) => a,
        None => return ToolResult::error("Missing required parameter: action"),
    };
    if !allowed.contains(&action) {
        return ToolResult::error(format!(
            "Action `{action}` is not supported by this tool. Allowed: {}",
            allowed.join(", ")
        ));
    }
    let Some(ticket_queue) = ctx.ticket_queue_handle().cloned() else {
        return ToolResult::error("Ticket queue unavailable in this context");
    };

    match action {
        "get" => action_get(&ticket_queue, &input, ctx),
        "list" => action_list(&ticket_queue, &input),
        "search" => action_search(&ticket_queue, &input),
        "create" => action_create(&ticket_queue, &input, ctx),
        "edit" => action_edit(&ticket_queue, &input, ctx),
        other => ToolResult::error(format!("Unknown action `{other}`")),
    }
}

fn resolve_key(
    ticket_queue: &TicketQueue,
    input: &Value,
    ctx: &ToolContext,
) -> Result<String, ToolResult> {
    if let Some(k) = input["key"].as_str() {
        return Ok(k.to_string());
    }
    resolve_current_key(ticket_queue, ctx)
}

pub(super) fn resolve_current_key(
    ticket_queue: &TicketQueue,
    ctx: &ToolContext,
) -> Result<String, ToolResult> {
    if let Some(key) = ctx.ticket_key.as_deref() {
        return Ok(key.to_string());
    }
    let agent_id = ctx.agent_id_str().ok_or_else(|| {
        ToolResult::error("Missing `key` and no agent_id set on this tool context")
    })?;
    match ticket_queue
        .find_ticket(|t| t.status == Status::InProgress && t.assignee.as_deref() == Some(agent_id))
    {
        Some(t) => Ok(t.key.clone()),
        None => Err(ToolResult::error(
            "Missing `key` and no current ticket assigned to this agent",
        )),
    }
}

pub(super) fn ticket_error_message(err: TicketError) -> String {
    err.to_string()
}

fn render_ticket(t: &Ticket) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n", t.key));
    out.push_str(&format!("- status: {}\n", status_label(t.status)));
    out.push_str(&format!("- reporter: {}\n", t.reporter));
    let labels_label = if t.labels.is_empty() {
        "(none)".to_string()
    } else {
        t.labels.join(", ")
    };
    out.push_str(&format!("- labels: {labels_label}\n"));
    if let Some(parent) = t.parent.as_deref() {
        out.push_str(&format!("- parent: {parent}\n"));
    }
    out.push('\n');
    match &t.task {
        serde_json::Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        other => {
            out.push_str("```json\n");
            out.push_str(&serde_json::to_string_pretty(other).unwrap_or_default());
            out.push_str("\n```\n");
        }
    }
    out.push_str("\n## Result\n");
    match t.result.as_ref() {
        Some(serde_json::Value::String(s)) => out.push_str(s),
        Some(other) => out.push_str(&other.to_string()),
        None => out.push_str("(no result)"),
    }
    out.push('\n');
    out
}

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Todo => "Todo",
        Status::InProgress => "InProgress",
        Status::Finished => "Finished",
        Status::Failed => "Failed",
    }
}

fn parse_status_for_list(s: &str) -> Result<Status, ToolResult> {
    match s {
        "Todo" => Ok(Status::Todo),
        "InProgress" => Ok(Status::InProgress),
        "Finished" => Ok(Status::Finished),
        "Failed" => Ok(Status::Failed),
        other => Err(ToolResult::error(format!(
            "Invalid status `{other}`. Expected one of Todo, InProgress, Finished, Failed"
        ))),
    }
}

fn truncate_for_preview(s: &str, max: usize) -> String {
    let one_line = s.lines().next().unwrap_or("");
    if one_line.chars().count() <= max {
        one_line.to_string()
    } else {
        let cut: String = one_line.chars().take(max).collect();
        format!("{cut}…")
    }
}

type SummaryRow<'a> = (&'a str, &'a str, Status, &'a [String]);

fn render_summary_list(tickets: &[SummaryRow<'_>]) -> String {
    let mut out = String::new();
    for (key, task_preview, status, labels) in tickets {
        let labels_label = if labels.is_empty() {
            String::new()
        } else {
            format!("[{}] ", labels.join(","))
        };
        out.push_str(&format!(
            "- {key} [{status}] {labels_label}{task_preview}\n",
            status = status_label(*status),
        ));
    }
    out
}

fn task_preview(task: &serde_json::Value) -> String {
    let raw = match task {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    truncate_for_preview(&raw, 80)
}

fn action_get(ticket_queue: &TicketQueue, input: &Value, ctx: &ToolContext) -> ToolResult {
    let key = match resolve_key(ticket_queue, input, ctx) {
        Ok(k) => k,
        Err(e) => return e,
    };
    match ticket_queue.get_ticket(&key) {
        Some(t) => ToolResult::success(render_ticket(&t)),
        None => ToolResult::error(format!("Ticket {key} not found")),
    }
}

fn action_list(ticket_queue: &TicketQueue, input: &Value) -> ToolResult {
    let label = input["label"].as_str().map(String::from);
    let status = input["status"].as_str().map(parse_status_for_list);
    let status = match status {
        Some(Ok(s)) => Some(s),
        Some(Err(e)) => return e,
        None => None,
    };

    let pool: Vec<Ticket> = ticket_queue.find_tickets(|t| {
        let status_ok = match status {
            Some(s) => t.status == s,
            None => true,
        };
        let label_ok = match label.as_deref() {
            Some(l) => t.labels.iter().any(|x| x == l),
            None => true,
        };
        status_ok && label_ok
    });

    if pool.is_empty() {
        return ToolResult::success("(no matching tickets)".to_string());
    }
    let previews: Vec<String> = pool
        .iter()
        .take(50)
        .map(|t| task_preview(&t.task))
        .collect();
    let rows: Vec<SummaryRow<'_>> = pool
        .iter()
        .take(50)
        .zip(previews.iter())
        .map(|(t, p)| (t.key.as_str(), p.as_str(), t.status, t.labels.as_slice()))
        .collect();
    ToolResult::success(render_summary_list(&rows))
}

fn action_search(ticket_queue: &TicketQueue, input: &Value) -> ToolResult {
    let query = match input["query"].as_str() {
        Some(q) => q,
        None => return ToolResult::error("Missing required parameter: query"),
    };
    let needle = query.to_lowercase();
    let hits = ticket_queue.find_tickets(|t| match &t.task {
        Value::String(s) => s.to_lowercase().contains(&needle),
        other => other.to_string().to_lowercase().contains(&needle),
    });
    if hits.is_empty() {
        return ToolResult::success("(no matching tickets)".to_string());
    }
    let previews: Vec<String> = hits
        .iter()
        .take(50)
        .map(|t| task_preview(&t.task))
        .collect();
    let rows: Vec<SummaryRow<'_>> = hits
        .iter()
        .take(50)
        .zip(previews.iter())
        .map(|(t, p)| (t.key.as_str(), p.as_str(), t.status, t.labels.as_slice()))
        .collect();
    ToolResult::success(render_summary_list(&rows))
}

fn action_create(ticket_queue: &TicketQueue, input: &Value, ctx: &ToolContext) -> ToolResult {
    let task = match input.get("task") {
        Some(v) => v.clone(),
        None => return ToolResult::error("Missing required parameter: task"),
    };

    let labels: Vec<String> = match input.get("labels") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(_) => return ToolResult::error("`labels` must be an array of strings"),
    };

    let ticket = Ticket::new(task).labels(labels);

    let reporter = ctx
        .agent_id_str()
        .expect("agent_id on ToolContext")
        .to_string();
    let key = ticket_queue.insert(ticket, reporter);
    ToolResult::success(format!("Created ticket {key}"))
}

fn action_edit(ticket_queue: &TicketQueue, input: &Value, ctx: &ToolContext) -> ToolResult {
    let key = match resolve_key(ticket_queue, input, ctx) {
        Ok(k) => k,
        Err(e) => return e,
    };

    let new_task = input.get("task").cloned();
    let new_labels: Option<Vec<String>> = match input.get("labels") {
        Some(Value::Array(arr)) => Some(
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
        ),
        Some(Value::Null) | None => None,
        Some(_) => return ToolResult::error("`labels` must be an array of strings"),
    };
    if new_task.is_none() && new_labels.is_none() {
        return ToolResult::error("Edit needs at least one of `task` or `labels`");
    }

    match ticket_queue.edit(&key, new_task, new_labels) {
        Ok(()) => ToolResult::success(format!("Edited ticket {key}")),
        Err(e) => ToolResult::error(ticket_error_message(e)),
    }
}

/// Validate `result` against the ticket's schema, append an NDJSON
/// `{ticket, result}` line to the configured results directory, attach
/// the payload to the ticket, and transition the ticket to `Finished`.
/// The `ticket` field is the resolved key. Called by `FinishTool` when no
/// `handover` was requested.
pub(super) fn write_result(
    ticket_queue: &TicketQueue,
    key: &str,
    result: Value,
    agent: &str,
) -> ToolResult {
    if let Err(violations) = ticket_queue.set_result(key, result) {
        return ToolResult::schema_error(violations.to_string());
    }
    match ticket_queue.set_finished_by(key, agent) {
        Ok(()) => ToolResult::success(format!("Ticket {key} marked finished")),
        Err(e) => ToolResult::error(ticket_error_message(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tool::ToolLike;
    use super::*;
    use crate::agents::tickets::TicketQueue;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// Build a context for a tool test, optionally with a "current
    /// ticket" already InProgress and assigned to `agent`.
    fn ctx_with(ticket_queue: Arc<TicketQueue>, agent: &str) -> ToolContext {
        ToolContext::new(PathBuf::from("/tmp"))
            .ticket_queue(ticket_queue)
            .agent_id(agent.to_string())
    }

    /// Insert one Todo ticket, claim it for `agent` (atomically labels +
    /// transitions to InProgress), so `queue.find_ticket(...)` resolves it
    /// as the current ticket for `agent`. The queue is rooted at its own
    /// isolated temp directory so the default `.agentwerk` writes never
    /// leak into the source tree.
    fn shared_with_one_ticket(agent: &str) -> (Arc<TicketQueue>, String) {
        let queue = TicketQueue::new();
        queue.dir(isolated_test_dir());
        queue.insert(Ticket::new("body").label(agent), "tester".into());
        let key = queue
            .claim(|t| t.status == Status::Todo, agent)
            .expect("claim must succeed");
        (queue, key)
    }

    /// A fresh directory for each `TicketQueue` under a process-lifetime,
    /// self-deleting temp root. Per-call isolation matters because `insert`
    /// numbers new keys past the highest `TICKET-<N>` folder already on disk,
    /// so a shared directory would leak ticket ids between tests.
    fn isolated_test_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::OnceLock;
        static ROOT: OnceLock<crate::test_util::TempDir> = OnceLock::new();
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = ROOT.get_or_init(|| crate::test_util::TempDir::new().unwrap());
        root.path()
            .join(format!("queue-{}", COUNTER.fetch_add(1, Ordering::Relaxed)))
    }

    async fn call(tool: &dyn ToolLike, input: serde_json::Value, ctx: &ToolContext) -> ToolResult {
        tool.call(input, ctx).await.unwrap()
    }

    fn unwrap_text(result: &ToolResult) -> &str {
        let (ToolResult::Success(s) | ToolResult::Error(s) | ToolResult::SchemaError(s)) = result;
        s
    }

    #[tokio::test]
    async fn read_get_defaults_key_to_current_ticket() {
        let (queue, key) = shared_with_one_ticket("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(&ReadTicketsTool, serde_json::json!({"action": "get"}), &ctx).await;
        let text = unwrap_text(&result);
        assert!(text.contains(&key), "expected key in output: {text}");
        assert!(text.contains("body"));
    }

    #[tokio::test]
    async fn read_list_filters_by_status() {
        let queue = TicketQueue::new();
        queue.dir(isolated_test_dir());
        queue.insert(Ticket::new("a"), "tester".into());
        queue.insert(Ticket::new("b"), "tester".into());
        queue.claim(|t| t.key == "TICKET-1", "alice");

        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            &ReadTicketsTool,
            serde_json::json!({"action": "list", "status": "InProgress"}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);
        assert!(text.contains("TICKET-1"));
        assert!(!text.contains("TICKET-2"));
    }

    #[tokio::test]
    async fn manage_create_stamps_reporter_from_agent_id() {
        let queue = TicketQueue::new();
        queue.dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            &ManageTicketsTool,
            serde_json::json!({"action": "create", "task": "new ticket"}),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success(_)));
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.task, serde_json::Value::String("new ticket".into()));
        assert_eq!(t.reporter, "alice");
    }

    #[tokio::test]
    async fn manage_create_with_labels_attaches_them() {
        let queue = TicketQueue::new();
        queue.dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            &ManageTicketsTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "labels": ["research"]
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success(_)));
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.labels, vec!["research".to_string()]);
        assert_eq!(t.status, Status::Todo);
    }

    #[tokio::test]
    async fn manage_create_with_named_label_routes_to_agent() {
        let queue = TicketQueue::new();
        queue.dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            &ManageTicketsTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "labels": ["alice"]
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success(_)));
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert!(t.labels.iter().any(|l| l == "alice"));
        assert_eq!(t.status, Status::Todo);
    }

    #[tokio::test]
    async fn a_ticket_the_model_creates_takes_the_schema_bound_to_its_label() {
        let queue = TicketQueue::new();
        queue.dir(isolated_test_dir());
        let schemas = crate::schemas::SchemaStore::new();
        schemas
            .label("analysis", serde_json::json!({"type": "string"}))
            .unwrap();
        queue.schemas(&schemas);

        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            &ManageTicketsTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "labels": ["analysis"]
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success(_)));
        assert!(queue.get_ticket("TICKET-1").unwrap().schema.is_none());

        queue.claim(|t| t.has_label("analysis"), "bob");
        assert!(queue.get_ticket("TICKET-1").unwrap().schema.is_some());
    }

    #[tokio::test]
    async fn manage_edit_updates_task_and_labels() {
        let (queue, key) = shared_with_one_ticket("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            &ManageTicketsTool,
            serde_json::json!({
                "action": "edit",
                "task": "new body",
                "labels": ["urgent", "review"]
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success(_)));
        let t = queue.get_ticket(&key).unwrap();
        assert_eq!(t.task, serde_json::Value::String("new body".into()));
        assert_eq!(t.labels, vec!["urgent".to_string(), "review".to_string()]);
    }

    #[tokio::test]
    async fn manage_rejects_unsupported_actions() {
        let (queue, _key) = shared_with_one_ticket("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        for action in ["done", "transition", "comment", "assign", "attach"] {
            let result = call(
                &ManageTicketsTool,
                serde_json::json!({"action": action}),
                &ctx,
            )
            .await;
            assert!(
                matches!(result, ToolResult::Error(_)),
                "{action}: {result:?}"
            );
        }
    }
}
