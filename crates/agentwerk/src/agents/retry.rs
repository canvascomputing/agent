//! Growing waits between retries of a transient LLM provider error.
//!
//! [`Retry`] tracks the attempts, so callers never have to.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Retry strategy. Owns the attempt counter; callers consume one
/// attempt at a time and use the returned attempt number for events.
pub(crate) trait Retry {
    /// Claim one retry. Returns `Some(attempt)` (1-based) if budget
    /// was available and decrements it; `None` when exhausted.
    fn try_consume(&mut self) -> Option<u32>;

    /// Total attempt budget. Useful for `RequestRetried` event
    /// payloads where observers want to see `attempt/max_attempts`.
    fn max_attempts(&self) -> u32;

    /// Delay before the next attempt fires. `server_hint`
    /// (e.g. HTTP `Retry-After`) takes precedence when honoured;
    /// the default returns `Duration::ZERO`, suitable for strategies
    /// that retry immediately without backoff.
    fn delay(&self, _server_hint: Option<Duration>) -> Duration {
        Duration::ZERO
    }
}

/// Limit on the wait before one attempt, so it cannot grow without bound
/// past a few attempts. Matches claude-code's `maxDelayMs` default.
const MAX_RETRY_DELAY: Duration = Duration::from_millis(32_000);

/// Exponential backoff `base_delay * 2^attempt`, clamped at 32 s,
/// extended by additive jitter in `[0, 25%]` of the clamped value. A
/// `server_hint` skips the limit and the jitter: the server is explicit
/// about what it wants.
pub(crate) struct ExponentialRetry {
    base_delay: Duration,
    max_attempts: u32,
    attempt: u32,
}

impl ExponentialRetry {
    pub(crate) fn new(base_delay: Duration, max_attempts: u32) -> Self {
        Self {
            base_delay,
            max_attempts,
            attempt: 0,
        }
    }
}

impl Retry for ExponentialRetry {
    fn try_consume(&mut self) -> Option<u32> {
        if self.attempt >= self.max_attempts {
            return None;
        }
        self.attempt += 1;
        Some(self.attempt)
    }

    fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    fn delay(&self, server_hint: Option<Duration>) -> Duration {
        if let Some(hint) = server_hint {
            return hint;
        }

        // `attempt` is 1-based after a successful `try_consume`; shift
        // by 1 so the first wait uses `base_delay * 2^0 = base_delay`.
        let exponent = self.attempt.saturating_sub(1).min(31);
        let base_ms = self.base_delay.as_millis() as u64;
        let exponential_ms = base_ms
            .saturating_mul(1u64 << exponent)
            .min(MAX_RETRY_DELAY.as_millis() as u64);
        let jitter_range = exponential_ms / 4;

        if jitter_range == 0 {
            return Duration::from_millis(exponential_ms);
        }

        let entropy = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64;
        let jitter_offset = entropy % jitter_range;

        Duration::from_millis(exponential_ms.saturating_add(jitter_offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(base_ms: u64) -> ExponentialRetry {
        ExponentialRetry::new(Duration::from_millis(base_ms), 10)
    }

    #[test]
    fn exponential_backoff_grows_per_consumed_attempt() {
        let mut policy = policy(1000);
        for expected_exponent in 0..3u32 {
            let attempt = policy.try_consume().expect("budget available");
            assert_eq!(attempt, expected_exponent + 1);
            let delay = policy.delay(None);
            let expected_base_ms = 1000u64 * (1u64 << expected_exponent);
            let jitter_range_ms = expected_base_ms / 4;
            let delay_ms = delay.as_millis() as u64;
            assert!(delay_ms >= expected_base_ms);
            assert!(delay_ms <= expected_base_ms + jitter_range_ms);
        }
    }

    #[test]
    fn server_hint_takes_precedence_over_backoff() {
        let mut policy = policy(1000);
        let _ = policy.try_consume();
        let delay = policy.delay(Some(Duration::from_millis(5000)));
        assert_eq!(delay, Duration::from_millis(5000));
    }

    #[test]
    fn delay_caps_at_max_retry_delay() {
        let mut policy = policy(1000);
        for _ in 0..21 {
            let _ = policy.try_consume();
        }
        let max_ms = MAX_RETRY_DELAY.as_millis() as u64;
        let jitter_range_ms = max_ms / 4;
        let delay_ms = policy.delay(None).as_millis() as u64;
        assert!(delay_ms >= max_ms);
        assert!(delay_ms <= max_ms + jitter_range_ms);
    }

    #[test]
    fn exponential_delay_saturates_instead_of_overflowing() {
        let mut policy = ExponentialRetry::new(Duration::from_millis(u64::MAX), 50);
        for _ in 0..11 {
            let _ = policy.try_consume();
        }
        let _delay = policy.delay(None);
    }

    #[test]
    fn try_consume_returns_none_once_budget_is_exhausted() {
        let mut policy = ExponentialRetry::new(Duration::from_millis(1), 2);
        assert_eq!(policy.try_consume(), Some(1));
        assert_eq!(policy.try_consume(), Some(2));
        assert_eq!(policy.try_consume(), None);
        assert_eq!(policy.try_consume(), None);
    }
}
