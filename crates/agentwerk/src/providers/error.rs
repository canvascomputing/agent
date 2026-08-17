//! What can go wrong before an LLM provider produces a response. A response
//! that did arrive reports through `ResponseStatus` instead.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Failure produced by a provider call.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProviderError {
    /// HTTP 401: invalid, revoked, or missing credentials.
    AuthenticationFailed { message: String },
    /// HTTP 403: authenticated but not allowed to use the resource.
    PermissionDenied { message: String },
    /// HTTP 400/404: unknown model id.
    ModelNotFound { message: String },
    /// HTTP 400 pre-flight: request tokens exceed the model's context window.
    ContextWindowExceeded { message: String },
    /// Provider-side safety filter blocked the request input.
    SafetyFilterTriggered { message: String },
    /// HTTP 429 / 529: retry with backoff, honouring `retry_delay` if set.
    RateLimited {
        message: String,
        status: u16,
        retry_delay: Option<Duration>,
    },
    /// HTTP error with no more specific classification (unclassified 4xx,
    /// generic 5xx). `retryable` is true for standard transient server
    /// errors (500/502/503/504).
    StatusUnclassified {
        status: u16,
        message: String,
        retryable: bool,
        retry_delay: Option<Duration>,
    },
    /// Network / TLS / connection failure before any HTTP response.
    ConnectionFailed { message: String },
    /// The stream was cut off mid-body after headers arrived. Distinct from
    /// `ConnectionFailed` (pre-response) and `ResponseMalformed` (structurally
    /// broken payload): the transport broke while chunks were still in flight.
    StreamInterrupted { message: String },
    /// The response arrived but its body could not be read: malformed JSON, an
    /// unexpected shape, or a broken frame.
    ResponseMalformed { message: String },
    /// No provider could be resolved from the environment: none was detected,
    /// a required variable was unset, or `LITELLM_PROVIDER` named one that does
    /// not exist. `message` states which.
    ProviderUnrecognized { message: String },
}

impl ProviderError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::ConnectionFailed { .. }
                | ProviderError::StreamInterrupted { .. }
                | ProviderError::StatusUnclassified {
                    retryable: true,
                    ..
                }
        )
    }

    pub fn retry_delay(&self) -> Option<Duration> {
        match self {
            ProviderError::RateLimited { retry_delay, .. } => *retry_delay,
            ProviderError::StatusUnclassified { retry_delay, .. } => *retry_delay,
            _ => None,
        }
    }

    pub fn kind(&self) -> RequestErrorKind {
        match self {
            ProviderError::AuthenticationFailed { .. } => RequestErrorKind::AuthenticationFailed,
            ProviderError::PermissionDenied { .. } => RequestErrorKind::PermissionDenied,
            ProviderError::ModelNotFound { .. } => RequestErrorKind::ModelNotFound,
            ProviderError::ContextWindowExceeded { .. } => RequestErrorKind::ContextWindowExceeded,
            ProviderError::SafetyFilterTriggered { .. } => RequestErrorKind::SafetyFilterTriggered,
            ProviderError::RateLimited { .. } => RequestErrorKind::RateLimited,
            ProviderError::StatusUnclassified { .. } => RequestErrorKind::StatusUnclassified,
            ProviderError::ConnectionFailed { .. } => RequestErrorKind::ConnectionFailed,
            ProviderError::StreamInterrupted { .. } => RequestErrorKind::StreamInterrupted,
            ProviderError::ResponseMalformed { .. } => RequestErrorKind::ResponseMalformed,
            ProviderError::ProviderUnrecognized { .. } => RequestErrorKind::ProviderUnrecognized,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::AuthenticationFailed { message } => {
                write!(f, "Authentication failed: {message}")
            }
            ProviderError::PermissionDenied { message } => {
                write!(f, "Permission denied: {message}")
            }
            ProviderError::ModelNotFound { message } => {
                write!(f, "Model not found: {message}")
            }
            ProviderError::ContextWindowExceeded { message } => {
                write!(f, "Context window exceeded: {message}")
            }
            ProviderError::SafetyFilterTriggered { message } => {
                write!(f, "Safety filter triggered: {message}")
            }
            ProviderError::RateLimited {
                message, status, ..
            } => {
                write!(f, "Rate limited (status {status}): {message}")
            }
            ProviderError::StatusUnclassified {
                status,
                message,
                retryable,
                ..
            } => {
                write!(
                    f,
                    "HTTP error (status {status}): {message} (retryable: {retryable})"
                )
            }
            ProviderError::ConnectionFailed { message } => {
                write!(f, "Connection failed: {message}")
            }
            ProviderError::StreamInterrupted { message } => {
                write!(f, "Stream interrupted: {message}")
            }
            ProviderError::ResponseMalformed { message } => {
                write!(f, "Response malformed: {message}")
            }
            ProviderError::ProviderUnrecognized { message } => {
                write!(f, "{message}")
            }
        }
    }
}

impl std::error::Error for ProviderError {}

/// Categorical discriminant of [`ProviderError`] for event observers. Mirrors
/// the variants of `ProviderError` without their payloads, so matching on
/// `kind` is a stable branching point independent of the detail carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestErrorKind {
    AuthenticationFailed,
    PermissionDenied,
    ModelNotFound,
    ContextWindowExceeded,
    SafetyFilterTriggered,
    RateLimited,
    StatusUnclassified,
    ConnectionFailed,
    StreamInterrupted,
    ResponseMalformed,
    ProviderUnrecognized,
}

impl RequestErrorKind {
    /// Every kind, in the order they are declared.
    pub const ALL: &'static [RequestErrorKind] = &[
        RequestErrorKind::AuthenticationFailed,
        RequestErrorKind::PermissionDenied,
        RequestErrorKind::ModelNotFound,
        RequestErrorKind::ContextWindowExceeded,
        RequestErrorKind::SafetyFilterTriggered,
        RequestErrorKind::RateLimited,
        RequestErrorKind::StatusUnclassified,
        RequestErrorKind::ConnectionFailed,
        RequestErrorKind::StreamInterrupted,
        RequestErrorKind::ResponseMalformed,
        RequestErrorKind::ProviderUnrecognized,
    ];

    /// The stable snake_case spelling, which is also the counter this failure
    /// adds to under `models.<name>`.
    pub fn as_str(&self) -> &'static str {
        match self {
            RequestErrorKind::AuthenticationFailed => "authentication_failed",
            RequestErrorKind::PermissionDenied => "permission_denied",
            RequestErrorKind::ModelNotFound => "model_not_found",
            RequestErrorKind::ContextWindowExceeded => "context_window_exceeded",
            RequestErrorKind::SafetyFilterTriggered => "safety_filter_triggered",
            RequestErrorKind::RateLimited => "rate_limited",
            RequestErrorKind::StatusUnclassified => "status_unclassified",
            RequestErrorKind::ConnectionFailed => "connection_failed",
            RequestErrorKind::StreamInterrupted => "stream_interrupted",
            RequestErrorKind::ResponseMalformed => "response_malformed",
            RequestErrorKind::ProviderUnrecognized => "provider_unrecognized",
        }
    }
}

impl fmt::Display for RequestErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result alias for [`Provider`](super::Provider) calls.
pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

// A proxy wraps another provider's error behind its own status and code, so the
// vendor classifier gives up and the loop ends the ticket instead of compacting.
// The two banks below are read after that classifier, before the generic
// fallback.

/// Matched case-insensitively against the whole body, so they survive wrapping.
const OVERFLOW_PATTERNS: &[&str] = &[
    // Anthropic
    "prompt is too long",
    "input length and `max_tokens` exceed context limit",
    "request_too_large",
    // OpenAI
    "this model's maximum context length",
    "exceeds the context window",
    "context_length_exceeded",
    "exceed_context_size_error",
    // Mistral
    "too large for model with",
    // Vertex / Gemini through LiteLLM. Nothing else phrases overflow this way.
    "input token count",
    "exceeds the maximum number of tokens",
    // LiteLLM's own prefix, for an upstream wording none of the above knows.
    "contextwindowexceedederror",
    // Broad on purpose: a false positive costs one compaction, a false negative
    // the ticket.
    "context window exceeded",
    "maximum context length",
];

/// Throttling signals, several of which also match [`OVERFLOW_PATTERNS`].
const RATE_LIMIT_PATTERNS: &[&str] = &[
    "rate limit",
    "too many requests",
    "throttling",
    // A class name like `litellm.RateLimitError` has no space for the form above.
    "ratelimit",
    // Google Vertex's RPC status for quota exhaustion.
    "resource_exhausted",
];

/// Read the upstream signal out of `body`, whatever the outer status says: a
/// proxy wraps a 429 inside another code often enough that the status alone
/// cannot be trusted. `None` leaves the body to `fallback_http_error`.
pub(crate) fn recover_wrapped_error(
    status: u16,
    body: &str,
    retry_delay: Option<Duration>,
) -> Option<ProviderError> {
    // One lowercase per call, not one per pattern: a wrapped body carries the
    // full upstream payload.
    let lower = body.to_lowercase();

    // A body saying both is throttled: reading overflow first would spend a
    // compaction and meet the same throttle on the next request.
    let looks_like_rate_limit = RATE_LIMIT_PATTERNS.iter().any(|p| lower.contains(p));

    if !looks_like_rate_limit && OVERFLOW_PATTERNS.iter().any(|p| lower.contains(p)) {
        return Some(ProviderError::ContextWindowExceeded {
            message: body.to_string(),
        });
    }

    if looks_like_rate_limit {
        return Some(ProviderError::RateLimited {
            status,
            message: body.to_string(),
            retry_delay,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_is_retryable_and_carries_retry_after() {
        let err = ProviderError::RateLimited {
            message: "slow down".into(),
            status: 429,
            retry_delay: Some(Duration::from_millis(500)),
        };
        assert!(err.is_retryable());
        assert_eq!(err.retry_delay(), Some(Duration::from_millis(500)));
    }

    #[test]
    fn connection_failed_is_retryable() {
        let err = ProviderError::ConnectionFailed {
            message: "dns".into(),
        };
        assert!(err.is_retryable());
        assert_eq!(err.retry_delay(), None);
    }

    #[test]
    fn stream_interrupted_is_retryable() {
        let err = ProviderError::StreamInterrupted {
            message: "error decoding response body".into(),
        };
        assert!(err.is_retryable());
        assert_eq!(err.retry_delay(), None);
        assert!(err.to_string().starts_with("Stream interrupted:"));
    }

    #[test]
    fn unexpected_status_honours_retryable_flag() {
        let retryable = ProviderError::StatusUnclassified {
            status: 503,
            message: "unavailable".into(),
            retryable: true,
            retry_delay: None,
        };
        let terminal = ProviderError::StatusUnclassified {
            status: 418,
            message: "teapot".into(),
            retryable: false,
            retry_delay: None,
        };
        assert!(retryable.is_retryable());
        assert!(!terminal.is_retryable());
    }

    #[test]
    fn classified_variants_are_not_retryable() {
        for err in [
            ProviderError::AuthenticationFailed {
                message: String::new(),
            },
            ProviderError::PermissionDenied {
                message: String::new(),
            },
            ProviderError::ModelNotFound {
                message: String::new(),
            },
            ProviderError::ContextWindowExceeded {
                message: String::new(),
            },
            ProviderError::SafetyFilterTriggered {
                message: String::new(),
            },
            ProviderError::ResponseMalformed {
                message: String::new(),
            },
        ] {
            assert!(!err.is_retryable(), "expected terminal: {err:?}");
        }
    }

    #[test]
    fn kind_covers_every_variant() {
        let every = [
            ProviderError::AuthenticationFailed {
                message: String::new(),
            },
            ProviderError::PermissionDenied {
                message: String::new(),
            },
            ProviderError::ModelNotFound {
                message: String::new(),
            },
            ProviderError::ContextWindowExceeded {
                message: String::new(),
            },
            ProviderError::SafetyFilterTriggered {
                message: String::new(),
            },
            ProviderError::RateLimited {
                message: String::new(),
                status: 429,
                retry_delay: None,
            },
            ProviderError::StatusUnclassified {
                status: 500,
                message: String::new(),
                retryable: true,
                retry_delay: None,
            },
            ProviderError::ConnectionFailed {
                message: String::new(),
            },
            ProviderError::StreamInterrupted {
                message: String::new(),
            },
            ProviderError::ResponseMalformed {
                message: String::new(),
            },
            ProviderError::ProviderUnrecognized {
                message: "no provider".into(),
            },
        ];
        let kinds: Vec<RequestErrorKind> = every.iter().map(|e| e.kind()).collect();
        assert_eq!(kinds.len(), 11);
    }

    #[test]
    fn all_variants_display_non_empty() {
        let variants = [
            ProviderError::AuthenticationFailed {
                message: "bad key".into(),
            },
            ProviderError::PermissionDenied {
                message: "nope".into(),
            },
            ProviderError::ModelNotFound {
                message: "no such model".into(),
            },
            ProviderError::ContextWindowExceeded {
                message: "too long".into(),
            },
            ProviderError::SafetyFilterTriggered {
                message: "blocked".into(),
            },
            ProviderError::RateLimited {
                message: "slow".into(),
                status: 429,
                retry_delay: Some(Duration::from_millis(1000)),
            },
            ProviderError::StatusUnclassified {
                status: 500,
                message: "boom".into(),
                retryable: true,
                retry_delay: None,
            },
            ProviderError::ConnectionFailed {
                message: "dns".into(),
            },
            ProviderError::StreamInterrupted {
                message: "chunk read error".into(),
            },
            ProviderError::ResponseMalformed {
                message: "bad json".into(),
            },
            ProviderError::ProviderUnrecognized {
                message: "ANTHROPIC_API_KEY environment variable not set".into(),
            },
        ];
        for v in &variants {
            assert!(!format!("{v}").is_empty(), "empty display: {v:?}");
        }
    }

    /// The exact body that triggered the original failure: LiteLLM wrapping a
    /// Vertex/Gemini 400 INVALID_ARGUMENT with a `code = "400"` outer field. The
    /// OpenAI vendor classifier returns None on this.
    #[test]
    fn litellm_vertex_overflow_classifies_as_context_window() {
        let body = r#"{"error":{"message":"litellm.ContextWindowExceededError: litellm.BadRequestError: ContextWindowExceededError: Vertex_ai_betaException - b'{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"The input token count exceeds the maximum number of tokens allowed 1048576.\",\n    \"status\": \"INVALID_ARGUMENT\"\n  }\n}\n'","type":null,"param":null,"code":"400"}}"#;
        assert!(matches!(
            recover_wrapped_error(400, body, None),
            Some(ProviderError::ContextWindowExceeded { .. })
        ));
    }

    /// LiteLLM wrapping a Vertex RESOURCE_EXHAUSTED behind its own
    /// `MidStreamFallbackError`. The outer status IS 429 so the fallback path
    /// would handle it, but a wrapped 429 returned with any other outer status
    /// must classify correctly too, and the `retry_delay` must propagate.
    #[test]
    fn litellm_rate_limit_wrap_classifies_as_rate_limited() {
        let body = r#"{"error":{"message":"litellm.MidStreamFallbackError: litellm.RateLimitError: litellm.RateLimitError: vertex_ai_betaException - b'{\n  \"error\": {\n    \"code\": 429,\n    \"message\": \"Resource exhausted. Please try again later.\",\n    \"status\": \"RESOURCE_EXHAUSTED\"\n  }\n}\n'. Received Model Group=gemini-3-flash-preview","type":null,"param":null,"code":"429"}}"#;
        let delay = Some(Duration::from_secs(2));
        match recover_wrapped_error(429, body, delay) {
            Some(ProviderError::RateLimited {
                status,
                retry_delay,
                ..
            }) => {
                assert_eq!(status, 429);
                assert_eq!(retry_delay, delay);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn throttling_with_overflow_wording_classifies_as_rate_limited() {
        // Compacting would not help and the next request would hit the same throttle.
        let body = r#"{"message":"Throttling error: maximum context length per minute reached"}"#;
        match recover_wrapped_error(400, body, None) {
            Some(ProviderError::RateLimited { .. }) => {}
            other => panic!("expected RateLimited (throttling wins), got {other:?}"),
        }
    }

    #[test]
    fn a_body_carrying_both_signals_is_a_rate_limit_whichever_vendor_sent_it() {
        // Each vendor's own overflow wording, throttled.
        for body in [
            r#"{"error":{"message":"Rate limit reached: prompt is too long"}}"#,
            r#"{"error":{"message":"429 Too Many Requests: this model's maximum context length is 8192"}}"#,
            r#"{"error":{"message":"litellm.RateLimitError: input token count exceeds the maximum number of tokens"}}"#,
        ] {
            assert!(
                matches!(
                    recover_wrapped_error(400, body, None),
                    Some(ProviderError::RateLimited { .. })
                ),
                "{body}"
            );
        }
    }

    /// First-party Anthropic phrasing, now that its own classifier reads codes
    /// alone: this bank is what answers an overflow, whoever sent it.
    #[test]
    fn anthropic_prompt_too_long_classifies_as_context_window() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#;
        assert!(matches!(
            recover_wrapped_error(400, body, None),
            Some(ProviderError::ContextWindowExceeded { .. })
        ));
    }

    #[test]
    fn unrelated_400_returns_none() {
        let body = r#"{"error":{"message":"invalid_api_key: incorrect API key provided","code":"invalid_api_key"}}"#;
        assert!(recover_wrapped_error(400, body, None).is_none());
    }
}
