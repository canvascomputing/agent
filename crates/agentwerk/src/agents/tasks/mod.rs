//! The task queue agents coordinate through, and the tasks themselves.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::{Event, PolicyViolation};

use super::policy::Policy;
use super::stats::Stats;

mod error;
mod queue;
mod reply;
mod store;
mod task;
mod trajectory;

#[cfg(test)]
pub(super) mod test_util;

pub use error::TaskError;
pub use queue::Queue;
pub use reply::{Author, Reply, ReplyContent};
pub use task::{Status, Task};
pub use trajectory::Trajectory;

pub(crate) use queue::Run;
pub(crate) use task::{Replies, TaskResult};

/// Whether the run-wide policy have been exceeded by the current
/// stats reading. Returns the tripping `PolicyViolation` and the
/// configured limit so callers can emit `PolicyViolated` and assemble
/// `FinishReason::PolicyViolated`. Used by the main loop's ending check
/// and the per-agent loop's pre-claim check.
pub(crate) fn policy_violated(policy: &Policy, stats: &Stats) -> Option<(PolicyViolation, u64)> {
    if let Some(limit) = policy.max_turns {
        if stats.event_count(Event::TURN_STARTED) >= u64::from(limit) {
            return Some((PolicyViolation::Turns, u64::from(limit)));
        }
    }
    if let Some(limit) = policy.max_input_tokens {
        if stats.input_tokens() >= limit {
            return Some((PolicyViolation::InputTokens, limit));
        }
    }
    if let Some(limit) = policy.max_output_tokens {
        if stats.output_tokens() >= limit {
            return Some((PolicyViolation::OutputTokens, limit));
        }
    }
    if let Some(limit) = policy.max_time {
        if stats.execution_duration().is_some_and(|d| d >= limit) {
            return Some((PolicyViolation::Time, limit.as_millis() as u64));
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

/// Trailing numeric part of a `t-N` ID. Falls back to `u32::MAX`
/// so malformed IDs sort last and tie-break stably.
pub(crate) fn numeric_id(id: &str) -> u32 {
    id.rsplit('-')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn policy_violated_returns_time_when_max_time_elapsed() {
        let policy = Policy {
            max_time: Some(Duration::from_millis(1)),
            ..Policy::default()
        };
        // Stamped far in the past so execution_duration trivially exceeds the
        // 1ms limit.
        let stats = Stats::new();
        stats.record(&crate::event::Event {
            created_at: 1,
            ..crate::event::Event::new(crate::event::Event::TASK_STARTED).task_id("t-1")
        });
        let trip = policy_violated(&policy, &stats);
        assert!(matches!(trip, Some((PolicyViolation::Time, _))));
    }

    #[test]
    fn policy_violated_returns_none_when_max_time_not_started() {
        let policy = Policy {
            max_time: Some(Duration::from_millis(1)),
            ..Policy::default()
        };
        // No task started, so execution_duration is None; the time limit must
        // not trip until one has.
        let stats = Stats::new();
        assert!(policy_violated(&policy, &stats).is_none());
    }
}
