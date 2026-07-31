//! Provider round-trip: sends the transcript, retries transient errors, and
//! routes overflow to reactive compaction.

use std::sync::Arc;

use crate::agents::retry::{ExponentialRetry, Retry};
use crate::event::{CompactReason, EventKind};
use crate::providers::types::{ResponseStatus, StreamEvent};
use crate::providers::{ContentBlock, ModelRequest, ProviderError, ProviderToolDefinition};
use crate::schemas::Schema;
use crate::tools::ToolCall;

use super::agent::TicketContext;
use super::wait_for_signal;
use super::Step;

pub(super) async fn run(context: &mut TicketContext<'_>) -> Step {
    // Let registered editors rewrite or drop messages before the request
    // is assembled; the re-read below then projects the edited transcript.
    context.ticket_system.run_reply_editor(&context.ticket_key);

    let Some(ticket) = context.ticket() else {
        return Step::NextTicket;
    };
    let tools =
        finish_tool_with_ticket_schema(ticket.schema.as_ref(), context.agent.tool_definitions());
    let model_name = context.model.name.clone();
    context.emit(EventKind::RequestStarted {
        model: model_name.clone(),
    });
    let request = ModelRequest {
        model: model_name,
        system_prompt: context.system_prompt.clone(),
        messages: ticket.to_messages(),
        tools,
        max_request_tokens: context.policies.max_request_tokens,
        tool_choice: None,
        reasoning_effort: context.model.get_reasoning_effort(),
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
            let ticket_system = Arc::clone(context.ticket_system);
            let emit_stream: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(move |event| {
                if let StreamEvent::TextDelta { text, .. } = event {
                    ticket_system.emit(
                        &ticket_key,
                        &agent_name,
                        EventKind::TextChunkReceived { content: text },
                    );
                }
            });
            let interrupt = &context.stop_signal;
            tokio::select! {
                biased;
                _ = wait_for_signal(interrupt) => return Step::Stop,
                result = provider.respond(request.clone(), emit_stream) => result,
            }
        };
        match outcome {
            Ok(response) => break response,
            Err(ProviderError::ContextWindowExceeded { .. }) => {
                return Step::Compact(CompactReason::Reactive);
            }
            Err(error) if error.is_retryable() => match retry.try_consume() {
                Some(attempt) => {
                    let delay = retry.delay(error.retry_delay());
                    context.emit(EventKind::RequestRetried {
                        model: request.model.clone(),
                        attempt,
                        max_attempts: retry.max_attempts(),
                        reason: error.kind(),
                        message: error.to_string(),
                    });
                    let interrupt = &context.stop_signal;
                    tokio::select! {
                        biased;
                        _ = wait_for_signal(interrupt) => return Step::Stop,
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
                None => {
                    context.fail_with(error.kind(), error.to_string());
                    return Step::NextTicket;
                }
            },
            Err(error) => {
                context.fail_with(error.kind(), error.to_string());
                return Step::NextTicket;
            }
        }
    };

    context.emit(EventKind::RequestFinished {
        model: response.model.clone(),
        usage: response.usage.clone(),
    });

    // The overflowed reply is discarded: compaction rewrites the transcript
    // and the next request regenerates it.
    if response.status == ResponseStatus::ContextWindowExceeded {
        return Step::Compact(CompactReason::Reactive);
    }
    context.ticket_system.add_reply(
        &context.ticket_key,
        crate::agents::tickets::Reply::assistant(&response.content),
    );

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
    if calls.is_empty() {
        Step::CheckTicket
    } else {
        Step::ToolCalls(calls)
    }
}

/// Advertise the finish tool's arguments in the shape the current ticket expects:
/// an object schema inlines to top-level arguments, everything else keeps the
/// `result` envelope. Shares `finish_tool_input_schema` with the `finish` tool
/// so the advertised shape and the parsed shape always agree.
fn finish_tool_with_ticket_schema(
    schema: Option<&Schema>,
    mut tools: Vec<ProviderToolDefinition>,
) -> Vec<ProviderToolDefinition> {
    for definition in &mut tools {
        if definition.name == crate::tools::TICKET_FINISH_TOOL {
            definition.input_schema = crate::tools::finish_tool_input_schema(
                &definition.name,
                definition.input_schema.clone(),
                schema,
            );
        }
    }
    tools
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
        let handler: Arc<dyn Fn(&crate::event::Event) + Send + Sync> = {
            let c = Arc::clone(&collected);
            Arc::new(move |e: &crate::event::Event| c.lock().unwrap().push(e.clone()))
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
        let handler: Arc<dyn Fn(&crate::event::Event) + Send + Sync> = {
            let c = Arc::clone(&collected);
            Arc::new(move |e: &crate::event::Event| c.lock().unwrap().push(e.clone()))
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

    // Message editing via edit_replies_on_event

    use std::sync::Arc;

    use crate::agents::agent::Agent;
    use crate::agents::tickets::{Reply, ReplyContent, TicketSystem};
    use crate::event::{Event, EventKind};
    use crate::providers::{ContentBlock, Message, Provider};
    use crate::tools::{Tool, ToolResult};

    type BoomEditor = Box<dyn Fn(&[Event], &mut Vec<Reply>) + Send + Sync>;

    /// Run a ticket whose first turn calls a tool that always fails, then
    /// writes a result. Registers `editor` when one is given. Returns the
    /// provider and temp dir so callers can inspect the requests and reload.
    async fn run_boom(
        editor: Option<BoomEditor>,
    ) -> (
        Arc<MockProvider>,
        Arc<TicketSystem>,
        crate::test_util::TempDir,
    ) {
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("boom")),
            Ok(write_result_response("done")),
        ]);
        let boom = Tool::new("boom", "Always fails")
            .handler(|_, _| async move { Ok(ToolResult::error("boom")) })
            .build();
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketSystem::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_schema_retries(10)
            .max_time(Duration::from_millis(500));
        if let Some(editor) = editor {
            tickets.edit_replies_on_event(editor);
        }
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider.clone() as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(boom)
                .build(),
        );
        tickets.task("go");
        let _ = tickets.finish().await;
        (provider, tickets, results_dir)
    }

    /// Editor that drops the whole failed tool exchange once a tool call
    /// fails: both the assistant's tool_use and the failed tool_result, so
    /// no unpaired block is left behind.
    fn drop_failed_exchange(events: &[Event], messages: &mut Vec<Reply>) {
        if events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ToolCallFailed { .. }))
        {
            messages.retain(|reply| {
                !reply.content.iter().any(|b| {
                    matches!(
                        b,
                        ReplyContent::ToolUse { .. }
                            | ReplyContent::ToolResult {
                                succeeded: false,
                                ..
                            }
                    )
                })
            });
        }
    }

    fn has_tool_blocks(messages: &[Message]) -> bool {
        messages.iter().any(|message| match message {
            Message::User { content } | Message::Assistant { content } => content.iter().any(|b| {
                matches!(
                    b,
                    ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
                )
            }),
            Message::System { .. } => false,
        })
    }

    #[tokio::test]
    async fn editor_drops_the_failed_tool_exchange() {
        let (provider, tickets, _dir) = run_boom(Some(Box::new(drop_failed_exchange))).await;

        // The editor dropped both sides of the boom exchange, so the second
        // request carries no tool blocks.
        assert!(
            !has_tool_blocks(&provider.received()[1]),
            "boom exchange must be gone: {:?}",
            provider.received()[1],
        );
        assert_eq!(tickets.tickets()[0].status, Status::Finished);
    }

    #[tokio::test]
    async fn editor_injects_message_into_next_request() {
        let (provider, _tickets, _dir) = run_boom(Some(Box::new(|events, messages| {
            if events
                .iter()
                .any(|e| matches!(e.kind, EventKind::ToolCallFailed { .. }))
            {
                messages.push(Reply::user_text("EDITOR HINT: change approach"));
            }
        })))
        .await;

        assert!(user_text(&provider.received()[1]).contains("EDITOR HINT"));
    }

    #[tokio::test]
    async fn editor_can_rewrite_a_message_in_place() {
        let (provider, _tickets, _dir) = run_boom(Some(Box::new(|events, messages| {
            if !events
                .iter()
                .any(|e| matches!(e.kind, EventKind::ToolCallFailed { .. }))
            {
                return;
            }
            for reply in messages.iter_mut() {
                for block in reply.content.iter_mut() {
                    if let ReplyContent::ToolResult { content, .. } = block {
                        *content = "REWRITTEN".into();
                    }
                }
            }
        })))
        .await;

        let rewritten = provider.received()[1].iter().any(|message| match message {
            Message::User { content } => content.iter().any(
                |b| matches!(b, ContentBlock::ToolResult { content, .. } if content == "REWRITTEN"),
            ),
            _ => false,
        });
        assert!(rewritten, "{:?}", provider.received()[1]);
    }

    #[tokio::test]
    async fn edit_survives_reload() {
        let (_provider, _tickets, dir) = run_boom(Some(Box::new(drop_failed_exchange))).await;

        let reloaded = TicketSystem::load(dir.path()).unwrap();
        let ticket = reloaded.tickets().into_iter().next().unwrap();
        // The boom exchange is gone; the later finish_ticket call remains.
        let keeps_boom = ticket.replies.iter().any(|reply| {
            reply.content.iter().any(|b| match b {
                ReplyContent::ToolUse { name, .. } => name == "boom",
                ReplyContent::ToolResult { succeeded, .. } => !succeeded,
                _ => false,
            })
        });
        assert!(!keeps_boom, "reloaded transcript must reflect the edit");
    }

    #[tokio::test]
    async fn no_editor_leaves_the_boom_exchange_in_the_transcript() {
        let (provider, _tickets, _dir) = run_boom(None).await;

        assert!(
            has_tool_blocks(&provider.received()[1]),
            "without an editor the boom exchange must remain",
        );
    }
}
