//! Runs the tools a model asks for, writes out oversized results, and counts
//! consecutive failures against the retry budget.

use std::sync::Arc;

use crate::agents::agent::Agent;
use crate::agents::policy::Policy;
use crate::agents::tasks::{Reply, Werk};
use crate::agents::PolicyViolation;
use crate::event::Event;
use crate::prompts::directives::{NO_TOOLS_REGISTERED, TOOL_NOT_FOUND, TOOL_PANICKED};
use crate::providers::ContentBlock;
use crate::tools::{Tool, ToolContext};

const MAX_CONCURRENT_CALLS: usize = 10;

impl Agent {
    pub(super) async fn call_tools(
        &self,
        werk: &Arc<Werk>,
        task_id: &str,
        tools: &[Tool],
        mut calls: Vec<ContentBlock>,
        policy: &Policy,
        consecutive_schema_failures: &mut u32,
    ) -> bool {
        let max_schema_retries = policy.max_schema_retries.unwrap_or(u32::MAX);

        for call in &mut calls {
            let ContentBlock::ToolUse { id, name, .. } = call else {
                continue;
            };
            let Some(tool) = self.get_tool(tools, name) else {
                continue;
            };
            let registered = tool.get_name().to_string();
            if registered != *name {
                self.emit_event(
                    werk,
                    task_id,
                    Event::new(Event::TOOL_CALL_REPAIRED).data(serde_json::json!({
                        "tool_name": registered,
                        "call_id": id,
                        "kind": "call_malformed",
                        "message": format!("resolved from `{name}`"),
                    })),
                );
                *name = registered;
            }
        }

        for call in &calls {
            let ContentBlock::ToolUse { id, name, input } = call else {
                continue;
            };
            self.emit_event(
                werk,
                task_id,
                Event::new(Event::TOOL_CALL_STARTED).data(serde_json::json!({
                    "tool_name": name,
                    "call_id": id,
                    "input": input,
                })),
            );
        }

        let tool_context = ToolContext::new(self.get_dir())
            .run(Arc::clone(&werk.run))
            .werk(Arc::clone(werk))
            .agent_id(self.get_id().to_string())
            .task_id(task_id.to_string())
            .directives(self.get_directives());
        let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CALLS));
        let mut answers: Vec<Option<Event>> = calls.iter().map(|_| None).collect();
        let mut cursor = 0;

        while cursor < calls.len() {
            let concurrent = self.tool_is_concurrent(tools, &calls[cursor]);
            if !concurrent {
                let future = self.call_tool(tools, &calls[cursor], &tool_context);
                answers[cursor] = tokio::spawn(future).await.ok();
                cursor += 1;
                continue;
            }

            let start = cursor;
            while cursor < calls.len() && self.tool_is_concurrent(tools, &calls[cursor]) {
                cursor += 1;
            }
            let mut running = tokio::task::JoinSet::new();
            for (offset, call) in calls[start..cursor].iter().enumerate() {
                let index = start + offset;
                let semaphore = Arc::clone(&semaphore);
                let future = self.call_tool(tools, call, &tool_context);
                running.spawn(async move {
                    let _permit = semaphore.acquire().await.unwrap();
                    (index, future.await)
                });
            }
            while let Some(joined) = running.join_next().await {
                if let Ok((index, result)) = joined {
                    answers[index] = Some(result);
                }
            }
        }

        let directives = self.get_directives();
        let mut results: Vec<Event> = calls
            .iter()
            .zip(answers)
            .map(|(call, answer)| {
                let tool_name = match call {
                    ContentBlock::ToolUse { name, .. } => name.as_str(),
                    _ => "unknown",
                };
                answer.unwrap_or_else(|| {
                    Event::error(directives.render(TOOL_PANICKED, &[("tool", tool_name)]))
                        .directive(TOOL_PANICKED)
                })
            })
            .collect();
        Event::cap_tool_results(&calls, &mut results, &tool_context);

        let mut first_schema_failure: Option<String> = None;
        let mut offloaded = std::collections::HashMap::new();
        let mut blocks: Vec<ContentBlock> = Vec::with_capacity(results.len());
        let mut events: Vec<Event> = Vec::with_capacity(results.len());
        for (call, result) in calls.iter().zip(results) {
            let ContentBlock::ToolUse { id, name, .. } = call else {
                continue;
            };
            for message in result.repairs() {
                events.push(
                    Event::new(Event::TOOL_CALL_REPAIRED).data(serde_json::json!({
                        "tool_name": name,
                        "call_id": id,
                        "kind": "value_mistyped",
                        "message": message,
                    })),
                );
            }
            let succeeded = result.get_name() == Event::TOOL_CALL_FINISHED;
            let content = if succeeded {
                let content = result.get_content().to_string();
                *consecutive_schema_failures = 0;
                let mut event = result.clone();
                if let Some(data) = event.data.as_object_mut() {
                    data.insert("tool_name".into(), name.clone().into());
                    data.insert("call_id".into(), id.clone().into());
                }
                events.push(event);
                if let Some(path) = result.output_path() {
                    offloaded.insert(id.clone(), path.into());
                }
                content
            } else {
                let content = result.get_content().to_string();
                let kind = result.get_data()["kind"]
                    .as_str()
                    .unwrap_or("execution_failed");
                *consecutive_schema_failures = consecutive_schema_failures.saturating_add(1);
                if kind == "schema_failed" && first_schema_failure.is_none() {
                    first_schema_failure = Some(content.clone());
                }
                let mut event = result.clone();
                if let Some(data) = event.data.as_object_mut() {
                    data.insert("tool_name".into(), name.clone().into());
                    data.insert("call_id".into(), id.clone().into());
                    data.insert("kind".into(), kind.into());
                }
                events.push(event);
                content
            };
            blocks.push(ContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content,
                succeeded,
            });
        }

        if let Some(message) = first_schema_failure {
            events.push(Event::new(Event::SCHEMA_RETRIED).data(serde_json::json!({
                "attempt": consecutive_schema_failures,
                "max_attempts": max_schema_retries,
                "kind": "schema_failed",
                "message": message,
            })));
        }
        werk.append_reply(task_id, Reply::user(&blocks, &offloaded));
        for event in events {
            self.emit_event(werk, task_id, event);
        }

        if *consecutive_schema_failures >= max_schema_retries {
            self.emit_event(
                werk,
                task_id,
                Event::new(Event::POLICY_VIOLATED).data(serde_json::json!({
                    "policy": PolicyViolation::MaxSchemaRetries,
                    "limit": u64::from(max_schema_retries),
                })),
            );
            self.fail_task(werk, task_id);
            return false;
        }
        true
    }

    fn tool_is_concurrent(&self, tools: &[Tool], call: &ContentBlock) -> bool {
        let ContentBlock::ToolUse { name, .. } = call else {
            return false;
        };
        self.get_tool(tools, name)
            .is_some_and(|tool| tool.is_concurrent())
    }

    fn call_tool(
        &self,
        tools: &[Tool],
        call: &ContentBlock,
        context: &ToolContext,
    ) -> impl std::future::Future<Output = Event> + Send + 'static {
        let ContentBlock::ToolUse { name, input, .. } = call else {
            unreachable!("Agent::request returns only tool-use blocks");
        };
        let tool_name = name.clone();
        let input = input.clone();
        let context = context.clone();
        let tool = self.get_tool(tools, name);
        let available = {
            let mut names: Vec<_> = tools.iter().map(Tool::get_name).collect();
            names.sort();
            names.join(", ")
        };
        async move {
            let Some(tool) = tool else {
                let (directive, content) = if available.is_empty() {
                    (
                        NO_TOOLS_REGISTERED,
                        context
                            .directives
                            .render(NO_TOOLS_REGISTERED, &[("name", &tool_name)]),
                    )
                } else {
                    (
                        TOOL_NOT_FOUND,
                        context.directives.render(
                            TOOL_NOT_FOUND,
                            &[("name", &tool_name), ("available", &available)],
                        ),
                    )
                };
                return Event::tool_failure(content, "not_found").directive(directive);
            };
            tool.invoke(input, &context).await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::Value;

    use crate::agents::agent::Agent;
    use crate::agents::policy::Policy;
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tasks::{Status, Task, Werk};
    use crate::event::Event;
    use crate::providers::ContentBlock;
    use crate::schemas::Schema;

    #[tokio::test]
    async fn write_result_finishes_task_with_valid_json() {
        let provider = MockProvider::with_results(vec![Ok(write_result_value(
            serde_json::json!({"partial_sum": 42}),
        ))]);
        let (events, provider, task) =
            run_one(provider, 3, 10, Some(schema_for_partial_sum())).await;

        assert_eq!(provider.requests(), 1);
        let done = events
            .iter()
            .filter(|e| e.get_name() == Event::TASK_FINISHED)
            .count();
        let failed = events
            .iter()
            .filter(|e| e.get_name() == Event::TASK_FAILED)
            .count();
        assert_eq!(done, 1);
        assert_eq!(failed, 0);
        assert_eq!(task.status, Status::Finished);
        assert_eq!(task.result.as_ref().unwrap()["partial_sum"], 42);
    }

    #[tokio::test]
    async fn a_result_wrapper_against_an_inlined_task_is_rejected_and_names_the_fields() {
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("wrapped")),
            Ok(write_result_value(serde_json::json!({"partial_sum": 1}))),
        ]);
        let (events, _, task) = run_one(provider, 3, 10, Some(schema_for_partial_sum())).await;

        let schema_retries = schema_retries_in(&events);
        assert_eq!(schema_retries.len(), 1, "the wrapper is not unwrapped");
        assert!(
            schema_retries[0].2.contains("partial_sum"),
            "the rejection names the fields to send: {}",
            schema_retries[0].2
        );
        assert_eq!(task.status, Status::Finished, "one retry recovers");
    }

    // Schema retries

    #[tokio::test]
    async fn schema_violation_emits_schema_retried_with_attempt_numbers() {
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("not json")),
            Ok(write_result_response("not json again")),
            Ok(write_result_value(serde_json::json!({"partial_sum": 42}))),
        ]);
        let (events, _, task) = run_one(provider, 3, 10, Some(schema_for_partial_sum())).await;

        let schema_retries = schema_retries_in(&events);
        let attempts: Vec<u32> = schema_retries.iter().map(|(a, ..)| *a).collect();
        assert_eq!(attempts, vec![1, 2]);
        for (_, max_attempts, _) in &schema_retries {
            assert_eq!(*max_attempts, 10);
        }
        assert_eq!(task.status, Status::Finished);
    }

    #[tokio::test]
    async fn a_schema_retry_message_carries_the_validator_detail() {
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("not json")),
            Ok(write_result_value(serde_json::json!({"partial_sum": 1}))),
        ]);
        let (events, _, _) = run_one(provider, 3, 10, Some(schema_for_partial_sum())).await;
        let schema_retries = schema_retries_in(&events);
        assert_eq!(schema_retries.len(), 1);
        assert!(
            schema_retries[0].2.contains("Schema validation failed"),
            "retry message must carry validator detail: {:?}",
            schema_retries[0].2,
        );
    }

    #[tokio::test]
    async fn an_argument_violation_retries_against_the_failing_tools_own_schema() {
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("task")),
            Ok(write_result_value(serde_json::json!({"partial_sum": 1}))),
        ]);
        let (events, _, _) = run_one(provider, 3, 10, Some(schema_for_partial_sum())).await;

        let schema_retries = schema_retries_in(&events);
        assert_eq!(schema_retries.len(), 1);
        let message = &schema_retries[0].2;
        assert!(message.contains("`task` rejected"), "{message}");
        assert!(!events.iter().any(|event| {
            event.get_name() == Event::TOOL_CALL_REPAIRED
                && event.get_data()["kind"] == "value_mistyped"
        }));
        // `partial_sum` belongs to the finish tool's schema, which is the one
        // the message must not print.
        assert!(!message.contains("partial_sum"), "{message}");
    }

    #[tokio::test]
    async fn a_conditional_requirement_retries_against_a_schema_that_states_it() {
        // The message and the shape printed beside it have to agree. A model
        // told `slug` is missing, beside a schema whose `required` list holds
        // only `action`, has nothing to correct against.
        let write_without_slug = crate::providers::types::ModelResponse {
            content: vec![crate::providers::ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "knowledge".into(),
                input: serde_json::json!({"action": "write", "description": "d", "content": "c"}),
            }],
            status: crate::providers::types::ResponseStatus::ToolUse,
            usage: crate::providers::types::TokenUsage::default(),
            model: "mock".into(),
        };
        let provider = MockProvider::with_results(vec![
            Ok(write_without_slug),
            Ok(write_result_value(serde_json::json!({"partial_sum": 1}))),
        ]);
        let (events, _, _) = run_one(provider, 3, 10, Some(schema_for_partial_sum())).await;

        let schema_retries = schema_retries_in(&events);
        assert_eq!(schema_retries.len(), 1);
        let message = &schema_retries[0].2;
        assert!(
            message.contains("missing required property `slug`"),
            "{message}"
        );

        // The shape printed under the message rejects the same call for the
        // same reason, so what the model reads back is what it was held to.
        let shape = message.split("accepts:").nth(1).expect("schema is printed");
        let advertised = Schema::new(serde_json::from_str(shape.trim()).expect("valid JSON"))
            .expect("a schema the model was shown");
        let violations = advertised
            .validate(serde_json::json!({"action": "write", "description": "d", "content": "c"}))
            .unwrap_err();
        assert!(
            violations.iter().any(|v| v.message.contains("`slug`")),
            "{{ violations }}"
        );
    }

    #[tokio::test]
    async fn a_schema_miss_answers_in_the_tool_result_and_injects_no_directive() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("not json")),
            Ok(write_result_value(serde_json::json!({"partial_sum": 1}))),
        ]);
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(10),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test"),
        );
        werk.add_task(Task::new("go").schema(schema_for_partial_sum()));

        let _ = werk.finish().await;

        let second = &provider.received()[1];
        let answered = second
            .iter()
            .filter_map(|message| match message {
                crate::providers::Message::User { content } => {
                    content.iter().find_map(|block| match block {
                        crate::providers::ContentBlock::ToolResult { content, .. } => {
                            Some(content.clone())
                        }
                        _ => None,
                    })
                }
                _ => None,
            })
            .next()
            .expect("the rejected call is answered");
        assert!(answered.contains("rejected your arguments"), "{answered}");
        assert!(answered.contains("partial_sum"), "{answered}");
        assert!(
            !user_text(second).contains("was not accepted"),
            "no directive block rides beside the tool result",
        );
    }

    #[tokio::test]
    async fn schema_retry_exhausted_emits_policy_violated_and_force_fails_task() {
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("nope")),
            Ok(write_result_response("still nope")),
            Ok(write_result_response("never")),
        ]);
        let (events, _, task) = run_one(provider, 3, 2, Some(schema_for_partial_sum())).await;

        let policy_violated = events.iter().any(|e| {
            e.get_name() == Event::POLICY_VIOLATED
                && e.get_data()["policy"] == "max_schema_retries"
                && e.get_data()["limit"] == 2
        });
        assert!(policy_violated, "expected MaxSchemaRetries PolicyViolated");
        assert_eq!(task.status, Status::Failed);
    }

    // Repeated failed tool calls (not just schema failures) count toward the budget.

    #[tokio::test]
    async fn repeated_unknown_tool_calls_trip_the_budget_and_fail_the_task() {
        // The tester agent has no `ghost_tool`, so every call is ToolNotFound;
        // three of them exhaust a budget of three and fail the task.
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("ghost_tool")),
            Ok(tool_call_response("ghost_tool")),
            Ok(tool_call_response("ghost_tool")),
        ]);
        let (events, _, task) = run_one(provider, 0, 3, None).await;

        assert!(
            events.iter().any(|e| {
                e.get_name() == Event::POLICY_VIOLATED
                    && e.get_data()["policy"] == "max_schema_retries"
                    && e.get_data()["limit"] == 3
            }),
            "expected MaxSchemaRetries PolicyViolated",
        );
        assert_eq!(task.status, Status::Failed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_calls_finish_before_the_serial_call_between_batches() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        use crate::providers::types::{ModelResponse, ResponseStatus, TokenUsage};
        use crate::providers::ContentBlock;
        use crate::tools::Tool;

        let response = ModelResponse {
            content: vec![
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "work".into(),
                    input: serde_json::json!({"step": 1}),
                },
                ContentBlock::ToolUse {
                    id: "c2".into(),
                    name: "work".into(),
                    input: serde_json::json!({"step": 2}),
                },
                ContentBlock::ToolUse {
                    id: "c3".into(),
                    name: "serial".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::ToolUse {
                    id: "c4".into(),
                    name: "work".into(),
                    input: serde_json::json!({"step": 3}),
                },
            ],
            status: ResponseStatus::ToolUse,
            usage: TokenUsage::default(),
            model: "mock".into(),
        };
        let provider =
            MockProvider::with_results(vec![Ok(response), Ok(write_result_response("done"))]);
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let completed = Arc::new(AtomicUsize::new(0));
        let serial_ran = Arc::new(AtomicBool::new(false));
        let work = {
            let barrier = Arc::clone(&barrier);
            let completed = Arc::clone(&completed);
            let serial_ran = Arc::clone(&serial_ran);
            Tool::new("work")
                .description("concurrent work")
                .concurrent(true)
                .handler(move |input: Value| {
                    let barrier = Arc::clone(&barrier);
                    let completed = Arc::clone(&completed);
                    let serial_ran = Arc::clone(&serial_ran);
                    async move {
                        let step = input["step"].as_u64().unwrap();
                        if step < 3 {
                            barrier.wait().await;
                        } else {
                            assert!(serial_ran.load(Ordering::SeqCst));
                        }
                        completed.fetch_add(1, Ordering::SeqCst);
                        Event::success("done")
                    }
                })
        };
        let serial = {
            let completed = Arc::clone(&completed);
            let serial_ran = Arc::clone(&serial_ran);
            Tool::new("serial")
                .description("serial work")
                .handler(move |_: Value| {
                    let completed = Arc::clone(&completed);
                    let serial_ran = Arc::clone(&serial_ran);
                    async move {
                        assert_eq!(completed.load(Ordering::SeqCst), 2);
                        serial_ran.store(true, Ordering::SeqCst);
                        Event::success("done")
                    }
                })
        };

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_secs(1)),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(work)
                .tool(serial),
        );
        werk.add_task("go");
        let _ = werk.finish().await;

        assert_eq!(completed.load(Ordering::SeqCst), 3);
        assert!(serial_ran.load(Ordering::SeqCst));
        assert_eq!(werk.get_tasks()[0].status, Status::Finished);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_call_is_answered_without_losing_the_other_results() {
        use crate::providers::types::{ModelResponse, ResponseStatus, TokenUsage};
        use crate::providers::ContentBlock;
        use crate::tools::Tool;

        let response = ModelResponse {
            content: vec![
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "explode".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::ToolUse {
                    id: "c2".into(),
                    name: "steady".into(),
                    input: serde_json::json!({}),
                },
            ],
            status: ResponseStatus::ToolUse,
            usage: TokenUsage::default(),
            model: "mock".into(),
        };
        let provider =
            MockProvider::with_results(vec![Ok(response), Ok(write_result_response("done"))]);
        let explode = Tool::new("explode")
            .description("panics")
            .concurrent(true)
            .handler(|_: Value| async { panic!("boom") });
        let steady = Tool::new("steady")
            .description("succeeds")
            .concurrent(true)
            .handler(|_: Value| async { Event::success("ok") });
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_secs(1)),
                ..Default::default()
            });
        let events = collect_events(&werk);
        werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(explode)
                .tool(steady),
        );
        werk.add_task("go");
        let _ = werk.finish().await;

        let events = events.lock().unwrap();
        assert!(events.iter().any(|event| {
            event.get_name() == Event::TOOL_CALL_FAILED
                && event.get_data()["tool_name"] == "explode"
                && event.get_content().contains("panicked")
        }));
        assert!(events.iter().any(|event| {
            event.get_name() == Event::TOOL_CALL_FINISHED
                && event.get_data()["tool_name"] == "steady"
                && event.get_content() == "ok"
        }));
        assert_eq!(werk.get_tasks()[0].status, Status::Finished);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_calls_have_independent_timeouts_and_the_agent_continues() {
        use crate::providers::types::{ModelResponse, ResponseStatus, TokenUsage};
        use crate::providers::ContentBlock;
        use crate::tools::Tool;

        let response = ModelResponse {
            content: vec![
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "wait".into(),
                    input: serde_json::json!({"milliseconds": 500}),
                },
                ContentBlock::ToolUse {
                    id: "c2".into(),
                    name: "wait".into(),
                    input: serde_json::json!({"milliseconds": 1}),
                },
            ],
            status: ResponseStatus::ToolUse,
            usage: TokenUsage::default(),
            model: "mock".into(),
        };
        let provider =
            MockProvider::with_results(vec![Ok(response), Ok(write_result_response("done"))]);
        let wait = Tool::new("wait")
            .description("Wait for the requested duration")
            .schema(serde_json::json!({
                "type": "object",
                "properties": {"milliseconds": {"type": "integer"}},
                "required": ["milliseconds"],
            }))
            .concurrent(true)
            .timeout(Duration::from_millis(100))
            .handler(|input: Value| async move {
                tokio::time::sleep(Duration::from_millis(
                    input["milliseconds"].as_u64().unwrap(),
                ))
                .await;
                Event::success("done")
            });
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_time: Some(Duration::from_secs(2)),
                ..Default::default()
            });
        let events = collect_events(&werk);
        werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(wait),
        );
        werk.add_task("go");

        let _ = werk.finish().await;

        let events = events.lock().unwrap();
        assert!(events.iter().any(|event| {
            event.get_name() == Event::TOOL_CALL_FAILED
                && event.get_data()["tool_name"] == "wait"
                && event.get_directive() == Some(crate::prompts::directives::TOOL_TIMED_OUT)
        }));
        assert!(events.iter().any(|event| {
            event.get_name() == Event::TOOL_CALL_FINISHED
                && event.get_data()["tool_name"] == "wait"
                && event.get_content() == "done"
        }));
        assert_eq!(werk.get_tasks()[0].status, Status::Finished);
    }

    #[tokio::test]
    async fn a_hallucinated_tool_name_finishes_the_task_under_the_registered_name() {
        let provider = MockProvider::with_results(vec![Ok(write_result_response_named(
            "finish_tool",
            "done",
        ))]);
        let (events, _, task) = run_one(provider, 0, 3, None).await;

        assert_eq!(task.status, Status::Finished);
        let names: Vec<&str> = events
            .iter()
            .filter_map(|e| {
                (e.get_name() == Event::TOOL_CALL_STARTED)
                    .then(|| e.get_data()["tool_name"].as_str())
                    .flatten()
            })
            .collect();
        assert_eq!(names, vec!["finish"]);
    }

    #[tokio::test]
    async fn a_hallucinated_tool_name_is_reported_as_a_repair() {
        let provider = MockProvider::with_results(vec![Ok(write_result_response_named(
            "finish_tool",
            "done",
        ))]);
        let (events, _, _) = run_one(provider, 0, 3, None).await;

        let repairs: Vec<(&str, &str, &str, &str)> = events
            .iter()
            .filter_map(|e| {
                (e.get_name() == Event::TOOL_CALL_REPAIRED).then(|| {
                    Some((
                        e.get_data()["tool_name"].as_str()?,
                        e.get_data()["call_id"].as_str()?,
                        e.get_data()["kind"].as_str()?,
                        e.get_data()["message"].as_str()?,
                    ))
                })?
            })
            .collect();
        assert_eq!(
            repairs,
            vec![(
                "finish",
                "call-1",
                "call_malformed",
                "resolved from `finish_tool`"
            )]
        );
    }

    #[tokio::test]
    async fn a_tool_name_that_needed_no_folding_reports_no_repair() {
        let provider = MockProvider::with_results(vec![Ok(write_result_response("done"))]);
        let (events, _, _) = run_one(provider, 0, 3, None).await;

        assert!(!events
            .iter()
            .any(|e| e.get_name() == Event::TOOL_CALL_REPAIRED));
    }

    #[tokio::test]
    async fn a_retyped_argument_emits_its_pointer_before_success() {
        let provider = MockProvider::with_results(vec![Ok(write_result_value(
            serde_json::json!({"partial_sum": "42"}),
        ))]);
        let (events, _, task) = run_one(provider, 3, 10, Some(schema_for_partial_sum())).await;

        assert_eq!(task.status, Status::Finished);
        let repairs: Vec<(&str, &str, &str, &str)> = events
            .iter()
            .filter_map(|e| {
                (e.get_name() == Event::TOOL_CALL_REPAIRED).then(|| {
                    Some((
                        e.get_data()["tool_name"].as_str()?,
                        e.get_data()["call_id"].as_str()?,
                        e.get_data()["kind"].as_str()?,
                        e.get_data()["message"].as_str()?,
                    ))
                })?
            })
            .collect();
        assert_eq!(
            repairs,
            vec![("finish", "call-1", "value_mistyped", "/partial_sum retyped")]
        );
        let sequence: Vec<&str> = events
            .iter()
            .filter(|event| event.get_data()["call_id"] == "call-1")
            .map(Event::get_name)
            .collect();
        assert_eq!(
            sequence,
            vec![
                Event::TOOL_CALL_STARTED,
                Event::TOOL_CALL_REPAIRED,
                Event::TOOL_CALL_FINISHED,
            ]
        );
        assert!(events.iter().any(|event| {
            event.get_name() == Event::TOOL_CALL_STARTED
                && event.get_data()["input"]["partial_sum"] == "42"
        }));
        assert!(events.iter().any(|event| {
            event.get_name() == Event::TOOL_CALL_FINISHED
                && event.get_data()["repairs"] == serde_json::json!(["/partial_sum retyped"])
        }));
    }

    #[tokio::test]
    async fn a_retyped_argument_is_reported_before_tool_failure() {
        let failed_call = crate::providers::types::ModelResponse {
            content: vec![ContentBlock::ToolUse {
                id: "boom-1".into(),
                name: "boom".into(),
                input: serde_json::json!({"count": "3"}),
            }],
            status: crate::providers::types::ResponseStatus::ToolUse,
            usage: crate::providers::types::TokenUsage::default(),
            model: "mock".into(),
        };
        let provider =
            MockProvider::with_results(vec![Ok(failed_call), Ok(write_result_response("done"))]);
        let boom = crate::tools::Tool::new("boom")
            .description("Always fails")
            .schema(serde_json::json!({
                "type": "object",
                "properties": {"count": {"type": "integer"}},
                "required": ["count"],
            }))
            .handler(|_: Value| async { Event::error("boom") });
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(3),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        let collected = collect_events(&werk);
        werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(boom),
        );
        werk.add_task("go");

        let _ = werk.finish().await;

        let events = collected.lock().unwrap().clone();
        let sequence: Vec<&str> = events
            .iter()
            .filter(|event| event.get_data()["call_id"] == "boom-1")
            .map(Event::get_name)
            .collect();
        assert_eq!(
            sequence,
            vec![
                Event::TOOL_CALL_STARTED,
                Event::TOOL_CALL_REPAIRED,
                Event::TOOL_CALL_FAILED,
            ]
        );
        assert!(events.iter().any(|event| {
            event.get_name() == Event::TOOL_CALL_STARTED
                && event.get_data()["input"]["count"] == "3"
        }));
        assert!(events.iter().any(|event| {
            event.get_name() == Event::TOOL_CALL_REPAIRED
                && event.get_data()["message"] == "/count retyped"
        }));
    }

    #[tokio::test]
    async fn repeated_execution_failures_trip_the_budget_and_fail_the_task() {
        use std::time::Duration;

        use crate::agents::agent::Agent;
        use crate::agents::tasks::Werk;
        use crate::tools::Tool;

        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("boom")),
            Ok(tool_call_response("boom")),
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
                max_schema_retries: Some(2),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        let collected = collect_events(&werk);
        werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(boom),
        );
        werk.add_task("go");
        let _ = werk.finish().await;

        assert_eq!(
            werk.get_tasks().into_iter().next().unwrap().status,
            Status::Failed
        );
        let events = collected.lock().unwrap().clone();
        assert!(events.iter().any(|e| {
            e.get_name() == Event::POLICY_VIOLATED
                && e.get_data()["policy"] == "max_schema_retries"
                && e.get_data()["limit"] == 2
        }));
    }

    /// The whole reply-editing pattern rests on this: a handler that rewrites
    /// the replies on a failure must find the result it is rewriting.
    #[tokio::test]
    async fn tool_call_failed_fires_after_its_tool_result_is_stored() {
        use std::sync::Mutex;
        use std::time::Duration;

        use crate::agents::agent::Agent;
        use crate::agents::tasks::{ReplyContent, Werk};
        use crate::tools::Tool;

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
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        // `None` until the handler runs, so the assertion also proves it did.
        let stored: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&stored);
        werk.on_event(move |werk, event| {
            if event.get_name() != Event::TOOL_CALL_FAILED {
                return;
            }
            let task = werk.get_task(&event.task_id).unwrap();
            let landed = task.replies.iter().any(|reply| {
                reply.content.iter().any(|block| {
                    matches!(
                        block,
                        ReplyContent::ToolResult {
                            succeeded: false,
                            ..
                        }
                    )
                })
            });
            *seen.lock().unwrap() = Some(landed);
        });
        werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(boom),
        );
        werk.add_task("go");
        let _ = werk.finish().await;

        assert_eq!(*stored.lock().unwrap(), Some(true));
    }

    #[tokio::test]
    async fn a_successful_tool_call_resets_the_failure_budget() {
        use std::time::Duration;

        use crate::agents::agent::Agent;
        use crate::agents::tasks::Werk;
        use crate::tools::Tool;

        // boom, ping, boom, finish: a budget of two would trip on the second
        // boom if the ping success did not reset the counter in between.
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("boom")),
            Ok(tool_call_response("ping")),
            Ok(tool_call_response("boom")),
            Ok(write_result_response("done")),
        ]);
        let boom = Tool::new("boom")
            .description("Always fails")
            .handler(|_: Value| async move { Event::error("boom") });
        let ping = Tool::new("ping")
            .description("Always succeeds")
            .handler(|_: Value| async move { Event::success("pong") });

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(2),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(boom)
                .tool(ping),
        );
        werk.add_task("go");
        let _ = werk.finish().await;

        assert_eq!(
            werk.get_tasks().into_iter().next().unwrap().status,
            Status::Finished
        );
    }

    #[tokio::test]
    async fn finish_awaits_completion_when_tool_call_is_in_flight() {
        use std::time::Duration;
        use tokio::sync::Notify;

        use crate::agents::agent::Agent;
        use crate::agents::tasks::Werk;
        use crate::tools::{TaskTool, Tool};

        let tool_started = Arc::new(Notify::new());
        let tool_unblocked = Arc::new(Notify::new());
        let tool_started_clone = Arc::clone(&tool_started);
        let tool_unblocked_clone = Arc::clone(&tool_unblocked);

        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("slow_tool")),
            Ok(write_result_response("done")),
        ]);

        let slow_tool = Tool::new("slow_tool")
            .description("Blocks until released")
            .handler(move |_: Value| {
                let s = Arc::clone(&tool_started_clone);
                let u = Arc::clone(&tool_unblocked_clone);
                async move {
                    s.notify_one();
                    u.notified().await;
                    Event::success("ok")
                }
            });

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(10),
                max_time: Some(Duration::from_secs(5)),
                ..Default::default()
            });
        werk.add_agent(
            Agent::new()
                .provider(provider)
                .model("mock")
                .role("test")
                .tool(TaskTool)
                .tool(slow_tool),
        );
        werk.add_task("go");

        let unblock = async move {
            tool_started.notified().await;
            tool_unblocked.notify_one();
        };

        tokio::join!(werk.finish(), unblock);
        assert_eq!(
            werk.get_tasks().into_iter().next().unwrap().status,
            Status::Finished
        );
    }

    // Tool result offloading

    #[tokio::test]
    async fn huge_tool_result_is_persisted_to_task_outputs_dir_and_task_finishes_done() {
        use crate::agents::tasks::ReplyContent;
        use crate::tools::Tool;

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
        let handler: Arc<dyn Fn(&crate::event::Event) + Send + Sync> = {
            let c = Arc::clone(&collected);
            Arc::new(move |e: &crate::event::Event| c.lock().unwrap().push(e.clone()))
        };

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

        let dump = Tool::new("dump")
            .description("Returns ~800 KB of text")
            .handler(|_: Value| async move { Event::success("x".repeat(800_000)) });

        werk.on_event(move |_, e| handler(e));
        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("claude-sonnet-4-20250514")
                .role("test")
                .tool(dump),
        );
        werk.add_task("go");

        let _ = werk.finish().await;
        let events = collected.lock().unwrap().clone();
        let task = werk
            .get_tasks()
            .into_iter()
            .next()
            .expect("task must exist");

        assert_eq!(provider.requests(), 2);
        assert!(failures_in(&events).is_empty());
        assert_eq!(task.status, Status::Finished);

        let relative_path: std::path::PathBuf =
            ["tasks", "t-1", "outputs", "call-1.txt"].iter().collect();
        let output_path = results_dir.path().join(&relative_path);
        let body = std::fs::read_to_string(&output_path).expect("offload file must exist");
        assert_eq!(body, "x".repeat(800_000));

        let tool_result_path = task.replies.iter().find_map(|r| {
            r.content.iter().find_map(|b| match b {
                ReplyContent::ToolResult {
                    tool_use_id, path, ..
                } if tool_use_id == "call-1" => path.clone(),
                _ => None,
            })
        });
        assert_eq!(tool_result_path.as_deref(), Some(relative_path.as_path()));
        assert!(events.iter().any(|event| {
            event.get_name() == Event::TOOL_CALL_FINISHED
                && event.get_data()["call_id"] == "call-1"
                && event.get_data()["output_path"] == relative_path.to_string_lossy().as_ref()
        }));

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
        use crate::tools::Tool;

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
        let werk = Werk::new();
        werk.set_dir(results_dir.path().to_path_buf())
            .set_policy(Policy {
                max_request_retries: 0,
                request_retry_delay: Duration::from_millis(1),
                max_schema_retries: Some(10),
                max_time: Some(Duration::from_millis(500)),
                ..Default::default()
            });

        let size_tool = Tool::new("size_tool")
            .description("Returns N bytes of 'x'")
            .schema(serde_json::json!({
                "type": "object",
                "properties": {"bytes": {"type": "integer"}},
                "required": ["bytes"],
            }))
            .concurrent(true)
            .handler(|input: Value| async move {
                let bytes = input["bytes"].as_u64().unwrap_or(0) as usize;
                Event::success("x".repeat(bytes))
            });

        werk.add_agent(
            Agent::new()
                .provider(provider.clone())
                .model("mock")
                .role("test")
                .tool(size_tool),
        );
        werk.add_task("go");

        let _ = werk.finish().await;
        let task = werk
            .get_tasks()
            .into_iter()
            .next()
            .expect("task must exist");
        assert_eq!(task.status, Status::Finished);

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
            .join("tasks")
            .join("t-1")
            .join("outputs")
            .join("c1.txt");
        assert!(stub.contains(expected_path.to_string_lossy().as_ref()));

        let body = std::fs::read_to_string(&expected_path).unwrap();
        assert_eq!(body, "x".repeat(48_000));
    }

    fn schema_for_partial_sum() -> Schema {
        Schema::new(serde_json::json!({
            "type": "object",
            "properties": {
                "partial_sum": { "type": "integer" }
            },
            "required": ["partial_sum"]
        }))
        .expect("valid schema")
    }
}
