//! Rewriting a ticket's replies to fit the context window: ahead of it filling
//! up, and after the LLM provider reports it has. Summarizing them is the
//! default; `TicketQueue::edit_replies_on_compaction` replaces it.

use std::sync::Arc;

use crate::agents::compaction::{self as algo, Compaction};
use crate::agents::tickets::Ticket;
use crate::event::{CompactReason, EventKind};

use super::agent::TicketContext;
use super::Step;

pub(super) async fn run(context: &mut TicketContext<'_>, reason: CompactReason) -> Option<Step> {
    let Some(mut ticket) = context.ticket() else {
        return None;
    };
    let window = context.model.get_context_window();
    let editor = context.ticket_queue.compaction_editor();
    // Only the built-in summarizer works in chunks, so only it can say how many
    // are coming. Counting them means splitting every message, which an editor
    // that never calls the summarizer would pay for nothing.
    let total = match editor {
        Some(_) => 1,
        None => algo::chunks_for_window(&ticket.to_messages(), window).len() as u32,
    };
    context.emit(EventKind::CompactionStarted { reason, total });

    let on_progress: Arc<dyn Fn(u32, u32) + Send + Sync> = {
        let ticket_queue = Arc::clone(context.ticket_queue);
        let agent_name = context.agent.get_name().to_string();
        let ticket_key = context.ticket_key.clone();
        Arc::new(move |completed, total| {
            ticket_queue.emit(
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

    // Moved out rather than cloned: the editor gets the replies as its own
    // argument, so a second copy on the ticket would only be one more thing to
    // read them from.
    let replies = std::mem::take(&mut ticket.replies);
    let compaction = Compaction::new(
        reason,
        ticket,
        context.agent.provider(),
        context.model.name.clone(),
        window,
        on_progress,
    );
    let edited = match editor {
        Some(editor) => editor(compaction, replies.clone()).await,
        None => algo::default_editor(compaction, replies.clone()).await,
    };
    let edited = match edited {
        Ok(edited) => edited,
        Err(error) => {
            context.emit(EventKind::CompactionFailed {
                reason,
                message: error.to_string(),
            });
            context.fail_ticket();
            return None;
        }
    };

    // Replies handed back untouched say compaction found nothing to drop.
    let applied = edited != replies;
    if applied {
        context
            .ticket_queue
            .edit_replies(&context.ticket_key, |current| *current = edited);
        // The last response's input tokens no longer describe the next request.
        context.ticket_queue.stats.reset_usage(&context.ticket_key);
    }

    if !applied && matches!(reason, CompactReason::Reactive) {
        context.emit(EventKind::CompactionFailed {
            reason,
            message: "context still exceeds window after compaction".into(),
        });
        context.fail_ticket();
        return None;
    }

    context.emit(EventKind::CompactionFinished { reason });
    match reason {
        // Proactive skips Evaluate, which would re-trigger its own threshold.
        CompactReason::Proactive => Some(Step::Request),
        CompactReason::Reactive => Some(Step::Evaluate),
    }
}

pub(super) fn proactive_compaction_needed(context: &TicketContext<'_>, ticket: &Ticket) -> bool {
    let tools = context.agent.tool_definitions();
    let window = context.model.get_context_window();
    let history = context
        .ticket_queue
        .stats()
        .usage_for_ticket(&context.ticket_key);

    algo::should_compact_proactively(
        window,
        context.policies.compact_at,
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
                    input_tokens: 180_000,
                    output_tokens: 0,
                },
            )),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("done")),
        ]);
        let (events, provider, ticket) = run_compaction(provider, |_| {}).await;

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
                    input_tokens: 180_000,
                    output_tokens: 0,
                },
            )),
            Err(rate_limit()),
        ]);
        let (events, _, _) = run_compaction(provider, |_| {}).await;

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
                    input_tokens: 180_000,
                    output_tokens: 0,
                },
            )),
            Ok(text_response_with_usage(
                "",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("done")),
        ]);
        let (events, provider, ticket) = run_compaction(provider, |_| {}).await;

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
        let (events, _, ticket) = run_compaction(provider, |_| {}).await;

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
                    input_tokens: 180_000,
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
        let (events, provider, ticket) = run_compaction(provider, |_| {}).await;

        assert_eq!(provider.requests(), 6);
        assert_eq!(compaction_starts(&events, CompactReason::Proactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Proactive), 1);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(ticket.status, Status::Finished);
    }

    // Editors

    /// A provider primed to trip the proactive threshold on its first turn,
    /// then answer the request that follows compaction.
    fn provider_that_overflows_then_finishes() -> std::sync::Arc<MockProvider> {
        MockProvider::with_results(vec![
            Ok(tool_call_response_with_usage(
                "primer",
                crate::providers::types::TokenUsage {
                    input_tokens: 180_000,
                    output_tokens: 0,
                },
            )),
            Ok(write_result_response("done")),
        ])
    }

    #[tokio::test]
    async fn an_installed_editor_replaces_the_built_in_summarizer() {
        let provider = provider_that_overflows_then_finishes();
        let (events, provider, ticket) = run_compaction(provider, |tickets| {
            tickets.edit_replies_on_compaction(|_compaction, _replies| async move {
                Ok(vec![crate::agents::tickets::Reply::user_text("EDITED")])
            });
        })
        .await;

        // Two requests, not three: the editor asked the provider for nothing.
        assert_eq!(provider.requests(), 2);
        assert_eq!(
            compaction_finishes(&events, crate::event::CompactReason::Proactive),
            1
        );
        assert_eq!(ticket.status, Status::Finished);
        assert_eq!(user_texts(&provider.received()[1]), vec!["EDITED"]);
    }

    #[tokio::test]
    async fn an_installed_editor_reports_one_chunk_on_compaction_started() {
        let provider = provider_that_overflows_then_finishes();
        let (events, _, _) = run_compaction(provider, |tickets| {
            tickets.edit_replies_on_compaction(|_, _| async move {
                Ok(vec![crate::agents::tickets::Reply::user_text("EDITED")])
            });
        })
        .await;

        let totals: Vec<u32> = events
            .iter()
            .filter_map(|e| match &e.kind {
                crate::event::EventKind::CompactionStarted { total, .. } => Some(*total),
                _ => None,
            })
            .collect();
        assert_eq!(
            totals,
            vec![1],
            "only the built-in summarizer works in chunks worth counting",
        );
    }

    #[tokio::test]
    async fn the_editor_gets_the_replies_and_a_ticket_that_does_not_repeat_them() {
        let provider = provider_that_overflows_then_finishes();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<(usize, usize)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = std::sync::Arc::clone(&seen);
        run_compaction(provider, move |tickets| {
            tickets.edit_replies_on_compaction(move |compaction, replies| {
                let captured = std::sync::Arc::clone(&captured);
                async move {
                    captured
                        .lock()
                        .unwrap()
                        .push((replies.len(), compaction.ticket().replies.len()));
                    Ok(vec![crate::agents::tickets::Reply::user_text("EDITED")])
                }
            });
        })
        .await;

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "the editor must have run once");
        let (argument, on_ticket) = seen[0];
        assert!(argument > 0, "the replies arrive as the argument");
        assert_eq!(on_ticket, 0, "and are not carried a second time");
    }

    #[tokio::test]
    async fn a_second_compaction_editor_replaces_the_first() {
        let provider = provider_that_overflows_then_finishes();
        let (_, provider, _) = run_compaction(provider, |tickets| {
            tickets.edit_replies_on_compaction(|_, _| async move {
                Ok(vec![crate::agents::tickets::Reply::user_text("FIRST")])
            });
            tickets.edit_replies_on_compaction(|_, _| async move {
                Ok(vec![crate::agents::tickets::Reply::user_text("SECOND")])
            });
        })
        .await;

        assert_eq!(user_texts(&provider.received()[1]), vec!["SECOND"]);
    }

    #[tokio::test]
    async fn an_editor_that_returns_the_replies_unchanged_fails_a_reactive_compaction() {
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("primer")),
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "overflow".into(),
            }),
        ]);
        let (events, _, ticket) = run_compaction(provider, |tickets| {
            tickets.edit_replies_on_compaction(|_, replies| async move { Ok(replies) });
        })
        .await;

        assert_eq!(
            compaction_finishes(&events, crate::event::CompactReason::Reactive),
            0,
            "compaction that dropped nothing must not report success",
        );
        assert_eq!(ticket.status, Status::Failed);
        assert!(failures_in(&events)
            .iter()
            .any(|message| message.contains("context still exceeds window")));
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, crate::event::EventKind::RequestFailed { .. })),
            "no request was made here, so none may be reported as failed",
        );
    }

    #[tokio::test]
    async fn an_editor_error_fails_the_ticket_and_emits_compaction_failed() {
        let provider = provider_that_overflows_then_finishes();
        let (events, _, ticket) = run_compaction(provider, |tickets| {
            tickets.edit_replies_on_compaction(|_, _| async move {
                Err(crate::providers::ProviderError::ConnectionFailed {
                    message: "editor could not reach its own service".into(),
                })
            });
        })
        .await;

        assert!(events.iter().any(|e| matches!(
            &e.kind,
            crate::event::EventKind::CompactionFailed { message, .. }
            if message.contains("editor could not reach its own service"),
        )));
        assert_eq!(ticket.status, Status::Failed);
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, crate::event::EventKind::RequestFailed { .. })),
            "the editor failed, not a request, and one failure reports once",
        );
    }

    #[tokio::test]
    async fn compaction_clears_the_ticket_usage() {
        let provider = provider_that_overflows_then_finishes();
        let queue_handle: std::sync::Arc<std::sync::Mutex<Option<_>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured = std::sync::Arc::clone(&queue_handle);
        let (_, _, ticket) = run_compaction(provider, move |tickets| {
            *captured.lock().unwrap() = Some(std::sync::Arc::clone(tickets));
            tickets.edit_replies_on_compaction(|_, _| async move {
                Ok(vec![crate::agents::tickets::Reply::user_text("EDITED")])
            });
        })
        .await;

        // The 180 000-token anchor that tripped the trigger described replies
        // the ticket no longer holds, so it must not survive compaction.
        let tickets = queue_handle.lock().unwrap().take().expect("queue captured");
        let history = tickets.stats().usage_for_ticket(&ticket.key);
        assert!(
            history.len() <= 1,
            "expected the pre-compaction usage to be dropped, got {history:?}",
        );
    }

    #[tokio::test]
    async fn compaction_leaves_the_original_task_in_place() {
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response_with_usage(
                "primer",
                crate::providers::types::TokenUsage {
                    input_tokens: 180_000,
                    output_tokens: 0,
                },
            )),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("done")),
        ]);
        let (events, _, ticket) = run_compaction(provider, |_| {}).await;

        assert_eq!(
            compaction_finishes(&events, crate::event::CompactReason::Proactive),
            1
        );
        assert_eq!(
            ticket.task,
            serde_json::Value::String("go".into()),
            "the ticket must still say what was asked",
        );
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
