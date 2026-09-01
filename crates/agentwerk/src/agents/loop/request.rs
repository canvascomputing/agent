//! One round-trip to the LLM provider: sends the messages, retries a transient
//! error, and summarizes the older messages when the context window overflows.

use std::sync::Arc;

use crate::agents::agent::Agent;
use crate::agents::policy::Policy;
use crate::agents::retry::{ExponentialRetry, Retry};
use crate::agents::tasks::{Reply, Werk};
use crate::event::Event;
use crate::providers::types::StreamEvent;
use crate::providers::{ContentBlock, ModelRequest, ProviderError};
use crate::tools::Tool;

impl Agent {
    pub(super) async fn request(
        &self,
        werk: &Arc<Werk>,
        task_id: &str,
        system_prompt: &str,
        policy: &Policy,
        tools: &[Tool],
    ) -> Result<Option<Vec<ContentBlock>>, ProviderError> {
        let Some(task) = werk.get_task(task_id) else {
            return Ok(None);
        };
        let model = self.get_model();
        self.emit_event(
            werk,
            task_id,
            Event::new(Event::REQUEST_STARTED).data(serde_json::json!({ "model": model.name })),
        );
        let request = ModelRequest {
            model: model.name.clone(),
            system_prompt: system_prompt.to_string(),
            messages: task.to_messages(),
            tools: tools.to_vec(),
            max_request_tokens: policy.max_request_tokens,
            reasoning_effort: model.get_reasoning_effort(),
        };

        let mut retry =
            ExponentialRetry::new(policy.request_retry_delay, policy.max_request_retries);
        let response = loop {
            let provider = self.get_provider();
            let agent_id = self.get_id().to_string();
            let stream_task_id = task_id.to_string();
            let stream_werk = Arc::clone(werk);
            let emit_stream: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(move |event| {
                let event = match event {
                    StreamEvent::TextDelta { text, .. } => Event::new(Event::TEXT_CHUNK_RECEIVED)
                        .data(serde_json::json!({ "content": text })),
                    StreamEvent::ToolCallRepaired { tool_name, call_id } => {
                        Event::new(Event::TOOL_CALL_REPAIRED).data(serde_json::json!({
                            "tool_name": tool_name,
                            "call_id": call_id,
                            "kind": "call_malformed",
                            "message": "rebuilt from text",
                        }))
                    }
                    StreamEvent::ToolCallDeclined { tool_name, kind } => {
                        Event::new(Event::TOOL_CALL_DECLINED).data(serde_json::json!({
                            "tool_name": tool_name,
                            "kind": kind,
                        }))
                    }
                };
                stream_werk.emit_event(event.task_id(&stream_task_id).agent_id(&agent_id));
            });
            let outcome = tokio::select! {
                biased;
                _ = werk.run.until_draining() => return Ok(None),
                result = provider.respond(request.clone(), emit_stream) => result,
            };
            match outcome {
                Ok(response) => break response,
                Err(error @ ProviderError::ContextWindowExceeded { .. }) => return Err(error),
                Err(error) if error.is_retryable() => match retry.try_consume() {
                    Some(attempt) => {
                        let delay = retry.delay(error.get_retry_delay());
                        self.emit_event(
                            werk,
                            task_id,
                            Event::new(Event::REQUEST_RETRIED).data(serde_json::json!({
                                "model": request.model,
                                "attempt": attempt,
                                "max_attempts": retry.max_attempts(),
                                "kind": error.get_kind(),
                                "message": error.to_string(),
                            })),
                        );
                        tokio::select! {
                            biased;
                            _ = werk.run.until_draining() => return Ok(None),
                            _ = tokio::time::sleep(delay) => {}
                        }
                    }
                    None => {
                        self.fail_request(werk, task_id, &error);
                        return Err(error);
                    }
                },
                Err(error) => {
                    self.fail_request(werk, task_id, &error);
                    return Err(error);
                }
            }
        };

        werk.append_reply(task_id, Reply::assistant(&response.content));
        self.emit_event(
            werk,
            task_id,
            Event::new(Event::REQUEST_FINISHED).data(serde_json::json!({
                "model": response.model,
                "usage": response.usage,
            })),
        );
        Ok(Some(
            response
                .content
                .into_iter()
                .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
                .collect(),
        ))
    }

    fn fail_request(&self, werk: &Werk, task_id: &str, error: &ProviderError) {
        self.emit_event(
            werk,
            task_id,
            Event::new(Event::REQUEST_FAILED).data(serde_json::json!({
                "model": self.get_model().name,
                "kind": error.get_kind(),
                "message": error.to_string(),
            })),
        );
        self.fail_task(werk, task_id);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::agents::policy::Policy;
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tasks::Status;

    // Request retries

    #[tokio::test]
    async fn retry_succeeds_after_rate_limit() {
        let provider = MockProvider::with_results(vec![
            Err(rate_limit()),
            Err(rate_limit()),
            Ok(write_result_response("ok")),
        ]);
        let (events, provider, task) = run_one(provider, 3, 10, None).await;

        assert_eq!(provider.requests(), 3);
        assert_eq!(retries_in(&events).len(), 2);
        assert!(failures_in(&events).is_empty());
        assert_eq!(task.status, Status::Finished);
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
    async fn the_finish_tool_the_model_is_shown_carries_the_tasks_schema() {
        use crate::schemas::Schema;
        let schema = Schema::new(serde_json::json!({
            "type": "object",
            "properties": { "verdict": { "type": "string" } },
            "required": ["verdict"],
        }))
        .expect("valid schema");
        let provider = MockProvider::with_results(vec![Ok(write_result_value(
            serde_json::json!({"verdict": "safe"}),
        ))]);
        let (_, provider, _) = run_one(provider, 3, 10, Some(schema)).await;

        let tools = provider.received_tools();
        let finish = tools[0]
            .iter()
            .find(|tool| tool.get_name() == "finish")
            .expect("finish is sent with every request");
        let shown = finish.get_input_schema().get_raw_schema();
        assert!(shown["properties"]["verdict"].is_object(), "{shown}");
        assert_eq!(shown["required"], serde_json::json!(["verdict"]), "{shown}");
    }

    #[tokio::test]
    async fn happy_path_emits_no_request_failed() {
        let provider = MockProvider::with_results(vec![Ok(write_result_response("ok"))]);
        let (events, _, task) = run_one(provider, 3, 10, None).await;

        assert!(retries_in(&events).is_empty());
        assert!(failures_in(&events).is_empty());
        assert_eq!(task.status, Status::Finished);
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
    async fn terminal_provider_error_marks_task_failed() {
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
            let (_, _, task) = run_one(provider, 3, 10, None).await;
            assert_eq!(
                task.status,
                Status::Failed,
                "terminal provider error must transition task to Failed"
            );
        }
    }

    #[tokio::test]
    async fn retry_exhausted_marks_task_failed() {
        let provider = MockProvider::with_results(vec![
            Err(rate_limit()),
            Err(rate_limit()),
            Err(rate_limit()),
        ]);
        let (_, _, task) = run_one(provider, 2, 10, None).await;
        assert_eq!(
            task.status,
            Status::Failed,
            "exhausted retry budget must transition task to Failed"
        );
    }

    // Backoff timing

    #[tokio::test(start_paused = true)]
    async fn request_retried_fires_after_backoff_sleep_not_before() {
        use crate::agents::agent::Agent;
        use crate::agents::tasks::Werk;
        use crate::event::Event;
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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 3,
                request_retry_delay: Duration::from_millis(1),
                ..Default::default()
            });
        werk.on_event(move |_, e| handler(e));
        werk.add_agent(Agent::new().provider(provider).model("mock").role("test"));
        werk.add_task("go");

        let run_fut = werk.finish_all_tasks();
        let check_fut = async {
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            let retries = || {
                collected
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|e| e.get_name() == Event::REQUEST_RETRIED)
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
        use crate::agents::tasks::Werk;
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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 3,
                request_retry_delay: Duration::from_secs(60),
                ..Default::default()
            });
        werk.on_event(move |_, e| handler(e));
        werk.add_agent(Agent::new().provider(provider).model("mock").role("test"));
        werk.add_task("go");

        let run_fut = werk.finish_all_tasks();
        let cancel_handle = Arc::clone(&werk);
        let cancel_fut = async {
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            cancel_handle.cancel_all_tasks();
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

    // Editing replies from an event handler

    use std::sync::Arc;

    use serde_json::Value;

    use crate::agents::agent::Agent;
    use crate::agents::tasks::{Reply, ReplyContent, Werk};
    use crate::event::Event;
    use crate::providers::{ContentBlock, Message};
    use crate::tools::Tool;

    type BoomHandler = Box<dyn Fn(&Arc<Werk>, &Event) + Send + Sync>;

    /// Run a task whose first turn calls a tool that always fails, then
    /// writes a result. Registers `handler` when one is given. Returns the
    /// provider and temp dir so callers can inspect the requests and reload.
    async fn run_boom(
        handler: Option<BoomHandler>,
    ) -> (Arc<MockProvider>, Arc<Werk>, crate::test_util::TempDir) {
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("boom")),
            Ok(write_result_response("done")),
        ]);
        let boom = Tool::new("boom")
            .description("Always fails")
            .handler(|_: Value| async move { Event::error("boom") });
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
        if let Some(handler) = handler {
            werk.on_event(handler);
        }
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .tool(boom),
        );
        werk.add_task("go");
        let _ = werk.finish_all_tasks().await;
        (provider, werk, results_dir)
    }

    /// Handler that drops the whole failed tool exchange once a tool call
    /// fails: both the assistant's tool_use and the failed tool_result, so
    /// no unpaired block is left behind.
    fn drop_failed_exchange(werk: &Arc<Werk>, event: &Event) {
        if event.get_name() != Event::TOOL_CALL_FAILED {
            return;
        }
        werk.edit_replies(&event.task_id, |replies| {
            replies.retain(|reply| {
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
        });
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
    async fn an_event_handler_drops_the_failed_tool_exchange() {
        let (provider, werk, _dir) = run_boom(Some(Box::new(drop_failed_exchange))).await;

        // The handler dropped both sides of the boom exchange, so the second
        // request carries no tool blocks.
        assert!(
            !has_tool_blocks(&provider.received()[1]),
            "boom exchange must be gone: {:?}",
            provider.received()[1],
        );
        assert_eq!(werk.get_tasks()[0].status, Status::Finished);
    }

    #[tokio::test]
    async fn an_event_handler_injects_a_message_into_the_next_request() {
        let (provider, _tasks, _dir) =
            run_boom(Some(Box::new(|werk: &Arc<Werk>, event: &Event| {
                if event.get_name() != Event::TOOL_CALL_FAILED {
                    return;
                }
                werk.edit_replies(&event.task_id, |replies| {
                    replies.push(Reply::user_text("HANDLER HINT: change approach"));
                });
            })))
            .await;

        assert!(user_text(&provider.received()[1]).contains("HANDLER HINT"));
    }

    #[tokio::test]
    async fn an_event_handler_rewrites_a_reply_in_place() {
        let (provider, _tasks, _dir) =
            run_boom(Some(Box::new(|werk: &Arc<Werk>, event: &Event| {
                if event.get_name() != Event::TOOL_CALL_FAILED {
                    return;
                }
                werk.edit_replies(&event.task_id, |replies| {
                    for reply in replies.iter_mut() {
                        for block in reply.content.iter_mut() {
                            if let ReplyContent::ToolResult { content, .. } = block {
                                *content = "REWRITTEN".into();
                            }
                        }
                    }
                });
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
        let (_provider, _tasks, dir) = run_boom(Some(Box::new(drop_failed_exchange))).await;

        let reloaded = Werk::load(dir.path()).unwrap();
        let task = reloaded.get_tasks().into_iter().next().unwrap();
        // The boom exchange is gone; the later finish_task call remains.
        let keeps_boom = task.replies.iter().any(|reply| {
            reply.content.iter().any(|b| match b {
                ReplyContent::ToolUse { name, .. } => name == "boom",
                ReplyContent::ToolResult { succeeded, .. } => !succeeded,
                _ => false,
            })
        });
        assert!(!keeps_boom, "reloaded replies must reflect the edit");
    }

    #[tokio::test]
    async fn no_handler_leaves_the_boom_exchange_in_the_replies() {
        let (provider, _tasks, _dir) = run_boom(None).await;

        assert!(
            has_tool_blocks(&provider.received()[1]),
            "without a handler the boom exchange must remain",
        );
    }
}
