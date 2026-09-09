//! Shared test infrastructure: mock provider, response builders, event filters, and test harnesses.

use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agents::agent::Agent;
use crate::agents::knowledge::Knowledge;
use crate::agents::policy::Policy;
use crate::agents::tasks::{FinishReason, Task, Werk};
use crate::event::Event;
use crate::providers::types::{ModelResponse, ResponseStatus, TokenUsage};
use crate::providers::{ContentBlock, Message, ProviderError, ProviderResult};
use crate::schemas::Schema;
use crate::tools::{TaskTool, Tool};

// Mock provider

pub struct MockProvider {
    results: Mutex<Vec<ProviderResult<ModelResponse>>>,
    requests: AtomicUsize,
    received: Mutex<Vec<Vec<Message>>>,
    received_system_prompts: Mutex<Vec<String>>,
    received_tools: Mutex<Vec<Vec<Tool>>>,
}

impl MockProvider {
    pub fn with_results(results: Vec<ProviderResult<ModelResponse>>) -> Arc<Self> {
        Arc::new(Self {
            results: Mutex::new(results),
            requests: AtomicUsize::new(0),
            received: Mutex::new(Vec::new()),
            received_system_prompts: Mutex::new(Vec::new()),
            received_tools: Mutex::new(Vec::new()),
        })
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    pub fn received(&self) -> Vec<Vec<Message>> {
        self.received.lock().unwrap().clone()
    }

    pub fn received_system_prompts(&self) -> Vec<String> {
        self.received_system_prompts.lock().unwrap().clone()
    }

    pub fn received_tools(&self) -> Vec<Vec<Tool>> {
        self.received_tools.lock().unwrap().clone()
    }
}

impl crate::providers::ProviderLike for MockProvider {
    fn respond(
        &self,
        request: crate::providers::ModelRequest,
        _on_event: Arc<dyn Fn(crate::providers::types::StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn std::future::Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
        self.received.lock().unwrap().push(request.messages.clone());
        self.received_system_prompts
            .lock()
            .unwrap()
            .push(request.system_prompt.clone());
        self.received_tools
            .lock()
            .unwrap()
            .push(request.tools.clone());
        self.requests.fetch_add(1, Ordering::Relaxed);
        let next = {
            let mut results = self.results.lock().unwrap();
            if results.is_empty() {
                Err(ProviderError::AuthenticationFailed {
                    message: "MockProvider exhausted".into(),
                })
            } else {
                results.remove(0)
            }
        };
        Box::pin(async move {
            tokio::task::yield_now().await;
            next
        })
    }
}

// Response builders

pub fn write_result_response(result: &str) -> ModelResponse {
    write_result_response_named("finish", result)
}

/// A finish call under a caller-chosen spelling, for tests that exercise how a
/// misnamed tool call is resolved.
pub fn write_result_response_named(tool_name: &str, result: &str) -> ModelResponse {
    ModelResponse {
        content: vec![ContentBlock::ToolUse {
            id: "call-1".into(),
            name: tool_name.into(),
            input: serde_json::json!({ "answer": result }),
        }],
        status: ResponseStatus::ToolUse,
        usage: TokenUsage::default(),
        model: "mock".into(),
    }
}

/// A finish call carrying a structured result as its top-level arguments.
pub fn write_result_value(result: serde_json::Value) -> ModelResponse {
    ModelResponse {
        content: vec![ContentBlock::ToolUse {
            id: "call-1".into(),
            name: "finish".into(),
            input: result,
        }],
        status: ResponseStatus::ToolUse,
        usage: TokenUsage::default(),
        model: "mock".into(),
    }
}

pub fn knowledge_write_response(slug: &str, description: &str, content: &str) -> ModelResponse {
    ModelResponse {
        content: vec![ContentBlock::ToolUse {
            id: "call-1".into(),
            name: "knowledge".into(),
            input: serde_json::json!({"action": "write", "slug": slug, "description": description, "content": content}),
        }],
        status: ResponseStatus::ToolUse,
        usage: TokenUsage::default(),
        model: "mock".into(),
    }
}

pub fn knowledge_read_response(slug: &str) -> ModelResponse {
    ModelResponse {
        content: vec![ContentBlock::ToolUse {
            id: "call-2".into(),
            name: "knowledge".into(),
            input: serde_json::json!({"action": "read", "slug": slug}),
        }],
        status: ResponseStatus::ToolUse,
        usage: TokenUsage::default(),
        model: "mock".into(),
    }
}

pub fn tool_call_response(tool_name: &str) -> ModelResponse {
    tool_call_response_with_usage(tool_name, TokenUsage::default())
}

pub fn tool_call_response_with_usage(tool_name: &str, usage: TokenUsage) -> ModelResponse {
    ModelResponse {
        content: vec![ContentBlock::ToolUse {
            id: "call-1".into(),
            name: tool_name.into(),
            input: serde_json::json!({}),
        }],
        status: ResponseStatus::ToolUse,
        usage,
        model: "mock".into(),
    }
}

pub fn text_response(text: &str) -> ModelResponse {
    ModelResponse {
        content: vec![ContentBlock::Text { text: text.into() }],
        status: ResponseStatus::EndTurn,
        usage: TokenUsage::default(),
        model: "mock".into(),
    }
}

pub fn paused_text_response(text: &str) -> ModelResponse {
    ModelResponse {
        content: vec![ContentBlock::Text { text: text.into() }],
        status: ResponseStatus::PauseTurn,
        usage: TokenUsage::default(),
        model: "mock".into(),
    }
}

pub fn text_response_with_usage(text: &str, usage: TokenUsage) -> ModelResponse {
    ModelResponse {
        content: vec![ContentBlock::Text { text: text.into() }],
        status: ResponseStatus::EndTurn,
        usage,
        model: "mock".into(),
    }
}

// Error builders

pub fn rate_limit() -> ProviderError {
    ProviderError::RateLimited {
        message: "rate limited".into(),
        status: 429,
        retry_delay: None,
    }
}

pub fn connection_failed(message: &str) -> ProviderError {
    ProviderError::ConnectionFailed {
        message: message.into(),
    }
}

// Event filters

pub fn retries_in(events: &[Event]) -> Vec<(u32, u32, String)> {
    events
        .iter()
        .filter_map(|event| {
            (event.get_name() == Event::REQUEST_RETRIED).then(|| {
                Some((
                    event.get_data().get("attempt")?.as_u64()? as u32,
                    event.get_data().get("max_attempts")?.as_u64()? as u32,
                    event.get_data().get("message")?.as_str()?.to_string(),
                ))
            })?
        })
        .collect()
}

pub fn failures_in(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| {
            matches!(
                event.get_name(),
                Event::REQUEST_FAILED | Event::COMPACTION_FAILED
            )
            .then(|| {
                event
                    .get_data()
                    .get("message")?
                    .as_str()
                    .map(str::to_string)
            })?
        })
        .collect()
}

pub fn schema_retries_in(events: &[Event]) -> Vec<(u32, u32, String)> {
    events
        .iter()
        .filter_map(|event| {
            (event.get_name() == Event::SCHEMA_RETRIED).then(|| {
                Some((
                    event.get_data().get("attempt")?.as_u64()? as u32,
                    event.get_data().get("max_attempts")?.as_u64()? as u32,
                    event.get_data().get("message")?.as_str()?.to_string(),
                ))
            })?
        })
        .collect()
}

/// Concatenated text of every user-role message, one block per line.
/// Reveals the corrective directive the loop injected on the prior turn.
pub fn user_text(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        if let Message::User { content } = message {
            for block in content {
                if let ContentBlock::Text { text } = block {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
    }
    out
}

// Agent / event builders

pub fn interactive_chatbot(provider: &Arc<MockProvider>) -> Agent {
    Agent::new()
        .interactive()
        .provider(provider.clone())
        .model("mock")
        .role("test")
}

pub fn task_agent(provider: &Arc<MockProvider>) -> Agent {
    Agent::new()
        .provider(provider.clone())
        .model("mock")
        .role("test")
}

pub fn collect_events(werk: &Werk) -> Arc<Mutex<Vec<Event>>> {
    let collected: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let handler: Arc<dyn Fn(&Event) + Send + Sync> = {
        let c = Arc::clone(&collected);
        Arc::new(move |e: &Event| c.lock().unwrap().push(e.clone()))
    };
    werk.on_event(move |_, e| handler(e));
    collected
}

// Harnesses

pub async fn run_one(
    provider: Arc<MockProvider>,
    max_request_retries: u32,
    max_schema_retries: u32,
    schema: Option<Schema>,
) -> (Vec<Event>, Arc<MockProvider>, Task) {
    let collected: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let handler: Arc<dyn Fn(&Event) + Send + Sync> = {
        let c = Arc::clone(&collected);
        Arc::new(move |e: &Event| c.lock().unwrap().push(e.clone()))
    };

    let results_dir = crate::test_util::TempDir::new().unwrap();
    let knowledge_dir = crate::test_util::TempDir::new().unwrap();
    let knowledge = Knowledge::load(knowledge_dir.path()).unwrap();
    let werk = Werk::new();
    werk.set_dir(results_dir.path().to_path_buf())
        .set_policy(Policy {
            max_request_retries,
            request_retry_delay: Duration::from_millis(1),
            max_schema_retries: Some(max_schema_retries),
            ..Default::default()
        });

    werk.on_event(move |_, e| handler(e));
    werk.add_agent(
        Agent::new()
            .provider(provider.clone())
            .model("mock")
            .role("test")
            .knowledge(&knowledge)
            .tool(TaskTool),
    );

    if let Some(schema) = schema {
        werk.add_task(Task::new("go").schema(schema));
    } else {
        werk.add_task("go");
    }

    tokio::time::timeout(Duration::from_secs(5), werk.finish())
        .await
        .expect("test run did not finish within 5s");
    assert_eq!(werk.get_finish_reason(), Some(FinishReason::Drained));
    let events = collected.lock().unwrap().clone();
    let task = werk
        .get_tasks()
        .into_iter()
        .next()
        .expect("task must exist");
    (events, provider, task)
}

pub async fn run_with_context_window(
    provider: Arc<MockProvider>,
    context_window_size: u64,
    task: impl Into<String>,
) -> (Vec<Event>, Arc<MockProvider>, Task) {
    let task: String = task.into();
    use crate::providers::Model;
    let collected: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let handler: Arc<dyn Fn(&Event) + Send + Sync> = {
        let c = Arc::clone(&collected);
        Arc::new(move |e: &Event| c.lock().unwrap().push(e.clone()))
    };

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
    werk.on_event(move |_, e| handler(e));
    werk.add_agent(
        Agent::new()
            .provider(provider.clone())
            .model(Model::new("mock").context_window(context_window_size))
            .role("test"),
    );
    werk.add_task(Task::new(task).schema(string_schema()));

    let _ = werk.finish().await;
    let events = collected.lock().unwrap().clone();
    let task = werk
        .get_tasks()
        .into_iter()
        .next()
        .expect("task must exist");
    (events, provider, task)
}

/// Run one task on a model with a 200 000-token window, so the proactive
/// threshold is reachable. `configure` runs before the agents start: for
/// installing a compaction editor, or moving the trigger.
pub async fn run_compaction(
    provider: Arc<MockProvider>,
    configure: impl FnOnce(&Arc<Werk>),
) -> (Vec<Event>, Arc<MockProvider>, Task) {
    let collected: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let handler: Arc<dyn Fn(&Event) + Send + Sync> = {
        let c = Arc::clone(&collected);
        Arc::new(move |e: &Event| c.lock().unwrap().push(e.clone()))
    };

    let results_dir = crate::test_util::TempDir::new().unwrap();
    let werk = Werk::new();
    werk.set_dir(results_dir.path().to_path_buf())
        .set_policy(Policy {
            max_request_retries: 0,
            request_retry_delay: Duration::from_millis(1),
            max_schema_retries: Some(10),
            max_time: Some(Duration::from_secs(30)),
            ..Default::default()
        });

    werk.on_event(move |_, e| handler(e));
    werk.add_agent(
        Agent::new()
            .provider(provider.clone())
            .model("claude-sonnet-4-20250514")
            .role("test")
            .tool(TaskTool),
    );
    configure(&werk);
    let schema = string_schema();
    werk.add_task(Task::new("go").schema(schema));

    let _ = werk.finish().await;
    let events = collected.lock().unwrap().clone();
    let task = werk
        .get_tasks()
        .into_iter()
        .next()
        .expect("task must exist");
    (events, provider, task)
}

pub fn string_schema() -> Schema {
    Schema::new(serde_json::json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"]
    }))
    .expect("valid schema")
}
