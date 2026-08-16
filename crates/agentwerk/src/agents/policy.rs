//! The execution limits, read by the loop before each turn.

use std::time::Duration;

/// Execution limits and retry tuning the loop reads from the ticket
/// queue. Set on a `TicketQueue` via `.max_turns(...)`,
/// `.max_time(...)`, `.max_input_tokens(...)`, etc. A breached limit emits
/// `EventKind::PolicyViolated` and halts execution. `None` means no limit,
/// except on `compact_at`, the one entry that cannot be breached: it moves a
/// trigger rather than limiting anything.
#[derive(Clone, Debug)]
pub(crate) struct Policies {
    pub max_turns: Option<u32>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// The output tokens of a single request, not its input.
    pub max_request_tokens: Option<u32>,
    /// Consecutive failed tool calls and silent no-tool replies; any
    /// successful call resets the count.
    pub max_schema_retries: Option<u32>,
    /// Retries on recoverable provider errors.
    pub max_request_retries: u32,
    /// Base of the exponential backoff between request retries.
    pub request_retry_delay: Duration,
    pub max_time: Option<Duration>,
    /// Fraction of the context window at which compaction fires. `None` uses
    /// the built-in default fraction.
    pub compact_at: Option<f64>,
}

impl Policies {
    pub const DEFAULT_MAX_SCHEMA_RETRIES: u32 = 10;
    pub const DEFAULT_MAX_REQUEST_RETRIES: u32 = 10;
    pub const DEFAULT_REQUEST_RETRY_DELAY: Duration = Duration::from_millis(500);
}

impl Default for Policies {
    fn default() -> Self {
        Self {
            max_turns: None,
            max_input_tokens: None,
            max_output_tokens: None,
            max_request_tokens: None,
            max_schema_retries: Some(Self::DEFAULT_MAX_SCHEMA_RETRIES),
            max_request_retries: Self::DEFAULT_MAX_REQUEST_RETRIES,
            request_retry_delay: Self::DEFAULT_REQUEST_RETRY_DELAY,
            max_time: None,
            compact_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_documented_values() {
        let p = Policies::default();
        assert_eq!(p.max_turns, None);
        assert_eq!(p.max_input_tokens, None);
        assert_eq!(p.max_output_tokens, None);
        assert_eq!(p.max_request_tokens, None);
        assert_eq!(p.max_schema_retries, Some(10));
        assert_eq!(p.max_request_retries, 10);
        assert_eq!(p.request_retry_delay, Duration::from_millis(500));
        assert_eq!(p.max_time, None);
        assert_eq!(p.compact_at, None);
    }
}
