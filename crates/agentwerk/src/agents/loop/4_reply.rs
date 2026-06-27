//! LLM turn: sends messages to the provider, handles retries, and triggers reactive compaction on overflow.

use std::sync::Arc;

use crate::agents::retry::{ExponentialRetry, Retry};
use crate::event::{CompactReason, EventKind};
use crate::providers::types::{ResponseStatus, StreamEvent};
use crate::providers::{ContentBlock, Message, ModelRequest, ProviderError};
use crate::tools::ToolCall;

use super::turn;
use super::turn::LoopContext;
use super::wait_for_signal;
use super::{Action, Reply};

pub(super) async fn run(context: &mut LoopContext<'_>, messages: Vec<Message>) -> Action<Reply> {
    let tools = context.agent.tool_definitions();
    let model_name = context.model.name.clone();
    context.ticket_system.emit(
        &context.ticket_key,
        context.agent.get_name(),
        EventKind::RequestStarted {
            model: model_name.clone(),
        },
    );
    let request = ModelRequest {
        model: model_name,
        system_prompt: context.system_prompt.clone(),
        messages,
        tools,
        max_request_tokens: context.policies.max_request_tokens,
        tool_choice: None,
    };

    let mut retry = ExponentialRetry::new(
        context.policies.request_retry_delay,
        context.policies.max_request_retries,
    );
    let response = loop {
        let outcome = {
            let provider = context.agent.provider();
            let agent_name = context.agent.get_name().to_string();
            let ticket_key = context.ticket_key.clone();
            let ts = Arc::clone(context.ticket_system);
            let emit_stream: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(move |event| {
                if let StreamEvent::TextDelta { text, .. } = event {
                    ts.emit(
                        &ticket_key,
                        &agent_name,
                        EventKind::TextChunkReceived { content: text },
                    );
                }
            });
            let interrupt = &context.stop_signal;
            tokio::select! {
                biased;
                _ = wait_for_signal(interrupt) => return Action::Stop,
                result = provider.respond(request.clone(), emit_stream) => result,
            }
        };
        match outcome {
            Ok(resp) => break resp,
            Err(ProviderError::ContextWindowExceeded { .. }) => {
                match turn::compact(context, CompactReason::Reactive).await {
                    Action::Stop => return Action::Stop,
                    Action::Replay => return Action::Replay,
                    Action::Pause => unreachable!("compact() never returns Pause"),
                    Action::Proceed(()) => {}
                }
            }
            Err(e) if e.is_retryable() => match retry.try_consume() {
                Some(attempt) => {
                    let delay = retry.delay(e.retry_delay());
                    context.ticket_system.emit(
                        &context.ticket_key,
                        context.agent.get_name(),
                        EventKind::RequestRetried {
                            attempt,
                            max_attempts: retry.max_attempts(),
                            kind: e.kind(),
                            message: e.to_string(),
                        },
                    );
                    let interrupt = &context.stop_signal;
                    tokio::select! {
                        biased;
                        _ = wait_for_signal(interrupt) => return Action::Stop,
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
                None => {
                    context.ticket_system.emit(
                        &context.ticket_key,
                        context.agent.get_name(),
                        EventKind::RequestFailed {
                            kind: e.kind(),
                            message: e.to_string(),
                        },
                    );
                    let _ = context
                        .ticket_system
                        .set_failed(&context.ticket_key);
                    return Action::Replay;
                }
            },
            Err(e) => {
                context.ticket_system.emit(
                    &context.ticket_key,
                    context.agent.get_name(),
                    EventKind::RequestFailed {
                        kind: e.kind(),
                        message: e.to_string(),
                    },
                );
                let _ = context
                    .ticket_system
                    .set_failed(&context.ticket_key);
                return Action::Replay;
            }
        }
    };

    context.ticket_system.emit(
        &context.ticket_key,
        context.agent.get_name(),
        EventKind::RequestFinished {
            model: response.model.clone(),
            usage: response.usage.clone(),
        },
    );

    let overflowed = response.status == ResponseStatus::ContextWindowExceeded;
    if !overflowed {
        context.ticket_system.add_reply(
            &context.ticket_key,
            crate::agents::tickets::Reply::assistant(&response.content),
        );
    }

    let calls: Vec<ToolCall> = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(ToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect();

    let reply = if calls.is_empty() {
        Reply::TextOnly
    } else {
        Reply::Calls(calls)
    };

    if overflowed {
        match turn::compact(context, CompactReason::Reactive).await {
            Action::Stop => return Action::Stop,
            Action::Replay => return Action::Replay,
            Action::Pause => unreachable!("compact() never returns Pause"),
            Action::Proceed(()) => {}
        }
    }

    Action::Proceed(reply)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::agents::r#loop::test_util::*;
    use crate::agents::tickets::Status;

    // Request retries

    #[tokio::test]
    async fn retry_succeeds_after_rate_limit() {
        let provider = MockProvider::with_results(vec![
            Err(rate_limit()),
            Err(rate_limit()),
            Ok(write_result_response("ok")),
        ]);
        let (events, provider, ticket) = run_one(provider, 3, 10, None).await;

        assert_eq!(provider.requests(), 3);
        assert_eq!(retries_in(&events).len(), 2);
        assert!(failures_in(&events).is_empty());
        assert_eq!(ticket.status, Status::Finished);
    }

    #[tokio::test]
    async fn no_retry_on_auth_error() {
        let provider = MockProvider::with_results(vec![Err(
            crate::providers::ProviderError::AuthenticationFailed {
                message: "unauthorized".into(),
            },
        )]);
        let (events, _, _) = run_one(provider, 3, 10, None).await;

        assert!(retries_in(&events).is_empty());
        let failures = failures_in(&events);
        assert!(!failures.is_empty());
        assert!(failures[0].contains("unauthorized"));
    }

    #[tokio::test]
    async fn retries_exhausted_emits_request_failed() {
        let provider = MockProvider::with_results(vec![
            Err(rate_limit()),
            Err(rate_limit()),
            Err(rate_limit()),
        ]);
        let (events, _, _) = run_one(provider, 2, 10, None).await;

        let retries: Vec<(u32, u32)> = retries_in(&events)
            .into_iter()
            .map(|(a, m, _)| (a, m))
            .collect();
        assert_eq!(retries, vec![(1, 2), (2, 2)]);
        let failures = failures_in(&events);
        assert!(!failures.is_empty());
        assert!(failures[0].contains("rate limited"));
    }

    #[tokio::test]
    async fn happy_path_emits_no_request_failed() {
        let provider = MockProvider::with_results(vec![Ok(write_result_response("ok"))]);
        let (events, _, ticket) = run_one(provider, 3, 10, None).await;

        assert!(retries_in(&events).is_empty());
        assert!(failures_in(&events).is_empty());
        assert_eq!(ticket.status, Status::Finished);
    }

    #[tokio::test]
    async fn max_retries_on_event_matches_policy() {
        for max_retries in [0u32, 1, 3, 5] {
            let results: Vec<_> = (0..=max_retries).map(|_| Err(rate_limit())).collect();
            let provider = MockProvider::with_results(results);
            let (events, _, _) = run_one(provider, max_retries, 10, None).await;

            let retries = retries_in(&events);
            assert_eq!(
                retries.len() as u32,
                max_retries,
                "max_retries={max_retries}"
            );
            for (_, evt_max, _) in &retries {
                assert_eq!(*evt_max, max_retries);
            }
        }
    }

    #[tokio::test]
    async fn max_request_retries_zero_goes_straight_to_request_failed() {
        let provider = MockProvider::with_results(vec![Err(rate_limit())]);
        let (events, _, _) = run_one(provider, 0, 10, None).await;

        assert!(retries_in(&events).is_empty());
        assert!(!failures_in(&events).is_empty());
    }

    #[tokio::test]
    async fn request_retried_attempt_numbers_are_one_based() {
        let provider = MockProvider::with_results(vec![
            Err(rate_limit()),
            Err(rate_limit()),
            Ok(write_result_response("ok")),
        ]);
        let (events, _, _) = run_one(provider, 4, 10, None).await;

        let attempts: Vec<u32> = retries_in(&events).into_iter().map(|(a, ..)| a).collect();
        assert_eq!(attempts, vec![1, 2]);
    }

    #[tokio::test]
    async fn request_retried_carries_provider_error_display() {
        let provider = MockProvider::with_results(vec![
            Err(connection_failed("dns lookup failed: no such host")),
            Ok(write_result_response("ok")),
        ]);
        let (events, _, _) = run_one(provider, 3, 10, None).await;

        let retries = retries_in(&events);
        assert_eq!(retries.len(), 1);
        assert!(retries[0].2.contains("dns lookup failed"));
    }

    #[tokio::test]
    async fn request_failed_carries_terminal_error_display_for_each_non_retryable_variant() {
        use crate::providers::ProviderError;
        let cases: Vec<(ProviderError, &'static str)> = vec![
            (
                ProviderError::AuthenticationFailed {
                    message: "bad key 401".into(),
                },
                "bad key 401",
            ),
            (
                ProviderError::PermissionDenied {
                    message: "no access 403".into(),
                },
                "no access 403",
            ),
            (
                ProviderError::ModelNotFound {
                    message: "unknown-model-xyz".into(),
                },
                "unknown-model-xyz",
            ),
            (
                ProviderError::SafetyFilterTriggered {
                    message: "blocked by safety-filter-7".into(),
                },
                "safety-filter-7",
            ),
            (
                ProviderError::ResponseMalformed {
                    message: "malformed-json-token".into(),
                },
                "malformed-json-token",
            ),
        ];

        for (err, needle) in cases {
            let provider = MockProvider::with_results(vec![Err(err)]);
            let (events, _, _) = run_one(provider, 3, 10, None).await;

            let failures = failures_in(&events);
            assert!(!failures.is_empty(), "{needle}");
            assert!(failures[0].contains(needle), "{needle}: {}", failures[0]);
            assert!(retries_in(&events).is_empty(), "{needle}");
        }
    }

    #[tokio::test]
    async fn terminal_provider_error_marks_ticket_failed() {
        use crate::providers::ProviderError;
        let cases: Vec<ProviderError> = vec![
            ProviderError::AuthenticationFailed {
                message: "bad key 401".into(),
            },
            ProviderError::PermissionDenied {
                message: "no access 403".into(),
            },
            ProviderError::ModelNotFound {
                message: "unknown-model-xyz".into(),
            },
            ProviderError::SafetyFilterTriggered {
                message: "blocked".into(),
            },
            ProviderError::ResponseMalformed {
                message: "bad json".into(),
            },
        ];

        for err in cases {
            let provider = MockProvider::with_results(vec![Err(err)]);
            let (_, _, ticket) = run_one(provider, 3, 10, None).await;
            assert_eq!(
                ticket.status,
                Status::Failed,
                "terminal provider error must transition ticket to Failed"
            );
        }
    }

    #[tokio::test]
    async fn retry_exhausted_marks_ticket_failed() {
        let provider = MockProvider::with_results(vec![
            Err(rate_limit()),
            Err(rate_limit()),
            Err(rate_limit()),
        ]);
        let (_, _, ticket) = run_one(provider, 2, 10, None).await;
        assert_eq!(
            ticket.status,
            Status::Failed,
            "exhausted retry budget must transition ticket to Failed"
        );
    }

    // Backoff timing

    #[tokio::test(start_paused = true)]
    async fn request_retried_fires_after_backoff_sleep_not_before() {
        use crate::agents::agent::Agent;
        use crate::agents::tickets::TicketSystem;
        use crate::event::EventKind;
        use crate::providers::Provider;
        use std::sync::{Arc, Mutex};

        let provider = MockProvider::with_results(vec![
            Err(crate::providers::ProviderError::RateLimited {
                message: "rl".into(),
                status: 429,
                retry_delay: Some(Duration::from_millis(1_000)),
            }),
            Ok(write_result_response("ok")),
        ]);
        let collected: Arc<Mutex<Vec<crate::event::Event>>> = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<dyn Fn(crate::event::Event) + Send + Sync> = {
            let c = Arc::clone(&collected);
            Arc::new(move |e| c.lock().unwrap().push(e))
        };

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(3)
            .request_retry_delay(Duration::from_millis(1));
        tickets.on_event(move |e| handler(e));
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .build(),
        );
        tickets.task("go");

        let run_fut = tickets.finish();
        let check_fut = async {
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            let retries = || {
                collected
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|e| matches!(e.kind, EventKind::RequestRetried { .. }))
                    .count()
            };
            assert_eq!(retries(), 1, "retry event fires immediately on Err");

            tokio::time::advance(Duration::from_millis(999)).await;
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            assert_eq!(retries(), 1);
            tokio::time::advance(Duration::from_millis(2)).await;
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
        };

        let (_, _) = tokio::join!(run_fut, check_fut);
    }

    // Cancellation interactions with retries

    #[tokio::test(start_paused = true)]
    async fn cancel_during_backoff_sleep_aborts_immediately() {
        use crate::agents::agent::Agent;
        use crate::agents::tickets::TicketSystem;
        use crate::providers::Provider;
        use std::sync::{Arc, Mutex};

        let provider =
            MockProvider::with_results(vec![Err(crate::providers::ProviderError::RateLimited {
                message: "rl".into(),
                status: 429,
                retry_delay: Some(Duration::from_secs(60)),
            })]);
        let collected: Arc<Mutex<Vec<crate::event::Event>>> = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<dyn Fn(crate::event::Event) + Send + Sync> = {
            let c = Arc::clone(&collected);
            Arc::new(move |e| c.lock().unwrap().push(e))
        };
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(3)
            .request_retry_delay(Duration::from_secs(60));
        tickets.on_event(move |e| handler(e));
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .build(),
        );
        tickets.task("go");

        let run_fut = tickets.finish();
        let cancel_handle = Arc::clone(&tickets);
        let cancel_fut = async {
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            cancel_handle.cancel();
            tokio::time::advance(Duration::from_millis(100)).await;
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
        };

        let _ = tokio::join!(run_fut, cancel_fut);
        let events = collected.lock().unwrap().clone();
        assert_eq!(retries_in(&events).len(), 1);
        assert!(failures_in(&events).is_empty());
    }
}
