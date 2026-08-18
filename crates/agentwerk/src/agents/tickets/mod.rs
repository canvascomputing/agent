//! The ticket queue agents coordinate through, and the tickets themselves.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::{EventName, PolicyKind};

use super::policy::Policies;
use super::stats::Stats;

mod error;
mod query;
mod reply;
mod store;
mod ticket;
mod ticket_queue;
mod trajectory;

#[cfg(test)]
pub(super) mod test_util;

pub use error::TicketError;
pub use query::{Query, QueryError, TicketMatcher};
pub use reply::{Author, Reply, ReplyContent};
pub use ticket::{Status, Ticket};
pub use ticket_queue::TicketQueue;
pub use trajectory::Trajectory;

pub(crate) use ticket::{Replies, TicketResult};
pub(crate) use ticket_queue::Run;

/// Whether the run-wide policies have been exceeded by the current
/// stats reading. Returns the tripping `PolicyKind` and the
/// configured limit so callers can emit `PolicyViolated` and assemble
/// `FinishReason::PolicyViolated`. Used by the main loop's ending check
/// and the per-agent loop's pre-claim check.
pub(crate) fn policy_violated_kind(
    policies: &Policies,
    stats: &Stats,
) -> Option<(PolicyKind, u64)> {
    if let Some(limit) = policies.max_turns {
        if stats.event_count(EventName::TurnStarted) >= u64::from(limit) {
            return Some((PolicyKind::Turns, u64::from(limit)));
        }
    }
    if let Some(limit) = policies.max_input_tokens {
        if stats.input_tokens() >= limit {
            return Some((PolicyKind::InputTokens, limit));
        }
    }
    if let Some(limit) = policies.max_output_tokens {
        if stats.output_tokens() >= limit {
            return Some((PolicyKind::OutputTokens, limit));
        }
    }
    if let Some(limit) = policies.max_time {
        if stats.execution_duration().is_some_and(|d| d >= limit) {
            return Some((PolicyKind::Time, limit.as_millis() as u64));
        }
    }
    None
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Trailing numeric part of a `TICKET-N` key. Falls back to `u32::MAX`
/// so malformed keys sort last and tie-break stably.
pub(crate) fn numeric_id(key: &str) -> u32 {
    key.rsplit('-')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn policy_violated_kind_returns_time_when_max_time_elapsed() {
        let policies = Policies {
            max_time: Some(Duration::from_millis(1)),
            ..Policies::default()
        };
        // Stamped far in the past so execution_duration trivially exceeds the
        // 1ms limit.
        let stats = Stats::new();
        stats.record(&crate::event::Event {
            created_at: 1,
            ..crate::event::Event::new("", "TICKET-1", None, crate::event::EventKind::TicketStarted)
        });
        let trip = policy_violated_kind(&policies, &stats);
        assert!(matches!(trip, Some((PolicyKind::Time, _))));
    }

    #[test]
    fn policy_violated_kind_returns_none_when_max_time_not_started() {
        let policies = Policies {
            max_time: Some(Duration::from_millis(1)),
            ..Policies::default()
        };
        // No ticket started, so execution_duration is None; the time limit must
        // not trip until one has.
        let stats = Stats::new();
        assert!(policy_violated_kind(&policies, &stats).is_none());
    }
}
