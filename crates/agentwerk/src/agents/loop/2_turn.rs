//! Per-agent driver: claims tickets from the shared system and runs each turn through compaction, reply, and tool calls.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::agents::agent::Agent;
use crate::agents::compaction as algo;
use crate::agents::policy::Policies;
use crate::agents::tickets::{policy_violated_kind, Reply, Status, Ticket, TicketSystem};
use crate::event::{CompactReason, EventKind, PolicyKind};
use crate::providers::{AsUserMessage, Message, Model, RequestErrorKind};

use super::{reply, tool_call, Action, POLL_INTERVAL};

const RESUME_OR_FINISH_DETAIL: &str =
    "Your last reply contained no tool call. Call `finish_ticket` with your result if the work is complete, or another tool to continue.";

pub(super) struct LoopContext<'a> {
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

impl<'a> LoopContext<'a> {
    pub(super) fn new(
        agent: &'a Agent,
        model: &'a Model,
        ticket_system: &'a Arc<TicketSystem>,
        stop_signal: Arc<AtomicBool>,
        policies: Policies,
        ticket_key: String,
        system_prompt: String,
    ) -> Self {
        Self {
            agent,
            model,
            ticket_system,
            stop_signal,

            ticket_key,
            system_prompt,
            policies,

            consecutive_schema_failures: 0,
        }
    }

    fn emit(&self, kind: EventKind) {
        self.ticket_system
            .emit(&self.ticket_key, self.agent.get_name(), kind);
    }

    fn fail_with(&self, kind: RequestErrorKind, message: String) {
        self.emit(EventKind::RequestFailed { kind, message });
        self.emit(EventKind::TicketFailed {
            key: self.ticket_key.clone(),
        });
        let _ = self.ticket_system.set_failed(&self.ticket_key);
    }
}

pub(super) async fn start_turn<'a>(
    context: &mut Option<LoopContext<'a>>,
    agent: &'a Agent,
    ticket_system: &'a Arc<TicketSystem>,
) -> Action<Vec<Message>> {
    let stop_signal = Arc::clone(&ticket_system.stop_signal.lock().unwrap());

    if stop_signal.load(Ordering::Relaxed) {
        return Action::Stop;
    }
    let policies = ticket_system.policies();
    if let Some((kind, limit)) = policy_violated_kind(&policies, &ticket_system.stats) {
        ticket_system.emit(
            "",
            agent.get_name(),
            EventKind::PolicyViolated { kind, limit },
        );
        return Action::Stop;
    }

    if context.is_none() {
        fn next_ticket_key(ticket_system: &Arc<TicketSystem>, agent: &Agent) -> Option<String> {
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

            ticket_system
                .claim(claimable, agent.get_name())
                .or_else(|| ticket_system.find_ticket(resumable).map(|t| t.key.clone()))
        }
        let Some(ticket_key) = next_ticket_key(ticket_system, agent) else {
            tokio::time::sleep(POLL_INTERVAL).await;
            return Action::Replay;
        };
        let Some(ticket) = ticket_system.get_ticket(&ticket_key) else {
            return Action::Replay;
        };
        let knowledge_index = agent.knowledge().index();
        // Lets the model see what knowledge pages it can read.
        let system_prompt = agent.system_prompt(Some(&knowledge_index));
        let agent_name = agent.get_name();

        ticket_system.emit(&ticket_key, agent_name, EventKind::TurnStarted);

        if ticket.replies.is_empty() {
            ticket_system.add_reply(&ticket_key, Reply::system_text(system_prompt.clone()));
            if let Some(context_msg) =
                agent.context_message(&policies, &ticket_system.stats, Some(&ticket_key))
            {
                ticket_system.add_reply(&ticket_key, Reply::user_text(context_msg));
            }
            let Message::User {
                content: task_blocks,
            } = ticket.as_user_message()
            else {
                unreachable!("Ticket::as_user_message returns Message::User");
            };
            ticket_system.add_reply(&ticket_key, Reply::user(&task_blocks, &HashMap::new()));
            ticket_system.emit(
                &ticket_key,
                agent_name,
                EventKind::TicketStarted {
                    key: ticket_key.clone(),
                },
            );
        }

        let model = &agent.model;
        *context = Some(LoopContext::new(
            agent,
            model,
            ticket_system,
            stop_signal,
            policies,
            ticket_key,
            system_prompt,
        ));
    }

    let context_ref = context
        .as_mut()
        .expect("context is Some after the claim branch");
    let Some(ticket) = context_ref
        .ticket_system
        .get_ticket(&context_ref.ticket_key)
    else {
        *context = None;
        return Action::Replay;
    };
    // The pool was called off mid-flight: walk off, leaving the ticket InProgress
    // (abandoned), the same way an external cancel leaves in-flight tickets.
    if context_ref.ticket_system.labels_cancelled(&ticket.labels) {
        *context = None;
        return Action::Replay;
    }
    let status = match ticket.status {
        Status::Finished => Some(EventKind::TicketFinished {
            key: context_ref.ticket_key.clone(),
        }),
        Status::Failed => Some(EventKind::TicketFailed {
            key: context_ref.ticket_key.clone(),
        }),
        _ => None,
    };
    if let Some(kind) = status {
        context_ref.emit(kind);
        *context = None;
        return Action::Replay;
    }
    if !ticket.is_waiting_for_response() {
        if context_ref.agent.is_interactive() {
            return Action::Pause;
        }
        let max = context_ref.policies.max_schema_retries.unwrap_or(u32::MAX);
        context_ref.consecutive_schema_failures =
            context_ref.consecutive_schema_failures.saturating_add(1);
        if context_ref.consecutive_schema_failures >= max {
            context_ref.emit(EventKind::PolicyViolated {
                kind: PolicyKind::MaxSchemaRetries,
                limit: u64::from(max),
            });
            let _ = context_ref
                .ticket_system
                .set_failed(&context_ref.ticket_key);
            return Action::Replay;
        }
        context_ref.emit(EventKind::SchemaRetried {
            attempt: context_ref.consecutive_schema_failures,
            max_attempts: max,
            message: RESUME_OR_FINISH_DETAIL.to_string(),
        });
        context_ref.ticket_system.add_reply(
            &context_ref.ticket_key,
            Reply::user_text(crate::prompts::retry_directive(RESUME_OR_FINISH_DETAIL)),
        );
        return Action::Replay;
    }
    Action::Proceed(ticket.to_messages())
}

pub(super) async fn run_agent(agent: Agent) {
    let ticket_system = agent
        .ticket_system
        .upgrade()
        .expect("Agent's TicketSystem was dropped before run() finished");
    let mut context: Option<LoopContext<'_>> = None;

    'agent: loop {
        macro_rules! phase {
            ($e:expr) => {
                match $e {
                    Action::Proceed(v) => v,
                    Action::Replay => continue 'agent,
                    // Release the context so the next iteration re-runs next_ticket_key.
                    Action::Pause => {
                        context = None;
                        tokio::time::sleep(POLL_INTERVAL).await;
                        continue 'agent;
                    }
                    Action::Stop => return,
                }
            };
        }

        let messages = phase!(start_turn(&mut context, &agent, &ticket_system).await);
        let context_ref = context
            .as_mut()
            .expect("start_turn populates context on Proceed");

        let messages = phase!(proactive_compact(context_ref, messages).await);
        let reply_val = phase!(reply::run(context_ref, messages).await);
        phase!(tool_call::run(context_ref, reply_val).await);
    }
}

pub(super) async fn compact(context: &mut LoopContext<'_>, reason: CompactReason) -> Action<()> {
    let Some(ticket) = context.ticket_system.get_ticket(&context.ticket_key) else {
        return Action::Stop;
    };
    let window = context.model.context_window;
    let messages = ticket.to_messages();
    let chunks_total = algo::chunks_for_window(&messages, window).len() as u32;
    context.emit(EventKind::CompactionStarted {
        reason,
        chunks_total,
    });

    let on_progress: Arc<dyn Fn(u32, u32) + Send + Sync> = {
        let ticket_system = Arc::clone(context.ticket_system);
        let agent_name = context.agent.get_name().to_string();
        let ticket_key = context.ticket_key.clone();
        Arc::new(move |completed, total| {
            ticket_system.emit(
                &ticket_key,
                &agent_name,
                EventKind::CompactionProgress {
                    reason,
                    completed,
                    total,
                },
            );
        })
    };

    let applied = match algo::run(
        &context.agent.provider(),
        &context.model.name,
        messages,
        window,
        context.ticket_system,
        &context.ticket_key,
        on_progress,
    )
    .await
    {
        Ok(applied) => applied,
        Err(e) => {
            context.emit(EventKind::CompactionFailed {
                reason,
                message: e.to_string(),
            });
            context.fail_with(e.kind(), e.to_string());
            return Action::Stop;
        }
    };

    if !applied && matches!(reason, CompactReason::Reactive) {
        context.fail_with(
            RequestErrorKind::ContextWindowExceeded,
            "context still exceeds window after compaction".into(),
        );
        return Action::Stop;
    }

    context.emit(EventKind::CompactionFinished { reason });
    match reason {
        CompactReason::Proactive => Action::Proceed(()),
        CompactReason::Reactive => Action::Replay,
    }
}

pub(super) async fn proactive_compact(
    context: &mut LoopContext<'_>,
    mut messages: Vec<Message>,
) -> Action<Vec<Message>> {
    let tools = context.agent.tool_definitions();
    let window = context.model.context_window;
    let history = context
        .ticket_system
        .stats()
        .usage_history(&context.ticket_key);

    if algo::should_compact_proactively(window, &history, &messages, &context.system_prompt, &tools)
    {
        match compact(context, CompactReason::Proactive).await {
            Action::Stop => return Action::Stop,
            Action::Replay => return Action::Replay,
            Action::Pause => unreachable!("compact() never returns Pause"),
            Action::Proceed(()) => {}
        }
        messages = context
            .ticket_system
            .get_ticket(&context.ticket_key)
            .map(|t| t.to_messages())
            .unwrap_or_default();
    }

    Action::Proceed(messages)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::agents::agent::Agent;
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tickets::{Status, TicketSystem};
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
        assert_eq!(tickets.last_result().as_deref(), Some("b-done"));
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
        tickets.task_labeled("a", "alice");

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
                    .first_ticket()
                    .and_then(|t| t.replies.last().map(|r| r.author == "assistant"))
                    .unwrap_or(false);
                if last_is_assistant {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let ticket = tickets_for_inject
                .first_ticket()
                .expect("ticket must exist");
            assert_eq!(ticket.status, Status::InProgress);
            assert_eq!(
                ticket.replies.last().map(|r| r.author.clone()),
                Some("assistant".into()),
                "gate must pause on the assistant text reply",
            );
            tickets_for_inject.reply(&key, "what next?");
        };

        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(tickets.finish(), inject);
        })
        .await
        .expect("test did not finish within 5s");

        let ticket = tickets.first_ticket().expect("ticket must exist");
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
                    .and_then(|t| t.replies.last().map(|r| r.author == "assistant"))
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

        let ticket = tickets.first_ticket().expect("ticket must exist");
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
            .any(|e| matches!(&e.kind, EventKind::TicketFailed { .. }));
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

        let ticket = tickets.first_ticket().expect("ticket must exist");
        assert_eq!(ticket.status, Status::Finished);
        assert_eq!(tickets.last_result().as_deref(), Some("done"));
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
        tickets.task_labeled("hunt", "research");
        tickets.task_labeled("triage", "analysis");

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
        assert_eq!(tickets.last_result().as_deref(), Some("first"));

        tickets.task("second");
        tokio::time::timeout(Duration::from_secs(5), tickets.finish())
            .await
            .expect("second finish did not finish within 5s");
        assert_eq!(tickets.last_result().as_deref(), Some("second"));
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
        assert_eq!(sys.last_result().as_deref(), Some("forwarded"));
    }

    // Tool result offloading

    #[tokio::test]
    async fn huge_tool_result_is_persisted_to_ticket_outputs_dir_and_ticket_finishes_done() {
        use crate::agents::tickets::ReplyContent;
        use crate::tools::{Tool, ToolResult};

        let provider = MockProvider::with_results(vec![
            Ok(crate::providers::types::ModelResponse {
                content: vec![crate::providers::ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "dump".into(),
                    input: serde_json::json!({}),
                }],
                status: crate::providers::types::ResponseStatus::ToolUse,
                usage: crate::providers::types::TokenUsage::default(),
                model: "mock".into(),
            }),
            Ok(write_result_response("done")),
        ]);

        let collected: Arc<std::sync::Mutex<Vec<crate::event::Event>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler: Arc<dyn Fn(crate::event::Event) + Send + Sync> = {
            let c = Arc::clone(&collected);
            Arc::new(move |e| c.lock().unwrap().push(e))
        };

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_schema_retries(10)
            .max_time(Duration::from_millis(500));

        let dump = Tool::new("dump", "Returns ~800 KB of text")
            .handler(|_input, _ctx| async move { Ok(ToolResult::success("x".repeat(800_000))) })
            .build();

        tickets.on_event(move |e| handler(e));
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider.clone() as Arc<dyn Provider>)
                .model("claude-sonnet-4-20250514")
                .role("test")
                .context("static")
                .tool(dump)
                .build(),
        );
        tickets.task("go");

        let _ = tickets.finish().await;
        let events = collected.lock().unwrap().clone();
        let ticket = tickets.first_ticket().expect("ticket must exist");

        assert_eq!(provider.requests(), 2);
        assert!(failures_in(&events).is_empty());
        assert_eq!(ticket.status, Status::Finished);

        let relative_path: std::path::PathBuf = ["tickets", "TICKET-1", "outputs", "call-1.txt"]
            .iter()
            .collect();
        let output_path = results_dir.path().join(&relative_path);
        let body = std::fs::read_to_string(&output_path).expect("offload file must exist");
        assert_eq!(body, "x".repeat(800_000));

        let tool_result_path = ticket.replies.iter().find_map(|r| {
            r.content.iter().find_map(|b| match b {
                ReplyContent::ToolResult { id, path, .. } if id == "call-1" => path.clone(),
                _ => None,
            })
        });
        assert_eq!(tool_result_path.as_deref(), Some(relative_path.as_path()));

        let stub_visible = provider.received()[1].iter().any(|m| match m {
            crate::providers::Message::User { content } => content.iter().any(|b| match b {
                crate::providers::ContentBlock::ToolResult { content, .. } => {
                    content.contains("<persisted-output>")
                        && content.contains("Full output saved to:")
                        && content.contains(output_path.to_string_lossy().as_ref())
                }
                _ => false,
            }),
            _ => false,
        });
        assert!(stub_visible);
    }

    #[tokio::test]
    async fn parallel_moderate_results_aggregate_offloads_largest_first() {
        use crate::tools::{Tool, ToolResult};

        let provider = MockProvider::with_results(vec![
            Ok(crate::providers::types::ModelResponse {
                content: vec![
                    crate::providers::ContentBlock::ToolUse {
                        id: "c1".into(),
                        name: "size_tool".into(),
                        input: serde_json::json!({"bytes": 48_000}),
                    },
                    crate::providers::ContentBlock::ToolUse {
                        id: "c2".into(),
                        name: "size_tool".into(),
                        input: serde_json::json!({"bytes": 47_000}),
                    },
                    crate::providers::ContentBlock::ToolUse {
                        id: "c3".into(),
                        name: "size_tool".into(),
                        input: serde_json::json!({"bytes": 46_000}),
                    },
                    crate::providers::ContentBlock::ToolUse {
                        id: "c4".into(),
                        name: "size_tool".into(),
                        input: serde_json::json!({"bytes": 45_000}),
                    },
                    crate::providers::ContentBlock::ToolUse {
                        id: "c5".into(),
                        name: "size_tool".into(),
                        input: serde_json::json!({"bytes": 44_000}),
                    },
                ],
                status: crate::providers::types::ResponseStatus::ToolUse,
                usage: crate::providers::types::TokenUsage::default(),
                model: "mock".into(),
            }),
            Ok(write_result_response("done")),
        ]);

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_schema_retries(10)
            .max_time(Duration::from_millis(500));

        let size_tool = Tool::new("size_tool", "Returns N bytes of 'x'")
            .schema(serde_json::json!({
                "type": "object",
                "properties": {"bytes": {"type": "integer"}},
                "required": ["bytes"],
            }))
            .read_only(true)
            .handler(|input, _ctx| async move {
                let bytes = input["bytes"].as_u64().unwrap_or(0) as usize;
                Ok(ToolResult::success("x".repeat(bytes)))
            })
            .build();

        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(size_tool)
                .build(),
        );
        tickets.task("go");

        let _ = tickets.finish().await;
        let ticket = tickets.first_ticket().expect("ticket must exist");
        assert_eq!(ticket.status, Status::Finished);

        let second = &provider.received()[1];
        let tool_results: Vec<&String> = second
            .iter()
            .flat_map(|m| match m {
                crate::providers::Message::User { content } => content
                    .iter()
                    .filter_map(|b| match b {
                        crate::providers::ContentBlock::ToolResult { content, .. } => Some(content),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        let stub_count = tool_results
            .iter()
            .filter(|c| c.starts_with("<persisted-output>"))
            .count();
        assert_eq!(stub_count, 1);

        let stub = tool_results
            .iter()
            .find(|c| c.starts_with("<persisted-output>"))
            .expect("stub must be present");
        let expected_path = results_dir
            .path()
            .join("tickets")
            .join("TICKET-1")
            .join("outputs")
            .join("c1.txt");
        assert!(stub.contains(expected_path.to_string_lossy().as_ref()));

        let body = std::fs::read_to_string(&expected_path).unwrap();
        assert_eq!(body, "x".repeat(48_000));
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

        tickets.task_labeled("alice work", "a");
        let _ = tickets.finish().await;
        assert!(store.index().contains("alice-note"));

        tickets.task_labeled("bob work", "b");
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

    // Compaction

    fn compaction_starts(
        events: &[crate::event::Event],
        expected: crate::event::CompactReason,
    ) -> usize {
        events
            .iter()
            .filter(|e| match &e.kind {
                crate::event::EventKind::CompactionStarted { reason, .. } => *reason == expected,
                _ => false,
            })
            .count()
    }

    fn compaction_finishes(
        events: &[crate::event::Event],
        expected: crate::event::CompactReason,
    ) -> usize {
        events
            .iter()
            .filter(|e| match &e.kind {
                crate::event::EventKind::CompactionFinished { reason } => *reason == expected,
                _ => false,
            })
            .count()
    }

    #[tokio::test]
    async fn first_overflow_attempts_compaction_before_request_failed() {
        let provider = MockProvider::with_results(vec![
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "prompt is 250000 tokens, exceeds 200000".into(),
            }),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
        ]);
        let (events, _, _) = run_one(provider, 0, 10, None).await;

        let started_idx = events
            .iter()
            .position(|e| matches!(&e.kind, crate::event::EventKind::CompactionStarted { .. }))
            .expect("compaction must have started");
        let finished_idx = events
            .iter()
            .position(|e| matches!(&e.kind, crate::event::EventKind::CompactionFinished { .. }))
            .expect("compaction must have finished");
        let request_failed_idx = events
            .iter()
            .position(|e| matches!(&e.kind, crate::event::EventKind::RequestFailed { .. }))
            .expect("the ticket must surface a request failure");
        assert!(started_idx < finished_idx);
        assert!(finished_idx < request_failed_idx);
    }

    #[tokio::test]
    async fn reactive_overflow_compacts_then_succeeds() {
        use crate::event::CompactReason;
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("primer")),
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "exceeded".into(),
            }),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("ok")),
        ]);
        let (events, provider, ticket) = run_one(provider, 0, 10, Some(string_schema())).await;

        assert_eq!(provider.requests(), 4);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(ticket.status, Status::Finished);

        let fourth = &provider.received()[3];
        assert_eq!(user_texts_filter(fourth), vec!["SUMMARY".to_string()]);
    }

    #[tokio::test]
    async fn reactive_overflow_recovers_with_token_arithmetic_message() {
        use crate::event::CompactReason;
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("primer")),
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "input length plus reserved output tokens exceeds the context limit"
                    .into(),
            }),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("ok")),
        ]);
        let (events, provider, ticket) = run_one(provider, 0, 10, Some(string_schema())).await;

        assert_eq!(provider.requests(), 4);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(ticket.status, Status::Finished);
    }

    #[tokio::test]
    async fn reactive_overflow_recovers_with_context_capacity_message() {
        use crate::event::CompactReason;
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("primer")),
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "request token count exceeds the available context size".into(),
            }),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("ok")),
        ]);
        let (events, provider, ticket) = run_one(provider, 0, 10, Some(string_schema())).await;

        assert_eq!(provider.requests(), 4);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(ticket.status, Status::Finished);
    }

    #[tokio::test]
    async fn oversized_single_user_message_recovers_via_chunked_summarization() {
        use crate::event::CompactReason;

        let provider = MockProvider::with_results(vec![
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "prompt token count exceeds context window".into(),
            }),
            Ok(text_response_with_usage(
                "PART_A",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(text_response_with_usage(
                "PART_B",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("ok")),
        ]);
        let (events, provider, ticket) =
            run_with_context_window(provider, 10_000, "x\n".repeat(25_000)).await;

        assert_eq!(provider.requests(), 4);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(ticket.status, Status::Finished);
    }

    #[tokio::test]
    async fn compaction_terminal_failure_transitions_ticket_to_failed() {
        let provider = MockProvider::with_results(vec![Err(
            crate::providers::ProviderError::ContextWindowExceeded {
                message: "overflow".into(),
            },
        )]);
        let (events, _, ticket) =
            run_with_context_window(provider, 10_000, "x\n".repeat(25_000)).await;

        assert_eq!(
            ticket.status,
            Status::Failed,
            "terminal compaction failure must transition the ticket to Failed",
        );
        let ticket_failed_count = events
            .iter()
            .filter(|e| matches!(&e.kind, crate::event::EventKind::TicketFailed { .. }))
            .count();
        assert_eq!(ticket_failed_count, 1);
    }

    #[tokio::test]
    async fn still_oversized_after_compaction_transitions_ticket_to_failed() {
        let provider = MockProvider::with_results(vec![Ok(text_response_with_usage(
            "SUMMARY",
            crate::providers::types::TokenUsage::default(),
        ))]);
        let (events, _, ticket) = run_with_context_window(provider, 1_000, "hi").await;

        assert_eq!(
            ticket.status,
            Status::Failed,
            "post-compaction window check must transition the ticket to Failed",
        );
        let ticket_failed_count = events
            .iter()
            .filter(|e| matches!(&e.kind, crate::event::EventKind::TicketFailed { .. }))
            .count();
        assert_eq!(ticket_failed_count, 1);
    }

    #[tokio::test]
    async fn reactive_overflow_twice_in_a_row_fails_the_ticket() {
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("primer")),
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "first overflow".into(),
            }),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "second overflow".into(),
            }),
        ]);
        let (events, _, ticket) = run_one(provider, 0, 10, Some(string_schema())).await;

        assert_eq!(
            compaction_finishes(&events, crate::event::CompactReason::Reactive),
            1
        );
        let failures = failures_in(&events);
        assert!(!failures.is_empty());
        assert_eq!(ticket.status, Status::Failed);
    }

    #[tokio::test]
    async fn proactive_threshold_triggers_compaction_before_next_request() {
        use crate::event::CompactReason;
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response_with_usage(
                "primer",
                crate::providers::types::TokenUsage {
                    input_tokens: 170_000,
                    output_tokens: 0,
                },
            )),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("done")),
        ]);
        let (events, provider, ticket) = run_compaction(provider).await;

        assert_eq!(provider.requests(), 3);
        assert_eq!(compaction_starts(&events, CompactReason::Proactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Proactive), 1);
        assert_eq!(ticket.status, Status::Finished);

        let third = &provider.received()[2];
        assert_eq!(third.len(), 1);
        match &third[0] {
            crate::providers::Message::User { content } => match &content[0] {
                crate::providers::ContentBlock::Text { text } => assert_eq!(text, "SUMMARY"),
                other => panic!("expected text summary, got {other:?}"),
            },
            other => panic!("expected user message, got {other:?}"),
        }

        let started_idx = events
            .iter()
            .position(|e| matches!(&e.kind, crate::event::EventKind::CompactionStarted { .. }))
            .expect("compaction must start");
        let finished_idx = events
            .iter()
            .position(|e| matches!(&e.kind, crate::event::EventKind::CompactionFinished { .. }))
            .expect("compaction must finish");
        let request_started: Vec<usize> = events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                matches!(&e.kind, crate::event::EventKind::RequestStarted { .. }).then_some(i)
            })
            .collect();
        assert!(request_started.len() >= 2);
        assert!(started_idx > request_started[0] && started_idx < request_started[1]);
        assert!(finished_idx > started_idx && finished_idx < request_started[1]);
    }

    #[tokio::test]
    async fn summarize_rate_limited_kills_ticket_without_retry() {
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response_with_usage(
                "primer",
                crate::providers::types::TokenUsage {
                    input_tokens: 170_000,
                    output_tokens: 0,
                },
            )),
            Err(rate_limit()),
        ]);
        let (events, _, _) = run_compaction(provider).await;

        assert_eq!(
            compaction_starts(&events, crate::event::CompactReason::Proactive),
            1
        );
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            crate::event::EventKind::CompactionFailed {
                reason: crate::event::CompactReason::Proactive,
                message,
            } if message.contains("rate limited"),
        )),);
        assert!(retries_in(&events).is_empty());
        let failures = failures_in(&events);
        assert!(!failures.is_empty());
        assert!(failures[0].contains("rate limited"));
    }

    #[tokio::test]
    async fn summary_empty_text_replaces_tail_with_empty_user_message() {
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response_with_usage(
                "primer",
                crate::providers::types::TokenUsage {
                    input_tokens: 170_000,
                    output_tokens: 0,
                },
            )),
            Ok(text_response_with_usage(
                "",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("done")),
        ]);
        let (events, provider, ticket) = run_compaction(provider).await;

        assert_eq!(
            compaction_starts(&events, crate::event::CompactReason::Proactive),
            1
        );
        assert_eq!(
            compaction_finishes(&events, crate::event::CompactReason::Proactive),
            1
        );
        assert_eq!(ticket.status, Status::Finished);

        let third = &provider.received()[2];
        assert_eq!(third.len(), 1);
        match &third[0] {
            crate::providers::Message::User { content } => match &content[0] {
                crate::providers::ContentBlock::Text { text } => assert_eq!(text, ""),
                other => panic!("expected empty text block, got {other:?}"),
            },
            other => panic!("expected user message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn response_status_context_window_exceeded_triggers_reactive_compaction() {
        use crate::providers::types::ModelResponse;
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("primer")),
            Ok(ModelResponse {
                content: vec![crate::providers::ContentBlock::Text {
                    text: "oops".into(),
                }],
                status: crate::providers::types::ResponseStatus::ContextWindowExceeded,
                usage: crate::providers::types::TokenUsage::default(),
                model: "mock".into(),
            }),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("recovered")),
        ]);
        let (events, _, ticket) = run_compaction(provider).await;

        assert_eq!(
            compaction_starts(&events, crate::event::CompactReason::Reactive),
            1
        );
        assert_eq!(
            compaction_finishes(&events, crate::event::CompactReason::Reactive),
            1
        );
        assert_eq!(ticket.status, Status::Finished);
    }

    #[tokio::test]
    async fn proactive_compact_does_not_consume_reactive_budget() {
        use crate::event::CompactReason;
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response_with_usage(
                "primer",
                crate::providers::types::TokenUsage {
                    input_tokens: 170_000,
                    output_tokens: 0,
                },
            )),
            Ok(text_response_with_usage(
                "SUMMARY-A",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(tool_call_response("primer")),
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "main request overflow after proactive".into(),
            }),
            Ok(text_response_with_usage(
                "SUMMARY-B",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("done")),
        ]);
        let (events, provider, ticket) = run_compaction(provider).await;

        assert_eq!(provider.requests(), 6);
        assert_eq!(compaction_starts(&events, CompactReason::Proactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Proactive), 1);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(ticket.status, Status::Finished);
    }

    // Blocking limit

    fn string_schema() -> crate::schemas::Schema {
        crate::schemas::Schema::parse(serde_json::json!({"type": "string"})).expect("valid schema")
    }

    fn user_texts_filter(messages: &[crate::providers::Message]) -> Vec<String> {
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
}
