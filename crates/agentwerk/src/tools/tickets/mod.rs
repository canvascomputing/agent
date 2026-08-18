//! The tools an agent reaches its own ticket queue through: reading it, adding
//! to it, and finishing the ticket it holds.

use std::path::Path;

use serde_json::Value;

use crate::agents::tickets::{Status, Ticket, TicketError, TicketQueue};
use crate::prompts::directives::{
    DirectiveStore, TICKET_EDIT_INCOMPLETE, TICKET_KEY_MISSING, TICKET_NOT_ASSIGNED,
    TICKET_NOT_FOUND, TICKET_QUEUE_UNAVAILABLE, TICKET_RESULT_MISSING, TICKET_STATUS_UNKNOWN,
    TICKET_TRANSITION_REJECTED,
};

use super::tool::{ToolContext, ToolResult};

mod finish;
mod tickets;

pub use finish::FinishTool;
pub use tickets::TicketsTool;

/// What the model asks the queue to do. The schema declares `action` as the
/// discriminator and states which fields each one requires; the variants say
/// the same in Rust, so `search` cannot arrive without a `query`.
#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum TicketsArgs {
    Ticket {
        key: Option<String>,
    },
    Result {
        key: Option<String>,
    },
    List {
        status: Option<String>,
        label: Option<String>,
    },
    Search {
        query: String,
    },
    Create {
        task: Value,
        label: Option<String>,
    },
    Edit {
        key: Option<String>,
        task: Option<Value>,
        label: Option<String>,
    },
}

pub(super) fn dispatch(args: TicketsArgs, ctx: &ToolContext) -> ToolResult {
    let Some(queue) = ctx.ticket_queue.clone() else {
        return ToolResult::error(ctx.directives.render(TICKET_QUEUE_UNAVAILABLE, &[]));
    };

    match args {
        TicketsArgs::Ticket { key } => action_ticket(&queue, key, ctx),
        TicketsArgs::Result { key } => action_result(&queue, key, ctx),
        TicketsArgs::List { status, label } => action_list(&queue, status, label, &ctx.directives),
        TicketsArgs::Search { query } => action_search(&queue, &query),
        TicketsArgs::Create { task, label } => action_create(&queue, task, label, ctx),
        TicketsArgs::Edit { key, task, label } => action_edit(&queue, key, task, label, ctx),
    }
}

/// The ticket an action names, or the one this agent is holding.
fn resolve_key(
    ticket_queue: &TicketQueue,
    key: Option<String>,
    ctx: &ToolContext,
) -> Result<String, ToolResult> {
    match key {
        Some(key) => Ok(key),
        None => resolve_current_key(ticket_queue, ctx),
    }
}

pub(super) fn resolve_current_key(
    ticket_queue: &TicketQueue,
    ctx: &ToolContext,
) -> Result<String, ToolResult> {
    if let Some(key) = ctx.ticket_key.as_deref() {
        return Ok(key.to_string());
    }
    let agent_id = ctx
        .agent_id
        .as_deref()
        .ok_or_else(|| ToolResult::error(ctx.directives.render(TICKET_KEY_MISSING, &[])))?;
    match ticket_queue.find_ticket(|t: &Ticket| {
        t.status == Status::InProgress && t.assignee.as_deref() == Some(agent_id)
    }) {
        Some(t) => Ok(t.key.clone()),
        None => Err(ToolResult::error(
            ctx.directives.render(TICKET_NOT_ASSIGNED, &[]),
        )),
    }
}

pub(super) fn ticket_error_message(err: TicketError, directives: &DirectiveStore) -> String {
    directives.render(TICKET_TRANSITION_REJECTED, &[("error", &err.to_string())])
}

fn render_ticket(t: &Ticket) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n", t.key));
    out.push_str(&format!("- status: {}\n", status_label(t.status)));
    out.push_str(&format!("- reporter: {}\n", t.reporter));
    out.push_str(&format!(
        "- label: {}\n",
        t.label.as_deref().unwrap_or("(none)")
    ));
    if let Some(parent) = t.parent.as_deref() {
        out.push_str(&format!("- parent: {parent}\n"));
    }
    out.push('\n');
    push_value(&mut out, &t.task);
    out.push_str("\n## Result\n");
    match t.result.as_ref() {
        Some(result) => push_value(&mut out, result),
        None => out.push_str("(no result)\n"),
    }
    out
}

/// The result of ticket `key`, with the file it is stored in, so the agent
/// can read it again without asking for it.
fn render_result(key: &str, path: &Path, result: &Value) -> String {
    let mut out = format!("# {key} result\n- file: {}\n\n", path.display());
    push_value(&mut out, result);
    out
}

/// Add a value the way it reads best: a string as it stands, anything
/// structured as pretty JSON in a fence.
fn push_value(out: &mut String, value: &Value) {
    match value {
        Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        other => {
            out.push_str("```json\n");
            out.push_str(&serde_json::to_string_pretty(other).unwrap_or_default());
            out.push_str("\n```\n");
        }
    }
}

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Todo => "Todo",
        Status::InProgress => "InProgress",
        Status::Finished => "Finished",
        Status::Failed => "Failed",
    }
}

fn parse_status_for_list(s: &str, directives: &DirectiveStore) -> Result<Status, ToolResult> {
    match s {
        "Todo" => Ok(Status::Todo),
        "InProgress" => Ok(Status::InProgress),
        "Finished" => Ok(Status::Finished),
        "Failed" => Ok(Status::Failed),
        other => Err(ToolResult::error(
            directives.render(TICKET_STATUS_UNKNOWN, &[("status", other)]),
        )),
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

type SummaryRow<'a> = (&'a str, &'a str, Status, Option<&'a str>);

fn render_summary_list(tickets: &[SummaryRow<'_>]) -> String {
    let mut out = String::new();
    for (key, task_preview, status, label) in tickets {
        // An unlabelled ticket prints no marker: a scan of up to 50 rows does
        // not need "(none)" on every default-scope line.
        let label = match label {
            Some(l) => format!("[{l}] "),
            None => String::new(),
        };
        out.push_str(&format!(
            "- {key} [{status}] {label}{task_preview}\n",
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

fn action_ticket(ticket_queue: &TicketQueue, key: Option<String>, ctx: &ToolContext) -> ToolResult {
    let key = match resolve_key(ticket_queue, key, ctx) {
        Ok(k) => k,
        Err(e) => return e,
    };
    match ticket_queue.get_ticket(&key) {
        Some(t) => ToolResult::success(render_ticket(&t)),
        None => ToolResult::error(ctx.directives.render(TICKET_NOT_FOUND, &[("key", &key)])),
    }
}

fn action_result(ticket_queue: &TicketQueue, key: Option<String>, ctx: &ToolContext) -> ToolResult {
    let key = match resolve_key(ticket_queue, key, ctx) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let Some(ticket) = ticket_queue.get_ticket(&key) else {
        return ToolResult::error(ctx.directives.render(TICKET_NOT_FOUND, &[("key", &key)]));
    };
    match ticket.result.as_ref() {
        Some(result) => {
            ToolResult::success(render_result(&key, &ticket_queue.result_path(&key), result))
        }
        None => ToolResult::error(ctx.directives.render(
            TICKET_RESULT_MISSING,
            &[("key", &key), ("status", status_label(ticket.status))],
        )),
    }
}

fn action_list(
    ticket_queue: &TicketQueue,
    status: Option<String>,
    label: Option<String>,
    directives: &DirectiveStore,
) -> ToolResult {
    let status = match status
        .as_deref()
        .map(|s| parse_status_for_list(s, directives))
    {
        Some(Ok(s)) => Some(s),
        Some(Err(e)) => return e,
        None => None,
    };

    let pool: Vec<Ticket> = ticket_queue.find_tickets(|t: &Ticket| {
        let status_ok = match status {
            Some(s) => t.status == s,
            None => true,
        };
        let label_ok = match label.as_deref() {
            Some(l) => t.has_label(l),
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
        .map(|(t, p)| (t.key.as_str(), p.as_str(), t.status, t.label.as_deref()))
        .collect();
    ToolResult::success(render_summary_list(&rows))
}

fn action_search(ticket_queue: &TicketQueue, query: &str) -> ToolResult {
    let needle = query.to_lowercase();
    let hits = ticket_queue.find_tickets(|t: &Ticket| match &t.task {
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
        .map(|(t, p)| (t.key.as_str(), p.as_str(), t.status, t.label.as_deref()))
        .collect();
    ToolResult::success(render_summary_list(&rows))
}

fn action_create(
    ticket_queue: &TicketQueue,
    task: Value,
    label: Option<String>,
    ctx: &ToolContext,
) -> ToolResult {
    let mut ticket = Ticket::new(task);
    if let Some(label) = label {
        ticket = ticket.label(label);
    }

    let reporter = ctx
        .agent_id
        .as_deref()
        .expect("agent_id on ToolContext")
        .to_string();
    let key = ticket_queue.insert(ticket, reporter);
    ToolResult::success(format!("Created ticket {key}"))
}

fn action_edit(
    ticket_queue: &TicketQueue,
    key: Option<String>,
    new_task: Option<Value>,
    new_label: Option<String>,
    ctx: &ToolContext,
) -> ToolResult {
    let key = match resolve_key(ticket_queue, key, ctx) {
        Ok(k) => k,
        Err(e) => return e,
    };
    if new_task.is_none() && new_label.is_none() {
        return ToolResult::error(ctx.directives.render(TICKET_EDIT_INCOMPLETE, &[]));
    }

    match ticket_queue.edit(&key, new_task, new_label) {
        Ok(()) => ToolResult::success(format!("Edited ticket {key}")),
        Err(e) => ToolResult::error(ticket_error_message(e, &ctx.directives)),
    }
}

#[cfg(test)]
mod tests {
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

    async fn call(
        tool: impl Into<crate::tools::Tool>,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> ToolResult {
        tool.into().call(input, ctx).await
    }

    fn unwrap_text(result: &ToolResult) -> &str {
        let s = result.content();
        s
    }

    #[tokio::test]
    async fn ticket_defaults_key_to_current_ticket() {
        let (queue, key) = shared_with_one_ticket("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(TicketsTool, serde_json::json!({"action": "ticket"}), &ctx).await;
        let text = unwrap_text(&result);
        assert!(text.contains(&key), "expected key in output: {text}");
        assert!(text.contains("body"));
    }

    #[tokio::test]
    async fn result_returns_the_result_of_another_agents_ticket() {
        let (queue, key) = shared_with_one_ticket("alice");
        queue
            .set_result(&key, serde_json::json!({"finding": "a lead"}))
            .unwrap();

        let ctx = ctx_with(Arc::clone(&queue), "bob");
        let result = call(
            TicketsTool,
            serde_json::json!({"action": "result", "key": key}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);
        assert!(matches!(result, ToolResult::Success { .. }), "{text}");
        assert!(text.contains("a lead"), "expected the result: {text}");
        assert!(
            text.contains("result.json"),
            "expected the result file: {text}"
        );
    }

    #[tokio::test]
    async fn result_defaults_key_to_current_ticket() {
        let (queue, key) = shared_with_one_ticket("alice");
        queue
            .set_result(&key, serde_json::json!("what alice found"))
            .unwrap();

        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(TicketsTool, serde_json::json!({"action": "result"}), &ctx).await;
        let text = unwrap_text(&result);
        assert!(text.contains("what alice found"), "{text}");
    }

    #[tokio::test]
    async fn result_errors_while_the_ticket_has_no_result() {
        let (queue, key) = shared_with_one_ticket("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TicketsTool,
            serde_json::json!({"action": "result", "key": key}),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Error { .. }), "{result:?}");
        assert!(unwrap_text(&result).contains("InProgress"));
    }

    #[tokio::test]
    async fn list_filters_by_status() {
        let queue = TicketQueue::new();
        queue.dir(isolated_test_dir());
        queue.insert(Ticket::new("a"), "tester".into());
        queue.insert(Ticket::new("b"), "tester".into());
        queue.claim(|t| t.key == "TICKET-1", "alice");

        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TicketsTool,
            serde_json::json!({"action": "list", "status": "InProgress"}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);
        assert!(text.contains("TICKET-1"));
        assert!(!text.contains("TICKET-2"));
    }

    #[tokio::test]
    async fn create_stamps_reporter_from_agent_id() {
        let queue = TicketQueue::new();
        queue.dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TicketsTool,
            serde_json::json!({"action": "create", "task": "new ticket"}),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success { .. }));
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.task, serde_json::Value::String("new ticket".into()));
        assert_eq!(t.reporter, "alice");
    }

    #[tokio::test]
    async fn create_with_a_label_attaches_it() {
        let queue = TicketQueue::new();
        queue.dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TicketsTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "label": "research"
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success { .. }));
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert!(t.has_label("research"));
        assert_eq!(t.status, Status::Todo);
    }

    #[tokio::test]
    async fn create_with_named_label_routes_to_agent() {
        let queue = TicketQueue::new();
        queue.dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TicketsTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "label": "alice"
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success { .. }));
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert!(t.has_label("alice"));
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
            TicketsTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "label": "analysis"
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success { .. }));
        assert!(queue.get_ticket("TICKET-1").unwrap().schema.is_none());

        queue.claim(|t| t.has_label("analysis"), "bob");
        assert!(queue.get_ticket("TICKET-1").unwrap().schema.is_some());
    }

    #[tokio::test]
    async fn edit_replaces_the_task_and_the_label() {
        let (queue, key) = shared_with_one_ticket("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TicketsTool,
            serde_json::json!({
                "action": "edit",
                "task": "new body",
                "label": "urgent"
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success { .. }));
        let t = queue.get_ticket(&key).unwrap();
        assert_eq!(t.task, serde_json::Value::String("new body".into()));
        assert!(t.has_label("urgent"));
    }

    #[tokio::test]
    async fn unsupported_actions_are_rejected() {
        let (queue, _key) = shared_with_one_ticket("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        for action in ["done", "transition", "comment", "assign", "attach"] {
            let result = call(TicketsTool, serde_json::json!({"action": action}), &ctx).await;
            assert!(
                matches!(result, ToolResult::Error { .. }),
                "{action}: {result:?}"
            );
        }
    }
}
