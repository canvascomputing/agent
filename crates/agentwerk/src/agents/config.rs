//! What a run may spend, how it retries, and when it compacts, read by the
//! loop before each turn.

use std::time::Duration;

/// A `Config` limits the turns, tokens, and time a run may spend, and allows
/// configuring retries and compaction. Set it with `TicketQueue::config`,
/// building it from the fields you care about:
/// `Config { max_turns: Some(40), ..Default::default() }`.
///
/// `None` means no limit, except on `compaction_threshold`, the one entry that
/// cannot be breached: it moves a trigger rather than limiting anything. A
/// breached limit emits [`EventKind::ConfigViolated`] and halts execution.
///
/// [`EventKind::ConfigViolated`]: crate::EventKind::ConfigViolated
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Total turns across every agent.
    pub max_turns: Option<u32>,
    /// Total input tokens across the run.
    pub max_input_tokens: Option<u64>,
    /// Total output tokens across the run.
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
    /// Total elapsed duration of the run.
    pub max_time: Option<Duration>,
    /// Fraction of the context window at which compaction fires. `None` uses
    /// [`Config::DEFAULT_COMPACTION_THRESHOLD`].
    pub compaction_threshold: Option<f64>,
}

impl Config {
    pub const DEFAULT_MAX_SCHEMA_RETRIES: u32 = 10;
    pub const DEFAULT_MAX_REQUEST_RETRIES: u32 = 10;
    pub const DEFAULT_REQUEST_RETRY_DELAY: Duration = Duration::from_millis(500);
    /// How full the context window gets before compaction fires, when the host
    /// sets no fraction of its own. What is left over covers the model's
    /// response budget and the slack the `bytes / 4` token estimate needs
    /// because it under-counts code and JSON. A share rather than a fixed
    /// count, so it holds from the smallest window to the largest.
    pub const DEFAULT_COMPACTION_THRESHOLD: f64 = 0.85;
}

impl Default for Config {
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
            compaction_threshold: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_documented_values() {
        assert_eq!(
            Config::default(),
            Config {
                max_turns: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_request_tokens: None,
                max_schema_retries: Some(10),
                max_request_retries: 10,
                request_retry_delay: Duration::from_millis(500),
                max_time: None,
                compaction_threshold: None,
            }
        );
    }
}
