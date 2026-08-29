//! The tools an agent reaches its own task queue through: reading it, adding
//! to it, and finishing the task it holds.

use std::path::Path;

use serde_json::Value;

use crate::agents::tasks::{Queue, Status, Task, TaskError};
use crate::agents::Query;
use crate::prompts::directives::{
    DirectiveStore, QUEUE_UNAVAILABLE, TASK_EDIT_INCOMPLETE, TASK_KEY_MISSING, TASK_NOT_ASSIGNED,
    TASK_NOT_FOUND, TASK_QUERY_INVALID, TASK_RESULT_MISSING, TASK_TRANSITION_REJECTED,
};

use super::tool::{ToolContext, ToolResult};

mod finish;
mod tasks;

pub use finish::FinishTool;
pub use tasks::TasksTool;

/// What the model asks the queue to do. The schema declares `action` as the
/// discriminator and states which fields each one requires; the variants say
/// the same in Rust, so `search` cannot arrive without a `query`.
#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum TasksArgs {
    Task {
        key: Option<String>,
    },
    Result {
        key: Option<String>,
    },
    List {
        aql: Option<String>,
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

pub(super) fn dispatch(args: TasksArgs, ctx: &ToolContext) -> ToolResult {
    let Some(queue) = ctx.queue.clone() else {
        return ToolResult::error(ctx.directives.render(QUEUE_UNAVAILABLE, &[]));
    };

    match args {
        TasksArgs::Task { key } => action_task(&queue, key, ctx),
        TasksArgs::Result { key } => action_result(&queue, key, ctx),
        TasksArgs::List { aql } => action_list(&queue, aql, &ctx.directives),
        TasksArgs::Create { task, label } => action_create(&queue, task, label, ctx),
        TasksArgs::Edit { key, task, label } => action_edit(&queue, key, task, label, ctx),
    }
}

/// The task an action names, or the one this agent is holding.
fn resolve_key(
    queue: &Queue,
    key: Option<String>,
    ctx: &ToolContext,
) -> Result<String, ToolResult> {
    match key {
        Some(key) => Ok(key),
        None => resolve_current_key(queue, ctx),
    }
}

pub(super) fn resolve_current_key(queue: &Queue, ctx: &ToolContext) -> Result<String, ToolResult> {
    if let Some(key) = ctx.task_key.as_deref() {
        return Ok(key.to_string());
    }
    // A closure, never `agent = {id}`: an id derives from a host-supplied label,
    // and AQL binds no values, so one carrying `=` or a quote rewrites the query.
    let agent_id = ctx
        .agent_id
        .clone()
        .ok_or_else(|| ToolResult::error(ctx.directives.render(TASK_KEY_MISSING, &[])))?;
    match queue.find_task(move |t: &Task| {
        t.status == Status::InProgress && t.assignee.as_deref() == Some(agent_id.as_str())
    }) {
        Some(t) => Ok(t.key.clone()),
        None => Err(ToolResult::error(
            ctx.directives.render(TASK_NOT_ASSIGNED, &[]),
        )),
    }
}

pub(super) fn task_error_message(err: TaskError, directives: &DirectiveStore) -> String {
    directives.render(TASK_TRANSITION_REJECTED, &[("error", &err.to_string())])
}

fn render_task(t: &Task) -> String {
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

/// The result of task `key`, with the file it is stored in, so the agent
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

fn render_summary_list(tasks: &[SummaryRow<'_>]) -> String {
    let mut out = String::new();
    for (key, task_preview, status, label) in tasks {
        // An unlabelled task prints no marker: a scan of up to 50 rows does
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

fn action_task(queue: &Queue, key: Option<String>, ctx: &ToolContext) -> ToolResult {
    let key = match resolve_key(queue, key, ctx) {
        Ok(k) => k,
        Err(e) => return e,
    };
    match queue.get_task(&key) {
        Some(t) => ToolResult::success(render_task(&t)),
        None => ToolResult::error(ctx.directives.render(TASK_NOT_FOUND, &[("key", &key)])),
    }
}

fn action_result(queue: &Queue, key: Option<String>, ctx: &ToolContext) -> ToolResult {
    let key = match resolve_key(queue, key, ctx) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let Some(task) = queue.get_task(&key) else {
        return ToolResult::error(ctx.directives.render(TASK_NOT_FOUND, &[("key", &key)]));
    };
    match task.result.as_ref() {
        Some(result) => ToolResult::success(render_result(&key, &queue.result_path(&key), result)),
        None => ToolResult::error(ctx.directives.render(
            TASK_RESULT_MISSING,
            &[("key", &key), ("status", status_label(task.status))],
        )),
    }
}

fn action_list(queue: &Queue, aql: Option<String>, directives: &DirectiveStore) -> ToolResult {
    let pool: Vec<Task> = match aql.as_deref().map(Query::new) {
        Some(Ok(query)) => queue.find_tasks(query),
        Some(Err(error)) => {
            return ToolResult::error(
                directives.render(TASK_QUERY_INVALID, &[("error", &error.to_string())]),
            )
        }
        None => queue.get_tasks(),
    };
    if pool.is_empty() {
        return ToolResult::success("(no matching tasks)".to_string());
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

fn action_create(
    queue: &Queue,
    task: Value,
    label: Option<String>,
    ctx: &ToolContext,
) -> ToolResult {
    let mut task = Task::new(task);
    if let Some(label) = label {
        task = task.label(label);
    }

    let reporter = ctx
        .agent_id
        .as_deref()
        .expect("agent_id on ToolContext")
        .to_string();
    let key = queue.insert(task, reporter);
    ToolResult::success(format!("Created task {key}"))
}

fn action_edit(
    queue: &Queue,
    key: Option<String>,
    new_task: Option<Value>,
    new_label: Option<String>,
    ctx: &ToolContext,
) -> ToolResult {
    let key = match resolve_key(queue, key, ctx) {
        Ok(k) => k,
        Err(e) => return e,
    };
    if new_task.is_none() && new_label.is_none() {
        return ToolResult::error(ctx.directives.render(TASK_EDIT_INCOMPLETE, &[]));
    }

    match queue.edit(&key, new_task, new_label) {
        Ok(()) => ToolResult::success(format!("Edited task {key}")),
        Err(e) => ToolResult::error(task_error_message(e, &ctx.directives)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::tasks::Queue;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// Build a context for a tool test, optionally with a "current
    /// task" already InProgress and assigned to `agent`.
    fn ctx_with(queue: Arc<Queue>, agent: &str) -> ToolContext {
        ToolContext::new(PathBuf::from("/tmp"))
            .queue(queue)
            .agent_id(agent.to_string())
    }

    /// Insert one Todo task, claim it for `agent` (atomically labels +
    /// transitions to InProgress), so `queue.find_task(...)` resolves it
    /// as the current task for `agent`. The queue is rooted at its own
    /// isolated temp directory so the default `.agentwerk` writes never
    /// leak into the source tree.
    fn shared_with_one_task(agent: &str) -> (Arc<Queue>, String) {
        let queue = Queue::new();
        queue.set_dir(isolated_test_dir());
        queue.insert(Task::new("body").label(agent), "tester".into());
        let key = queue
            .claim(&Query::from("status = Todo"), agent)
            .expect("claim must succeed");
        (queue, key)
    }

    /// A fresh directory for each `Queue` under a process-lifetime,
    /// self-deleting temp root. Per-call isolation matters because `insert`
    /// numbers new keys past the highest `t-<N>` folder already on disk,
    /// so a shared directory would leak task ids between tests.
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
    async fn task_defaults_key_to_current_task() {
        let (queue, key) = shared_with_one_task("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(TasksTool, serde_json::json!({"action": "task"}), &ctx).await;
        let text = unwrap_text(&result);
        assert!(text.contains(&key), "expected key in output: {text}");
        assert!(text.contains("body"));
    }

    #[tokio::test]
    async fn result_returns_the_result_of_another_agents_task() {
        let (queue, key) = shared_with_one_task("alice");
        queue
            .set_result(&key, serde_json::json!({"finding": "a lead"}))
            .unwrap();

        let ctx = ctx_with(Arc::clone(&queue), "bob");
        let result = call(
            TasksTool,
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
    async fn result_defaults_key_to_current_task() {
        let (queue, key) = shared_with_one_task("alice");
        queue
            .set_result(&key, serde_json::json!("what alice found"))
            .unwrap();

        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(TasksTool, serde_json::json!({"action": "result"}), &ctx).await;
        let text = unwrap_text(&result);
        assert!(text.contains("what alice found"), "{text}");
    }

    #[tokio::test]
    async fn result_errors_while_the_task_has_no_result() {
        let (queue, key) = shared_with_one_task("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({"action": "result", "key": key}),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Error { .. }), "{result:?}");
        assert!(unwrap_text(&result).contains("InProgress"));
    }

    /// Two tasks, the first claimed by `alice` and labelled `review`, the
    /// second still Todo and unlabelled.
    fn queue_with_two_tasks() -> Arc<Queue> {
        let queue = Queue::new();
        queue.set_dir(isolated_test_dir());
        queue.insert(Task::new("a").label("review"), "tester".into());
        queue.insert(Task::new("b"), "tester".into());
        queue.claim(&Query::from("t-1"), "alice");
        queue
    }

    #[tokio::test]
    async fn list_without_a_filter_returns_every_task() {
        let queue = queue_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(TasksTool, serde_json::json!({"action": "list"}), &ctx).await;
        let text = unwrap_text(&result);
        assert!(text.contains("t-1"), "{text}");
        assert!(text.contains("t-2"), "{text}");
    }

    #[tokio::test]
    async fn list_stops_at_fifty_tasks() {
        let queue = Queue::new();
        queue.set_dir(isolated_test_dir());
        for i in 1..=51 {
            queue.insert(Task::new(format!("task {i}")), "tester".into());
        }
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(TasksTool, serde_json::json!({"action": "list"}), &ctx).await;
        let text = unwrap_text(&result);
        assert_eq!(text.lines().count(), 50, "{text}");
        assert!(!text.contains("t-51"), "{text}");
    }

    #[tokio::test]
    async fn list_filters_by_the_status_the_aql_names() {
        let queue = queue_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({"action": "list", "aql": "status = InProgress"}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);
        assert!(text.contains("t-1"), "{text}");
        assert!(!text.contains("t-2"), "{text}");
    }

    #[tokio::test]
    async fn list_answers_in_the_order_the_aql_names() {
        let queue = queue_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({"action": "list", "aql": "ORDER BY key DESC"}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);
        assert!(text.find("t-2") < text.find("t-1"), "{text}");
    }

    #[tokio::test]
    async fn list_filters_by_the_window_the_aql_names() {
        let queue = queue_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({"action": "list", "aql": "created > -1h"}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);
        assert!(text.contains("t-1"), "{text}");

        let result = call(
            TasksTool,
            serde_json::json!({"action": "list", "aql": "created < -1h"}),
            &ctx,
        )
        .await;
        assert!(unwrap_text(&result).contains("no matching tasks"));
    }

    #[tokio::test]
    async fn list_answers_no_matching_tasks_when_the_aql_selects_none() {
        let queue = queue_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({"action": "list", "aql": "status = Finished"}),
            &ctx,
        )
        .await;
        assert!(unwrap_text(&result).contains("no matching tasks"));
    }

    #[tokio::test]
    async fn an_invalid_aql_answers_with_the_parse_error() {
        let queue = queue_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({"action": "list", "aql": "assignee = alice"}),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Error { .. }), "{result:?}");
        assert!(unwrap_text(&result).contains("agent"));
    }

    #[tokio::test]
    async fn create_stamps_reporter_from_agent_id() {
        let queue = Queue::new();
        queue.set_dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({"action": "create", "task": "new task"}),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success { .. }));
        let t = queue.get_task("t-1").unwrap();
        assert_eq!(t.task, serde_json::Value::String("new task".into()));
        assert_eq!(t.reporter, "alice");
    }

    #[tokio::test]
    async fn create_with_a_label_attaches_it() {
        let queue = Queue::new();
        queue.set_dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "label": "research"
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success { .. }));
        let t = queue.get_task("t-1").unwrap();
        assert_eq!(t.label.as_deref(), Some("research"));
        assert_eq!(t.status, Status::Todo);
    }

    #[tokio::test]
    async fn create_with_named_label_routes_to_agent() {
        let queue = Queue::new();
        queue.set_dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "label": "alice"
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success { .. }));
        let t = queue.get_task("t-1").unwrap();
        assert_eq!(t.label.as_deref(), Some("alice"));
        assert_eq!(t.status, Status::Todo);
    }

    #[tokio::test]
    async fn a_task_the_model_creates_takes_the_schema_bound_to_its_label() {
        let queue = Queue::new();
        queue.set_dir(isolated_test_dir());
        let schemas = crate::schemas::SchemaStore::new();
        schemas
            .label("analysis", serde_json::json!({"type": "string"}))
            .unwrap();
        queue.set_schemas(&schemas);

        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "label": "analysis"
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success { .. }));
        assert!(queue.get_task("t-1").unwrap().schema.is_none());

        queue.claim(&Query::from("analysis"), "bob");
        assert!(queue.get_task("t-1").unwrap().schema.is_some());
    }

    #[tokio::test]
    async fn edit_replaces_the_task_and_the_label() {
        let (queue, key) = shared_with_one_task("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        let result = call(
            TasksTool,
            serde_json::json!({
                "action": "edit",
                "task": "new body",
                "label": "urgent"
            }),
            &ctx,
        )
        .await;
        assert!(matches!(result, ToolResult::Success { .. }));
        let t = queue.get_task(&key).unwrap();
        assert_eq!(t.task, serde_json::Value::String("new body".into()));
        assert_eq!(t.label.as_deref(), Some("urgent"));
    }

    #[tokio::test]
    async fn unsupported_actions_are_rejected() {
        let (queue, _key) = shared_with_one_task("alice");
        let ctx = ctx_with(Arc::clone(&queue), "alice");
        for action in ["done", "transition", "comment", "assign", "attach"] {
            let result = call(TasksTool, serde_json::json!({"action": action}), &ctx).await;
            assert!(
                matches!(result, ToolResult::Error { .. }),
                "{action}: {result:?}"
            );
        }
    }
}
