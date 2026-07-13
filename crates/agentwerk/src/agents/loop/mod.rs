//! Multi-agent loop driver. One tokio task per registered agent,
//! reading the shared `TicketSystem` through the upgraded
//! `Weak<TicketSystem>` stamped at `bind_agent`.

use std::time::Duration;

use crate::event::CompactReason;
use crate::tools::ToolCall;

mod agent;
mod compact;
mod main;
mod request;
mod tool_call;

pub(super) use self::main::run_main_loop;
use self::main::wait_for_signal;

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// What the agent does next with its claimed ticket.
enum Step {
    /// Re-read the ticket and route: request, compact, move on, or stop.
    CheckTicket,
    // Proactive resumes at Request so the ticket check cannot re-trigger
    // its own proactive threshold; Reactive re-checks like any replay.
    Compact(CompactReason),
    Request,
    ToolCalls(Vec<ToolCall>),
    /// Stop working this ticket; the outer loop claims the next one.
    NextTicket,
    /// The ticket's pool was called off: abandon the ticket where it
    /// stands (it stays InProgress) and look for other work.
    Cancel,
    /// End the agent task.
    Stop,
}

#[cfg(test)]
pub(crate) mod test_util;

#[cfg(test)]
mod tests {
    use crate::agents::r#loop::test_util::*;
    use crate::agents::tickets::{Author, ReplyContent};

    // Reply transcript

    #[tokio::test]
    async fn replies_capture_full_transcript() {
        let provider = MockProvider::with_results(vec![Ok(write_result_response("ok"))]);
        let (_, _, ticket) = run_one(provider, 3, 10, None).await;

        let replies = &ticket.replies;
        assert_eq!(replies.len(), 5, "got {replies:?}");

        assert_eq!(replies[0].author, Author::System);
        assert!(matches!(&replies[0].content[..], [ReplyContent::Text(_)]));

        assert_eq!(replies[1].author, Author::User);
        assert!(
            matches!(&replies[1].content[..], [ReplyContent::Text(t)] if t.starts_with("## Context")),
            "second reply must be the auto-injected context prelude",
        );

        assert_eq!(replies[2].author, Author::User);
        assert!(
            matches!(&replies[2].content[..], [ReplyContent::Text(t)] if t == "go"),
            "third reply must carry the task body",
        );

        assert_eq!(replies[3].author, Author::Assistant);
        assert!(
            matches!(&replies[3].content[..], [ReplyContent::ToolUse { name, .. }] if name == "finish_ticket"),
            "assistant reply must mirror the model's ToolUse block",
        );

        assert_eq!(replies[4].author, Author::User);
        assert!(
            matches!(&replies[4].content[..], [ReplyContent::ToolResult { .. }]),
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
            Ok(tool_call_response("manage_tickets")),
            Err(crate::providers::ProviderError::ContextWindowExceeded {
                message: "exceeded".into(),
            }),
            Ok(text_response_with_usage(
                "SUMMARY",
                crate::providers::types::TokenUsage::default(),
            )),
            Ok(write_result_response("ok")),
        ]);
        let (_, _, ticket) = run_one(provider, 0, 10, Some(string_schema())).await;

        let replies = &ticket.replies;

        assert_eq!(replies[0].author, Author::System);

        let summary_idx = replies
            .iter()
            .position(|r| {
                r.author == Author::User
                    && matches!(&r.content[..], [ReplyContent::Text(t)] if t == "SUMMARY")
            })
            .expect("expected a `user` reply carrying the summariser text");
        assert!(summary_idx >= 1, "summary must follow the system prompt");

        assert!(
            !replies
                .iter()
                .any(|r| { matches!(&r.content[..], [ReplyContent::Text(t)] if t == "go") }),
            "compaction must drop pre-compaction non-system replies",
        );
    }
}
