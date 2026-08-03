//! What can go wrong before an LLM provider produces a response. A response
//! that did arrive reports through `ResponseStatus` instead.

use std::fmt;
use std::time::Duration;

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
    /// Provider construction failed to resolve a provider from the
    /// environment: no provider was detected, a required env var was unset,
    /// or `LITELLM_PROVIDER` named an unknown provider. `message` states the
    /// specific failure.
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

    /// Categorical discriminant for event observers. One variant per
    /// `ProviderError` case; payloads stripped.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

impl serde::Serialize for RequestErrorKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Result alias for [`Provider`](super::Provider) calls.
pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

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
}
