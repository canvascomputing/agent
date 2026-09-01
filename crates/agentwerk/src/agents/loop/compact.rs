//! Rewriting a task's replies to fit the context window: ahead of it filling
//! up, and after the LLM provider reports it has. The older messages are
//! summarized; the four `Compaction*` events report each step of it.

use std::sync::Arc;

use crate::agents::agent::Agent;
use crate::agents::compaction::{self as algo, Compaction};
use crate::agents::policy::Policy;
use crate::agents::tasks::{Task, Werk};
use crate::event::Event;
use crate::tools::Tool;

use super::CompactReason;

impl Agent {
    pub(super) async fn compact(
        &self,
        werk: &Arc<Werk>,
        task_id: &str,
        reason: CompactReason,
    ) -> bool {
        let Some(mut task) = werk.get_task(task_id) else {
            return false;
        };
        let model = self.get_model();
        let window = model.get_context_window();
        let total = algo::chunks_for_window(&task.to_messages(), window).len() as u32;
        self.emit_event(
            werk,
            task_id,
            Event::new(Event::COMPACTION_STARTED)
                .data(serde_json::json!({ "trigger": reason, "total": total })),
        );

        let on_progress: Arc<dyn Fn(u32, u32) + Send + Sync> = {
            let werk = Arc::clone(werk);
            let agent_id = self.get_id().to_string();
            let task_id = task_id.to_string();
            Arc::new(move |completed, total| {
                werk.emit_event(
                    Event::new(Event::COMPACTION_PROGRESS)
                        .data(serde_json::json!({
                            "trigger": reason,
                            "completed": completed,
                            "total": total,
                        }))
                        .task_id(&task_id)
                        .agent_id(&agent_id),
                );
            })
        };

        let replies = std::mem::take(&mut task.replies);
        let compaction = Compaction::new(
            self.get_provider(),
            model.name.clone(),
            window,
            on_progress,
            self.get_directives(),
        );
        let edited = match algo::summarize_replies(compaction, replies.clone()).await {
            Ok(edited) => edited,
            Err(error) => {
                self.emit_event(
                    werk,
                    task_id,
                    Event::new(Event::COMPACTION_FAILED).data(serde_json::json!({
                        "trigger": reason,
                        "kind": "summarization_failed",
                        "message": error.to_string(),
                    })),
                );
                self.fail_task(werk, task_id);
                return false;
            }
        };

        let applied = edited != replies;
        if applied {
            werk.edit_replies(task_id, |current| *current = edited);
            werk.stats.reset_usage(task_id);
        }

        if !applied && matches!(reason, CompactReason::Reactive) {
            self.emit_event(
                werk,
                task_id,
                Event::new(Event::COMPACTION_FAILED).data(serde_json::json!({
                    "trigger": reason,
                    "kind": "context_still_exceeded",
                    "message": "context still exceeds window after compaction",
                })),
            );
            self.fail_task(werk, task_id);
            return false;
        }

        self.emit_event(
            werk,
            task_id,
            Event::new(Event::COMPACTION_FINISHED).data(serde_json::json!({ "trigger": reason })),
        );
        true
    }

    pub(super) fn needs_compaction(
        &self,
        werk: &Werk,
        task_id: &str,
        task: &Task,
        system_prompt: &str,
        policy: &Policy,
        tools: &[Tool],
    ) -> bool {
        let history = werk.stats.usage_for_task(task_id);
        algo::should_compact_proactively(
            self.get_model().get_context_window(),
            policy.compaction_threshold,
            &history,
            &task.to_messages(),
            system_prompt,
            tools,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tasks::Status;

    use super::CompactReason;

    fn compaction_starts(events: &[crate::event::Event], expected: CompactReason) -> usize {
        let expected = serde_json::to_value(expected).unwrap();
        events
            .iter()
            .filter(|e| {
                e.get_name() == crate::event::Event::COMPACTION_STARTED
                    && e.get_data()["trigger"] == expected
            })
            .count()
    }

    fn compaction_finishes(events: &[crate::event::Event], expected: CompactReason) -> usize {
        let expected = serde_json::to_value(expected).unwrap();
        events
            .iter()
            .filter(|e| {
                e.get_name() == crate::event::Event::COMPACTION_FINISHED
                    && e.get_data()["trigger"] == expected
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
            .position(|e| e.get_name() == crate::event::Event::COMPACTION_STARTED)
            .expect("compaction must have started");
        let finished_idx = events
            .iter()
            .position(|e| e.get_name() == crate::event::Event::COMPACTION_FINISHED)
            .expect("compaction must have finished");
        let request_failed_idx = events
            .iter()
            .position(|e| e.get_name() == crate::event::Event::REQUEST_FAILED)
            .expect("the task must surface a request failure");
        assert!(started_idx < finished_idx);
        assert!(finished_idx < request_failed_idx);
    }

    #[tokio::test]
    async fn reactive_overflow_compacts_then_succeeds() {
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
            .filter(|e| e.get_name() == crate::event::Event::TASK_FAILED)
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
            .filter(|e| e.get_name() == crate::event::Event::TASK_FAILED)
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

        assert_eq!(compaction_finishes(&events, CompactReason::Reactive), 1);
        assert!(events.iter().any(|event| {
            event.get_name() == crate::event::Event::COMPACTION_FAILED
                && event.get_data()["trigger"] == "reactive"
                && event.get_data()["kind"] == "context_still_exceeded"
        }));
        let failures = failures_in(&events);
        assert!(!failures.is_empty());
        assert_eq!(task.status, Status::Failed);
    }

    #[tokio::test]
    async fn proactive_threshold_triggers_compaction_before_next_request() {
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
            .position(|e| e.get_name() == crate::event::Event::COMPACTION_STARTED)
            .expect("compaction must start");
        let finished_idx = events
            .iter()
            .position(|e| e.get_name() == crate::event::Event::COMPACTION_FINISHED)
            .expect("compaction must finish");
        let request_started: Vec<usize> = events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                (e.get_name() == crate::event::Event::REQUEST_STARTED).then_some(i)
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

        assert_eq!(compaction_starts(&events, CompactReason::Proactive), 1);
        assert!(events.iter().any(|e| {
            e.get_name() == crate::event::Event::COMPACTION_FAILED
                && e.get_data()["trigger"] == "proactive"
                && e.get_data()["kind"] == "summarization_failed"
                && e.get_data()["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("rate limited"))
        }));
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

        assert_eq!(compaction_starts(&events, CompactReason::Proactive), 1);
        assert_eq!(compaction_finishes(&events, CompactReason::Proactive), 1);
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
        let werk_handle: std::sync::Arc<std::sync::Mutex<Option<_>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured = std::sync::Arc::clone(&werk_handle);
        let (_, _, task) = run_compaction(provider, move |tasks| {
            *captured.lock().unwrap() = Some(std::sync::Arc::clone(tasks));
        })
        .await;

        // The 180 000-token anchor that tripped the trigger described replies
        // the task no longer holds, so it must not survive compaction.
        let tasks = werk_handle.lock().unwrap().take().expect("Werk captured");
        let history = tasks.stats.usage_for_task(&task.id);
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

        assert_eq!(compaction_finishes(&events, CompactReason::Proactive), 1);
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
