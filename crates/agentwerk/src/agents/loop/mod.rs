//! Multi-agent loop driver. One tokio task per registered agent,
//! reading the shared `Werk` through the upgraded
//! `Weak<Werk>` set when the agent was added.

use std::time::Duration;

mod agent;
mod compact;
mod main;
mod request;
mod tool_call;

pub(super) use self::main::run_main_loop;

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Why the older messages were summarized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CompactReason {
    /// The next request was estimated to be too long for the model, ahead of
    /// any failure.
    Proactive,
    /// The LLM provider reported the context window exceeded.
    Reactive,
}

#[cfg(test)]
pub(crate) mod test_util;

#[cfg(test)]
mod tests {
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tasks::{Author, ReplyContent};

    // Reply transcript

    #[tokio::test]
    async fn replies_capture_full_transcript() {
        let provider = MockProvider::with_results(vec![Ok(write_result_response("ok"))]);
        let (_, _, task) = run_one(provider, 3, 10, None).await;

        let replies = &task.replies;
        assert_eq!(replies.len(), 4, "got {replies:?}");

        assert_eq!(replies[0].author, Author::System);
        assert!(matches!(
            &replies[0].content[..],
            [ReplyContent::Text { text: _ }]
        ));

        assert_eq!(replies[1].author, Author::User);
        assert!(
            matches!(&replies[1].content[..], [ReplyContent::Text { text: t }] if t == "go"),
            "second reply must carry the task body",
        );

        assert_eq!(replies[2].author, Author::Assistant);
        assert!(
            matches!(&replies[2].content[..], [ReplyContent::ToolUse { name, .. }] if name == "finish"),
            "assistant reply must mirror the model's ToolUse block",
        );

        assert_eq!(replies[3].author, Author::User);
        assert!(
            matches!(&replies[3].content[..], [ReplyContent::ToolResult { .. }]),
            "tool-result reply must carry a ToolResult block",
        );

        for w in replies.windows(2) {
            assert!(
                w[0].created_at <= w[1].created_at,
                "reply timestamps must be monotonic",
            );
        }
    }

    #[tokio::test]
    async fn replies_after_compaction_keep_only_system_and_summary() {
        let provider = MockProvider::with_results(vec![
            Ok(tool_call_response("task")),
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "exceeded".into(),
            }),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("ok")),
        ]);
        let (_, _, task) = run_one(provider, 0, 10, Some(string_schema())).await;

        let replies = &task.replies;

        assert_eq!(replies[0].author, Author::System);

        let summary_idx = replies
            .iter()
            .position(|r| {
                r.author == Author::User
                    && matches!(&r.content[..], [ReplyContent::Text { text: t }] if t == "SUMMARY")
            })
            .expect("expected a `user` reply carrying the summariser text");
        assert!(summary_idx >= 1, "summary must follow the system prompt");

        assert!(
            !replies.iter().any(|r| {
                matches!(&r.content[..], [ReplyContent::Text { text: t }] if t == "go")
            }),
            "compaction must drop pre-compaction non-system replies",
        );
    }
}
