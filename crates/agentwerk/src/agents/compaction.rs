//! Context-window compaction: collapse an over-long transcript into
//! a single summary message before the next request would overflow.

use std::sync::Arc;

use crate::agents::tickets::TicketSystem;
use crate::prompts::compaction_directive;
use crate::providers::types::StreamEvent;
use crate::providers::{
    ContentBlock, Message, ModelRequest, Provider, ProviderError, ProviderResult,
    ProviderToolDefinition, TokenUsage,
};

/// Distance below the window at which compaction fires. Covers the
/// model's response budget (~20 k) and a safety margin (~13 k) because
/// the `bytes / 4` token estimate under-counts code and JSON.
const COMPACTION_HEADROOM_TOKENS: u64 = 33_000;

/// Token count at which compaction fires for a model with context
/// window `window`. `None` when the window is unknown.
pub(crate) fn compaction_threshold(window: Option<u64>) -> Option<u64> {
    window.map(|size| size.saturating_sub(COMPACTION_HEADROOM_TOKENS))
}

/// Estimate of the next request's input-token count: the last response's
/// reported input tokens plus a `bytes / 4` estimate over the full
/// request body the provider will see: every message in the current
/// vector, the system prompt, and every tool definition. Sums *all*
/// messages on purpose: this overcounts after the first iteration but
/// the resulting conservatism keeps the proactive seam ahead of the
/// real overflow. Reads the last entry of `history` for the input-token
/// anchor; an empty history anchors at 0.
pub(crate) fn estimate_next_request_tokens(
    history: &[TokenUsage],
    messages: &[Message],
    system_prompt: &str,
    tools: &[ProviderToolDefinition],
) -> u64 {
    let last_input = history.last().map(|u| u.input_tokens).unwrap_or(0);
    let bytes = messages.iter().map(message_bytes).sum::<usize>()
        + system_prompt.len()
        + tools.iter().map(tool_definition_bytes).sum::<usize>();
    last_input + (bytes / 4) as u64
}

/// Per-turn input-token growth implied by the last two recorded usages.
/// `0` when fewer than two samples exist or the series is shrinking
/// (`saturating_sub` handles tool-result trims that briefly lower the
/// running input count).
fn next_delta(history: &[TokenUsage]) -> u64 {
    match history {
        [.., a, b] => b.input_tokens.saturating_sub(a.input_tokens),
        _ => 0,
    }
}

fn message_bytes(message: &Message) -> usize {
    match message {
        Message::System { content } => content.len(),
        Message::User { content } | Message::Assistant { content } => {
            content.iter().map(block_bytes).sum()
        }
    }
}

fn block_bytes(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
        ContentBlock::ToolResult { content, .. } => content.len(),
        ContentBlock::Thinking {
            thinking,
            signature,
        } => thinking.len() + signature.len(),
        ContentBlock::RedactedThinking { data } => data.len(),
    }
}

fn tool_definition_bytes(tool: &ProviderToolDefinition) -> usize {
    tool.name.len() + tool.description.len() + tool.input_schema.to_string().len()
}

/// `true` when the estimated next-request input plus one more turn's
/// growth would cross the proactive compaction threshold. `false` when
/// the window is unknown or the history is empty. Extending the
/// estimate by `next_delta(history)` makes the trigger fire one turn
/// before a request that would otherwise overflow.
pub(crate) fn should_compact_proactively(
    window: Option<u64>,
    history: &[TokenUsage],
    messages: &[Message],
    system_prompt: &str,
    tools: &[ProviderToolDefinition],
) -> bool {
    let Some(threshold) = compaction_threshold(window) else {
        return false;
    };
    if history.is_empty() {
        return false;
    }
    let estimate = estimate_next_request_tokens(history, messages, system_prompt, tools);
    estimate.saturating_add(next_delta(history)) >= threshold
}

/// Run one compaction round-trip for the ticket at `ticket_key`: read
/// the current transcript, summarise it via [`compact`], and apply the
/// result through [`TicketSystem::summarize`]. Returns `Ok(true)` when a
/// summary was applied, `Ok(false)` when the ticket is missing or the
/// transcript collapses to a no-op.
pub(crate) async fn run(
    provider: &Arc<dyn Provider>,
    model: &str,
    messages: Vec<Message>,
    window: Option<u64>,
    ticket_system: &TicketSystem,
    ticket_key: &str,
    on_progress: Arc<dyn Fn(u32, u32) + Send + Sync>,
) -> ProviderResult<bool> {
    let Some(summary) = compact(provider, model, &messages, window, on_progress).await? else {
        return Ok(false);
    };
    ticket_system.summarize(ticket_key, summary);
    Ok(true)
}

/// Compact `messages` into a single summary by sending them to the
/// provider with no tools and the compaction directive as the system
/// prompt. When `window` is set and the transcript would overflow it,
/// split the largest splittable message in half (recursively until
/// every chunk fits) and summarise each chunk separately; the resulting
/// partial summaries are joined with a blank line. Returns `Ok(None)`
/// when the transcript collapses to a no-op (a single message that
/// already fits, or nothing to summarise); the caller treats that as a
/// no-op.
pub(crate) async fn compact(
    provider: &Arc<dyn Provider>,
    model: &str,
    messages: &[Message],
    window: Option<u64>,
    on_progress: Arc<dyn Fn(u32, u32) + Send + Sync>,
) -> ProviderResult<Option<String>> {
    let chunks = chunks_for_window(messages, window);
    if messages.len() <= 1 && chunks.len() <= 1 {
        return Ok(None);
    }
    let total = chunks.len() as u32;
    let mut summaries = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        if let Some(text) = summarize_chunk(provider, model, chunk).await? {
            summaries.push(text);
        }
        on_progress((i as u32) + 1, total);
    }
    if summaries.is_empty() {
        return Ok(None);
    }
    Ok(Some(summaries.join("\n\n")))
}

async fn summarize_chunk(
    provider: &Arc<dyn Provider>,
    model: &str,
    messages: &[Message],
) -> ProviderResult<Option<String>> {
    let request = ModelRequest {
        model: model.to_string(),
        system_prompt: compaction_directive().to_string(),
        messages: messages.to_vec(),
        tools: Vec::new(),
        max_request_tokens: None,
        tool_choice: None,
        // The summarizer does not think; capture only the summary text.
        reasoning_effort: Default::default(),
    };
    let on_stream: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(|_| {});
    let response = provider.respond(request, on_stream).await?;
    let summary = response
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            fn kind(block: &ContentBlock) -> &'static str {
                match block {
                    ContentBlock::Text { .. } => "text",
                    ContentBlock::ToolUse { .. } => "tool_use",
                    ContentBlock::ToolResult { .. } => "tool_result",
                    ContentBlock::Thinking { .. } => "thinking",
                    ContentBlock::RedactedThinking { .. } => "redacted_thinking",
                }
            }
            let kinds: Vec<&str> = response.content.iter().map(kind).collect();
            ProviderError::ResponseMalformed {
                message: format!(
                    "compaction reply contained no text (status={:?}, model={}, blocks={}, kinds=[{}], usage={:?})",
                    response.status,
                    response.model,
                    response.content.len(),
                    kinds.join(", "),
                    response.usage,
                ),
            }
        })?;
    Ok(Some(summary))
}

pub(crate) fn chunks_for_window(messages: &[Message], window: Option<u64>) -> Vec<Vec<Message>> {
    let Some(window) = window else {
        return vec![messages.to_vec()];
    };
    let max_tokens_per_chunk = window.saturating_mul(7) / 10;
    chunks_within(messages, max_tokens_per_chunk)
}

fn chunks_within(messages: &[Message], max_tokens_per_chunk: u64) -> Vec<Vec<Message>> {
    let bytes: usize = messages.iter().map(message_bytes).sum();
    let estimate = (bytes / 4) as u64;
    if estimate <= max_tokens_per_chunk {
        return vec![messages.to_vec()];
    }
    let Some(index) = messages
        .iter()
        .enumerate()
        .max_by_key(|(_, message)| message_bytes(message))
        .map(|(index, _)| index)
    else {
        return vec![messages.to_vec()];
    };
    let Some(halves) = split_in_half(&messages[index]) else {
        return vec![messages.to_vec()];
    };
    let before = &messages[..index];
    let after = &messages[index + 1..];
    let mut result = Vec::new();
    for half in halves {
        let mut chunk = Vec::with_capacity(before.len() + 1 + after.len());
        chunk.extend_from_slice(before);
        chunk.push(half);
        chunk.extend_from_slice(after);
        result.extend(chunks_within(&chunk, max_tokens_per_chunk));
    }
    result
}

fn split_in_half(message: &Message) -> Option<Vec<Message>> {
    let Message::User { content } = message else {
        return None;
    };
    if content.len() != 1 {
        return None;
    }
    let ContentBlock::Text { text } = &content[0] else {
        return None;
    };
    if text.is_empty() {
        return None;
    }
    let split_at = find_split_index(text, text.len() / 2);
    if split_at == 0 || split_at == text.len() {
        return None;
    }
    let (first, second) = text.split_at(split_at);
    Some(vec![Message::user(first), Message::user(second)])
}

fn find_split_index(text: &str, target: usize) -> usize {
    let target = target.min(text.len());
    let mut index = target;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    if let Some(newline_at) = text[..index].rfind('\n') {
        return newline_at + 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_threshold_saturates_on_tiny_or_zero_window() {
        assert_eq!(compaction_threshold(Some(100)), Some(0));
        assert_eq!(compaction_threshold(Some(0)), Some(0));
    }

    #[test]
    fn compaction_threshold_is_none_for_unknown_window() {
        assert_eq!(compaction_threshold(None), None);
    }

    #[test]
    fn estimate_sums_last_input_tokens_and_byte_quarters() {
        // 400 bytes / 4 = 100; plus last response's 5_000 input tokens = 5_100.
        let history = [TokenUsage {
            input_tokens: 5_000,
            output_tokens: 200,
        }];
        let messages = [Message::user("x".repeat(400))];
        assert_eq!(
            estimate_next_request_tokens(&history, &messages, "", &[]),
            5_100,
        );
    }

    #[test]
    fn estimate_with_empty_history_anchors_at_zero() {
        let messages = [Message::user("x".repeat(400))];
        assert_eq!(estimate_next_request_tokens(&[], &messages, "", &[]), 100);
    }

    #[test]
    fn estimate_includes_system_prompt_and_tool_definitions() {
        // bytes = system_prompt + tool(name+description+schema) + message
        //       = 100 + (3 + 50 + "{}".len()) + 4 = 159
        // estimate = 0 + 159/4 = 39
        let history = [TokenUsage::default()];
        let messages = [Message::user("hi!!")];
        let tools = vec![ProviderToolDefinition {
            name: "tot".into(),
            description: "x".repeat(50),
            input_schema: serde_json::json!({}),
        }];
        let system_prompt = "x".repeat(100);
        let got = estimate_next_request_tokens(&history, &messages, &system_prompt, &tools);
        assert_eq!(got, 39);
    }

    #[test]
    fn should_compact_proactively_is_false_when_window_unknown() {
        let history = [TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
        }];
        let messages = [Message::user("hi")];
        assert!(!should_compact_proactively(
            None,
            &history,
            &messages,
            "",
            &[]
        ));
    }

    #[test]
    fn should_compact_proactively_is_false_when_history_empty() {
        // No samples yet → the trigger cannot reason about growth and
        // defers; the loop has not produced a request to anchor against.
        let messages = [Message::user("hi")];
        assert!(!should_compact_proactively(
            Some(200_000),
            &[],
            &messages,
            "",
            &[],
        ));
    }

    #[test]
    fn should_compact_proactively_is_false_when_under_threshold() {
        let history = [TokenUsage {
            input_tokens: 1_000,
            output_tokens: 0,
        }];
        let messages = [Message::user("hi")];
        assert!(!should_compact_proactively(
            Some(200_000),
            &history,
            &messages,
            "",
            &[],
        ));
    }

    #[test]
    fn should_compact_proactively_is_true_when_estimate_crosses_threshold() {
        // Threshold = 200_000 - 33_000 = 167_000; single-entry history
        // gives delta = 0, so estimate alone (170_000 + tiny msg) crosses.
        let history = [TokenUsage {
            input_tokens: 170_000,
            output_tokens: 0,
        }];
        let messages = [Message::user("hi")];
        assert!(should_compact_proactively(
            Some(200_000),
            &history,
            &messages,
            "",
            &[],
        ));
    }

    #[test]
    fn should_compact_proactively_uses_last_delta_to_fire_one_turn_early() {
        // Threshold = 200_000 - 33_000 = 167_000. The current estimate
        // sits at 160_000 (under threshold), but the last per-turn delta
        // was 10_000 — the next request after this one would land at
        // ~170_000 and overflow. Trigger must fire now, not next turn.
        let history = [
            TokenUsage {
                input_tokens: 150_000,
                output_tokens: 0,
            },
            TokenUsage {
                input_tokens: 160_000,
                output_tokens: 0,
            },
        ];
        let messages = [Message::user("hi")];
        assert!(should_compact_proactively(
            Some(200_000),
            &history,
            &messages,
            "",
            &[],
        ));
    }

    #[test]
    fn should_compact_proactively_ignores_shrinking_series() {
        // Threshold = 167_000. Latest entry is 160_000 (under), and the
        // delta is negative — saturating_sub clamps it to 0, so the
        // trigger behaves like a single-sample history and stays quiet.
        let history = [
            TokenUsage {
                input_tokens: 170_000,
                output_tokens: 0,
            },
            TokenUsage {
                input_tokens: 160_000,
                output_tokens: 0,
            },
        ];
        let messages = [Message::user("hi")];
        assert!(!should_compact_proactively(
            Some(200_000),
            &history,
            &messages,
            "",
            &[],
        ));
    }

    // ---- compact ----

    use crate::providers::types::{ModelResponse, ResponseStatus};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex as StdMutex;

    /// Scripted provider: serves one canned result per `respond` call
    /// in FIFO order, and records the request it received so tests
    /// can assert on it.
    struct ScriptedProvider {
        results: StdMutex<Vec<ProviderResult<ModelResponse>>>,
        received: StdMutex<Vec<ModelRequest>>,
    }

    impl ScriptedProvider {
        fn new(results: Vec<ProviderResult<ModelResponse>>) -> Arc<Self> {
            Arc::new(Self {
                results: StdMutex::new(results),
                received: StdMutex::new(Vec::new()),
            })
        }

        fn last_request(&self) -> Option<ModelRequest> {
            self.received.lock().unwrap().last().cloned()
        }

        fn call_count(&self) -> usize {
            self.received.lock().unwrap().len()
        }
    }

    impl Provider for ScriptedProvider {
        fn respond(
            &self,
            request: ModelRequest,
            _on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
            self.received.lock().unwrap().push(request);
            let mut results = self.results.lock().unwrap();
            if results.is_empty() {
                panic!("ScriptedProvider out of scripted results");
            }
            let next = results.remove(0);
            Box::pin(async move { next })
        }
    }

    fn summary_response(text: &str) -> ModelResponse {
        ModelResponse {
            content: vec![ContentBlock::Text { text: text.into() }],
            status: ResponseStatus::EndTurn,
            usage: TokenUsage::default(),
            model: "mock".into(),
        }
    }

    fn noop_progress() -> Arc<dyn Fn(u32, u32) + Send + Sync> {
        Arc::new(|_, _| {})
    }

    #[tokio::test]
    async fn compact_returns_the_provider_summary() {
        let provider: Arc<dyn Provider> =
            ScriptedProvider::new(vec![Ok(summary_response("SUMMARY"))]);
        let messages = vec![
            Message::user("task"),
            Message::assistant("turn 0"),
            Message::user("turn 1 result"),
        ];

        let summary = compact(&provider, "mock", &messages, None, noop_progress())
            .await
            .expect("compact should succeed");

        assert_eq!(summary.as_deref(), Some("SUMMARY"));
    }

    #[tokio::test]
    async fn compact_is_a_noop_when_messages_are_too_short() {
        for len in [0, 1] {
            let provider = ScriptedProvider::new(Vec::new());
            let provider_handle: Arc<dyn Provider> = provider.clone();
            let messages: Vec<Message> = (0..len).map(|i| Message::user(format!("m{i}"))).collect();

            let summary = compact(&provider_handle, "mock", &messages, None, noop_progress())
                .await
                .expect("no-op should succeed");

            assert!(summary.is_none(), "len={len}: must short-circuit");
            assert_eq!(
                provider.call_count(),
                0,
                "len={len}: provider must not be called"
            );
        }
    }

    #[tokio::test]
    async fn compact_propagates_provider_error() {
        let provider: Arc<dyn Provider> =
            ScriptedProvider::new(vec![Err(ProviderError::ConnectionFailed {
                message: "dns".into(),
            })]);
        let messages = vec![Message::user("task"), Message::assistant("turn 0")];

        let err = compact(&provider, "mock", &messages, None, noop_progress())
            .await
            .expect_err("should propagate the connection failure");

        assert!(matches!(err, ProviderError::ConnectionFailed { .. }));
    }

    #[tokio::test]
    async fn compact_rejects_text_less_reply() {
        let no_text = ModelResponse {
            content: vec![ContentBlock::ToolUse {
                id: "x".into(),
                name: "irrelevant".into(),
                input: serde_json::json!({}),
            }],
            status: ResponseStatus::EndTurn,
            usage: TokenUsage::default(),
            model: "mock".into(),
        };
        let provider: Arc<dyn Provider> = ScriptedProvider::new(vec![Ok(no_text)]);
        let messages = vec![Message::user("task"), Message::assistant("turn 0")];

        let err = compact(&provider, "mock", &messages, None, noop_progress())
            .await
            .expect_err("text-less reply must fail");

        assert!(matches!(err, ProviderError::ResponseMalformed { .. }));
    }

    // ---- chunking ----

    #[test]
    fn binary_split_at_newline_preserves_total_text() {
        let original = "line 1\nline 2\nline 3\nline 4\n";
        let message = Message::user(original);
        let halves = split_in_half(&message).expect("should split");
        assert_eq!(halves.len(), 2);
        let joined = halves
            .iter()
            .map(|m| match m {
                Message::User { content } => match &content[0] {
                    ContentBlock::Text { text } => text.clone(),
                    _ => panic!("not text"),
                },
                _ => panic!("not user"),
            })
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(joined, original);
    }

    #[test]
    fn binary_split_falls_back_to_char_midpoint_when_no_newline() {
        let text = "x".repeat(200);
        let message = Message::user(&text);
        let halves = split_in_half(&message).expect("should split");
        assert_eq!(halves.len(), 2);
        match (&halves[0], &halves[1]) {
            (Message::User { content: c1 }, Message::User { content: c2 }) => {
                let len1 = match &c1[0] {
                    ContentBlock::Text { text } => text.len(),
                    _ => panic!(),
                };
                let len2 = match &c2[0] {
                    ContentBlock::Text { text } => text.len(),
                    _ => panic!(),
                };
                assert_eq!(len1 + len2, 200);
                assert_eq!(len1, 100);
            }
            _ => panic!("halves must be User messages"),
        }
    }

    #[test]
    fn messages_within_window_pass_through_as_single_chunk() {
        let messages = vec![Message::user("hi"), Message::assistant("ok")];
        let chunks = chunks_for_window(&messages, Some(200_000));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 2);
    }

    #[test]
    fn single_oversized_user_message_splits_into_multiple_chunks() {
        let payload = "x".repeat(10_000);
        let messages = vec![Message::user(payload)];
        let chunks = chunks_for_window(&messages, Some(1_000));
        assert!(
            chunks.len() >= 2,
            "expected multiple chunks, got {}",
            chunks.len()
        );
        for chunk in &chunks {
            let bytes: usize = chunk.iter().map(message_bytes).sum();
            assert!(
                (bytes / 4) as u64 <= 700,
                "chunk of {bytes} bytes ({} tokens) exceeds max 700",
                bytes / 4,
            );
        }
    }

    // ---- compact (continued) ----

    #[tokio::test]
    async fn compact_builds_a_tool_less_request() {
        let provider = ScriptedProvider::new(vec![Ok(summary_response("SUMMARY"))]);
        let provider_handle: Arc<dyn Provider> = provider.clone();
        let messages = vec![
            Message::user("task"),
            Message::assistant("turn 0"),
            Message::user("turn 1 result"),
        ];

        compact(&provider_handle, "mock", &messages, None, noop_progress())
            .await
            .unwrap();

        let req = provider.last_request().expect("provider was called");
        assert!(req.tools.is_empty(), "tools must be disabled");
        assert!(req.tool_choice.is_none(), "tool_choice must be unset");
        assert_eq!(req.messages.len(), messages.len());
        assert_eq!(req.system_prompt, compaction_directive());
    }

    #[tokio::test]
    async fn compact_fires_one_progress_event_per_chunk() {
        let provider: Arc<dyn Provider> = ScriptedProvider::new(vec![
            Ok(summary_response("PART_A")),
            Ok(summary_response("PART_B")),
            Ok(summary_response("PART_C")),
            Ok(summary_response("PART_D")),
        ]);
        let messages = vec![Message::user("x\n".repeat(2_000))];
        let captured: Arc<StdMutex<Vec<(u32, u32)>>> = Arc::new(StdMutex::new(Vec::new()));
        let on_progress: Arc<dyn Fn(u32, u32) + Send + Sync> = {
            let captured = Arc::clone(&captured);
            Arc::new(move |completed, total| {
                captured.lock().unwrap().push((completed, total));
            })
        };

        compact(&provider, "mock", &messages, Some(1_000), on_progress)
            .await
            .expect("chunked compaction should succeed");

        let progress = captured.lock().unwrap().clone();
        assert!(progress.len() >= 2, "expected ≥2 chunks, got {progress:?}");
        let total = progress[0].1;
        for (i, (completed, t)) in progress.iter().enumerate() {
            assert_eq!(*t, total, "total must stay constant across events");
            assert_eq!(
                *completed,
                (i as u32) + 1,
                "completed must increment 1, 2, 3, …",
            );
        }
        assert_eq!(progress.last().unwrap().0, total);
    }

    #[tokio::test]
    async fn compact_emits_no_progress_when_short_circuiting() {
        let provider: Arc<dyn Provider> = ScriptedProvider::new(Vec::new());
        let messages = vec![Message::user("only one")];
        let captured: Arc<StdMutex<Vec<(u32, u32)>>> = Arc::new(StdMutex::new(Vec::new()));
        let on_progress: Arc<dyn Fn(u32, u32) + Send + Sync> = {
            let captured = Arc::clone(&captured);
            Arc::new(move |completed, total| {
                captured.lock().unwrap().push((completed, total));
            })
        };

        let summary = compact(&provider, "mock", &messages, None, on_progress)
            .await
            .expect("short-circuit must succeed");

        assert!(summary.is_none());
        assert!(captured.lock().unwrap().is_empty());
    }
}
