//! Per-agent driver: an outer loop claims tickets, an inner state machine
//! drives each claimed ticket through requests, tool calls, and compaction.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::agents::agent::Agent;
use crate::agents::policy::Policies;
use crate::agents::tickets::{policy_violated_kind, Reply, Status, Ticket, TicketSystem};
use crate::event::{CompactReason, EventKind, PolicyKind};
use crate::providers::{AsUserMessage, Message, Model, RequestErrorKind};

use super::{compact, request, tool_call, Step, POLL_INTERVAL};

const RESUME_OR_FINISH_DETAIL: &str =
    "Your last reply contained no tool call. Call `finish_ticket` with your result if the work is complete, or another tool to continue.";

pub(super) struct TicketContext<'a> {
    pub(super) agent: &'a Agent,
    pub(super) model: &'a Model,
    pub(super) ticket_system: &'a Arc<TicketSystem>,
    pub(super) stop_signal: Arc<AtomicBool>,

    pub(super) ticket_key: String,
    pub(super) system_prompt: String,
    pub(super) policies: Policies,

    // Spans turns; trips max_schema_retries.
    pub(super) consecutive_schema_failures: u32,
}

impl<'a> TicketContext<'a> {
    pub(super) fn emit(&self, kind: EventKind) {
        self.ticket_system
            .emit(&self.ticket_key, self.agent.get_name(), kind);
    }

    pub(super) fn ticket(&self) -> Option<Ticket> {
        self.ticket_system.get_ticket(&self.ticket_key)
    }

    /// The corrective directive to inject for `detail`: the agent's
    /// `on_failure` hook if it returns a replacement, else the default.
    pub(super) fn failure_directive(&self, detail: &str) -> String {
        if let Some(hook) = self.agent.on_failure() {
            if let Some(text) = hook(detail) {
                return text;
            }
        }
        crate::prompts::retry_directive(detail)
    }

    pub(super) fn fail_with(&self, kind: RequestErrorKind, message: String) {
        self.emit(EventKind::RequestFailed {
            model: self.model.name.clone(),
            kind,
            message,
        });
        let _ = self
            .ticket_system
            .set_failed_by(&self.ticket_key, self.agent.get_name());
    }
}

pub(super) async fn run_agent(agent: Agent) {
    let ticket_system = agent
        .ticket_system
        .upgrade()
        .expect("Agent's TicketSystem was dropped before run() finished");

    loop {
        if should_stop(&agent, &ticket_system) {
            return;
        }
        let Some(mut context) = claim(&agent, &ticket_system) else {
            tokio::time::sleep(POLL_INTERVAL).await;
            continue;
        };
        let mut step = Step::CheckTicket;
        loop {
            step = match step {
                Step::CheckTicket => check_ticket(&mut context),
                Step::Compact(reason) => compact::run(&mut context, reason).await,
                Step::Request => request::run(&mut context).await,
                Step::ToolCalls(calls) => tool_call::run(&mut context, calls).await,
                Step::NextTicket | Step::Cancel => break,
                Step::Stop => return,
            };
        }
    }
}

fn should_stop(agent: &Agent, ticket_system: &TicketSystem) -> bool {
    // finish() swaps in a fresh signal per run, so a snapshot taken once per
    // agent task would miss the reset; re-read the current signal every check.
    let stop_signal = Arc::clone(&ticket_system.stop_signal.lock().unwrap());
    if stop_signal.load(Ordering::Relaxed) {
        return true;
    }
    let policies = ticket_system.policies();
    if let Some((kind, limit)) = policy_violated_kind(&policies, &ticket_system.stats) {
        ticket_system.emit(
            "",
            agent.get_name(),
            EventKind::PolicyViolated { kind, limit },
        );
        return true;
    }
    false
}

/// Claim a `Todo` ticket for this agent, or resume one of its `InProgress`
/// tickets; seed the transcript on first contact.
fn claim<'a>(agent: &'a Agent, ticket_system: &'a Arc<TicketSystem>) -> Option<TicketContext<'a>> {
    let claimable = |t: &Ticket| {
        t.status == Status::Todo
            && agent.handles_labels(&t.labels)
            && !ticket_system.labels_cancelled(&t.labels)
    };
    let resumable = |t: &Ticket| {
        t.status == Status::InProgress
            && t.labels.iter().any(|l| l == agent.get_name())
            && (t.is_waiting_for_response() || !agent.is_interactive())
            && !ticket_system.labels_cancelled(&t.labels)
    };
    let ticket_key = ticket_system
        .claim(claimable, agent.get_name())
        .or_else(|| ticket_system.find_ticket(resumable).map(|t| t.key.clone()))?;
    let ticket = ticket_system.get_ticket(&ticket_key)?;

    let knowledge_index = agent.knowledge().index();
    // Lets the model see what knowledge pages it can read.
    let system_prompt = agent.system_prompt(Some(&knowledge_index));
    let policies = ticket_system.policies();
    let agent_name = agent.get_name();

    ticket_system.emit(&ticket_key, agent_name, EventKind::TurnStarted);

    if ticket.replies.is_empty() {
        ticket_system.add_reply(&ticket_key, Reply::system_text(system_prompt.clone()));
        if let Some(context_message) =
            agent.context_message(&policies, &ticket_system.stats, Some(&ticket_key))
        {
            ticket_system.add_reply(&ticket_key, Reply::user_text(context_message));
        }
        let Message::User {
            content: task_blocks,
        } = ticket.as_user_message()
        else {
            unreachable!("Ticket::as_user_message returns Message::User");
        };
        ticket_system.add_reply(&ticket_key, Reply::user(&task_blocks, &HashMap::new()));
        ticket_system.emit(&ticket_key, agent_name, EventKind::TicketStarted);
    }

    let stop_signal = Arc::clone(&ticket_system.stop_signal.lock().unwrap());
    Some(TicketContext {
        agent,
        model: &agent.model,
        ticket_system,
        stop_signal,

        ticket_key,
        system_prompt,
        policies,

        consecutive_schema_failures: 0,
    })
}

/// Re-read the ticket and decide the next step.
fn check_ticket(context: &mut TicketContext<'_>) -> Step {
    if should_stop(context.agent, context.ticket_system) {
        return Step::Stop;
    }
    let Some(ticket) = context.ticket() else {
        return Step::NextTicket;
    };
    if context.ticket_system.labels_cancelled(&ticket.labels) {
        return Step::Cancel;
    }
    // The transition itself already emitted the terminal event; the agent
    // only moves on to fresh work.
    if ticket.is_resolved() {
        return Step::NextTicket;
    }
    if !ticket.is_waiting_for_response() {
        if context.agent.is_interactive() {
            // Pause until a caller reply lands; the resume claim re-checks.
            return Step::NextTicket;
        }
        return silence_retry(context);
    }
    if compact::proactive_compaction_needed(context, &ticket) {
        return Step::Compact(CompactReason::Proactive);
    }
    Step::Request
}

/// The model replied without a tool call: prompt it to resume or finish,
/// counting the silence toward the schema-retry budget.
fn silence_retry(context: &mut TicketContext<'_>) -> Step {
    let max = context.policies.max_schema_retries.unwrap_or(u32::MAX);
    context.consecutive_schema_failures = context.consecutive_schema_failures.saturating_add(1);
    if context.consecutive_schema_failures >= max {
        context.emit(EventKind::PolicyViolated {
            kind: PolicyKind::MaxSchemaRetries,
            limit: u64::from(max),
        });
        let _ = context
            .ticket_system
            .set_failed_by(&context.ticket_key, context.agent.get_name());
        return Step::NextTicket;
    }
    context.emit(EventKind::SchemaRetried {
        attempt: context.consecutive_schema_failures,
        max_attempts: max,
        message: RESUME_OR_FINISH_DETAIL.to_string(),
    });
    context.ticket_system.add_reply(
        &context.ticket_key,
        Reply::user_text(context.failure_directive(RESUME_OR_FINISH_DETAIL)),
    );
    Step::CheckTicket
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::agents::agent::Agent;
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tickets::{Author, Status, Ticket, TicketSystem};
    use crate::agents::Knowledge;
    use crate::providers::Provider;
    use crate::tools::{FinishTicketTool, HandoverTicketTool, ManageTicketsTool};

    // Run lifecycle

    #[tokio::test]
    async fn finish_drains_late_added_tickets() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("a-done")),
            Ok(write_result_response("b-done")),
        ]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));
        tickets.agent(
            Agent::new()
                .name("agent")
                .provider(provider as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(ManageTicketsTool)
                .build(),
        );

        tickets.start();

        tickets.task("a");
        tickets.task("b");

        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(tickets.results().len(), 2);
        assert_eq!(tickets.last_result(), Some(serde_json::json!("b-done")));
    }

    // on_failure hook

    #[tokio::test]
    async fn on_failure_hook_replaces_the_silence_directive() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("just thinking, no tool call")),
            Ok(write_result_response("done")),
        ]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));
        tickets.agent(
            Agent::new()
                .name("agent")
                .provider(provider.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .on_failure(|_| Some("PLEASE CALL A TOOL NOW".into()))
                .build(),
        );

        tickets.start();
        tickets.task("go");
        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("finish did not finish within 5s");

        let injected = user_text(&provider.received()[1]);
        assert!(
            injected.contains("PLEASE CALL A TOOL NOW"),
            "hook directive must be injected: {injected:?}",
        );
        assert!(
            !injected.contains("was not accepted"),
            "default framing must be suppressed: {injected:?}",
        );
    }

    #[tokio::test]
    async fn on_failure_hook_returning_none_keeps_the_default_directive() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("just thinking, no tool call")),
            Ok(write_result_response("done")),
        ]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));
        tickets.agent(
            Agent::new()
                .name("agent")
                .provider(provider.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .on_failure(|_| None)
                .build(),
        );

        tickets.start();
        tickets.task("go");
        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("finish did not finish within 5s");

        let injected = user_text(&provider.received()[1]);
        assert!(
            injected.contains("was not accepted"),
            "None must fall back to the built-in directive: {injected:?}",
        );
    }

    #[tokio::test]
    async fn finish_waits_through_handover_chain() {
        // alice hands TICKET-1 off to bob. The handover inserts the child
        // before finishing the parent, so finish() must not observe an
        // empty queue in between and drain the chain early.
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(handover_response("bob", "continue", "alice-done")),
            Ok(write_result_response("bob-done")),
        ]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));
        for name in ["alice", "bob"] {
            tickets.agent(
                Agent::new()
                    .name(name)
                    .provider(Arc::clone(&provider) as Arc<dyn Provider>)
                    .model("mock")
                    .role("test")
                    .tool(HandoverTicketTool)
                    .tool(FinishTicketTool)
                    .build(),
            );
        }

        tickets.start();
        tickets.ticket(Ticket::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(tickets.results().len(), 2);
        assert_eq!(
            tickets.get_ticket("TICKET-2").unwrap().status,
            Status::Finished
        );
    }

    #[tokio::test]
    async fn finish_waits_through_create_ticket_on_result_follow_up() {
        // The handler mints a follow-up when TICKET-1 finishes. finish()
        // must not drain in the window between the finish transition and
        // the handler's insert.
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("first-done")),
            Ok(write_result_response("follow-up-done")),
        ]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));
        tickets.create_ticket_on_result(|done| {
            (done.key == "TICKET-1").then(|| Ticket::new("follow up").label("alice"))
        });
        tickets.agent(
            Agent::new()
                .name("alice")
                .provider(Arc::clone(&provider) as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .build(),
        );

        tickets.start();
        tickets.ticket(Ticket::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(tickets.results().len(), 2);
        assert_eq!(
            tickets.get_ticket("TICKET-2").unwrap().status,
            Status::Finished
        );
    }

    #[tokio::test]
    async fn ticket_finished_event_fires_exactly_once_per_ticket() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(write_result_response("done"))]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));
        let finished_events = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&finished_events);
        tickets.on_event(move |e| {
            if matches!(e.kind, crate::event::EventKind::TicketFinished) {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        tickets.agent(
            Agent::new()
                .name("alice")
                .provider(Arc::clone(&provider) as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .build(),
        );

        tickets.start();
        tickets.ticket(Ticket::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(finished_events.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn loop_pauses_after_text_reply_then_resumes_when_caller_replies() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("hi")),
            Ok(write_result_response("done")),
        ]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));
        tickets.agent(interactive_chatbot(&provider));
        let key = tickets.task("hello");

        let tickets_for_inject = Arc::clone(&tickets);
        let inject = async move {
            for _ in 0..200 {
                let last_is_assistant = tickets_for_inject
                    .tickets()
                    .into_iter()
                    .next()
                    .and_then(|t| t.replies.last().map(|r| r.author == Author::Assistant))
                    .unwrap_or(false);
                if last_is_assistant {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let ticket = tickets_for_inject
                .tickets()
                .into_iter()
                .next()
                .expect("ticket must exist");
            assert_eq!(ticket.status, Status::InProgress);
            assert_eq!(
                ticket.replies.last().map(|r| r.author),
                Some(Author::Assistant),
                "gate must pause on the assistant text reply",
            );
            tickets_for_inject.reply(&key, "what next?");
        };

        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(tickets.finish(), inject);
        })
        .await
        .expect("test did not finish within 5s");

        let ticket = tickets
            .tickets()
            .into_iter()
            .next()
            .expect("ticket must exist");
        assert_eq!(ticket.status, Status::Finished);
    }

    #[tokio::test]
    async fn paused_interactive_ticket_emits_turn_started_exactly_once() {
        use crate::event::EventKind;

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_time(Duration::from_millis(500));
        let collected = collect_events(&tickets);
        tickets.agent(interactive_chatbot(&provider));
        tickets.task("hello");

        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("test did not finish within 5s");

        let events = collected.lock().unwrap().clone();
        let turn_started = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TurnStarted))
            .count();
        assert_eq!(
            turn_started, 1,
            "paused interactive ticket must not re-emit TurnStarted on every poll",
        );
    }

    #[tokio::test]
    async fn loop_releases_paused_context_when_new_todo_arrives() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("hi")),
            Ok(write_result_response("second-done")),
        ]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_time(Duration::from_millis(500));
        tickets.agent(interactive_chatbot(&provider));
        let first_key = tickets.task("first chat");
        tickets.start();

        let tickets_for_drive = Arc::clone(&tickets);
        let drive = async move {
            for _ in 0..200 {
                let paused = tickets_for_drive
                    .get_ticket(&first_key)
                    .and_then(|t| t.replies.last().map(|r| r.author == Author::Assistant))
                    .unwrap_or(false);
                if paused {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let second_key = tickets_for_drive.task("second chat");
            for _ in 0..400 {
                if tickets_for_drive
                    .get_ticket(&second_key)
                    .map(|t| t.status == Status::Finished)
                    .unwrap_or(false)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let first = tickets_for_drive.get_ticket(&first_key).unwrap();
            let second = tickets_for_drive.get_ticket(&second_key).unwrap();
            assert_eq!(
                first.status,
                Status::InProgress,
                "first chat remains paused; no caller replied",
            );
            assert_eq!(
                second.status,
                Status::Finished,
                "agent must release the paused first chat and claim the new Todo",
            );
        };

        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(tickets.finish(), drive);
        })
        .await
        .expect("test did not finish within 5s");
    }

    #[tokio::test]
    async fn loop_fails_ticket_when_silence_exceeds_schema_retry_budget() {
        use crate::event::{EventKind, PolicyKind};

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_schema_retries(1);
        let collected = collect_events(&tickets);
        tickets.agent(task_agent(&provider));
        tickets.task("go");

        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("test did not finish within 5s");

        let ticket = tickets
            .tickets()
            .into_iter()
            .next()
            .expect("ticket must exist");
        assert_eq!(ticket.status, Status::Failed);

        let events = collected.lock().unwrap().clone();
        let policy_violated = events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::PolicyViolated {
                    kind: PolicyKind::MaxSchemaRetries,
                    limit: 1,
                },
            )
        });
        assert!(policy_violated, "expected PolicyViolated MaxSchemaRetries");
        let ticket_failed = events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::TicketFailed));
        assert!(ticket_failed, "expected TicketFailed");
    }

    #[tokio::test]
    async fn loop_finishes_ticket_after_one_silence_and_recovery() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("hi")),
            Ok(write_result_response("done")),
        ]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_schema_retries(3);
        tickets.agent(task_agent(&provider));
        tickets.task("go");

        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("test did not finish within 5s");

        let ticket = tickets
            .tickets()
            .into_iter()
            .next()
            .expect("ticket must exist");
        assert_eq!(ticket.status, Status::Finished);
        assert_eq!(tickets.last_result(), Some(serde_json::json!("done")));
    }

    #[tokio::test]
    async fn silence_retry_emits_schema_retried_event_with_attempt_1() {
        use crate::event::EventKind;

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("hi")),
            Ok(write_result_response("done")),
        ]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_schema_retries(3);
        let collected = collect_events(&tickets);
        tickets.agent(task_agent(&provider));
        tickets.task("go");

        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
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
            vec![(1, super::RESUME_OR_FINISH_DETAIL.to_string())],
            "exactly one SchemaRetried at attempt 1 with the silence detail",
        );
    }

    #[tokio::test]
    async fn cancel_stops_a_running_workshop() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));

        tickets.start();
        tickets.cancel();

        tokio::time::timeout(Duration::from_secs(2), tickets.finish())
            .await
            .expect("run did not exit within 2s of cancel()");
    }

    #[tokio::test]
    async fn cancel_label_keeps_that_pool_off_the_queue_while_others_run() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));

        let analyst = MockProvider::with_results(vec![Ok(write_result_response("analyzed"))]);
        let researcher = MockProvider::with_results(vec![Ok(write_result_response("hunted"))]);
        tickets.agent(
            Agent::new()
                .name("analyst")
                .label("analysis")
                .provider(analyst as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .build(),
        );
        tickets.agent(
            Agent::new()
                .name("researcher")
                .label("research")
                .provider(Arc::clone(&researcher) as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .build(),
        );

        // Call off the research pool on the live run (start() resets signals), then
        // enqueue both tickets; the analysis pool runs on.
        tickets.start();
        tickets.cancel_label("research");
        tickets.ticket(Ticket::new("hunt").label("research"));
        tickets.ticket(Ticket::new("triage").label("analysis"));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let analyzed = tickets
                .tickets()
                .iter()
                .any(|t| t.status == Status::Finished && t.task.as_str() == Some("triage"));
            if analyzed {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                tickets.cancel();
                panic!("analysis ticket did not finish within 5s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let hunt = tickets
            .tickets()
            .into_iter()
            .find(|t| t.task.as_str() == Some("hunt"))
            .expect("research ticket exists");
        assert_eq!(
            hunt.status,
            Status::Todo,
            "a cancelled pool's ticket is never claimed",
        );
        assert_eq!(researcher.requests(), 0, "the researcher never ran");

        tickets.cancel();
        tokio::time::timeout(Duration::from_secs(2), tickets.finish())
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
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));
        tickets.agent(
            Agent::new()
                .name("agent")
                .provider(provider as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(ManageTicketsTool)
                .build(),
        );

        tickets.task("first");
        tickets.finish().await;
        assert_eq!(tickets.last_result(), Some(serde_json::json!("first")));

        tickets.task("second");
        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("second finish did not finish within 5s");
        assert_eq!(tickets.last_result(), Some(serde_json::json!("second")));
    }

    #[tokio::test]
    async fn agent_finish_forwards_to_bound_system() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(write_result_response("forwarded"))]);
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1));
        let agent = tickets.agent(
            Agent::new()
                .name("agent")
                .provider(provider as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(ManageTicketsTool)
                .build(),
        );

        agent.task("hello");
        let sys = tokio::time::timeout(Duration::from_secs(5), agent.finish())
            .await
            .expect("agent.finish did not finish within 5s");
        assert_eq!(sys.last_result(), Some(serde_json::json!("forwarded")));
    }

    // Cross-ticket memory

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
            .filter(|text| !text.starts_with("## Context\n\n"))
            .collect()
    }

    #[tokio::test]
    async fn messages_contain_only_the_current_tickets_task() {
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("ok")),
            Ok(write_result_response("ok")),
        ]);
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_schema_retries(10)
            .max_time(Duration::from_millis(500));
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(crate::tools::ManageTicketsTool)
                .build(),
        );
        tickets.task("first");
        tickets.task("second");
        let _ = tickets.finish().await;

        let calls = provider.received();
        assert_eq!(calls.len(), 2);
        assert_eq!(user_texts(&calls[0]), vec!["first".to_string()]);
        assert_eq!(user_texts(&calls[1]), vec!["second".to_string()]);
    }

    #[tokio::test]
    async fn model_writes_in_ticket_n_become_visible_in_ticket_n_plus_one_system_prompt() {
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

        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_time(Duration::from_millis(500));
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .knowledge(&store)
                .build(),
        );
        tickets.task("first");
        tickets.task("second");
        let _ = tickets.finish().await;

        let prompts = provider.received_system_prompts();
        assert_eq!(prompts.len(), 3);
        assert!(
            !prompts[0].contains("api-config"),
            "ticket 1 turn 1 sees an empty knowledge store: {:?}",
            prompts[0]
        );
        assert!(
            prompts[2].contains("## Knowledge"),
            "ticket 2 should render the knowledge section: {:?}",
            prompts[2]
        );
        assert!(
            prompts[2].contains("API runs on port 3000"),
            "ticket 2 should see ticket 1's write: {:?}",
            prompts[2]
        );
    }

    #[tokio::test]
    async fn system_prompt_does_not_change_after_mid_ticket_knowledge_write() {
        let provider = MockProvider::with_results(vec![
            Ok(knowledge_write_response(
                "mid-ticket",
                "Written mid-ticket",
                "# Mid\n\nContent.",
            )),
            Ok(write_result_response("ok")),
        ]);
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let knowledge_dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(knowledge_dir.path()).unwrap();

        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_time(Duration::from_millis(500));
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .knowledge(&store)
                .build(),
        );
        tickets.task("hi");
        let _ = tickets.finish().await;

        let prompts = provider.received_system_prompts();
        assert_eq!(prompts.len(), 2);
        assert_eq!(
            prompts[0], prompts[1],
            "mid-ticket knowledge write must not change the system prompt within the same ticket"
        );
        assert!(store.index().contains("mid-ticket"));
    }

    #[tokio::test]
    async fn agent_a_writes_in_one_ticket_then_agent_b_sees_it_in_its_next_ticket() {
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

        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_time(Duration::from_millis(500));

        tickets.agent(
            Agent::new()
                .name("alice")
                .label("a")
                .provider(p_a.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .knowledge(&store)
                .build(),
        );
        tickets.agent(
            Agent::new()
                .name("bob")
                .label("b")
                .provider(p_b.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .knowledge(&store)
                .build(),
        );

        tickets.ticket(Ticket::new("alice work").label("a"));
        let _ = tickets.finish().await;
        assert!(store.index().contains("alice-note"));

        tickets.ticket(Ticket::new("bob work").label("b"));
        let _ = tickets.finish().await;

        let bob_prompts = p_b.received_system_prompts();
        assert_eq!(bob_prompts.len(), 1, "bob processed exactly one ticket");
        assert!(
            bob_prompts[0].contains("Note from Alice"),
            "bob should see alice's write: {:?}",
            bob_prompts[0]
        );
    }

    #[tokio::test]
    async fn knowledge_write_then_read_across_tickets() {
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

        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_time(Duration::from_millis(500));
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .knowledge(&store)
                .build(),
        );
        tickets.task("first");
        tickets.task("second");
        let _ = tickets.finish().await;

        let prompts = provider.received_system_prompts();
        assert_eq!(prompts.len(), 4);

        assert!(
            !prompts[0].contains("## Knowledge"),
            "ticket 1 turn 1 should not have Knowledge section: {:?}",
            prompts[0]
        );
        assert_eq!(
            prompts[0], prompts[1],
            "ticket 1 turn 2 prompt must be byte-identical to turn 1"
        );
        assert_eq!(
            prompts[0], prompts[2],
            "ticket 1 turn 3 prompt must be byte-identical to turn 1"
        );

        assert!(
            prompts[3].contains("## Knowledge"),
            "ticket 2 should render the knowledge section: {:?}",
            prompts[3]
        );
        assert!(
            prompts[3].contains("api-config"),
            "ticket 2 should see the page slug: {:?}",
            prompts[3]
        );
        assert!(
            prompts[3].contains("API runs on port 3000"),
            "ticket 2 should see the index summary: {:?}",
            prompts[3]
        );
        assert!(
            !prompts[3].contains("Rate limit: 100 req/min"),
            "ticket 2 should NOT contain full page body: {:?}",
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
