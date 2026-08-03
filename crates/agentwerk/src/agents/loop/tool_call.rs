//! Runs the tools a model asks for, writes out oversized results, and counts
//! consecutive failures against the retry budget.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::event::{EventKind, PolicyKind, ToolFailureKind};
use crate::prompts::schema_retry_detail;
use crate::providers::ContentBlock;
use crate::tools::{ToolCall, ToolContext, ToolError};

use super::agent::TicketContext;
use super::Step;

pub(super) async fn run(context: &mut TicketContext<'_>, mut calls: Vec<ToolCall>) -> Step {
    let max_schema_retries = context.policies.max_schema_retries.unwrap_or(u32::MAX);

    // Report the registered name, so a model alternating spellings of one tool
    // does not split its row in `tool_stats()`.
    for call in &mut calls {
        if let Some(tool) = context.agent.tool_registry().get(&call.name) {
            call.name = tool.name().to_string();
        }
    }

    for call in &calls {
        context.emit(EventKind::ToolCallStarted {
            tool_name: call.name.clone(),
            call_id: call.id.clone(),
            input: call.input.clone(),
        });
    }
    let tool_context = ToolContext::new(context.agent.dir())
        .interrupt_signal(std::sync::Arc::clone(&context.stop_signal))
        .registry(std::sync::Arc::new(context.agent.tool_registry().clone()))
        .ticket_queue(std::sync::Arc::clone(context.ticket_queue))
        .agent_name(context.agent.get_name().to_string())
        .ticket_key(context.ticket_key.clone())
        .knowledge(context.agent.knowledge());
    let outcomes = context
        .agent
        .tool_registry()
        .execute(&calls, &tool_context)
        .await;

    let mut schema_failure_message: Option<String> = None;
    for (block, tool_result, _path) in &outcomes {
        let ContentBlock::ToolResult { tool_use_id, .. } = block else {
            continue;
        };
        let call = calls.iter().find(|call| &call.id == tool_use_id);
        let tool_name = call.map(|call| call.name.clone()).unwrap_or_default();
        // The files this call opened. Feeds the per-path open tally;
        // empty for tools that open no file.
        let opened_paths = call
            .map(|call| {
                context
                    .agent
                    .tool_registry()
                    .get(&call.name)
                    .map(|tool| tool.opened_paths(&call.input))
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        match tool_result {
            Ok(output) => {
                // Any successful tool call is progress: clear the counter.
                context.consecutive_schema_failures = 0;
                for path in &opened_paths {
                    context.emit(EventKind::FileOpenFinished { path: path.clone() });
                }
                context.emit(EventKind::ToolCallFinished {
                    tool_name,
                    call_id: tool_use_id.clone(),
                    output: output.clone(),
                });
            }
            Err(error) => {
                // Any tool failure (bad arguments, unknown tool, schema
                // mismatch) counts toward the budget, so a stuck agent
                // fails its ticket instead of looping until the time limit.
                context.consecutive_schema_failures =
                    context.consecutive_schema_failures.saturating_add(1);
                if matches!(error, ToolError::SchemaValidationFailed { .. })
                    && schema_failure_message.is_none()
                {
                    schema_failure_message = Some(error.message());
                }
                let failure_kind = match error {
                    ToolError::ToolNotFound { .. } => ToolFailureKind::ToolNotFound,
                    ToolError::ExecutionFailed { .. } => ToolFailureKind::ExecutionFailed,
                    ToolError::SchemaValidationFailed { .. } => {
                        ToolFailureKind::SchemaValidationFailed
                    }
                };
                // A path fails with the call that named it, so it carries the
                // call's reason.
                for path in &opened_paths {
                    context.emit(EventKind::FileOpenFailed {
                        path: path.clone(),
                        reason: failure_kind,
                    });
                }
                context.emit(EventKind::ToolCallFailed {
                    tool_name,
                    call_id: tool_use_id.clone(),
                    reason: failure_kind,
                    message: error.message(),
                });
            }
        }
    }

    let mut paths: HashMap<String, PathBuf> = HashMap::new();
    let mut blocks: Vec<ContentBlock> = Vec::with_capacity(outcomes.len());
    for (block, _, path) in outcomes {
        if let (ContentBlock::ToolResult { tool_use_id, .. }, Some(path)) = (&block, path) {
            paths.insert(tool_use_id.clone(), path);
        }
        blocks.push(block);
    }
    if let Some(validator_message) = &schema_failure_message {
        let schema = context
            .ticket()
            .and_then(|ticket| ticket.schema)
            .and_then(|schema| serde_json::to_value(&schema).ok());
        let schema_detail = schema_retry_detail(validator_message, schema.as_ref());
        context.emit(EventKind::SchemaRetried {
            attempt: context.consecutive_schema_failures,
            max_attempts: max_schema_retries,
            message: schema_detail.clone(),
        });
        blocks.push(ContentBlock::Text {
            text: context.retry_directive(&schema_detail),
        });
    }
    context.ticket_queue.add_reply(
        &context.ticket_key,
        crate::agents::tickets::Reply::user(&blocks, &paths),
    );

    if context.consecutive_schema_failures >= max_schema_retries {
        context.emit(EventKind::PolicyViolated {
            policy: PolicyKind::MaxSchemaRetries,
            limit: u64::from(max_schema_retries),
        });
        let _ = context
            .ticket_queue
            .set_failed_by(&context.ticket_key, context.agent.get_name());
        return Step::NextTicket;
    }
    Step::CheckTicket
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::agents::agent::Agent;
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tickets::{Status, Ticket, TicketQueue};
    use crate::event::{EventKind, PolicyKind};
    use crate::providers::Provider;
    use crate::schemas::Schema;

    #[tokio::test]
    async fn write_result_finishes_ticket_with_valid_json() {
        let provider = MockProvider::with_results(vec![Ok(write_result_value(
            serde_json::json!({"partial_sum": 42}),
        ))]);
        let (events, provider, ticket) =
            run_one(provider, 3, 10, Some(schema_for_partial_sum())).await;

        assert_eq!(provider.requests(), 1);
        let done = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TicketFinished))
            .count();
        let failed = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TicketFailed))
            .count();
        assert_eq!(done, 1);
        assert_eq!(failed, 0);
        assert_eq!(ticket.status, Status::Finished);
        assert_eq!(ticket.result.as_ref().unwrap()["partial_sum"], 42);
    }

    // Schema retries

    #[tokio::test]
    async fn schema_violation_emits_schema_retried_with_attempt_numbers() {
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("not json")),
            Ok(write_result_response("not json again")),
            Ok(write_result_value(serde_json::json!({"partial_sum": 42}))),
        ]);
        let (events, _, ticket) = run_one(provider, 3, 10, Some(schema_for_partial_sum())).await;

        let schema_retries = schema_retries_in(&events);
        let attempts: Vec<u32> = schema_retries.iter().map(|(a, ..)| *a).collect();
        assert_eq!(attempts, vec![1, 2]);
        for (_, max_attempts, _) in &schema_retries {
            assert_eq!(*max_attempts, 10);
        }
        assert_eq!(ticket.status, Status::Finished);
    }

    #[tokio::test]
    async fn schema_retry_appends_directive_to_user_message() {
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
    async fn directive_editor_reads_the_reason_and_amends_the_default_directive() {
        let results_dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("not json")),
            Ok(write_result_value(serde_json::json!({"partial_sum": 1}))),
        ]);
        let tickets = TicketQueue::new();
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
                .edit_directive_on_retry(|detail, directive| {
                    *directive = format!("REDO NOW. {detail}\n{directive}")
                })
                .build(),
        );
        tickets.ticket(Ticket::new("go").schema(schema_for_partial_sum()));

        let _ = tickets.finish().await;

        let injected = user_text(&provider.received()[1]);
        assert!(
            injected.contains("REDO NOW."),
            "editor's text must be injected: {injected:?}",
        );
        assert!(
            injected.contains("Schema validation failed"),
            "editor must receive the bare validator reason: {injected:?}",
        );
        // The directive arrives pre-filled, so amending it keeps the framing.
        assert!(
            injected.contains("was not accepted"),
            "default directive must survive an editor that only prepends: {injected:?}",
        );
    }

    #[tokio::test]
    async fn schema_retry_exhausted_emits_policy_violated_and_force_fails_ticket() {
        let provider = MockProvider::with_results(vec![
            Ok(write_result_response("nope")),
            Ok(write_result_response("still nope")),
            Ok(write_result_response("never")),
        ]);
        let (events, _, ticket) = run_one(provider, 3, 2, Some(schema_for_partial_sum())).await;

        let policy_violated = events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::PolicyViolated {
                    policy: PolicyKind::MaxSchemaRetries,
                    limit: 2,
                },
            )
        });
        assert!(policy_violated, "expected MaxSchemaRetries PolicyViolated");
        assert_eq!(ticket.status, Status::Failed);
    }

    // Repeated failed tool calls (not just schema failures) count toward the budget.

    #[tokio::test]
    async fn repeated_unknown_tool_calls_trip_the_budget_and_fail_the_ticket() {
        // The tester agent has no `ghost_tool`, so every call is ToolNotFound;
        // three of them exhaust a budget of three and fail the ticket.
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("ghost_tool")),
            Ok(tool_call_response("ghost_tool")),
            Ok(tool_call_response("ghost_tool")),
        ]);
        let (events, _, ticket) = run_one(provider, 0, 3, None).await;

        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::PolicyViolated {
                    policy: PolicyKind::MaxSchemaRetries,
                    limit: 3,
                },
            )),
            "expected MaxSchemaRetries PolicyViolated",
        );
        assert_eq!(ticket.status, Status::Failed);
    }

    #[tokio::test]
    async fn a_hallucinated_tool_name_finishes_the_ticket_under_the_registered_name() {
        let provider = MockProvider::with_results(vec![Ok(write_result_response_named(
            "finish_tool",
            "done",
        ))]);
        let (events, _, ticket) = run_one(provider, 0, 3, None).await;

        assert_eq!(ticket.status, Status::Finished);
        let names: Vec<&str> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::ToolCallStarted { tool_name, .. } => Some(tool_name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["finish"]);
    }

    #[tokio::test]
    async fn repeated_execution_failures_trip_the_budget_and_fail_the_ticket() {
        use std::sync::Arc;
        use std::time::Duration;

        use crate::agents::agent::Agent;
        use crate::agents::tickets::TicketQueue;
        use crate::providers::Provider;
        use crate::tools::{Tool, ToolResult};

        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("boom")),
            Ok(tool_call_response("boom")),
        ]);
        let boom = Tool::new("boom", "Always fails")
            .handler(|_, _| async move { Ok(ToolResult::error("boom")) })
            .build();

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_schema_retries(2)
            .max_time(Duration::from_millis(500));
        let collected = collect_events(&tickets);
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(boom)
                .build(),
        );
        tickets.task("go");
        let _ = tickets.finish().await;

        assert_eq!(
            tickets.tickets().into_iter().next().unwrap().status,
            Status::Failed
        );
        let events = collected.lock().unwrap().clone();
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            EventKind::PolicyViolated {
                policy: PolicyKind::MaxSchemaRetries,
                limit: 2,
            },
        )));
    }

    #[tokio::test]
    async fn a_successful_tool_call_resets_the_failure_budget() {
        use std::sync::Arc;
        use std::time::Duration;

        use crate::agents::agent::Agent;
        use crate::agents::tickets::TicketQueue;
        use crate::providers::Provider;
        use crate::tools::{Tool, ToolResult};

        // boom, ping, boom, finish: a budget of two would trip on the second
        // boom if the ping success did not reset the counter in between.
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("boom")),
            Ok(tool_call_response("ping")),
            Ok(tool_call_response("boom")),
            Ok(write_result_value(serde_json::json!("done"))),
        ]);
        let boom = Tool::new("boom", "Always fails")
            .handler(|_, _| async move { Ok(ToolResult::error("boom")) })
            .build();
        let ping = Tool::new("ping", "Always succeeds")
            .handler(|_, _| async move { Ok(ToolResult::success("pong")) })
            .build();

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_schema_retries(2)
            .max_time(Duration::from_millis(500));
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(boom)
                .tool(ping)
                .build(),
        );
        tickets.task("go");
        let _ = tickets.finish().await;

        assert_eq!(
            tickets.tickets().into_iter().next().unwrap().status,
            Status::Finished
        );
    }

    #[tokio::test]
    async fn finish_awaits_completion_when_tool_call_is_in_flight() {
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::Notify;

        use crate::agents::agent::Agent;
        use crate::agents::tickets::TicketQueue;
        use crate::providers::Provider;
        use crate::tools::{ManageTicketsTool, Tool, ToolResult};

        let tool_started = Arc::new(Notify::new());
        let tool_unblocked = Arc::new(Notify::new());
        let tool_started_clone = Arc::clone(&tool_started);
        let tool_unblocked_clone = Arc::clone(&tool_unblocked);

        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("slow_tool")),
            Ok(write_result_value(serde_json::json!("done"))),
        ]);

        let slow_tool = Tool::new("slow_tool", "Blocks until released")
            .handler(move |_, _| {
                let s = Arc::clone(&tool_started_clone);
                let u = Arc::clone(&tool_unblocked_clone);
                async move {
                    s.notify_one();
                    u.notified().await;
                    Ok(ToolResult::success("ok"))
                }
            })
            .build();

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
        tickets
            .dir(results_dir.path().to_path_buf())
            .max_request_retries(0)
            .request_retry_delay(Duration::from_millis(1))
            .max_schema_retries(10)
            .max_time(Duration::from_secs(5));
        tickets.agent(
            Agent::new()
                .name("tester")
                .provider(provider as Arc<dyn Provider>)
                .model("mock")
                .role("test")
                .tool(ManageTicketsTool)
                .tool(slow_tool)
                .build(),
        );
        tickets.task("go");

        let unblock = async move {
            tool_started.notified().await;
            tool_unblocked.notify_one();
        };

        tokio::join!(tickets.finish(), unblock);
        assert_eq!(
            tickets.tickets().into_iter().next().unwrap().status,
            Status::Finished
        );
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
        let handler: Arc<dyn Fn(&crate::event::Event) + Send + Sync> = {
            let c = Arc::clone(&collected);
            Arc::new(move |e: &crate::event::Event| c.lock().unwrap().push(e.clone()))
        };

        let results_dir = crate::test_util::TempDir::new().unwrap();
        let tickets = TicketQueue::new();
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
                .tool(dump)
                .build(),
        );
        tickets.task("go");

        let _ = tickets.finish().await;
        let events = collected.lock().unwrap().clone();
        let ticket = tickets
            .tickets()
            .into_iter()
            .next()
            .expect("ticket must exist");

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
                ReplyContent::ToolResult {
                    tool_use_id, path, ..
                } if tool_use_id == "call-1" => path.clone(),
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
        let tickets = TicketQueue::new();
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
        let ticket = tickets
            .tickets()
            .into_iter()
            .next()
            .expect("ticket must exist");
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

    fn schema_for_partial_sum() -> Schema {
        Schema::parse(serde_json::json!({
            "type": "object",
            "properties": {
                "partial_sum": { "type": "integer" }
            },
            "required": ["partial_sum"]
        }))
        .expect("valid schema")
    }
}
