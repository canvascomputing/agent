//! Drives one agent: it claims a ticket, then works it through requests, tool
//! calls, and summarizing until the ticket is resolved.

use std::collections::HashMap;
use std::sync::Arc;

use crate::agents::agent::Agent;
use crate::agents::policy::Policy;
use crate::agents::tickets::{policy_violated, Reply, Run, Status, Ticket, TicketQueue};
use crate::event::{CompactReason, Event, EventKind, PolicyViolation};
use crate::prompts::directives::{NO_TOOL_CALLED, REPLY_REJECTED};
use crate::providers::{AsUserMessage, Message, Model, RequestErrorKind};
use crate::tools::{FinishTool, ToolRegistry};

use super::{compact, request, tool_call, Step, POLL_INTERVAL};

pub(super) struct TicketContext<'a> {
    pub(super) agent: &'a Agent,
    pub(super) model: &'a Model,
    pub(super) ticket_queue: &'a Arc<TicketQueue>,
    pub(super) run: Arc<Run>,

    pub(super) ticket_key: String,
    pub(super) system_prompt: String,
    pub(super) policy: Policy,

    pub(super) tools: ToolRegistry,

    // Spans turns; trips max_schema_retries.
    pub(super) consecutive_schema_failures: u32,
}

impl<'a> TicketContext<'a> {
    pub(super) fn emit(&self, kind: EventKind) -> Event {
        self.ticket_queue
            .emit(&self.ticket_key, self.agent.id(), kind)
    }

    pub(super) fn ticket(&self) -> Option<Ticket> {
        self.ticket_queue.get_ticket(&self.ticket_key)
    }

    /// The corrective directive to inject for `detail`. Everything the
    /// `SchemaRetried` this retry emitted carries is bound alongside it, so a
    /// replacement can read how far into the budget this is and who it
    /// addresses without reaching for an event.
    pub(super) fn retry_directive(&self, detail: &str, attempt: u32, max_attempts: u32) -> String {
        self.agent.directives().render(
            REPLY_REJECTED,
            &[
                ("detail", detail),
                ("attempt", &attempt.to_string()),
                ("max_attempts", &max_attempts.to_string()),
                ("ticket", &self.ticket_key),
                ("agent", self.agent.id()),
            ],
        )
    }

    /// Fail the ticket without naming a cause. The caller has already emitted
    /// the event that does.
    pub(super) fn fail_ticket(&self) {
        let _ = self
            .ticket_queue
            .set_failed_by(&self.ticket_key, self.agent.id());
    }

    /// Fail the ticket because a request did not come back. Reserved for the
    /// request path, so `RequestFailed` never reports a request that was
    /// never made.
    pub(super) fn fail_with(&self, reason: RequestErrorKind, message: String) {
        self.emit(EventKind::RequestFailed {
            model: self.model.name.clone(),
            reason,
            message,
        });
        self.fail_ticket();
    }
}

pub(super) async fn run_agent(agent: Agent) {
    let ticket_queue = agent
        .ticket_queue
        .upgrade()
        .expect("Agent's TicketQueue was dropped before run() finished");

    loop {
        if run_is_over(&agent, &ticket_queue) {
            return;
        }
        let Some(mut context) = claim(&agent, &ticket_queue) else {
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
fn run_is_over(agent: &Agent, ticket_queue: &TicketQueue) -> bool {
    if !ticket_queue.run.is_working() {
        return true;
    }
    let policy = ticket_queue.get_policy();
    if let Some((violation, limit)) = policy_violated(&policy, &ticket_queue.stats) {
        ticket_queue.emit(
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

/// Claim a `Todo` ticket for this agent, or resume one of its `InProgress`
/// tickets; write the first message when there is none.
fn claim<'a>(agent: &'a Agent, ticket_queue: &'a Arc<TicketQueue>) -> Option<TicketContext<'a>> {
    let claimable = |t: &Ticket| {
        t.status == Status::Todo
            && agent.handles(t.label.as_deref())
            && !ticket_queue.is_cancelled(t)
    };
    // On the id, not the label: agents sharing a label must not take over each
    // other's started tickets.
    let resumable = |t: &Ticket| {
        t.status == Status::InProgress
            && t.assignee.as_deref() == Some(agent.id())
            && (t.is_waiting_for_response() || !agent.is_interactive())
            && !ticket_queue.is_cancelled(t)
    };
    let ticket_key = ticket_queue
        .claim(claimable, agent.id())
        .or_else(|| ticket_queue.find_ticket(resumable).map(|t| t.key.clone()))?;
    let ticket = ticket_queue.get_ticket(&ticket_key)?;

    let mut tools = agent.tool_registry().clone();
    // Rebinding, not registering: an interactive agent carries no `finish`
    // unless it asked for one, and this must not hand it back.
    if tools.contains(FinishTool::NAME) {
        tools.register(FinishTool::from_schema(ticket.schema.clone()));
    }

    let knowledge_index = agent.knowledge().index();
    let policy = ticket_queue.get_policy();
    // Lets the model see what knowledge pages it can read.
    let system_prompt = agent.system_prompt(
        Some(&knowledge_index),
        &policy,
        &ticket_queue.stats,
        &ticket_key,
    );
    let agent_id = agent.id();

    ticket_queue.emit(&ticket_key, agent_id, EventKind::TurnStarted);

    if ticket.replies.is_empty() {
        ticket_queue.add_reply(&ticket_key, Reply::system_text(system_prompt.clone()));
        let Message::User {
            content: task_blocks,
        } = ticket.as_user_message()
        else {
            unreachable!("Ticket::as_user_message returns Message::User");
        };
        ticket_queue.add_reply(&ticket_key, Reply::user(&task_blocks, &HashMap::new()));
    }

    Some(TicketContext {
        agent,
        model: &agent.model,
        ticket_queue,
        run: Arc::clone(&ticket_queue.run),

        ticket_key,
        system_prompt,
        policy,
        tools,

        consecutive_schema_failures: 0,
    })
}

/// Re-read the ticket and decide the next step.
fn evaluate(context: &mut TicketContext<'_>) -> Option<Step> {
    // The pure check, not `run_is_over`: the outer loop emits the
    // `PolicyViolated` a moment later, and emitting it twice would double-count.
    if !context.ticket_queue.run.is_working()
        || policy_violated(&context.policy, &context.ticket_queue.stats).is_some()
    {
        return None;
    }
    let Some(ticket) = context.ticket() else {
        return None;
    };
    if context.ticket_queue.is_cancelled(&ticket) {
        return None;
    }
    // The transition itself already emitted the terminal event; the agent
    // only moves on to fresh work.
    if !ticket.is_pending() {
        return None;
    }
    if !ticket.is_waiting_for_response() {
        if context.agent.is_interactive() {
            // Pause until a caller reply lands; the resume claim re-checks.
            return None;
        }
        return silence_retry(context);
    }
    if compact::proactive_compaction_needed(context, &ticket) {
        return Some(Step::Compact(CompactReason::Proactive));
    }
    Some(Step::Request)
}

/// The model replied without a tool call: prompt it to resume or finish,
/// counting the silence toward the schema-retry budget.
fn silence_retry(context: &mut TicketContext<'_>) -> Option<Step> {
    let max = context.policy.max_schema_retries.unwrap_or(u32::MAX);
    context.consecutive_schema_failures = context.consecutive_schema_failures.saturating_add(1);
    if context.consecutive_schema_failures >= max {
        context.emit(EventKind::PolicyViolated {
            policy: PolicyViolation::MaxSchemaRetries,
            limit: u64::from(max),
        });
        let _ = context
            .ticket_queue
            .set_failed_by(&context.ticket_key, context.agent.id());
        return None;
    }
    let detail = context.agent.directives().render(NO_TOOL_CALLED, &[]);
    let attempt = context.consecutive_schema_failures;
    context.emit(EventKind::SchemaRetried {
        attempt,
        max_attempts: max,
        message: detail.clone(),
    });
    context.ticket_queue.add_reply(
        &context.ticket_key,
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
    use crate::agents::tickets::{Author, Status, Ticket, TicketQueue};
    use crate::agents::Knowledge;
    use crate::tools::{FinishTool, TicketsTool};

    // Run lifecycle

    #[tokio::test]
    async fn finish_drains_late_added_tickets() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("a-done")),
            Ok(write_result_response("b-done")),
        ]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        tickets.agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TicketsTool)
                .build(),
        );

        tickets.start();

        tickets.ticket("a");
        tickets.ticket("b");

        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(tickets.results().len(), 2);
        assert_eq!(tickets.results().pop(), Some(serde_json::json!("b-done")));
    }

    // retry directive

    #[tokio::test]
    async fn a_replacement_replaces_the_silence_directive() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("just thinking, no tool call")),
            Ok(write_result_response("done")),
        ]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        tickets.agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .directives(|_| Some("PLEASE CALL A TOOL NOW"))
                .build(),
        );

        tickets.start();
        tickets.ticket("go");
        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
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
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        tickets.agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .directives(|_| Some("attempt {attempt} of {max_attempts}"))
                .build(),
        );

        tickets.start();
        tickets.ticket("go");
        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
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
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        // An id is `<label>-<n>`, so the two agents read their own name back.
        fn addressed(_: &str) -> Option<&'static str> {
            Some("{agent}, CALL A TOOL")
        }
        tickets.agent(
            Agent::new()
                .label("scout")
                .provider(scout.clone())
                .model("mock")
                .role("test")
                .directives(addressed)
                .build(),
        );
        tickets.agent(
            Agent::new()
                .label("worker")
                .provider(worker.clone())
                .model("mock")
                .role("test")
                .directives(addressed)
                .build(),
        );

        tickets.start();
        tickets.ticket(Ticket::new("go").label("scout"));
        tickets.ticket(Ticket::new("go").label("worker"));
        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
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
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        tickets.agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .build(),
        );

        tickets.start();
        tickets.ticket("go");
        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
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
        // alice hands TICKET-1 off to bob. The handover inserts the child
        // before finishing the parent, so finish() must not observe an
        // empty queue in between and drain the chain early.
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(handover_response("bob", "continue", "alice-done")),
            Ok(write_result_response("bob-done")),
        ]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        for label in ["alice", "bob"] {
            tickets.agent(
                Agent::new()
                    .label(label)
                    .provider(provider.clone())
                    .model("mock")
                    .role("test")
                    .tool(FinishTool)
                    .build(),
            );
        }

        tickets.start();
        tickets.ticket(Ticket::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(tickets.results().len(), 2);
        assert_eq!(
            tickets.get_ticket("TICKET-2").unwrap().status,
            Status::Finished
        );
    }

    #[tokio::test]
    async fn finish_waits_through_an_on_result_follow_up() {
        // The handler mints a follow-up when TICKET-1 finishes. finish()
        // must not drain in the window between the finish transition and
        // the handler's insert.
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("first-done")),
            Ok(write_result_response("follow-up-done")),
        ]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        tickets.on_result(|queue, done, _| {
            if done.key == "TICKET-1" {
                queue.ticket(Ticket::new("follow up").label("alice"));
            }
        });
        tickets.agent(
            Agent::new()
                .label("alice")
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .build(),
        );

        tickets.start();
        tickets.ticket(Ticket::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(tickets.results().len(), 2);
        assert_eq!(
            tickets.get_ticket("TICKET-2").unwrap().status,
            Status::Finished
        );
    }

    #[tokio::test]
    async fn a_hook_files_the_report_once_aql_finds_both_scans_finished() {
        // Two scans run side by side on their own agents. The hook selects the
        // finished ones with AQL after each result and, once both are in,
        // writes their results into the ticket that reports on them.
        use std::sync::atomic::{AtomicBool, Ordering};

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("clean")),
            Ok(write_result_response("clean")),
            Ok(write_result_response("report-done")),
        ]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        // Both scans can finish at once, so the first handler to see the pair
        // takes the flag and the other returns: without it the report is filed
        // twice.
        let filed = Arc::new(AtomicBool::new(false));
        tickets.on_result(move |queue, done, _| {
            if !done.has_label("scan") {
                return;
            }
            let scans = queue.find_results("label = scan AND status = Finished");
            if scans.len() < 2 || filed.swap(true, Ordering::SeqCst) {
                return;
            }
            let verdicts: Vec<String> = scans.iter().map(|scan| scan.to_string()).collect();
            queue.ticket(Ticket::labeled(
                "report",
                format!("Write the report from {}.", verdicts.join(" and ")),
            ));
        });

        for label in ["scan", "scan", "report"] {
            tickets.agent(
                Agent::new()
                    .label(label)
                    .provider(provider.clone())
                    .model("mock")
                    .role("test")
                    .build(),
            );
        }

        tickets.start();
        tickets.ticket(Ticket::labeled("scan", "scan a.py"));
        tickets.ticket(Ticket::labeled("scan", "scan b.py"));

        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
            .await
            .expect("finish did not finish within 5s");

        let report = tickets.find_ticket("label = report").unwrap();
        assert_eq!(
            report.task,
            serde_json::json!("Write the report from \"clean\" and \"clean\".")
        );
        assert_eq!(report.status, Status::Finished);
        assert_eq!(
            tickets.find_results("label = report"),
            vec![serde_json::json!("report-done")]
        );
    }

    #[tokio::test]
    async fn ticket_finished_event_fires_exactly_once_per_ticket() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(write_result_response("done"))]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        let finished_events = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&finished_events);
        tickets.on_event(move |_, e| {
            if matches!(e.kind, crate::event::EventKind::TicketFinished) {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        tickets.agent(
            Agent::new()
                .label("alice")
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .build(),
        );

        tickets.start();
        tickets.ticket(Ticket::new("a").label("alice"));

        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
            .await
            .expect("finish did not finish within 5s");

        assert_eq!(finished_events.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn interactive_pause_holds_when_an_event_handler_edits_replies() {
        // A handler that edits on every event must not perturb the interactive
        // gate: the ticket pauses on the assistant text reply, and no second
        // request fires. Exhausting the single mock response would fail the
        // ticket, so `requests == 1` and `InProgress` prove the gate held.
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        // Unfiltered on purpose: it also fires for the run-level events, whose
        // empty key names no ticket.
        tickets.on_event(|queue, event| {
            queue.edit_replies(&event.ticket_key, |_replies| {});
        });
        tickets.agent(interactive_chatbot(&provider));
        tickets.ticket("hello");
        tickets.start();

        for _ in 0..200 {
            let last_is_assistant = tickets
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
        // Give the loop room to (wrongly) fire another request if buggy.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let ticket = tickets.tickets().into_iter().next().unwrap();
        assert_eq!(ticket.status, Status::InProgress);
        assert_eq!(
            ticket.replies.last().map(|r| r.author),
            Some(Author::Assistant),
        );
        assert_eq!(
            provider.requests(),
            1,
            "an edit must not trigger a re-request"
        );

        tickets.cancel_all();
        tickets.finish_all().await;
    }

    #[tokio::test]
    async fn loop_pauses_after_text_reply_then_resumes_when_caller_replies() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider =
            MockProvider::with_results(vec![Ok(text_response("hi")), Ok(text_response("and now"))]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        tickets.agent(interactive_chatbot(&provider));
        let key = tickets.ticket("hello");

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
            tickets.start();
            // The pause is not the end of the ticket, so the reply lands first
            // and the finish then waits out the turn it sets off.
            inject.await;
            tickets.finish_all().await;
        })
        .await
        .expect("test did not finish within 5s");

        let ticket = tickets
            .tickets()
            .into_iter()
            .next()
            .expect("ticket must exist");
        // The reply is what proves the resume: an interactive agent has no
        // `finish`, so the ticket pauses again instead of ending.
        assert_eq!(provider.requests(), 2, "the caller's reply drove a turn");
        assert_eq!(ticket.status, Status::InProgress);
        assert_eq!(
            ticket.replies.last().map(|r| r.author),
            Some(Author::Assistant),
        );
    }

    #[tokio::test]
    async fn finish_returns_when_an_interactive_agent_pauses_for_input() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tickets.agent(interactive_chatbot(&provider));
        let key = tickets.ticket("hello");
        tickets.start();

        // Pausing for input is no lifecycle transition and leaves the ticket
        // `InProgress`, so this returns only if `RequestFinished` reaches the
        // waiter after the reply is in the store.
        let waited = key.clone();
        tokio::time::timeout(
            Duration::from_secs(5),
            tickets.finish(move |t: &Ticket| t.key == waited),
        )
        .await
        .expect("finish did not return within 5s");
        let ticket = tickets.get_ticket(&key).unwrap();
        assert_eq!(ticket.status, Status::InProgress);
        assert!(ticket
            .replies
            .last()
            .is_some_and(|r| r.author == Author::Assistant));
        tickets.cancel_all();
    }

    #[tokio::test]
    async fn paused_interactive_ticket_emits_turn_started_exactly_once() {
        use crate::event::EventKind;

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        let collected = collect_events(&tickets);
        tickets.agent(interactive_chatbot(&provider));
        tickets.ticket("hello");

        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
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
            Ok(text_response("hi again")),
        ]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tickets.agent(interactive_chatbot(&provider));
        let first_key = tickets.ticket("first chat");
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
            let second_key = tickets_for_drive.ticket("second chat");
            for _ in 0..400 {
                if tickets_for_drive
                    .get_ticket(&second_key)
                    .is_some_and(|t| t.replies.iter().any(|r| r.author == Author::Assistant))
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
            tokio::join!(tickets.finish_all(), drive);
        })
        .await
        .expect("test did not finish within 5s");
    }

    #[tokio::test]
    async fn loop_fails_ticket_when_silence_exceeds_schema_retry_budget() {
        use crate::event::{EventKind, PolicyViolation};

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(text_response("hi"))]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(1),
                ..Default::default()
            });
        let collected = collect_events(&tickets);
        tickets.agent(task_agent(&provider));
        tickets.ticket("go");

        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
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
                    policy: PolicyViolation::MaxSchemaRetries,
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
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        tickets.agent(task_agent(&provider));
        tickets.ticket("go");

        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
            .await
            .expect("test did not finish within 5s");

        let ticket = tickets
            .tickets()
            .into_iter()
            .next()
            .expect("ticket must exist");
        assert_eq!(ticket.status, Status::Finished);
        assert_eq!(tickets.results().pop(), Some(serde_json::json!("done")));
    }

    #[tokio::test]
    async fn silence_retry_emits_schema_retried_event_with_attempt_1() {
        use crate::event::EventKind;

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(text_response("hi")),
            Ok(write_result_response("done")),
        ]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                ..Default::default()
            });
        let collected = collect_events(&tickets);
        tickets.agent(task_agent(&provider));
        tickets.ticket("go");

        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
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
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        tickets.start();
        tickets.cancel_all();

        tokio::time::timeout(Duration::from_secs(2), tickets.finish_all())
            .await
            .expect("run did not exit within 2s of cancel()");
    }

    #[tokio::test]
    async fn cancel_keeps_the_matching_pool_off_the_queue_while_others_run() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });

        let analyst = MockProvider::with_results(vec![Ok(write_result_response("analyzed"))]);
        let researcher = MockProvider::with_results(vec![Ok(write_result_response("hunted"))]);
        tickets.agent(
            Agent::new()
                .label("analysis")
                .provider(analyst)
                .model("mock")
                .role("test")
                .build(),
        );
        tickets.agent(
            Agent::new()
                .label("research")
                .provider(researcher.clone())
                .model("mock")
                .role("test")
                .build(),
        );

        // Call off the research pool on the live run (start() resets signals), then
        // enqueue both tickets; the analysis pool runs on.
        tickets.start();
        tickets.cancel(|t: &Ticket| t.has_label("research"));
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
                tickets.cancel_all();
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

        tickets.cancel_all();
        tokio::time::timeout(Duration::from_secs(2), tickets.finish_all())
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
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        tickets.agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TicketsTool)
                .build(),
        );

        tickets.ticket("first");
        tickets.finish_all().await;
        assert_eq!(tickets.results().pop(), Some(serde_json::json!("first")));

        tickets.ticket("second");
        tokio::time::timeout(Duration::from_secs(5), tickets.finish_all())
            .await
            .expect("second finish did not finish within 5s");
        assert_eq!(tickets.results().pop(), Some(serde_json::json!("second")));
    }

    #[tokio::test]
    async fn agent_finish_forwards_to_bound_queue() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![Ok(write_result_response("forwarded"))]);
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        let agent = tickets.agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TicketsTool)
                .build(),
        );

        agent.ticket("hello");
        let queue = agent.start();
        tokio::time::timeout(Duration::from_secs(5), queue.finish_all())
            .await
            .expect("the run did not end within 5s");
        assert_eq!(queue.results().pop(), Some(serde_json::json!("forwarded")));
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
            .collect()
    }

    #[tokio::test]
    async fn messages_contain_only_the_current_tickets_task() {
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("ok")),
            Ok(write_result_response("ok")),
        ]);
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(10),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tickets.agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .tool(crate::tools::TicketsTool)
                .build(),
        );
        tickets.ticket("first");
        tickets.ticket("second");
        let _ = tickets.finish_all().await;

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

        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tickets.agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .knowledge(&store)
                .build(),
        );
        tickets.ticket("first");
        tickets.ticket("second");
        let _ = tickets.finish_all().await;

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

        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tickets.agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .knowledge(&store)
                .build(),
        );
        tickets.ticket("hi");
        let _ = tickets.finish_all().await;

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

        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });

        tickets.agent(
            Agent::new()
                .label("a")
                .provider(p_a.clone())
                .model("mock")
                .role("test")
                .knowledge(&store)
                .build(),
        );
        tickets.agent(
            Agent::new()
                .label("b")
                .provider(p_b.clone())
                .model("mock")
                .role("test")
                .knowledge(&store)
                .build(),
        );

        tickets.ticket(Ticket::new("alice work").label("a"));
        let _ = tickets.finish_all().await;
        assert!(store.index().contains("alice-note"));

        tickets.ticket(Ticket::new("bob work").label("b"));
        let _ = tickets.finish_all().await;

        let bob_prompts = p_b.received_system_prompts();
        assert_eq!(bob_prompts.len(), 1, "bob processed exactly one ticket");
        assert!(
            bob_prompts[0].contains("Note from Alice"),
            "bob should see alice's write: {:?}",
            bob_prompts[0]
        );
    }

    #[tokio::test]
    async fn a_schema_bound_to_a_label_reaches_the_first_message_of_its_ticket() {
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

        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tickets.schemas(&schemas);
        tickets.agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .label("analysis")
                .build(),
        );
        tickets.ticket(Ticket::new("audit").label("analysis"));
        let _ = tickets.finish_all().await;

        let task_message = &user_texts(&provider.received()[0])[0];
        assert!(
            task_message.contains("verdict"),
            "the bound schema must be in the task message: {task_message:?}",
        );
        assert_eq!(tickets.tickets()[0].status, Status::Finished);
    }

    #[tokio::test]
    async fn a_claimed_ticket_binds_its_schema_to_the_finish_tool() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets.dir(results_dir.path().to_path_buf());
        let agent = Agent::new()
            .provider(MockProvider::with_results(vec![]))
            .model("mock")
            .role("test")
            .build();
        tickets.agent(agent.clone());
        tickets.ticket(
            Ticket::new("audit").schema(
                crate::schemas::Schema::new(serde_json::json!({
                    "type": "object",
                    "properties": { "verdict": { "type": "string" } },
                    "required": ["verdict"],
                }))
                .unwrap(),
            ),
        );

        let context = claim(&agent, &tickets).expect("the ticket is claimable");
        let finish = context.tools.get("finish").expect("finish is bound");

        let declared = finish.input_schema().get_raw_schema();
        assert!(
            declared["properties"]["result"]["properties"]["verdict"].is_object(),
            "{declared}"
        );
    }

    #[tokio::test]
    async fn a_claimed_ticket_offers_an_interactive_agent_no_finish_tool() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets.dir(results_dir.path().to_path_buf());
        let agent = interactive_chatbot(&MockProvider::with_results(vec![]));
        tickets.agent(agent.clone());
        tickets.ticket("hello");

        let context = claim(&agent, &tickets).expect("the ticket is claimable");
        assert!(context.tools.get("finish").is_none());
    }

    #[tokio::test]
    async fn an_agent_leaves_a_ticket_its_label_mate_started_alone() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets.dir(results_dir.path().to_path_buf());
        let mut first = Agent::new()
            .label("resume_pool")
            .provider(MockProvider::with_results(vec![]))
            .model("mock")
            .role("test")
            .build();
        let mut second = Agent::new()
            .label("resume_pool")
            .provider(MockProvider::with_results(vec![]))
            .model("mock")
            .role("test")
            .build();
        tickets.bind_agent(&mut first);
        tickets.bind_agent(&mut second);
        tickets.ticket(Ticket::new("work").label("resume_pool"));

        claim(&first, &tickets).expect("the first agent claims the open ticket");

        assert!(
            claim(&second, &tickets).is_none(),
            "a label mate must not take over a started ticket",
        );
        assert!(
            claim(&first, &tickets).is_some(),
            "the agent that started it resumes it",
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

        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        tickets.agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .knowledge(&store)
                .build(),
        );
        tickets.ticket("first");
        tickets.ticket("second");
        let _ = tickets.finish_all().await;

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
