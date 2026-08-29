//! Drives one agent: it claims a task, then works it through requests, tool
//! calls, and summarizing until the task is resolved.

use std::collections::HashMap;
use std::sync::Arc;

use crate::agents::agent::Agent;
use crate::agents::policy::Policy;
use crate::agents::query::Matcher;
use crate::agents::tasks::{policy_violated, Queue, Reply, Run, Status, Task};
use crate::event::{CompactReason, Event, EventKind, PolicyViolation};
use crate::prompts::directives::{NO_TOOL_CALLED, REPLY_REJECTED};
use crate::providers::{AsUserMessage, Message, Model, RequestErrorKind};
use crate::tools::{FinishTool, ToolRegistry};

use super::{compact, request, tool_call, Step, POLL_INTERVAL};

pub(super) struct TaskContext<'a> {
    pub(super) agent: &'a Agent,
    pub(super) model: &'a Model,
    pub(super) queue: &'a Arc<Queue>,
    pub(super) run: Arc<Run>,

    pub(super) task_key: String,
    pub(super) system_prompt: String,
    pub(super) policy: Policy,

    pub(super) tools: ToolRegistry,

    // Spans turns; trips max_schema_retries.
    pub(super) consecutive_schema_failures: u32,
}

impl<'a> TaskContext<'a> {
    pub(super) fn emit(&self, kind: EventKind) -> Event {
        self.queue.emit(&self.task_key, self.agent.id(), kind)
    }

    pub(super) fn task(&self) -> Option<Task> {
        self.queue.get_task(&self.task_key)
    }

    /// The corrective directive to inject for `detail`. Everything the
    /// `SchemaRetried` this retry emitted carries is bound alongside it, so a
    /// replacement can read how far into the budget this is and who it
    /// addresses without reaching for an event.
    pub(super) fn retry_directive(&self, detail: &str, attempt: u32, max_attempts: u32) -> String {
        self.agent.get_directives().render(
            REPLY_REJECTED,
            &[
                ("detail", detail),
                ("attempt", &attempt.to_string()),
                ("max_attempts", &max_attempts.to_string()),
                ("task", &self.task_key),
                ("agent", self.agent.id()),
            ],
        )
    }

    /// Fail the task without naming a cause. The caller has already emitted
    /// the event that does.
    pub(super) fn fail_task(&self) {
        let _ = self.queue.set_failed_by(&self.task_key, self.agent.id());
    }

    /// Fail the task because a request did not come back. Reserved for the
    /// request path, so `RequestFailed` never reports a request that was
    /// never made.
    pub(super) fn fail_with(&self, reason: RequestErrorKind, message: String) {
        self.emit(EventKind::RequestFailed {
            model: self.model.name.clone(),
            reason,
            message,
        });
        self.fail_task();
    }
}

pub(super) async fn run_agent(agent: Agent) {
    let queue = agent
        .queue
        .upgrade()
        .expect("Agent's Queue was dropped before run() finished");

    loop {
        if run_is_over(&agent, &queue) {
            return;
        }
        let Some(mut context) = claim(&agent, &queue) else {
            tokio::time::sleep(POLL_INTERVAL).await;
            continue;
        };
        let mut step = Some(Step::Evaluate);
        while let Some(current) = step {
            step = match current {
                Step::Evaluate => evaluate(&mut context),
                Step::Compact(reason) => compact::run(&mut context, reason).await,
                Step::Request => request::run(&mut context).await,
                Step::ToolCalls(calls) => tool_call::run(&mut context, calls).await,
            };
        }
    }
}

/// True once the agent has no reason to claim again: the run has left
/// `Working`, or a limit is breached. It emits the `PolicyViolated` for a
/// breach, so only the outer loop calls it, once, on the way out.
fn run_is_over(agent: &Agent, queue: &Queue) -> bool {
    if !queue.run.is_working() {
        return true;
    }
    let policy = queue.get_policy();
    if let Some((violation, limit)) = policy_violated(&policy, &queue.stats) {
        queue.emit(
            "",
            agent.id(),
            EventKind::PolicyViolated {
                policy: violation,
                limit,
            },
        );
        return true;
    }
    false
}

/// Claim a `Todo` task for this agent, or resume one of its `InProgress`
/// tasks; write the first message when there is none.
fn claim<'a>(agent: &'a Agent, queue: &'a Arc<Queue>) -> Option<TaskContext<'a>> {
    let label = agent.label.clone();
    let claimable = (move |t: &Task| {
        t.status == Status::Todo
            && Agent::handles(label.as_deref(), t.label.as_deref())
            && !t.is_cancelled()
    })
    .into_query();
    // On the id, not the label: agents sharing a label must not take over each
    // other's started tasks.
    let agent_id = agent.id().to_string();
    let interactive = agent.is_interactive();
    let resumable = move |t: &Task| {
        t.status == Status::InProgress
            && t.assignee.as_deref() == Some(agent_id.as_str())
            && (t.is_waiting_for_response() || !interactive)
            && !t.is_cancelled()
    };
    let task_key = queue
        .claim(&claimable, agent.id())
        .or_else(|| queue.find_task(resumable).map(|t| t.key.clone()))?;
    let task = queue.get_task(&task_key)?;

    let mut tools = agent.tool_registry().clone();
    // Rebinding, not registering: an interactive agent carries no `finish`
    // unless it asked for one, and this must not hand it back.
    if tools.contains(FinishTool::NAME) {
        tools.register(FinishTool::from_schema(task.schema.clone()));
    }

    let knowledge_index = agent.get_knowledge().index();
    let policy = queue.get_policy();
    // Lets the model see what knowledge pages it can read.
    let system_prompt =
        agent.system_prompt(Some(&knowledge_index), &policy, &queue.stats, &task_key);
    let agent_id = agent.id();

    queue.emit(&task_key, agent_id, EventKind::TurnStarted);

    if task.replies.is_empty() {
        queue.append_reply(&task_key, Reply::system_text(system_prompt.clone()));
        let Message::User {
            content: task_blocks,
        } = task.as_user_message()
        else {
            unreachable!("Task::as_user_message returns Message::User");
        };
        queue.append_reply(&task_key, Reply::user(&task_blocks, &HashMap::new()));
    }

    Some(TaskContext {
        agent,
        model: agent.get_model(),
        queue,
        run: Arc::clone(&queue.run),

        task_key,
        system_prompt,
        policy,
        tools,

        consecutive_schema_failures: 0,
    })
}

/// Re-read the task and decide the next step.
fn evaluate(context: &mut TaskContext<'_>) -> Option<Step> {
    // The pure check, not `run_is_over`: the outer loop emits the
    // `PolicyViolated` a moment later, and emitting it twice would double-count.
    if !context.queue.run.is_working()
        || policy_violated(&context.policy, &context.queue.stats).is_some()
    {
        return None;
    }
    let Some(task) = context.task() else {
        return None;
    };
    if task.is_cancelled() {
        return None;
    }
    // The transition itself already emitted the terminal event; the agent
    // only moves on to fresh work.
    if !task.is_pending() {
        return None;
    }
    if !task.is_waiting_for_response() {
        if context.agent.is_interactive() {
            // Pause until a caller reply lands; the resume claim re-checks.
            return None;
        }
        return silence_retry(context);
    }
    if compact::proactive_compaction_needed(context, &task) {
        return Some(Step::Compact(CompactReason::Proactive));
    }
    Some(Step::Request)
}

/// The model replied without a tool call: prompt it to resume or finish,
/// counting the silence toward the schema-retry budget.
fn silence_retry(context: &mut TaskContext<'_>) -> Option<Step> {
    let max = context.policy.max_schema_retries.unwrap_or(u32::MAX);
    context.consecutive_schema_failures = context.consecutive_schema_failures.saturating_add(1);
    if context.consecutive_schema_failures >= max {
        context.emit(EventKind::PolicyViolated {
            policy: PolicyViolation::MaxSchemaRetries,
            limit: u64::from(max),
        });
        let _ = context
            .queue
            .set_failed_by(&context.task_key, context.agent.id());
        return None;
    }
    let detail = context.agent.get_directives().render(NO_TOOL_CALLED, &[]);
    let attempt = context.consecutive_schema_failures;
    context.emit(EventKind::SchemaRetried {
        attempt,
        max_attempts: max,
        message: detail.clone(),
    });
    context.queue.append_reply(
        &context.task_key,
        Reply::user_text(context.retry_directive(&detail, attempt, max)),
    );
    Some(Step::Evaluate)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::agents::policy::Policy;
    use crate::prompts::directives::DirectiveStore;

    use super::claim;
    use crate::agents::agent::Agent;
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tasks::{Author, Queue, Status, Task};
    use crate::agents::Knowledge;
    use crate::tools::{FinishTool, TasksTool};

    // Run lifecycle

    #[tokio::test]
    async fn finish_drains_late_added_tasks() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("a-done")),
            Ok(write_result_response("b-done")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        tasks.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TasksTool),
        );

        tasks.start();

        tasks.add_task("a");
        tasks.add_task("b");

        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(tasks.get_results().len(), 2);
        assert_eq!(tasks.get_results().pop(), Some(serde_json::json!("b-done")));
    }

    // retry directive

    #[tokio::test]
    async fn a_replacement_replaces_the_silence_directive() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("just thinking, no tool call")),
            Ok(write_result_response("done")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        tasks.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .directives(|_| Some("PLEASE CALL A TOOL NOW")),
        );

        tasks.start();
        tasks.add_task("go");
        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("finish did not finish within 5s");

        let injected = user_text(&provider.received()[1]);
        assert!(
            injected.contains("PLEASE CALL A TOOL NOW"),
            "the replacement must be injected: {injected:?}",
        );
        assert!(
            !injected.contains("was not accepted"),
            "default framing must be suppressed: {injected:?}",
        );
    }

    #[tokio::test]
    async fn a_replacement_reads_the_attempt_the_retry_bound() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("just thinking, no tool call")),
            Ok(write_result_response("done")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        tasks.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .directives(|_| Some("attempt {attempt} of {max_attempts}")),
        );

        tasks.start();
        tasks.add_task("go");
        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("finish did not finish within 5s");

        let injected = user_text(&provider.received()[1]);
        assert!(
            injected.contains("attempt 1 of 3"),
            "the retry must bind what it emitted: {injected:?}",
        );
    }

    #[tokio::test]
    async fn a_replacement_names_the_agent_it_addresses() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let scout = MockProvider::with_results(vec![
            Ok(text_response("just thinking, no tool call")),
            Ok(write_result_response("scout-done")),
        ]);
        let worker = MockProvider::with_results(vec![
            Ok(text_response("just thinking, no tool call")),
            Ok(write_result_response("worker-done")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        // An id is `<label>-<n>`, so the two agents read their own name back.
        fn addressed(_: &str) -> Option<&'static str> {
            Some("{agent}, CALL A TOOL")
        }
        tasks.add_agent(
            Agent::new()
                .label("scout")
                .provider(scout.clone())
                .model("mock")
                .role("test")
                .directives(addressed),
        );
        tasks.add_agent(
            Agent::new()
                .label("worker")
                .provider(worker.clone())
                .model("mock")
                .role("test")
                .directives(addressed),
        );

        tasks.start();
        tasks.add_task(Task::new("go").label("scout"));
        tasks.add_task(Task::new("go").label("worker"));
        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("finish did not finish within 5s");

        let scouted = user_text(&scout.received()[1]);
        assert!(
            scouted.contains("scout-1, CALL A TOOL"),
            "the directive must bind the agent it addresses: {scouted:?}",
        );
        let worked = user_text(&worker.received()[1]);
        assert!(
            worked.contains("worker-1, CALL A TOOL"),
            "every agent must read its own id: {worked:?}",
        );
    }

    #[tokio::test]
    async fn the_built_in_directive_is_injected_when_nothing_replaces_it() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("just thinking, no tool call")),
            Ok(write_result_response("done")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        tasks.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test"),
        );

        tasks.start();
        tasks.add_task("go");
        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("finish did not finish within 5s");

        let injected = user_text(&provider.received()[1]);
        assert!(
            injected.contains("was not accepted"),
            "an unreplaced directive must render its built-in text: {injected:?}",
        );
    }

    #[tokio::test]
    async fn finish_waits_through_handover_chain() {
        // alice hands t-1 off to bob. The handover inserts the child
        // before finishing the parent, so finish() must not observe an
        // empty queue in between and drain the chain early.
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(handover_response("bob", "continue", "alice-done")),
            Ok(write_result_response("bob-done")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        for label in ["alice", "bob"] {
            tasks.add_agent(
                Agent::new()
                    .label(label)
                    .provider(provider.clone())
                    .model("mock")
                    .role("test")
                    .tool(FinishTool),
            );
        }

        tasks.start();
        tasks.add_task(Task::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(tasks.get_results().len(), 2);
        assert_eq!(tasks.get_task("t-2").unwrap().status, Status::Finished);
    }

    #[tokio::test]
    async fn finish_waits_through_an_on_result_follow_up() {
        // The handler mints a follow-up when t-1 finishes. finish()
        // must not drain in the window between the finish transition and
        // the handler's insert.
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("first-done")),
            Ok(write_result_response("follow-up-done")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        tasks.on_result(|queue, done, _| {
            if done.key == "t-1" {
                queue.add_task(Task::new("follow up").label("alice"));
            }
        });
        tasks.add_agent(
            Agent::new()
                .label("alice")
                .provider(provider.clone())
                .model("mock")
                .role("test"),
        );

        tasks.start();
        tasks.add_task(Task::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(tasks.get_results().len(), 2);
        assert_eq!(tasks.get_task("t-2").unwrap().status, Status::Finished);
    }

    #[tokio::test]
    async fn a_hook_files_the_report_once_aql_finds_both_scans_finished() {
        // Two scans run side by side on their own agents. The hook selects the
        // finished ones with AQL after each result and, once both are in,
        // writes their results into the task that reports on them.
        use std::sync::atomic::{AtomicBool, Ordering};

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("clean")),
            Ok(write_result_response("clean")),
            Ok(write_result_response("report-done")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        // Both scans can finish at once, so the first handler to see the pair
        // takes the flag and the other returns: without it the report is filed
        // twice.
        let filed = Arc::new(AtomicBool::new(false));
        tasks.on_result(move |queue, done, _| {
            if !done.has_label("scan") {
                return;
            }
            let scans = queue.find_results("label = scan AND status = Finished");
            if scans.len() < 2 || filed.swap(true, Ordering::SeqCst) {
                return;
            }
            let verdicts: Vec<String> = scans.iter().map(|scan| scan.to_string()).collect();
            queue.add_task(Task::labeled(
                "report",
                format!("Write the report from {}.", verdicts.join(" and ")),
            ));
        });

        for label in ["scan", "scan", "report"] {
            tasks.add_agent(
                Agent::new()
                    .label(label)
                    .provider(provider.clone())
                    .model("mock")
                    .role("test"),
            );
        }

        tasks.start();
        tasks.add_task(Task::labeled("scan", "scan a.py"));
        tasks.add_task(Task::labeled("scan", "scan b.py"));

        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("finish did not finish within 5s");

        let report = tasks.find_task("label = report").unwrap();
        assert_eq!(
            report.task,
            serde_json::json!("Write the report from \"clean\" and \"clean\".")
        );
        assert_eq!(report.status, Status::Finished);
        assert_eq!(
            tasks.find_results("label = report"),
            vec![serde_json::json!("report-done")]
        );
    }

    #[tokio::test]
    async fn task_finished_event_fires_exactly_once_per_task() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(write_result_response("done"))]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        let finished_events = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&finished_events);
        tasks.on_event(move |_, e| {
            if matches!(e.kind, crate::event::EventKind::TaskFinished) {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        tasks.add_agent(
            Agent::new()
                .label("alice")
                .provider(provider.clone())
                .model("mock")
                .role("test"),
        );

        tasks.start();
        tasks.add_task(Task::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(finished_events.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn interactive_pause_holds_when_an_event_handler_edits_replies() {
        // A handler that edits on every event must not perturb the interactive
        // gate: the task pauses on the assistant text reply, and no second
        // request fires. Exhausting the single mock response would fail the
        // task, so `requests == 1` and `InProgress` prove the gate held.
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        // Unfiltered on purpose: it also fires for the run-level events, whose
        // empty key names no task.
        tasks.on_event(|queue, event| {
            queue.edit_replies(&event.task_key, |_replies| {});
        });
        tasks.add_agent(interactive_chatbot(&provider));
        tasks.add_task("hello");
        tasks.start();

        for _ in 0..200 {
            let last_is_assistant = tasks
                .get_tasks()
                .into_iter()
                .next()
                .and_then(|t| t.replies.last().map(|r| r.author == Author::Assistant))
                .unwrap_or(false);
            if last_is_assistant {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Give the loop room to (wrongly) fire another request if buggy.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let task = tasks.get_tasks().into_iter().next().unwrap();
        assert_eq!(task.status, Status::InProgress);
        assert_eq!(
            task.replies.last().map(|r| r.author),
            Some(Author::Assistant),
        );
        assert_eq!(
            provider.requests(),
            1,
            "an edit must not trigger a re-request"
        );

        tasks.cancel_all_tasks();
        tasks.finish_all_tasks().await;
    }

    #[tokio::test]
    async fn loop_pauses_after_text_reply_then_resumes_when_caller_replies() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider =
            MockProvider::with_results(vec![Ok(text_response("hi")), Ok(text_response("and now"))]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        tasks.add_agent(interactive_chatbot(&provider));
        let key = tasks.add_task("hello");

        let tasks_for_inject = Arc::clone(&tasks);
        let inject = async move {
            for _ in 0..200 {
                let last_is_assistant = tasks_for_inject
                    .get_tasks()
                    .into_iter()
                    .next()
                    .and_then(|t| t.replies.last().map(|r| r.author == Author::Assistant))
                    .unwrap_or(false);
                if last_is_assistant {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let task = tasks_for_inject
                .get_tasks()
                .into_iter()
                .next()
                .expect("task must exist");
            assert_eq!(task.status, Status::InProgress);
            assert_eq!(
                task.replies.last().map(|r| r.author),
                Some(Author::Assistant),
                "gate must pause on the assistant text reply",
            );
            tasks_for_inject.add_reply(&key, "what next?");
        };

        tokio::time::timeout(Duration::from_secs(5), async {
            tasks.start();
            // The pause is not the end of the task, so the reply lands first
            // and the finish then waits out the turn it sets off.
            inject.await;
            tasks.finish_all_tasks().await;
        })
        .await
        .expect("test did not finish within 5s");

        let task = tasks
            .get_tasks()
            .into_iter()
            .next()
            .expect("task must exist");
        // The reply is what proves the resume: an interactive agent has no
        // `finish`, so the task pauses again instead of ending.
        assert_eq!(provider.requests(), 2, "the caller's reply drove a turn");
        assert_eq!(task.status, Status::InProgress);
        assert_eq!(
            task.replies.last().map(|r| r.author),
            Some(Author::Assistant),
        );
    }

    #[tokio::test]
    async fn finish_returns_when_an_interactive_agent_pauses_for_input() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tasks.add_agent(interactive_chatbot(&provider));
        let key = tasks.add_task("hello");
        tasks.start();

        // Pausing for input is no lifecycle transition and leaves the task
        // `InProgress`, so this returns only if `RequestFinished` reaches the
        // waiter after the reply is in the store.
        let waited = key.clone();
        tokio::time::timeout(
            Duration::from_secs(5),
            tasks.finish_results(move |t: &Task| t.key == waited),
        )
        .await
        .expect("finish did not return within 5s");
        let task = tasks.get_task(&key).unwrap();
        assert_eq!(task.status, Status::InProgress);
        assert!(task
            .replies
            .last()
            .is_some_and(|r| r.author == Author::Assistant));
        tasks.cancel_all_tasks();
    }

    #[tokio::test]
    async fn paused_interactive_task_emits_turn_started_exactly_once() {
        use crate::event::EventKind;

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        let collected = collect_events(&tasks);
        tasks.add_agent(interactive_chatbot(&provider));
        tasks.add_task("hello");

        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("test did not finish within 5s");

        let events = collected.lock().unwrap().clone();
        let turn_started = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TurnStarted))
            .count();
        assert_eq!(
            turn_started, 1,
            "paused interactive task must not re-emit TurnStarted on every poll",
        );
    }

    #[tokio::test]
    async fn loop_releases_paused_context_when_new_todo_arrives() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("hi")),
            Ok(text_response("hi again")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tasks.add_agent(interactive_chatbot(&provider));
        let first_key = tasks.add_task("first chat");
        tasks.start();

        let tasks_for_drive = Arc::clone(&tasks);
        let drive = async move {
            for _ in 0..200 {
                let paused = tasks_for_drive
                    .get_task(&first_key)
                    .and_then(|t| t.replies.last().map(|r| r.author == Author::Assistant))
                    .unwrap_or(false);
                if paused {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let second_key = tasks_for_drive.add_task("second chat");
            for _ in 0..400 {
                if tasks_for_drive
                    .get_task(&second_key)
                    .is_some_and(|t| t.replies.iter().any(|r| r.author == Author::Assistant))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let first = tasks_for_drive.get_task(&first_key).unwrap();
            let second = tasks_for_drive.get_task(&second_key).unwrap();
            assert_eq!(
                first.status,
                Status::InProgress,
                "first chat remains paused; no caller replied",
            );
            // Its own answer, not a status: an interactive agent has no
            // `finish`, so the second chat pauses like the first.
            assert_eq!(
                second.status,
                Status::InProgress,
                "agent must release the paused first chat and claim the new Todo",
            );
            assert!(
                second.replies.iter().any(|r| r.author == Author::Assistant),
                "second chat must have been answered",
            );
        };

        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(tasks.finish_all_tasks(), drive);
        })
        .await
        .expect("test did not finish within 5s");
    }

    #[tokio::test]
    async fn loop_fails_task_when_silence_exceeds_schema_retry_budget() {
        use crate::event::{EventKind, PolicyViolation};

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(1),
                ..Default::default()
            });
        let collected = collect_events(&tasks);
        tasks.add_agent(task_agent(&provider));
        tasks.add_task("go");

        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("test did not finish within 5s");

        let task = tasks
            .get_tasks()
            .into_iter()
            .next()
            .expect("task must exist");
        assert_eq!(task.status, Status::Failed);

        let events = collected.lock().unwrap().clone();
        let policy_violated = events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::PolicyViolated {
                    policy: PolicyViolation::MaxSchemaRetries,
                    limit: 1,
                },
            )
        });
        assert!(policy_violated, "expected PolicyViolated MaxSchemaRetries");
        let task_failed = events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::TaskFailed));
        assert!(task_failed, "expected TaskFailed");
    }

    #[tokio::test]
    async fn loop_finishes_task_after_one_silence_and_recovery() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("hi")),
            Ok(write_result_response("done")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        tasks.add_agent(task_agent(&provider));
        tasks.add_task("go");

        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("test did not finish within 5s");

        let task = tasks
            .get_tasks()
            .into_iter()
            .next()
            .expect("task must exist");
        assert_eq!(task.status, Status::Finished);
        assert_eq!(tasks.get_results().pop(), Some(serde_json::json!("done")));
    }

    #[tokio::test]
    async fn silence_retry_emits_schema_retried_event_with_attempt_1() {
        use crate::event::EventKind;

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("hi")),
            Ok(write_result_response("done")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        let collected = collect_events(&tasks);
        tasks.add_agent(task_agent(&provider));
        tasks.add_task("go");

        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("test did not finish within 5s");

        let events = collected.lock().unwrap().clone();
        let schema_retries: Vec<(u32, String)> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::SchemaRetried {
                    attempt, message, ..
                } => Some((*attempt, message.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            schema_retries,
            vec![(
                1,
                DirectiveStore::default().render(crate::prompts::directives::NO_TOOL_CALLED, &[]),
            )],
            "exactly one SchemaRetried at attempt 1 with the silence detail",
        );
    }

    #[tokio::test]
    async fn cancel_stops_a_running_workshop() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        tasks.start();
        tasks.cancel_all_tasks();

        tokio::time::timeout(Duration::from_secs(2), tasks.finish_all_tasks())
            .await
            .expect("run did not exit within 2s of cancel()");
    }

    #[tokio::test]
    async fn cancel_keeps_the_matching_pool_off_the_queue_while_others_run() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        let analyst = MockProvider::with_results(vec![Ok(write_result_response("analyzed"))]);
        let researcher = MockProvider::with_results(vec![Ok(write_result_response("hunted"))]);
        tasks.add_agent(
            Agent::new()
                .label("analysis")
                .provider(analyst)
                .model("mock")
                .role("test"),
        );
        tasks.add_agent(
            Agent::new()
                .label("research")
                .provider(researcher.clone())
                .model("mock")
                .role("test"),
        );

        // Call off the research pool on the live run (start() resets signals), then
        // enqueue both tasks; the analysis pool runs on.
        tasks.start();
        tasks.cancel_tasks("label = research");
        tasks.add_task(Task::new("hunt").label("research"));
        tasks.add_task(Task::new("triage").label("analysis"));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let analyzed = tasks
                .get_tasks()
                .iter()
                .any(|t| t.status == Status::Finished && t.task.as_str() == Some("triage"));
            if analyzed {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                tasks.cancel_all_tasks();
                panic!("analysis task did not finish within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let hunt = tasks
            .get_tasks()
            .into_iter()
            .find(|t| t.task.as_str() == Some("hunt"))
            .expect("research task exists");
        assert_eq!(
            hunt.status,
            Status::Todo,
            "a cancelled pool's task is never claimed",
        );
        assert_eq!(researcher.requests(), 0, "the researcher never ran");

        tasks.cancel_all_tasks();
        tokio::time::timeout(Duration::from_secs(2), tasks.finish_all_tasks())
            .await
            .expect("finish returns after cancel()");
    }

    #[tokio::test]
    async fn finish_after_run_resets_signal() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("first")),
            Ok(write_result_response("second")),
        ]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        tasks.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TasksTool),
        );

        tasks.add_task("first");
        tasks.finish_all_tasks().await;
        assert_eq!(tasks.get_results().pop(), Some(serde_json::json!("first")));

        tasks.add_task("second");
        tokio::time::timeout(Duration::from_secs(5), tasks.finish_all_tasks())
            .await
            .expect("second finish did not finish within 5s");
        assert_eq!(tasks.get_results().pop(), Some(serde_json::json!("second")));
    }

    #[tokio::test]
    async fn agent_finish_forwards_to_bound_queue() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(write_result_response("forwarded"))]);
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        let agent = tasks.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TasksTool),
        );

        agent.add_task("hello");
        let queue = agent.start();
        tokio::time::timeout(Duration::from_secs(5), queue.finish_all_tasks())
            .await
            .expect("the run did not end within 5s");
        assert_eq!(
            queue.get_results().pop(),
            Some(serde_json::json!("forwarded"))
        );
    }

    // Cross-task memory

    fn user_texts(messages: &[crate::providers::Message]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| match m {
                crate::providers::Message::User { content } => {
                    content.iter().find_map(|b| match b {
                        crate::providers::ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                }
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn messages_contain_only_the_current_tasks_task() {
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("ok")),
            Ok(write_result_response("ok")),
        ]);
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(10),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tasks.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .tool(crate::tools::TasksTool),
        );
        tasks.add_task("first");
        tasks.add_task("second");
        let _ = tasks.finish_all_tasks().await;

        let calls = provider.received();
        assert_eq!(calls.len(), 2);
        assert_eq!(user_texts(&calls[0]), vec!["first".to_string()]);
        assert_eq!(user_texts(&calls[1]), vec!["second".to_string()]);
    }

    #[tokio::test]
    async fn model_writes_in_task_n_become_visible_in_task_n_plus_one_system_prompt() {
        let provider = MockProvider::with_results(vec![
            Ok(knowledge_write_response(
                "api-config",
                "API runs on port 3000",
                "# API Config\n\nPort 3000.",
            )),
            Ok(write_result_response("done 1")),
            Ok(write_result_response("done 2")),
        ]);
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let knowledge_dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(knowledge_dir.path()).unwrap();

        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tasks.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .knowledge(&store),
        );
        tasks.add_task("first");
        tasks.add_task("second");
        let _ = tasks.finish_all_tasks().await;

        let prompts = provider.received_system_prompts();
        assert_eq!(prompts.len(), 3);
        assert!(
            !prompts[0].contains("api-config"),
            "task 1 turn 1 sees an empty knowledge store: {:?}",
            prompts[0]
        );
        assert!(
            prompts[2].contains("## Knowledge"),
            "task 2 should render the knowledge section: {:?}",
            prompts[2]
        );
        assert!(
            prompts[2].contains("API runs on port 3000"),
            "task 2 should see task 1's write: {:?}",
            prompts[2]
        );
    }

    #[tokio::test]
    async fn system_prompt_does_not_change_after_mid_task_knowledge_write() {
        let provider = MockProvider::with_results(vec![
            Ok(knowledge_write_response(
                "mid-task",
                "Written mid-task",
                "# Mid\n\nContent.",
            )),
            Ok(write_result_response("ok")),
        ]);
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let knowledge_dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(knowledge_dir.path()).unwrap();

        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tasks.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .knowledge(&store),
        );
        tasks.add_task("hi");
        let _ = tasks.finish_all_tasks().await;

        let prompts = provider.received_system_prompts();
        assert_eq!(prompts.len(), 2);
        assert_eq!(
            prompts[0], prompts[1],
            "mid-task knowledge write must not change the system prompt within the same task"
        );
        assert!(store.index().contains("mid-task"));
    }

    #[tokio::test]
    async fn agent_a_writes_in_one_task_then_agent_b_sees_it_in_its_next_task() {
        let p_a = MockProvider::with_results(vec![
            Ok(knowledge_write_response(
                "alice-note",
                "Note from Alice",
                "# Alice\n\nAlice's note.",
            )),
            Ok(write_result_response("alice done")),
        ]);
        let p_b = MockProvider::with_results(vec![Ok(write_result_response("bob done"))]);

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let knowledge_dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(knowledge_dir.path()).unwrap();

        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });

        tasks.add_agent(
            Agent::new()
                .label("a")
                .provider(p_a.clone())
                .model("mock")
                .role("test")
                .knowledge(&store),
        );
        tasks.add_agent(
            Agent::new()
                .label("b")
                .provider(p_b.clone())
                .model("mock")
                .role("test")
                .knowledge(&store),
        );

        tasks.add_task(Task::new("alice work").label("a"));
        let _ = tasks.finish_all_tasks().await;
        assert!(store.index().contains("alice-note"));

        tasks.add_task(Task::new("bob work").label("b"));
        let _ = tasks.finish_all_tasks().await;

        let bob_prompts = p_b.received_system_prompts();
        assert_eq!(bob_prompts.len(), 1, "bob processed exactly one task");
        assert!(
            bob_prompts[0].contains("Note from Alice"),
            "bob should see alice's write: {:?}",
            bob_prompts[0]
        );
    }

    #[tokio::test]
    async fn a_schema_bound_to_a_label_reaches_the_first_message_of_its_task() {
        let provider = MockProvider::with_results(vec![Ok(write_result_value(
            serde_json::json!({"verdict": "done"}),
        ))]);
        let results_dir = crate::test_util::TempDir::new().unwrap();

        let schemas = crate::schemas::SchemaStore::new();
        schemas
            .label(
                "analysis",
                serde_json::json!({
                    "type": "object",
                    "properties": { "verdict": { "type": "string" } },
                    "required": ["verdict"],
                }),
            )
            .unwrap();

        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tasks.set_schemas(&schemas);
        tasks.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .label("analysis"),
        );
        tasks.add_task(Task::new("audit").label("analysis"));
        let _ = tasks.finish_all_tasks().await;

        let task_message = &user_texts(&provider.received()[0])[0];
        assert!(
            task_message.contains("verdict"),
            "the bound schema must be in the task message: {task_message:?}",
        );
        assert_eq!(tasks.get_tasks()[0].status, Status::Finished);
    }

    #[tokio::test]
    async fn a_claimed_task_binds_its_schema_to_the_finish_tool() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tasks = Queue::new();
        tasks.set_dir(results_dir.path().to_path_buf());
        // Bound rather than cloned into the queue: `finish` is registered on
        // the agent that joins one, and this claims through that agent.
        let mut agent = Agent::new()
            .provider(MockProvider::with_results(vec![]))
            .model("mock")
            .role("test");
        tasks.bind_agent(&mut agent);
        tasks.add_task(
            Task::new("audit").schema(
                crate::schemas::Schema::new(serde_json::json!({
                    "type": "object",
                    "properties": { "verdict": { "type": "string" } },
                    "required": ["verdict"],
                }))
                .unwrap(),
            ),
        );

        let context = claim(&agent, &tasks).expect("the task is claimable");
        let finish = context.tools.get("finish").expect("finish is bound");

        let declared = finish.input_schema().get_raw_schema();
        assert!(
            declared["properties"]["result"]["properties"]["verdict"].is_object(),
            "{declared}"
        );
    }

    #[tokio::test]
    async fn a_claimed_task_offers_an_interactive_agent_no_finish_tool() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tasks = Queue::new();
        tasks.set_dir(results_dir.path().to_path_buf());
        let agent = interactive_chatbot(&MockProvider::with_results(vec![]));
        tasks.add_agent(agent.clone());
        tasks.add_task("hello");

        let context = claim(&agent, &tasks).expect("the task is claimable");
        assert!(context.tools.get("finish").is_none());
    }

    #[tokio::test]
    async fn an_agent_leaves_a_task_its_label_mate_started_alone() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tasks = Queue::new();
        tasks.set_dir(results_dir.path().to_path_buf());
        let mut first = Agent::new()
            .label("resume_pool")
            .provider(MockProvider::with_results(vec![]))
            .model("mock")
            .role("test");
        let mut second = Agent::new()
            .label("resume_pool")
            .provider(MockProvider::with_results(vec![]))
            .model("mock")
            .role("test");
        tasks.bind_agent(&mut first);
        tasks.bind_agent(&mut second);
        tasks.add_task(Task::new("work").label("resume_pool"));

        claim(&first, &tasks).expect("the first agent claims the open task");

        assert!(
            claim(&second, &tasks).is_none(),
            "a label mate must not take over a started task",
        );
        assert!(
            claim(&first, &tasks).is_some(),
            "the agent that started it resumes it",
        );
    }

    #[tokio::test]
    async fn knowledge_write_then_read_across_tasks() {
        let provider = MockProvider::with_results(vec![
            Ok(knowledge_write_response(
                "api-config",
                "API runs on port 3000",
                "# API Config\n\nThe API server listens on port 3000.\nRate limit: 100 req/min.\nSee also: [[error-codes]]",
            )),
            Ok(knowledge_read_response("api-config")),
            Ok(write_result_response("done 1")),
            Ok(write_result_response("done 2")),
        ]);

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let knowledge_dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(knowledge_dir.path()).unwrap();

        let tasks = Queue::new();
        tasks
            .set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tasks.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .knowledge(&store),
        );
        tasks.add_task("first");
        tasks.add_task("second");
        let _ = tasks.finish_all_tasks().await;

        let prompts = provider.received_system_prompts();
        assert_eq!(prompts.len(), 4);

        assert!(
            !prompts[0].contains("## Knowledge"),
            "task 1 turn 1 should not have Knowledge section: {:?}",
            prompts[0]
        );
        assert_eq!(
            prompts[0], prompts[1],
            "task 1 turn 2 prompt must be byte-identical to turn 1"
        );
        assert_eq!(
            prompts[0], prompts[2],
            "task 1 turn 3 prompt must be byte-identical to turn 1"
        );

        assert!(
            prompts[3].contains("## Knowledge"),
            "task 2 should render the knowledge section: {:?}",
            prompts[3]
        );
        assert!(
            prompts[3].contains("api-config"),
            "task 2 should see the page slug: {:?}",
            prompts[3]
        );
        assert!(
            prompts[3].contains("API runs on port 3000"),
            "task 2 should see the index summary: {:?}",
            prompts[3]
        );
        assert!(
            !prompts[3].contains("Rate limit: 100 req/min"),
            "task 2 should NOT contain full page body: {:?}",
            prompts[3]
        );

        let bundle_dir = knowledge_dir.path().join("knowledge");
        let page_path = bundle_dir.join("pages").join("api-config.md");
        assert!(page_path.exists(), "page file should exist on disk");
        let page_raw = std::fs::read_to_string(&page_path).unwrap();
        assert!(page_raw.contains("Rate limit: 100 req/min"));
        assert!(page_raw.contains("---"));

        let index_path = bundle_dir.join("index.md");
        assert!(index_path.exists(), "index.md should exist on disk");
        let index_raw = std::fs::read_to_string(&index_path).unwrap();
        assert!(index_raw.contains("* [api-config](pages/api-config.md) - API runs on port 3000"));

        let received = provider.received();
        let turn3_messages = &received[2];
        let all_tool_results: Vec<&String> = turn3_messages
            .iter()
            .filter_map(|m| match m {
                crate::providers::Message::User { content } => Some(
                    content
                        .iter()
                        .filter_map(|b| match b {
                            crate::providers::ContentBlock::ToolResult { content, .. } => {
                                Some(content)
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .flatten()
            .collect();
        let read_result = all_tool_results
            .iter()
            .find(|r| !r.starts_with("page written"))
            .expect("should have a non-write tool result (the read result)");
        assert!(
            !read_result.contains("---"),
            "read result should not contain frontmatter delimiters: {read_result}"
        );
        assert!(
            !read_result.contains("timestamp:"),
            "read result should not contain timestamp field: {read_result}"
        );
        assert!(
            read_result.contains("Rate limit: 100 req/min"),
            "read result should contain page body: {read_result}"
        );
    }
}
