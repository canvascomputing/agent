//! Rewriting a task's replies to fit the context window: ahead of it filling
//! up, and after the LLM provider reports it has. The older messages are
//! summarized; the four `Compaction*` events report each step of it.

use std::sync::Arc;

use crate::agents::compaction::{self as algo, Compaction};
use crate::agents::tasks::Task;
use crate::event::{CompactReason, EventKind};

use super::agent::TaskContext;
use super::Step;

pub(super) async fn run(context: &mut TaskContext<'_>, reason: CompactReason) -> Option<Step> {
    let Some(mut task) = context.task() else {
        return None;
    };
    let window = context.model.get_context_window();
    let total = algo::chunks_for_window(&task.to_messages(), window).len() as u32;
    context.emit(EventKind::CompactionStarted { reason, total });

    let on_progress: Arc<dyn Fn(u32, u32) + Send + Sync> = {
        let queue = Arc::clone(context.queue);
        let agent_id = context.agent.id().to_string();
        let task_key = context.task_key.clone();
        Arc::new(move |completed, total| {
            queue.emit(
                &task_key,
                &agent_id,
                EventKind::CompactionProgress {
                    reason,
                    completed,
                    total,
                },
            );
        })
    };

    // Moved out rather than cloned: the summarizer gets the replies as its own
    // argument, so a second copy on the task would only be one more thing to
    // read them from.
    let replies = std::mem::take(&mut task.replies);
    let compaction = Compaction::new(
        context.agent.get_provider(),
        context.model.name.clone(),
        window,
        on_progress,
        context.agent.get_directives(),
    );
    let edited = match algo::summarize_replies(compaction, replies.clone()).await {
        Ok(edited) => edited,
        Err(error) => {
            context.emit(EventKind::CompactionFailed {
                reason,
                message: error.to_string(),
            });
            context.fail_task();
            return None;
        }
    };

    // Replies handed back untouched say compaction found nothing to drop.
    let applied = edited != replies;
    if applied {
        context
            .queue
            .edit_replies(&context.task_key, |current| *current = edited);
        // The last response's input tokens no longer describe the next request.
        context.queue.stats.reset_usage(&context.task_key);
    }

    if !applied && matches!(reason, CompactReason::Reactive) {
        context.emit(EventKind::CompactionFailed {
            reason,
            message: "context still exceeds window after compaction".into(),
        });
        context.fail_task();
        return None;
    }

    context.emit(EventKind::CompactionFinished { reason });
    match reason {
        // Proactive skips Evaluate, which would re-trigger its own threshold.
        CompactReason::Proactive => Some(Step::Request),
        CompactReason::Reactive => Some(Step::Evaluate),
    }
}

pub(super) fn proactive_compaction_needed(context: &TaskContext<'_>, task: &Task) -> bool {
    let tools = context.tools.tools();
    let window = context.model.get_context_window();
    let history = context.queue.stats.usage_for_task(&context.task_key);

    algo::should_compact_proactively(
        window,
        context.policy.compaction_threshold,
        &history,
        &task.to_messages(),
        &context.system_prompt,
        &tools,
    )
}

#[cfg(test)]
mod tests {
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tasks::Status;

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
            .expect("the task must surface a request failure");
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
        let (events, provider, task) = run_one(provider, 0, 10, Some(string_schema())).await;

        assert_eq!(provider.requests(), 4);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(task.status, Status::Finished);

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
        let (events, provider, task) = run_one(provider, 0, 10, Some(string_schema())).await;

        assert_eq!(provider.requests(), 4);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(task.status, Status::Finished);
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
        let (events, provider, task) = run_one(provider, 0, 10, Some(string_schema())).await;

        assert_eq!(provider.requests(), 4);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(task.status, Status::Finished);
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
        let (events, provider, task) =
            run_with_context_window(provider, 10_000, "x\n".repeat(25_000)).await;

        assert_eq!(provider.requests(), 4);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(task.status, Status::Finished);
    }

    #[tokio::test]
    async fn compaction_terminal_failure_transitions_task_to_failed() {
        let provider = MockProvider::with_results(vec![Err(
            crate::providers::ProviderError::ContextWindowExceeded {
                message: "overflow".into(),
            },
        )]);
        let (events, _, task) =
            run_with_context_window(provider, 10_000, "x\n".repeat(25_000)).await;

        assert_eq!(
            task.status,
            Status::Failed,
            "terminal compaction failure must transition the task to Failed",
        );
        let task_failed_count = events
            .iter()
            .filter(|e| matches!(&e.kind, crate::event::EventKind::TaskFailed))
            .count();
        assert_eq!(task_failed_count, 1);
    }

    #[tokio::test]
    async fn still_oversized_after_compaction_transitions_task_to_failed() {
        let provider = MockProvider::with_results(vec![Ok(text_response_with_usage(
            "SUMMARY",
            crate::providers::types::TokenUsage::default(),
        ))]);
        let (events, _, task) = run_with_context_window(provider, 1_000, "hi").await;

        assert_eq!(
            task.status,
            Status::Failed,
            "post-compaction window check must transition the task to Failed",
        );
        let task_failed_count = events
            .iter()
            .filter(|e| matches!(&e.kind, crate::event::EventKind::TaskFailed))
            .count();
        assert_eq!(task_failed_count, 1);
    }

    #[tokio::test]
    async fn reactive_overflow_twice_in_a_row_fails_the_task() {
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
        let (events, _, task) = run_one(provider, 0, 10, Some(string_schema())).await;

        assert_eq!(
            compaction_finishes(&events, crate::event::CompactReason::Reactive),
            1
        );
        let failures = failures_in(&events);
        assert!(!failures.is_empty());
        assert_eq!(task.status, Status::Failed);
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
        let (events, provider, task) = run_compaction(provider, |_| {}).await;

        assert_eq!(provider.requests(), 3);
        assert_eq!(compaction_starts(&events, CompactReason::Proactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Proactive), 1);
        assert_eq!(task.status, Status::Finished);

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
    async fn summarize_rate_limited_kills_task_without_retry() {
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
        let (events, provider, task) = run_compaction(provider, |_| {}).await;

        assert_eq!(
            compaction_starts(&events, crate::event::CompactReason::Proactive),
            1
        );
        assert_eq!(
            compaction_finishes(&events, crate::event::CompactReason::Proactive),
            1
        );
        assert_eq!(task.status, Status::Finished);

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
        let (events, provider, task) = run_compaction(provider, |_| {}).await;

        assert_eq!(provider.requests(), 6);
        assert_eq!(compaction_starts(&events, CompactReason::Proactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Proactive), 1);
        assert_eq!(compaction_starts(&events, CompactReason::Reactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(failures_in(&events).is_empty());
        assert_eq!(task.status, Status::Finished);
    }

    #[tokio::test]
    async fn compaction_clears_the_task_usage() {
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
        let queue_handle: std::sync::Arc<std::sync::Mutex<Option<_>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured = std::sync::Arc::clone(&queue_handle);
        let (_, _, task) = run_compaction(provider, move |tasks| {
            *captured.lock().unwrap() = Some(std::sync::Arc::clone(tasks));
        })
        .await;

        // The 180 000-token anchor that tripped the trigger described replies
        // the task no longer holds, so it must not survive compaction.
        let tasks = queue_handle.lock().unwrap().take().expect("queue captured");
        let history = tasks.stats.usage_for_task(&task.key);
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
        let (events, _, task) = run_compaction(provider, |_| {}).await;

        assert_eq!(
            compaction_finishes(&events, crate::event::CompactReason::Proactive),
            1
        );
        assert_eq!(
            task.task,
            serde_json::Value::String("go".into()),
            "the task must still say what was asked",
        );
    }

    fn string_schema() -> crate::schemas::Schema {
        crate::schemas::Schema::new(serde_json::json!({"type": "string"})).expect("valid schema")
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
