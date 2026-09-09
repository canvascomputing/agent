//! Runs one agent from task claim through requests, tool calls, compaction, and resolution.

use std::sync::Arc;

use crate::agents::agent::Agent;
use crate::agents::policy::{Policy, PolicyViolation};
use crate::agents::query::Matcher;
use crate::agents::tasks::{policy_violated, Author, Reply, Status, Task, Werk};
use crate::event::Event;
use crate::prompts::directives::{NO_TOOL_CALLED, REPLY_REJECTED};
use crate::prompts::RenderError;
use crate::providers::ProviderError;

use super::{CompactReason, POLL_INTERVAL};

impl Agent {
    pub(super) async fn run(self) {
        let werk = self
            .werk
            .upgrade()
            .expect("Agent's Werk was dropped before run() finished");

        loop {
            if self.run_is_over(&werk) {
                return;
            }
            let Some(task) = self.claim_task(&werk) else {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            };
            self.run_task(&werk, task).await;
        }
    }

    async fn run_task(&self, werk: &Arc<Werk>, task: Task) {
        let task_id = task.id.clone();
        let tools = self.get_tools(&task);
        let policy = werk.get_policy();
        let mut frozen_system_prompt = task
            .get_replies()
            .iter()
            .filter(|reply| reply.get_author() == Author::System)
            .flat_map(|reply| reply.get_content())
            .find_map(|content| content.get_text())
            .map(str::to_owned);
        let mut consecutive_schema_failures = 0;

        self.emit_event(werk, &task_id, Event::new(Event::TURN_STARTED));
        loop {
            if !werk.run.is_working() || policy_violated(&policy, &werk.stats).is_some() {
                break;
            }
            let Some(task) = werk.get_task(&task_id) else {
                break;
            };
            if task.is_cancelled() || !task.is_pending() {
                break;
            }
            if !task.is_waiting_for_response() {
                if self.is_interactive()
                    || !self.silence_retry(
                        werk,
                        &task_id,
                        &policy,
                        &mut consecutive_schema_failures,
                    )
                {
                    break;
                }
                continue;
            }
            if frozen_system_prompt.is_none() {
                match self.create_system_prompt(werk, &task, &policy) {
                    Ok(created) => frozen_system_prompt = Some(created),
                    Err(error) => {
                        self.fail_render(werk, &task_id, error);
                        break;
                    }
                }
            }
            let system_prompt = frozen_system_prompt
                .as_deref()
                .expect("system prompt prepared");
            let task = werk.get_task(&task_id).expect("claimed task exists");
            if self.needs_compaction(werk, &task_id, &task, system_prompt, &policy, &tools)
                && !self.compact(werk, &task_id, CompactReason::Proactive).await
            {
                break;
            }

            let calls = match self
                .request(werk, &task_id, system_prompt, &policy, &tools)
                .await
            {
                Ok(Some(calls)) => calls,
                Ok(None) => break,
                Err(ProviderError::ContextWindowExceeded { .. }) => {
                    if self.compact(werk, &task_id, CompactReason::Reactive).await {
                        continue;
                    }
                    break;
                }
                Err(_) => break,
            };
            if calls.is_empty() {
                continue;
            }
            if !self
                .call_tools(
                    werk,
                    &task_id,
                    &tools,
                    calls,
                    &policy,
                    &mut consecutive_schema_failures,
                )
                .await
            {
                break;
            }
        }
    }

    fn create_system_prompt(
        &self,
        werk: &Werk,
        task: &Task,
        policy: &Policy,
    ) -> Result<String, RenderError> {
        let context_values =
            crate::prompts::context_values(&self.get_dir(), policy, &werk.stats, &task.id);
        let rendered_role = werk.render_prompt(self.get_role(), &context_values)?;
        let knowledge_index = self.get_knowledge().get_index();
        let knowledge_body = knowledge_index.trim_matches('\n');
        let system_prompt = match (rendered_role.is_empty(), knowledge_body.is_empty()) {
            (_, true) => rendered_role,
            (true, false) => format!("## Knowledge\n\n{knowledge_body}"),
            (false, false) => {
                format!("{rendered_role}\n\n## Knowledge\n\n{knowledge_body}")
            }
        };
        let initial_task_reply = if task.replies.is_empty() {
            Some(task.initial_reply(werk)?)
        } else {
            None
        };

        werk.append_reply(&task.id, Reply::system_text(system_prompt.clone()));
        if let Some(initial_task_reply) = initial_task_reply {
            werk.append_reply(&task.id, initial_task_reply);
        }
        Ok(system_prompt)
    }

    fn run_is_over(&self, werk: &Werk) -> bool {
        if !werk.run.is_working() {
            return true;
        }
        let policy = werk.get_policy();
        if let Some((violation, limit)) = policy_violated(&policy, &werk.stats) {
            werk.emit_event(
                Event::new(Event::POLICY_VIOLATED)
                    .data(serde_json::json!({ "policy": violation, "limit": limit }))
                    .agent_id(self.get_id()),
            );
            return true;
        }
        false
    }

    fn claim_task(&self, werk: &Arc<Werk>) -> Option<Task> {
        let label = self.label.clone();
        let claimable = (move |task: &Task| {
            task.status == Status::Todo
                && Agent::handles(label.as_deref(), task.label.as_deref())
                && !task.is_cancelled()
        })
        .into_query();
        let agent_id = self.get_id().to_string();
        let interactive = self.is_interactive();
        let resumable = move |task: &Task| {
            task.status == Status::InProgress
                && task.assignee.as_deref() == Some(agent_id.as_str())
                && (task.is_waiting_for_response() || !interactive)
                && !task.is_cancelled()
        };
        let task_id = werk
            .claim(&claimable, self.get_id())
            .or_else(|| werk.find_task(resumable).map(|task| task.id.clone()))?;
        werk.get_task(&task_id)
    }

    pub(super) fn emit_event(&self, werk: &Werk, task_id: &str, event: Event) -> Event {
        werk.emit_event(event.task_id(task_id).agent_id(self.get_id()))
    }

    fn fail_render(&self, werk: &Werk, task_id: &str, error: RenderError) {
        self.emit_event(
            werk,
            task_id,
            Event::prompt_render_failed(&error.expression, &error.message),
        );
        self.fail_task(werk, task_id);
    }

    pub(super) fn fail_task(&self, werk: &Werk, task_id: &str) {
        let _ = werk.set_failed_by(task_id, self.get_id());
    }

    fn silence_retry(
        &self,
        werk: &Werk,
        task_id: &str,
        policy: &Policy,
        consecutive_schema_failures: &mut u32,
    ) -> bool {
        let max = policy.max_schema_retries.unwrap_or(u32::MAX);
        *consecutive_schema_failures = consecutive_schema_failures.saturating_add(1);
        if *consecutive_schema_failures >= max {
            self.emit_event(
                werk,
                task_id,
                Event::new(Event::POLICY_VIOLATED).data(serde_json::json!({
                    "policy": PolicyViolation::MaxSchemaRetries,
                    "limit": u64::from(max),
                })),
            );
            self.fail_task(werk, task_id);
            return false;
        }
        let detail = self.get_directives().render(NO_TOOL_CALLED, &[]);
        let attempt = *consecutive_schema_failures;
        self.emit_event(
            werk,
            task_id,
            Event::new(Event::SCHEMA_RETRIED).data(serde_json::json!({
                "attempt": attempt,
                "max_attempts": max,
                "kind": "tool_not_called",
                "message": detail,
            })),
        );
        let directive = self.get_directives().render(
            REPLY_REJECTED,
            &[
                ("detail", &detail),
                ("attempt", &attempt.to_string()),
                ("max_attempts", &max.to_string()),
                ("task_id", task_id),
                ("agent", self.get_id()),
            ],
        );
        werk.append_reply(task_id, Reply::user_text(directive));
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::agents::policy::Policy;
    use crate::prompts::directives::{DirectiveStore, REPLY_REJECTED};

    use crate::agents::agent::Agent;
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tasks::{Author, Status, Task, Werk};
    use crate::agents::Knowledge;
    use crate::tools::{EventTool, TaskTool};

    // Execution lifecycle

    #[tokio::test]
    async fn finish_drains_late_added_tasks() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("a-done")),
            Ok(write_result_response("b-done")),
        ]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TaskTool),
        );

        werk.start();

        werk.add_task("a");
        werk.add_task("b");

        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(werk.get_results().len(), 2);
        assert_eq!(werk.get_results().pop(), Some(serde_json::json!("b-done")));
    }

    // retry directive

    #[tokio::test]
    async fn a_replacement_replaces_the_silence_directive() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("just thinking, no tool call")),
            Ok(write_result_response("done")),
        ]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .directive(REPLY_REJECTED, "PLEASE CALL A TOOL NOW"),
        );

        werk.start();
        werk.add_task("go");
        tokio::time::timeout(Duration::from_secs(5), werk.finish())
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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .directive(REPLY_REJECTED, "attempt {attempt} of {max_attempts}"),
        );

        werk.start();
        werk.add_task("go");
        tokio::time::timeout(Duration::from_secs(5), werk.finish())
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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        // An id is `<label>-<n>`, so the two agents read their own name back.
        werk.add_agent(
            Agent::new()
                .label("scout")
                .provider(scout.clone())
                .model("mock")
                .role("test")
                .directive(REPLY_REJECTED, "{agent}, CALL A TOOL"),
        );
        werk.add_agent(
            Agent::new()
                .label("worker")
                .provider(worker.clone())
                .model("mock")
                .role("test")
                .directive(REPLY_REJECTED, "{agent}, CALL A TOOL"),
        );

        werk.start();
        werk.add_task(Task::new("go").label("scout"));
        werk.add_task(Task::new("go").label("worker"));
        tokio::time::timeout(Duration::from_secs(5), werk.finish())
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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test"),
        );

        werk.start();
        werk.add_task("go");
        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("finish did not finish within 5s");

        let injected = user_text(&provider.received()[1]);
        assert!(
            injected.contains("was not accepted"),
            "an unreplaced directive must render its built-in text: {injected:?}",
        );
    }

    #[tokio::test]
    async fn finish_waits_through_an_on_result_follow_up() {
        // The handler creates a follow-up when t-1 finishes. finish()
        // must not drain in the window between the finish transition and
        // the handler's insert.
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("first-done")),
            Ok(write_result_response("follow-up-done")),
        ]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        werk.on_result(|werk, done, _| {
            if done.id == "t-1" {
                werk.add_task(Task::new("follow up").label("alice"));
            }
        });
        werk.add_agent(
            Agent::new()
                .label("alice")
                .provider(provider.clone())
                .model("mock")
                .role("test"),
        );

        werk.start();
        werk.add_task(Task::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(werk.get_results().len(), 2);
        assert_eq!(werk.get_task("t-2").unwrap().status, Status::Finished);
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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        // Both scans can finish at once, so the first handler to see the pair
        // takes the flag and the other returns: without it the report is filed
        // twice.
        let filed = Arc::new(AtomicBool::new(false));
        werk.on_result(move |werk, done, _| {
            if done.get_label() != Some("scan") {
                return;
            }
            let scans = werk.find_results("task.label = scan AND task.status = finished");
            if scans.len() < 2 || filed.swap(true, Ordering::SeqCst) {
                return;
            }
            let verdicts: Vec<String> = scans.iter().map(|scan| scan.to_string()).collect();
            werk.add_task(Task::labeled(
                "report",
                format!("Write the report from {}.", verdicts.join(" and ")),
            ));
        });

        for label in ["scan", "scan", "report"] {
            werk.add_agent(
                Agent::new()
                    .label(label)
                    .provider(provider.clone())
                    .model("mock")
                    .role("test"),
            );
        }

        werk.start();
        werk.add_task(Task::labeled("scan", "scan a.py"));
        werk.add_task(Task::labeled("scan", "scan b.py"));

        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("finish did not finish within 5s");

        let report = werk.find_task("task.label = report").unwrap();
        assert_eq!(
            report.task,
            serde_json::json!("Write the report from \"clean\" and \"clean\".")
        );
        assert_eq!(report.status, Status::Finished);
        assert_eq!(
            werk.find_results("task.label = report"),
            vec![serde_json::json!("report-done")]
        );
    }

    #[tokio::test]
    async fn task_finished_event_fires_exactly_once_per_task() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(write_result_response("done"))]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        let finished_events = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&finished_events);
        werk.on_event(move |_, e| {
            if e.get_name() == crate::event::Event::TASK_FINISHED {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        werk.add_agent(
            Agent::new()
                .label("alice")
                .provider(provider.clone())
                .model("mock")
                .role("test"),
        );

        werk.start();
        werk.add_task(Task::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(finished_events.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn event_tool_finishes_its_current_task_without_a_silence_retry() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(crate::providers::ModelResponse {
            content: vec![crate::providers::ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "event".into(),
                input: serde_json::json!({
                    "name": crate::event::Event::TASK_FINISHED,
                    "data": { "result": { "verdict": "safe" } }
                }),
            }],
            status: crate::providers::ResponseStatus::ToolUse,
            usage: crate::providers::TokenUsage::default(),
            model: "mock".into(),
        })]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(2),
                ..Default::default()
            });
        let events = collect_events(&werk);
        werk.add_agent(
            Agent::new()
                .label("alice")
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .tool(EventTool),
        );
        werk.add_task(
            Task::new("audit").label("alice").schema(
                crate::schemas::Schema::new(serde_json::json!({
                    "type": "object",
                    "properties": { "verdict": { "type": "string" } },
                    "required": ["verdict"]
                }))
                .unwrap(),
            ),
        );

        let results = tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("event did not finish the task within 5s");

        assert_eq!(provider.requests(), 1);
        assert_eq!(results, vec![serde_json::json!({ "verdict": "safe" })]);
        let events = events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.get_name() == crate::event::Event::TASK_FINISHED)
                .count(),
            1,
        );
        assert!(!events.iter().any(|event| {
            event.get_name() == crate::event::Event::SCHEMA_RETRIED
                && event.get_data()["kind"] == "tool_not_called"
        }));
    }

    #[tokio::test]
    async fn interactive_pause_holds_when_an_event_handler_edits_replies() {
        // A handler that edits on every event must not perturb the interactive
        // gate: the task pauses on the assistant text reply, and no second
        // request fires. Exhausting the single mock response would fail the
        // task, so `requests == 1` and `in_progress` prove the gate held.
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        // Unfiltered on purpose: it also fires for the run-level events, whose
        // empty ID names no task.
        werk.on_event(|werk, event| {
            werk.edit_replies(&event.task_id, |_replies| {});
        });
        werk.add_agent(interactive_chatbot(&provider));
        werk.add_task("hello");
        werk.start();

        for _ in 0..200 {
            let last_is_assistant = werk
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
        // Wait long enough to catch an incorrect second request.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let task = werk.get_tasks().into_iter().next().unwrap();
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

        werk.cancel_all_tasks();
        werk.finish().await;
    }

    #[tokio::test]
    async fn loop_pauses_after_text_reply_then_resumes_when_caller_replies() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider =
            MockProvider::with_results(vec![Ok(text_response("hi")), Ok(text_response("and now"))]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        werk.add_agent(interactive_chatbot(&provider));
        let id = werk.add_task("hello");

        let werk_for_inject = Arc::clone(&werk);
        let inject = async move {
            for _ in 0..200 {
                let last_is_assistant = werk_for_inject
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
            let task = werk_for_inject
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
            werk_for_inject.add_reply(&id, "what next?");
        };

        tokio::time::timeout(Duration::from_secs(5), async {
            werk.start();
            // The pause is not the end of the task, so the reply arrives first
            // and the finish then waits out the turn it sets off.
            inject.await;
            werk.finish().await;
        })
        .await
        .expect("test did not finish within 5s");

        let task = werk
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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        werk.add_agent(interactive_chatbot(&provider));
        let id = werk.add_task("hello");
        werk.start();

        // Pausing for input is no lifecycle transition and leaves the task
        // `in_progress`, so this returns only if `request_finished` reaches the
        // waiter after the reply is in the store.
        let waited = id.clone();
        tokio::time::timeout(
            Duration::from_secs(5),
            werk.finish_tasks(move |t: &Task| t.id == waited),
        )
        .await
        .expect("finish did not return within 5s");
        let task = werk.get_task(&id).unwrap();
        assert_eq!(task.status, Status::InProgress);
        assert!(task
            .replies
            .last()
            .is_some_and(|r| r.author == Author::Assistant));
        werk.cancel_all_tasks();
    }

    #[tokio::test]
    async fn paused_interactive_task_emits_turn_started_exactly_once() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        let collected = collect_events(&werk);
        werk.add_agent(interactive_chatbot(&provider));
        werk.add_task("hello");

        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("test did not finish within 5s");

        let events = collected.lock().unwrap().clone();
        let turn_started = events
            .iter()
            .filter(|e| e.get_name() == crate::event::Event::TURN_STARTED)
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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        werk.add_agent(interactive_chatbot(&provider));
        let first_key = werk.add_task("first chat");
        werk.start();

        let werk_for_drive = Arc::clone(&werk);
        let drive = async move {
            for _ in 0..200 {
                let paused = werk_for_drive
                    .get_task(&first_key)
                    .and_then(|t| t.replies.last().map(|r| r.author == Author::Assistant))
                    .unwrap_or(false);
                if paused {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let second_key = werk_for_drive.add_task("second chat");
            for _ in 0..400 {
                if werk_for_drive
                    .get_task(&second_key)
                    .is_some_and(|t| t.replies.iter().any(|r| r.author == Author::Assistant))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let first = werk_for_drive.get_task(&first_key).unwrap();
            let second = werk_for_drive.get_task(&second_key).unwrap();
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
            tokio::join!(werk.finish(), drive);
        })
        .await
        .expect("test did not finish within 5s");
    }

    #[tokio::test]
    async fn loop_fails_task_when_silence_exceeds_schema_retry_budget() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(1),
                ..Default::default()
            });
        let collected = collect_events(&werk);
        werk.add_agent(task_agent(&provider));
        werk.add_task("go");

        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("test did not finish within 5s");

        let task = werk
            .get_tasks()
            .into_iter()
            .next()
            .expect("task must exist");
        assert_eq!(task.status, Status::Failed);

        let events = collected.lock().unwrap().clone();
        let policy_violated = events.iter().any(|e| {
            e.get_name() == crate::event::Event::POLICY_VIOLATED
                && e.get_data()["policy"] == "max_schema_retries"
                && e.get_data()["limit"] == 1
        });
        assert!(policy_violated, "expected PolicyViolated MaxSchemaRetries");
        let task_failed = events
            .iter()
            .any(|e| e.get_name() == crate::event::Event::TASK_FAILED);
        assert!(task_failed, "expected TaskFailed");
    }

    #[tokio::test]
    async fn loop_finishes_task_after_one_silence_and_recovery() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("hi")),
            Ok(write_result_response("done")),
        ]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        werk.add_agent(task_agent(&provider));
        werk.add_task("go");

        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("test did not finish within 5s");

        let task = werk
            .get_tasks()
            .into_iter()
            .next()
            .expect("task must exist");
        assert_eq!(task.status, Status::Finished);
        assert_eq!(werk.get_results().pop(), Some(serde_json::json!("done")));
    }

    #[tokio::test]
    async fn silence_retry_emits_schema_retried_event_with_attempt_1() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("hi")),
            Ok(write_result_response("done")),
        ]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        let collected = collect_events(&werk);
        werk.add_agent(task_agent(&provider));
        werk.add_task("go");

        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("test did not finish within 5s");

        let events = collected.lock().unwrap().clone();
        let schema_retries: Vec<(u64, String)> = events
            .iter()
            .filter(|e| e.get_name() == crate::event::Event::SCHEMA_RETRIED)
            .map(|e| {
                (
                    e.get_data()["attempt"].as_u64().unwrap(),
                    e.get_data()["message"].as_str().unwrap().to_owned(),
                )
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
    async fn cancel_stops_a_running_werk() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        werk.start();
        werk.cancel_all_tasks();

        tokio::time::timeout(Duration::from_secs(2), werk.finish())
            .await
            .expect("run did not exit within 2s of cancel()");
    }

    #[tokio::test]
    async fn cancel_keeps_the_matching_pool_off_the_queue_while_others_run() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        let analyst = MockProvider::with_results(vec![Ok(write_result_response("analyzed"))]);
        let researcher = MockProvider::with_results(vec![Ok(write_result_response("hunted"))]);
        werk.add_agent(
            Agent::new()
                .label("analysis")
                .provider(analyst)
                .model("mock")
                .role("test"),
        );
        werk.add_agent(
            Agent::new()
                .label("research")
                .provider(researcher.clone())
                .model("mock")
                .role("test"),
        );

        // Cancel research tasks in the current execution before adding both tasks; analysis continues.
        werk.start();
        werk.cancel_tasks("task.label = research");
        werk.add_task(Task::new("hunt").label("research"));
        werk.add_task(Task::new("triage").label("analysis"));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let analyzed = werk
                .get_tasks()
                .iter()
                .any(|t| t.status == Status::Finished && t.task.as_str() == Some("triage"));
            if analyzed {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                werk.cancel_all_tasks();
                panic!("analysis task did not finish within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let hunt = werk
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

        werk.cancel_all_tasks();
        tokio::time::timeout(Duration::from_secs(2), werk.finish())
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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TaskTool),
        );

        werk.add_task("first");
        werk.finish().await;
        assert_eq!(werk.get_results().pop(), Some(serde_json::json!("first")));

        werk.add_task("second");
        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("second finish did not finish within 5s");
        assert_eq!(werk.get_results().pop(), Some(serde_json::json!("second")));
    }

    #[tokio::test]
    async fn agent_finish_forwards_to_bound_werk() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(write_result_response("forwarded"))]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        let agent = werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TaskTool),
        );

        agent.add_task("hello");
        let werk = agent.start();
        tokio::time::timeout(Duration::from_secs(5), werk.finish())
            .await
            .expect("the run did not end within 5s");
        assert_eq!(
            werk.get_results().pop(),
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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(10),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .tool(crate::tools::TaskTool),
        );
        werk.add_task("first");
        werk.add_task("second");
        let _ = werk.finish().await;

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

        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .knowledge(&store),
        );
        werk.add_task("first");
        werk.add_task("second");
        let _ = werk.finish().await;

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

        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .knowledge(&store),
        );
        werk.add_task("hi");
        let _ = werk.finish().await;

        let prompts = provider.received_system_prompts();
        assert_eq!(prompts.len(), 2);
        assert_eq!(
            prompts[0], prompts[1],
            "mid-task knowledge write must not change the system prompt within the same task"
        );
        assert!(store.get_index().contains("mid-task"));
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

        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });

        werk.add_agent(
            Agent::new()
                .label("a")
                .provider(p_a.clone())
                .model("mock")
                .role("test")
                .knowledge(&store),
        );
        werk.add_agent(
            Agent::new()
                .label("b")
                .provider(p_b.clone())
                .model("mock")
                .role("test")
                .knowledge(&store),
        );

        werk.add_task(Task::new("alice work").label("a"));
        let _ = werk.finish().await;
        assert!(store.get_index().contains("alice-note"));

        werk.add_task(Task::new("bob work").label("b"));
        let _ = werk.finish().await;

        let bob_prompts = p_b.received_system_prompts();
        assert_eq!(bob_prompts.len(), 1, "bob processed exactly one task");
        assert!(
            bob_prompts[0].contains("Note from Alice"),
            "bob should see alice's write: {:?}",
            bob_prompts[0]
        );
    }

    #[tokio::test]
    async fn a_task_schema_reaches_the_first_message_of_its_task() {
        let provider = MockProvider::with_results(vec![Ok(write_result_value(
            serde_json::json!({"verdict": "done"}),
        ))]);
        let results_dir = crate::test_util::TempDir::new().unwrap();

        let schema = crate::schemas::Schema::new(serde_json::json!({
            "type": "object",
            "properties": { "verdict": { "type": "string" } },
            "required": ["verdict"],
        }))
        .unwrap();

        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .label("analysis"),
        );
        werk.add_task(Task::new("audit").label("analysis").schema(schema));
        let _ = werk.finish().await;

        let task_message = &user_texts(&provider.received()[0])[0];
        assert!(
            task_message.contains("verdict"),
            "the task schema must be in the task message: {task_message:?}",
        );
        assert_eq!(werk.get_tasks()[0].status, Status::Finished);
    }

    #[tokio::test]
    async fn a_claimed_task_binds_its_schema_to_the_finish_tool() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf());
        // Bound rather than cloned into the Werk: `finish` is registered on
        // the agent that joins one, and this claims through that agent.
        let mut agent = Agent::new()
            .provider(MockProvider::with_results(vec![]))
            .model("mock")
            .role("test");
        werk.bind_agent(&mut agent);
        werk.add_task(
            Task::new("audit").schema(
                crate::schemas::Schema::new(serde_json::json!({
                    "type": "object",
                    "properties": { "verdict": { "type": "string" } },
                    "required": ["verdict"],
                }))
                .unwrap(),
            ),
        );

        let task = agent.claim_task(&werk).expect("the task is claimable");
        let tools = agent.get_tools(&task);
        let finish = agent.get_tool(&tools, "finish").expect("finish is bound");

        let declared = finish.get_input_schema().get_raw_schema();
        assert!(declared["properties"]["verdict"].is_object(), "{declared}");
        assert_eq!(declared["required"], serde_json::json!(["verdict"]));
    }

    #[tokio::test]
    async fn a_claimed_task_binds_its_schema_inside_the_event_tool() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf());
        let mut agent = Agent::new()
            .provider(MockProvider::with_results(vec![]))
            .model("mock")
            .role("test")
            .tool(crate::tools::EventTool);
        werk.bind_agent(&mut agent);
        werk.add_task(
            Task::new("audit").schema(
                crate::schemas::Schema::new(serde_json::json!({
                    "type": "object",
                    "properties": { "verdict": { "type": "string" } },
                    "required": ["verdict"],
                }))
                .unwrap(),
            ),
        );

        let task = agent.claim_task(&werk).expect("the task is claimable");
        let tools = agent.get_tools(&task);
        let event = agent.get_tool(&tools, "event").expect("event is bound");
        let declared = event.get_input_schema().get_raw_schema();

        assert!(
            declared["allOf"][0]["then"]["properties"]["data"]["properties"]["result"]
                ["properties"]["verdict"]
                .is_object()
        );
    }

    #[tokio::test]
    async fn a_claimed_task_offers_an_interactive_agent_no_finish_tool() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf());
        let agent = interactive_chatbot(&MockProvider::with_results(vec![]));
        werk.add_agent(agent.clone());
        werk.add_task("hello");

        let task = agent.claim_task(&werk).expect("the task is claimable");
        let tools = agent.get_tools(&task);
        assert!(agent.get_tool(&tools, "finish").is_none());
    }

    #[tokio::test]
    async fn an_agent_leaves_a_task_its_label_mate_started_alone() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf());
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
        werk.bind_agent(&mut first);
        werk.bind_agent(&mut second);
        werk.add_task(Task::new("work").label("resume_pool"));

        first
            .claim_task(&werk)
            .expect("the first agent claims the open task");

        assert!(
            second.claim_task(&werk).is_none(),
            "a label mate must not take over a started task",
        );
        assert!(
            first.claim_task(&werk).is_some(),
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

        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .knowledge(&store),
        );
        werk.add_task("first");
        werk.add_task("second");
        let _ = werk.finish().await;

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

        let page_path = knowledge_dir.path().join("pages").join("api-config.md");
        assert!(page_path.exists(), "page file should exist on disk");
        let page_raw = std::fs::read_to_string(&page_path).unwrap();
        assert!(page_raw.contains("Rate limit: 100 req/min"));
        assert!(page_raw.contains("---"));

        let index_path = knowledge_dir.path().join("index.md");
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
