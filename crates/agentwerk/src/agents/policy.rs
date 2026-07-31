//! The execution limits, read by the loop before each turn.

use std::time::Duration;

/// Execution limits and retry tuning the loop reads from the ticket
/// system. Set on a `TicketSystem` via `.max_turns(...)`,
/// `.max_time(...)`, `.max_input_tokens(...)`, etc. A breach emits
/// `EventKind::PolicyViolated` and halts execution.
#[derive(Clone, Debug)]
pub(crate) struct Policies {
    /// Limit on the total number of turns. `None` for no limit.
    pub max_turns: Option<u32>,
    /// Limit on the total input tokens. `None` for no limit.
    pub max_input_tokens: Option<u64>,
    /// Limit on the total output tokens. `None` for no limit.
    pub max_output_tokens: Option<u64>,
    /// Limit on the input tokens per request. `None` for no limit.
    pub max_request_tokens: Option<u32>,
    /// Limit on the schema-validation retry attempts. `None` for no limit.
    pub max_schema_retries: Option<u32>,
    /// Limit on the retry attempts on recoverable provider errors.
    pub max_request_retries: u32,
    /// Base delay between request retries.
    pub request_retry_delay: Duration,
    /// Limit on the total elapsed duration. `None` for no limit.
    pub max_time: Option<Duration>,
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
    }
}
