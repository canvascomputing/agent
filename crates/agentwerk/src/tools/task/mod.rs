//! Lets an agent read and edit its `Werk` or finish its current task.

use std::path::Path;

use serde_json::Value;

use crate::agents::tasks::{Status, Task, TaskError, Werk};
use crate::agents::Query;
use crate::prompts::directives::{
    DirectiveStore, TASK_EDIT_INCOMPLETE, TASK_ID_MISSING, TASK_NOT_ASSIGNED, TASK_NOT_FOUND,
    TASK_QUERY_INVALID, TASK_RESULT_MISSING, TASK_TRANSITION_REJECTED, WERK_UNAVAILABLE,
};

use super::tool::{Event, ToolContext};

mod finish;
mod tool;

pub use finish::FinishTool;
pub use tool::TaskTool;

/// What the model asks the Werk to do. The schema declares `action` as the
/// discriminator and states which fields each one requires; the variants say
/// the same in Rust, so `search` cannot arrive without a `query`.
#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum TaskArgs {
    Task {
        id: Option<String>,
    },
    Result {
        id: Option<String>,
    },
    List {
        aql: Option<String>,
    },
    Create {
        task: Value,
        label: Option<String>,
    },
    Edit {
        id: Option<String>,
        task: Option<Value>,
        label: Option<String>,
    },
}

pub(super) fn dispatch(args: TaskArgs, ctx: &ToolContext) -> Event {
    let Some(werk) = ctx.werk.clone() else {
        return Event::error(ctx.directives.render(WERK_UNAVAILABLE, &[]))
            .directive(WERK_UNAVAILABLE);
    };

    match args {
        TaskArgs::Task { id } => action_task(&werk, id, ctx),
        TaskArgs::Result { id } => action_result(&werk, id, ctx),
        TaskArgs::List { aql } => action_list(&werk, aql, &ctx.directives),
        TaskArgs::Create { task, label } => action_create(&werk, task, label, ctx),
        TaskArgs::Edit { id, task, label } => action_edit(&werk, id, task, label, ctx),
    }
}

/// The task an action names, or the one this agent is holding.
fn resolve_id(werk: &Werk, id: Option<String>, ctx: &ToolContext) -> Result<String, Box<Event>> {
    match id {
        Some(id) => Ok(id),
        None => resolve_current_id(werk, ctx),
    }
}

pub(super) fn resolve_current_id(werk: &Werk, ctx: &ToolContext) -> Result<String, Box<Event>> {
    if let Some(id) = ctx.task_id.as_deref() {
        return Ok(id.to_string());
    }
    // A closure, never `task.assignee = {id}`: an id derives from a host-supplied label,
    // and AQL binds no values, so one carrying `=` or a quote rewrites the query.
    let agent_id = ctx.agent_id.clone().ok_or_else(|| {
        Event::error(ctx.directives.render(TASK_ID_MISSING, &[])).directive(TASK_ID_MISSING)
    })?;
    match werk.find_task(move |t: &Task| {
        t.status == Status::InProgress && t.assignee.as_deref() == Some(agent_id.as_str())
    }) {
        Some(t) => Ok(t.id.clone()),
        None => Err(Event::error(ctx.directives.render(TASK_NOT_ASSIGNED, &[])).into()),
    }
}

pub(super) fn task_error_message(err: TaskError, directives: &DirectiveStore) -> String {
    directives.render(TASK_TRANSITION_REJECTED, &[("error", &err.to_string())])
}

fn render_task(t: &Task) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n", t.id));
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

/// The result of task `id`, with the file it is stored in, so the agent
/// can read it again without asking for it.
fn render_result(id: &str, path: &Path, result: &Value) -> String {
    let mut out = format!("# {id} result\n- file: {}\n\n", path.display());
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
    s.get_name()
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
    for (id, task_preview, status, label) in tasks {
        // An unlabelled task prints no marker: a scan of up to 50 rows does
        // not need "(none)" on every default-scope line.
        let label = match label {
            Some(l) => format!("[{l}] "),
            None => String::new(),
        };
        out.push_str(&format!(
            "- {id} [{status}] {label}{task_preview}\n",
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

fn action_task(werk: &Werk, id: Option<String>, ctx: &ToolContext) -> Event {
    let id = match resolve_id(werk, id, ctx) {
        Ok(k) => k,
        Err(event) => return *event,
    };
    match werk.get_task(&id) {
        Some(t) => Event::success(render_task(&t)),
        None => Event::error(ctx.directives.render(TASK_NOT_FOUND, &[("id", &id)]))
            .directive(TASK_NOT_FOUND),
    }
}

fn action_result(werk: &Werk, id: Option<String>, ctx: &ToolContext) -> Event {
    let id = match resolve_id(werk, id, ctx) {
        Ok(k) => k,
        Err(event) => return *event,
    };
    let Some(task) = werk.get_task(&id) else {
        return Event::error(ctx.directives.render(TASK_NOT_FOUND, &[("id", &id)]))
            .directive(TASK_NOT_FOUND);
    };
    match task.result.as_ref() {
        Some(result) => Event::success(render_result(&id, &werk.result_path(&id), result)),
        None => Event::error(ctx.directives.render(
            TASK_RESULT_MISSING,
            &[("id", &id), ("status", status_label(task.status))],
        )),
    }
}

fn action_list(werk: &Werk, aql: Option<String>, directives: &DirectiveStore) -> Event {
    let pool: Vec<Task> = match aql.as_deref().map(Query::new) {
        Some(Ok(query)) => werk.find_tasks(query),
        Some(Err(error)) => {
            return Event::error(
                directives.render(TASK_QUERY_INVALID, &[("error", &error.to_string())]),
            )
        }
        None => werk.get_tasks(),
    };
    if pool.is_empty() {
        return Event::success("(no matching tasks)".to_string());
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
        .map(|(t, p)| (t.id.as_str(), p.as_str(), t.status, t.label.as_deref()))
        .collect();
    Event::success(render_summary_list(&rows))
}

fn action_create(werk: &Werk, task: Value, label: Option<String>, ctx: &ToolContext) -> Event {
    let mut task = Task::new(task);
    if let Some(label) = label {
        task = task.label(label);
    }

    let reporter = ctx
        .agent_id
        .as_deref()
        .expect("agent_id on ToolContext")
        .to_string();
    let id = werk.insert(task, reporter);
    Event::success(format!("Created task {id}"))
}

fn action_edit(
    werk: &Werk,
    id: Option<String>,
    new_task: Option<Value>,
    new_label: Option<String>,
    ctx: &ToolContext,
) -> Event {
    let id = match resolve_id(werk, id, ctx) {
        Ok(k) => k,
        Err(event) => return *event,
    };
    if new_task.is_none() && new_label.is_none() {
        return Event::error(ctx.directives.render(TASK_EDIT_INCOMPLETE, &[]))
            .directive(TASK_EDIT_INCOMPLETE);
    }

    match werk.edit(&id, new_task, new_label) {
        Ok(()) => Event::success(format!("Edited task {id}")),
        Err(e) => Event::error(task_error_message(e, &ctx.directives)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::tasks::Werk;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// Build a context for a tool test, optionally with a "current
    /// task" already InProgress and assigned to `agent`.
    fn ctx_with(werk: Arc<Werk>, agent: &str) -> ToolContext {
        ToolContext::new(PathBuf::from("/tmp"))
            .werk(werk)
            .agent_id(agent.to_string())
    }

    /// Insert one `todo` task, claim it for `agent` (atomically labels +
    /// transitions to `in_progress`), so `werk.find_task(...)` resolves it
    /// as the current task for `agent`. The Werk is rooted at its own
    /// isolated temp directory so the default `.agentwerk` writes never
    /// leak into the source tree.
    fn shared_with_one_task(agent: &str) -> (Arc<Werk>, String) {
        let werk = Werk::new();
        werk.set_dir(isolated_test_dir());
        werk.insert(Task::new("body").label(agent), "tester".into());
        let id = werk
            .claim(&Query::from("task.status = todo"), agent)
            .expect("claim must succeed");
        (werk, id)
    }

    /// A fresh directory for each `Werk` under a process-lifetime,
    /// self-deleting temp root. Per-call isolation matters because `insert`
    /// numbers new IDs past the highest `t-<N>` folder already on disk,
    /// so a shared directory would leak task IDs between tests.
    fn isolated_test_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::OnceLock;
        static ROOT: OnceLock<crate::test_util::TempDir> = OnceLock::new();
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = ROOT.get_or_init(|| crate::test_util::TempDir::new().unwrap());
        root.path()
            .join(format!("werk-{}", COUNTER.fetch_add(1, Ordering::Relaxed)))
    }

    async fn call(
        tool: impl Into<crate::tools::Tool>,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Event {
        tool.into().call(input, ctx).await
    }

    fn unwrap_text(result: &Event) -> &str {
        let s = result.get_content();
        s
    }

    #[tokio::test]
    async fn task_defaults_id_to_current_task() {
        let (werk, id) = shared_with_one_task("alice");
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(TaskTool, serde_json::json!({"action": "task"}), &ctx).await;
        let text = unwrap_text(&result);
        assert!(text.contains(&id), "expected id in output: {text}");
        assert!(text.contains("body"));
    }

    #[tokio::test]
    async fn result_returns_the_result_of_another_agents_task() {
        let (werk, id) = shared_with_one_task("alice");
        werk.set_result(&id, serde_json::json!({"finding": "a lead"}))
            .unwrap();

        let ctx = ctx_with(Arc::clone(&werk), "bob");
        let result = call(
            TaskTool,
            serde_json::json!({"action": "result", "id": id}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);
        assert!(result.get_name() == Event::TOOL_CALL_FINISHED, "{text}");
        assert!(text.contains("a lead"), "expected the result: {text}");
        assert!(
            text.contains("result.json"),
            "expected the result file: {text}"
        );
    }

    #[tokio::test]
    async fn result_defaults_id_to_current_task() {
        let (werk, id) = shared_with_one_task("alice");
        werk.set_result(&id, serde_json::json!("what alice found"))
            .unwrap();

        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(TaskTool, serde_json::json!({"action": "result"}), &ctx).await;
        let text = unwrap_text(&result);
        assert!(text.contains("what alice found"), "{text}");
    }

    #[tokio::test]
    async fn key_is_not_an_alias_for_id() {
        let werk = Werk::new();
        werk.set_dir(isolated_test_dir());
        werk.add_task("body");
        let ctx = ToolContext::new(PathBuf::from("/tmp")).werk(werk);

        let result = call(
            TaskTool,
            serde_json::json!({"action": "task", "key": "t-1"}),
            &ctx,
        )
        .await;

        assert!(result.get_name() == Event::TOOL_CALL_FAILED);
        assert!(unwrap_text(&result).contains("Provide its `id`"));
    }

    #[tokio::test]
    async fn task_not_found_directive_binds_the_id() {
        let werk = Werk::new();
        werk.set_dir(isolated_test_dir());
        let ctx = ToolContext::new(PathBuf::from("/tmp")).werk(werk);

        let result = call(
            TaskTool,
            serde_json::json!({"action": "task", "id": "t-404"}),
            &ctx,
        )
        .await;

        assert!(result.get_name() == Event::TOOL_CALL_FAILED);
        assert!(unwrap_text(&result).contains("No task with `id` t-404"));
    }

    #[tokio::test]
    async fn result_errors_while_the_task_has_no_result() {
        let (werk, id) = shared_with_one_task("alice");
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({"action": "result", "id": id}),
            &ctx,
        )
        .await;
        assert!(result.get_name() == Event::TOOL_CALL_FAILED, "{result:?}");
        assert!(unwrap_text(&result).contains("in_progress"));
    }

    /// Two tasks, the first claimed by `alice` and labelled `review`, the
    /// second still Todo and unlabelled.
    fn werk_with_two_tasks() -> Arc<Werk> {
        let werk = Werk::new();
        werk.set_dir(isolated_test_dir());
        werk.insert(Task::new("a").label("review"), "tester".into());
        werk.insert(Task::new("b"), "tester".into());
        werk.claim(&Query::from("t-1"), "alice");
        werk
    }

    #[tokio::test]
    async fn list_without_a_filter_returns_every_task() {
        let werk = werk_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(TaskTool, serde_json::json!({"action": "list"}), &ctx).await;
        let text = unwrap_text(&result);
        assert!(text.contains("t-1"), "{text}");
        assert!(text.contains("t-2"), "{text}");
    }

    #[tokio::test]
    async fn list_stops_at_fifty_tasks() {
        let werk = Werk::new();
        werk.set_dir(isolated_test_dir());
        for i in 1..=51 {
            werk.insert(Task::new(format!("task {i}")), "tester".into());
        }
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(TaskTool, serde_json::json!({"action": "list"}), &ctx).await;
        let text = unwrap_text(&result);
        assert_eq!(text.lines().count(), 50, "{text}");
        assert!(!text.contains("t-51"), "{text}");
    }

    #[tokio::test]
    async fn list_filters_by_the_status_the_aql_names() {
        let werk = werk_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({"action": "list", "aql": "task.status = in_progress"}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);
        assert!(text.contains("t-1"), "{text}");
        assert!(!text.contains("t-2"), "{text}");
    }

    #[tokio::test]
    async fn list_accepts_task_label_shorthand() {
        let werk = werk_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({"action": "list", "aql": "review"}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);

        assert!(text.contains("t-1"), "{text}");
        assert!(!text.contains("t-2"), "{text}");
    }

    #[tokio::test]
    async fn list_answers_in_the_order_the_aql_names() {
        let werk = werk_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({"action": "list", "aql": "ORDER BY task.id DESC"}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);
        assert!(text.find("t-2") < text.find("t-1"), "{text}");
    }

    #[tokio::test]
    async fn list_filters_by_the_window_the_aql_names() {
        let werk = werk_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({"action": "list", "aql": "task.created > -1h"}),
            &ctx,
        )
        .await;
        let text = unwrap_text(&result);
        assert!(text.contains("t-1"), "{text}");

        let result = call(
            TaskTool,
            serde_json::json!({"action": "list", "aql": "task.created < -1h"}),
            &ctx,
        )
        .await;
        assert!(unwrap_text(&result).contains("no matching tasks"));
    }

    #[tokio::test]
    async fn list_answers_no_matching_tasks_when_the_aql_selects_none() {
        let werk = werk_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({"action": "list", "aql": "task.status = finished"}),
            &ctx,
        )
        .await;
        assert!(unwrap_text(&result).contains("no matching tasks"));
    }

    #[tokio::test]
    async fn an_invalid_aql_answers_with_the_parse_error() {
        let werk = werk_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({"action": "list", "aql": "assignee = alice"}),
            &ctx,
        )
        .await;
        assert!(result.get_name() == Event::TOOL_CALL_FAILED, "{result:?}");
        assert!(unwrap_text(&result).contains("agent"));
    }

    #[tokio::test]
    async fn list_projects_an_event_query_to_tasks() {
        let werk = werk_with_two_tasks();
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({"action": "list", "aql": "event.name = task_created"}),
            &ctx,
        )
        .await;

        assert!(result.get_name() == Event::TOOL_CALL_FINISHED, "{result:?}");
        let output = unwrap_text(&result);
        assert!(output.contains("t-1"));
        assert!(output.contains("t-2"));
    }

    #[tokio::test]
    async fn create_stamps_reporter_from_agent_id() {
        let werk = Werk::new();
        werk.set_dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({"action": "create", "task": "new task"}),
            &ctx,
        )
        .await;
        assert!(result.get_name() == Event::TOOL_CALL_FINISHED);
        let t = werk.get_task("t-1").unwrap();
        assert_eq!(t.task, serde_json::Value::String("new task".into()));
        assert_eq!(t.reporter, "alice");
    }

    #[tokio::test]
    async fn create_with_a_label_attaches_it() {
        let werk = Werk::new();
        werk.set_dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "label": "research"
            }),
            &ctx,
        )
        .await;
        assert!(result.get_name() == Event::TOOL_CALL_FINISHED);
        let t = werk.get_task("t-1").unwrap();
        assert_eq!(t.label.as_deref(), Some("research"));
        assert_eq!(t.status, Status::Todo);
    }

    #[tokio::test]
    async fn create_with_named_label_routes_to_agent() {
        let werk = Werk::new();
        werk.set_dir(isolated_test_dir());
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({
                "action": "create",
                "task": "new",
                "label": "alice"
            }),
            &ctx,
        )
        .await;
        assert!(result.get_name() == Event::TOOL_CALL_FINISHED);
        let t = werk.get_task("t-1").unwrap();
        assert_eq!(t.label.as_deref(), Some("alice"));
        assert_eq!(t.status, Status::Todo);
    }

    #[tokio::test]
    async fn edit_replaces_the_task_and_the_label() {
        let (werk, id) = shared_with_one_task("alice");
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        let result = call(
            TaskTool,
            serde_json::json!({
                "action": "edit",
                "task": "new body",
                "label": "urgent"
            }),
            &ctx,
        )
        .await;
        assert!(result.get_name() == Event::TOOL_CALL_FINISHED);
        let t = werk.get_task(&id).unwrap();
        assert_eq!(t.task, serde_json::Value::String("new body".into()));
        assert_eq!(t.label.as_deref(), Some("urgent"));
    }

    #[tokio::test]
    async fn unsupported_actions_are_rejected() {
        let (werk, _id) = shared_with_one_task("alice");
        let ctx = ctx_with(Arc::clone(&werk), "alice");
        for action in ["done", "transition", "comment", "assign", "attach"] {
            let result = call(TaskTool, serde_json::json!({"action": action}), &ctx).await;
            assert!(
                result.get_name() == Event::TOOL_CALL_FAILED,
                "{action}: {result:?}"
            );
        }
    }
}
