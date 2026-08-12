//! Context-window compaction: rewrite a ticket's replies before the next
//! request would overflow. Summarizing them into one message is the default;
//! `TicketQueue::edit_replies_on_compaction` replaces it.

use std::sync::Arc;

use crate::agents::tickets::{Author, Reply, Ticket};
use crate::event::CompactReason;
use crate::prompts::compaction_directive;
use crate::providers::types::StreamEvent;
use crate::providers::{
    ContentBlock, Message, ModelRequest, Provider, ProviderError, ProviderResult,
    ProviderToolDefinition, TokenUsage,
};

/// How full the context window gets before compaction fires, when the host
/// sets no fraction of its own. What is left over covers the model's response
/// budget and the slack the `bytes / 4` token estimate needs because it
/// under-counts code and JSON. A share rather than a fixed count, so it holds
/// from the smallest window to the largest.
const DEFAULT_COMPACT_AT: f64 = 0.85;

/// Token count at which compaction fires for a model with context window
/// `window`, at `compact_at` of it or at [`DEFAULT_COMPACT_AT`] when that is
/// unset. `None` when the window is unknown.
pub(crate) fn compaction_threshold(window: Option<u64>, compact_at: Option<f64>) -> Option<u64> {
    Some((window? as f64 * compact_at.unwrap_or(DEFAULT_COMPACT_AT)) as u64)
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
    compact_at: Option<f64>,
    history: &[TokenUsage],
    messages: &[Message],
    system_prompt: &str,
    tools: &[ProviderToolDefinition],
) -> bool {
    let Some(threshold) = compaction_threshold(window, compact_at) else {
        return false;
    };
    if history.is_empty() {
        return false;
    }
    let estimate = estimate_next_request_tokens(history, messages, system_prompt, tools);
    estimate.saturating_add(next_delta(history)) >= threshold
}

/// One compaction round-trip, handed to the editor installed with
/// [`TicketQueue::edit_replies_on_compaction`]. It owns everything it needs, so
/// it can cross an await: the ticket is a copy taken when compaction started.
///
/// [`TicketQueue::edit_replies_on_compaction`]: crate::TicketQueue::edit_replies_on_compaction
pub struct Compaction {
    reason: CompactReason,
    ticket: Ticket,
    provider: Provider,
    model: String,
    window: Option<u64>,
    on_progress: Arc<dyn Fn(u32, u32) + Send + Sync>,
}

impl Compaction {
    pub(crate) fn new(
        reason: CompactReason,
        ticket: Ticket,
        provider: Provider,
        model: String,
        window: Option<u64>,
        on_progress: Arc<dyn Fn(u32, u32) + Send + Sync>,
    ) -> Self {
        Self {
            reason,
            ticket,
            provider,
            model,
            window,
            on_progress,
        }
    }

    /// Get why compaction fired: ahead of an overflow, or after one.
    pub fn reason(&self) -> CompactReason {
        self.reason
    }

    /// Get the ticket being compacted: its key, label, task, and status as
    /// they stood when compaction started. `replies` is empty here, because the
    /// replies travel to the editor as their own argument rather than as a
    /// second copy on the ticket.
    pub fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    /// Get the model's context window, or `None` when it is unknown.
    pub fn window(&self) -> Option<u64> {
        self.window
    }

    /// Summarize `replies`: one tool-less request per chunk, with the answers
    /// joined by a blank line. Replies that would overflow the window are split
    /// first, so a single oversized reply still gets through. Replies the model
    /// would not see, an empty slice or system replies alone, summarize to an
    /// empty string without a request.
    pub async fn summarize(&self, replies: &[Reply]) -> ProviderResult<String> {
        let messages: Vec<Message> = replies.iter().filter_map(Reply::as_message).collect();
        // Chunking hands back one empty chunk here, and an LLM provider rejects
        // a request carrying no messages.
        if messages.is_empty() {
            return Ok(String::new());
        }
        let chunks = chunks_for_window(&messages, self.window);
        let total = chunks.len() as u32;
        let mut summaries = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.iter().enumerate() {
            let request = ModelRequest {
                model: self.model.clone(),
                system_prompt: compaction_directive().to_string(),
                messages: chunk.clone(),
                tools: Vec::new(),
                max_request_tokens: None,
                tool_choice: None,
                // The summarizer does not think; capture only the summary text.
                reasoning_effort: Default::default(),
            };
            let on_stream: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(|_| {});
            let response = self.provider.respond(request, on_stream).await?;
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
            summaries.push(summary);
            (self.on_progress)((index as u32) + 1, total);
        }
        Ok(summaries.join("\n\n"))
    }
}

/// What compaction does when no editor is installed: collapse everything the
/// model would see into one summary, keeping the system reply that carries the
/// system prompt. Replies that already hold nothing to collapse come back
/// unchanged, which the loop reads as a no-op.
pub(crate) async fn default_editor(
    compaction: Compaction,
    replies: Vec<Reply>,
) -> ProviderResult<Vec<Reply>> {
    let messages: Vec<Message> = replies.iter().filter_map(Reply::as_message).collect();
    if messages.len() <= 1 && chunks_for_window(&messages, compaction.window()).len() <= 1 {
        return Ok(replies);
    }
    let summary = compaction.summarize(&replies).await?;
    let mut kept: Vec<Reply> = replies
        .into_iter()
        .filter(|reply| reply.author == Author::System)
        .collect();
    kept.push(Reply::user_text(summary));
    Ok(kept)
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
    fn the_default_fraction_applies_when_none_is_set() {
        assert_eq!(compaction_threshold(Some(200_000), None), Some(170_000));
    }

    #[test]
    fn the_default_fraction_scales_down_to_a_tiny_window() {
        // The rule this replaced subtracted a fixed 33 000 tokens, which left
        // any window under that compacting from the first turn.
        assert_eq!(compaction_threshold(Some(32_000), None), Some(27_200));
        assert_eq!(compaction_threshold(Some(100), None), Some(85));
        assert_eq!(compaction_threshold(Some(0), None), Some(0));
    }

    #[test]
    fn compaction_threshold_is_none_for_unknown_window() {
        assert_eq!(compaction_threshold(None, None), None);
        assert_eq!(compaction_threshold(None, Some(0.8)), None);
    }

    #[test]
    fn the_threshold_is_the_configured_fraction_of_the_window() {
        assert_eq!(
            compaction_threshold(Some(200_000), Some(0.8)),
            Some(160_000)
        );
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
            None,
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
            None,
            &history,
            &messages,
            "",
            &[],
        ));
    }

    #[test]
    fn should_compact_proactively_is_true_when_estimate_crosses_threshold() {
        // Threshold = 200_000 * 0.85 = 170_000; single-entry history gives
        // delta = 0, so the estimate alone (175_000 + tiny msg) crosses.
        let history = [TokenUsage {
            input_tokens: 175_000,
            output_tokens: 0,
        }];
        let messages = [Message::user("hi")];
        assert!(should_compact_proactively(
            Some(200_000),
            None,
            &history,
            &messages,
            "",
            &[],
        ));
    }

    #[test]
    fn should_compact_proactively_uses_last_delta_to_fire_one_turn_early() {
        // Threshold = 200_000 * 0.85 = 170_000. The current estimate sits at
        // 165_000 (under threshold), but the last per-turn delta was 10_000:
        // the next request after this one would land at ~175_000 and overflow.
        // Trigger must fire now, not next turn.
        let history = [
            TokenUsage {
                input_tokens: 155_000,
                output_tokens: 0,
            },
            TokenUsage {
                input_tokens: 165_000,
                output_tokens: 0,
            },
        ];
        let messages = [Message::user("hi")];
        assert!(should_compact_proactively(
            Some(200_000),
            None,
            &history,
            &messages,
            "",
            &[],
        ));
    }

    #[test]
    fn should_compact_proactively_ignores_shrinking_series() {
        // Threshold = 170_000. Latest entry is 160_000 (under), and the
        // delta is negative, so saturating_sub clamps it to 0 and the
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
            None,
            &history,
            &messages,
            "",
            &[],
        ));
    }

    #[test]
    fn should_compact_proactively_follows_the_configured_fraction() {
        // 100_000 sits under the default threshold of 170_000 but over the
        // 80_000 that two fifths of the window asks for.
        let history = [TokenUsage {
            input_tokens: 100_000,
            output_tokens: 0,
        }];
        let messages = [Message::user("hi")];
        let fires = |compact_at| {
            should_compact_proactively(Some(200_000), compact_at, &history, &messages, "", &[])
        };
        assert!(!fires(None));
        assert!(fires(Some(0.4)));
    }

    // Compact

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

    impl crate::providers::ProviderLike for ScriptedProvider {
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

    fn compaction_for(provider: impl Into<Provider>, window: Option<u64>) -> Compaction {
        compaction_reporting_to(provider, window, noop_progress())
    }

    fn compaction_reporting_to(
        provider: impl Into<Provider>,
        window: Option<u64>,
        on_progress: Arc<dyn Fn(u32, u32) + Send + Sync>,
    ) -> Compaction {
        Compaction::new(
            CompactReason::Proactive,
            Ticket::new("task"),
            provider.into(),
            "mock".into(),
            window,
            on_progress,
        )
    }

    /// The replies a ticket carries after one turn: the system prompt, the
    /// task, and an exchange with the model.
    fn worked_replies() -> Vec<Reply> {
        vec![
            Reply::system_text("system prompt"),
            Reply::user_text("task"),
            Reply::assistant(&[ContentBlock::Text {
                text: "turn 0".into(),
            }]),
            Reply::user_text("turn 1 result"),
        ]
    }

    #[tokio::test]
    async fn summarize_returns_the_provider_summary() {
        let provider = ScriptedProvider::new(vec![Ok(summary_response("SUMMARY"))]);
        let compaction = compaction_for(provider, None);

        let summary = compaction
            .summarize(&worked_replies())
            .await
            .expect("summarize should succeed");

        assert_eq!(summary, "SUMMARY");
    }

    #[tokio::test]
    async fn summarize_makes_no_request_for_replies_the_model_would_not_see() {
        // An editor summarizing the head of a short ticket hands over a slice
        // that maps to no messages, which an LLM provider would reject.
        for replies in [Vec::new(), vec![Reply::system_text("system prompt")]] {
            let provider = ScriptedProvider::new(Vec::new());
            let compaction = compaction_for(provider.clone(), None);

            let summary = compaction
                .summarize(&replies)
                .await
                .expect("an empty summary should succeed");

            assert_eq!(summary, "");
            assert_eq!(provider.call_count(), 0, "the provider must not be called");
        }
    }

    #[tokio::test]
    async fn the_default_editor_keeps_the_system_reply_and_appends_the_summary() {
        let provider = ScriptedProvider::new(vec![Ok(summary_response("SUMMARY"))]);
        let compaction = compaction_for(provider, None);

        let kept = default_editor(compaction, worked_replies())
            .await
            .expect("the default handler should succeed");

        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].author, Author::System);
        assert_eq!(kept[1].author, Author::User);
        assert!(
            matches!(&kept[1].content[..], [crate::agents::tickets::ReplyContent::Text { text }] if text == "SUMMARY"),
            "the summary must replace everything but the system reply, got {:?}",
            kept[1].content,
        );
    }

    #[tokio::test]
    async fn the_default_editor_returns_the_replies_unchanged_when_there_is_nothing_to_collapse() {
        // The system reply is not sent to the model, so one message is all
        // these replies amount to: there is nothing to collapse into a summary.
        for replies in [
            Vec::new(),
            vec![Reply::user_text("only one")],
            vec![
                Reply::system_text("system prompt"),
                Reply::user_text("task"),
            ],
        ] {
            let provider = ScriptedProvider::new(Vec::new());
            let compaction = compaction_for(provider.clone(), None);

            let kept = default_editor(compaction, replies.clone())
                .await
                .expect("a no-op should succeed");

            assert_eq!(kept, replies, "must hand the replies back untouched");
            assert_eq!(provider.call_count(), 0, "the provider must not be called");
        }
    }

    #[tokio::test]
    async fn summarize_propagates_a_provider_error() {
        let provider = ScriptedProvider::new(vec![Err(ProviderError::ConnectionFailed {
            message: "dns".into(),
        })]);
        let compaction = compaction_for(provider, None);

        let error = compaction
            .summarize(&worked_replies())
            .await
            .expect_err("should propagate the connection failure");

        assert!(matches!(error, ProviderError::ConnectionFailed { .. }));
    }

    #[tokio::test]
    async fn summarize_rejects_a_text_less_reply() {
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
        let provider = ScriptedProvider::new(vec![Ok(no_text)]);
        let compaction = compaction_for(provider, None);

        let error = compaction
            .summarize(&worked_replies())
            .await
            .expect_err("a text-less reply must fail");

        assert!(matches!(error, ProviderError::ResponseMalformed { .. }));
    }

    // Chunking

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

    // Summarize (continued)

    #[tokio::test]
    async fn summarize_builds_a_tool_less_request() {
        let provider = ScriptedProvider::new(vec![Ok(summary_response("SUMMARY"))]);
        let compaction = compaction_for(provider.clone(), None);
        let replies = worked_replies();

        compaction.summarize(&replies).await.unwrap();

        let request = provider.last_request().expect("provider was called");
        assert!(request.tools.is_empty(), "tools must be disabled");
        assert!(request.tool_choice.is_none(), "tool_choice must be unset");
        // The system reply carries the system prompt, which travels in its own
        // field rather than as a message.
        assert_eq!(request.messages.len(), replies.len() - 1);
        assert_eq!(request.system_prompt, compaction_directive());
    }

    #[tokio::test]
    async fn summarize_fires_one_progress_event_per_chunk() {
        let provider = ScriptedProvider::new(vec![
            Ok(summary_response("PART_A")),
            Ok(summary_response("PART_B")),
            Ok(summary_response("PART_C")),
            Ok(summary_response("PART_D")),
        ]);
        let captured: Arc<StdMutex<Vec<(u32, u32)>>> = Arc::new(StdMutex::new(Vec::new()));
        let on_progress: Arc<dyn Fn(u32, u32) + Send + Sync> = {
            let captured = Arc::clone(&captured);
            Arc::new(move |completed, total| {
                captured.lock().unwrap().push((completed, total));
            })
        };
        let compaction = compaction_reporting_to(provider, Some(1_000), on_progress);

        compaction
            .summarize(&[Reply::user_text("x\n".repeat(2_000))])
            .await
            .expect("chunked summarizing should succeed");

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
    async fn the_default_editor_emits_no_progress_when_there_is_nothing_to_collapse() {
        let provider = ScriptedProvider::new(Vec::new());
        let captured: Arc<StdMutex<Vec<(u32, u32)>>> = Arc::new(StdMutex::new(Vec::new()));
        let on_progress: Arc<dyn Fn(u32, u32) + Send + Sync> = {
            let captured = Arc::clone(&captured);
            Arc::new(move |completed, total| {
                captured.lock().unwrap().push((completed, total));
            })
        };
        let compaction = compaction_reporting_to(provider, None, on_progress);

        default_editor(compaction, vec![Reply::user_text("only one")])
            .await
            .expect("a no-op should succeed");

        assert!(captured.lock().unwrap().is_empty());
    }
}
