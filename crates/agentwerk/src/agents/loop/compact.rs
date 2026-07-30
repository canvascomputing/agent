//! Transcript compaction: proactive before the window fills, reactive after the provider reports overflow.

use std::sync::Arc;

use crate::agents::compaction as algo;
use crate::agents::tickets::Ticket;
use crate::event::{CompactReason, EventKind};
use crate::providers::RequestErrorKind;

use super::agent::TicketContext;
use super::Step;

pub(super) async fn run(context: &mut TicketContext<'_>, reason: CompactReason) -> Step {
    let Some(ticket) = context.ticket() else {
        return Step::Stop;
    };
    let window = context.model.get_context_window();
    let messages = ticket.to_messages();
    let total = algo::chunks_for_window(&messages, window).len() as u32;
    context.emit(EventKind::CompactionStarted { reason, total });

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
        Err(error) => {
            context.emit(EventKind::CompactionFailed {
                reason,
                message: error.to_string(),
            });
            context.fail_with(error.kind(), error.to_string());
            return Step::Stop;
        }
    };

    if !applied && matches!(reason, CompactReason::Reactive) {
        context.fail_with(
            RequestErrorKind::ContextWindowExceeded,
            "context still exceeds window after compaction".into(),
        );
        return Step::Stop;
    }

    context.emit(EventKind::CompactionFinished { reason });
    match reason {
        CompactReason::Proactive => Step::Request,
        CompactReason::Reactive => Step::CheckTicket,
    }
}

pub(super) fn proactive_compaction_needed(context: &TicketContext<'_>, ticket: &Ticket) -> bool {
    let tools = context.agent.tool_definitions();
    let window = context.model.get_context_window();
    let history = context
        .ticket_system
        .stats()
        .usage_history(&context.ticket_key);

    algo::should_compact_proactively(
        window,
        &history,
        &ticket.to_messages(),
        &context.system_prompt,
        &tools,
    )
}

#[cfg(test)]
mod tests {
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tickets::Status;

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
            // A single-message transcript collapses to a no-op, so prime
            // one turn before the overflow to give compaction something.
            Ok(tool_call_response("primer")),
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
        assert_eq!(user_texts(fourth), vec!["SUMMARY".to_string()]);
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
            .filter(|e| matches!(&e.kind, crate::event::EventKind::TicketFailed))
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
            .filter(|e| matches!(&e.kind, crate::event::EventKind::TicketFailed))
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

    fn string_schema() -> crate::schemas::Schema {
        crate::schemas::Schema::parse(serde_json::json!({"type": "string"})).expect("valid schema")
    }

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
}
