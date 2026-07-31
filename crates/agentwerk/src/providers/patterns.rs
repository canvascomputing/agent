//! Cross-provider error-body pattern bank. A vendor's per-code
//! classifier only recognises that vendor's wording, but in practice
//! errors arrive WRAPPED: a LiteLLM (or any OpenAI-compatible) proxy
//! returns an HTTP 400 whose body carries another provider's error
//! verbatim inside its message text, with LiteLLM's own status/code
//! on the outside. The vendor classifier sees `code = "400"` and
//! gives up; the wrapped overflow signal is then lost and the agent
//! loop treats the response as terminal instead of triggering
//! compaction. This bank runs after the vendor returns None and
//! before the generic fallback, so any of the substrings below re-classifies
//! the error correctly, however deeply it is wrapped.

use std::time::Duration;

use super::error::ProviderError;

/// Substrings that signal context-window overflow. Matched case-
/// insensitively against the full body, so they survive arbitrary
/// JSON wrapping. Grouped by origin so a reviewer adding a new
/// provider knows which cluster to extend.
const OVERFLOW_PATTERNS: &[&str] = &[
    // Anthropic: emitted by direct provider AND by any proxy passing
    // its error through (wording is stable across the message types).
    "prompt is too long",
    "request_too_large",
    // OpenAI: emitted by direct provider AND by every OpenAI-compatible
    // proxy that echoes the upstream code/message untouched.
    "this model's maximum context length",
    "exceeds the context window",
    "context_length_exceeded",
    // Mistral: distinctive wording, no overlap with the others.
    "too large for model with",
    // Vertex / Gemini: not a first-party provider here, but the user
    // hit this exact wording through LiteLLM-passthrough: Gemini's
    // 1 M-token limit phrases overflow this way and nothing else does.
    "input token count",
    "exceeds the maximum number of tokens",
    // LiteLLM's own error-class name. Catches any upstream overflow
    // regardless of vendor: LiteLLM prepends
    // `litellm.ContextWindowExceededError:` before forwarding, and
    // this matches that even when the wrapped upstream wording is
    // something none of the above patterns recognise.
    "contextwindowexceedederror",
    // Generic fallbacks for upstreams not enumerated above. The cost
    // of a false positive here is one extra compaction attempt; the
    // cost of a false negative is a ticket that fails instead of
    // recovering. We err toward the former.
    "context window exceeded",
    "maximum context length",
];

/// Substrings that look overflow-ish but are actually throttling /
/// rate-limit signals. Without this exclusion, bodies like
/// `"Throttling error: too many tokens per minute"` would match the
/// `"maximum context length"` family patterns and cause a useless
/// compaction. Checked FIRST so the rule "rate-limit beats overflow"
/// is unambiguous.
const NON_OVERFLOW_PATTERNS: &[&str] = &[
    // Prose forms.
    "rate limit",
    "too many requests",
    "throttling",
    // Camelcase exception-class names. LiteLLM and other proxies
    // forward errors as `litellm.RateLimitError: ...`; lowercased the
    // class name has no space and the prose pattern above misses.
    "ratelimit",
    // Google Vertex's RPC status for quota exhaustion, surfaces
    // through LiteLLM-passthrough when the upstream is Gemini.
    "resource_exhausted",
];

/// Final pass before `fallback_http_error`. Returns:
/// - `Some(ContextWindowExceeded)` when the body signals overflow and
///   nothing in the body suggests it's actually throttling.
/// - `Some(RateLimited)` when the body signals rate-limiting,
///   regardless of the outer HTTP status (LiteLLM occasionally wraps
///   a 429 inside a different outer code, so we can't trust status
///   alone to mean "this was a rate limit").
/// - `None` when neither matches, so the caller falls through to
///   `StatusUnclassified`, preserving existing behaviour for
///   genuinely unrecognised errors.
pub(crate) fn refine(
    status: u16,
    body: &str,
    retry_delay: Option<Duration>,
) -> Option<ProviderError> {
    // One lowercase per call: the body can be large (wrapped errors
    // carry the full upstream payload), and every pattern is
    // case-insensitive, so we pay this once and substring-scan against
    // the lowered string.
    let lower = body.to_lowercase();

    // Rate-limit exclusion runs FIRST. A body that says both
    // "throttling" and "maximum tokens" is a rate limit, not an
    // overflow: the inverse would burn a compaction round-trip and
    // still hit the same throttle on the next request.
    let looks_like_rate_limit = NON_OVERFLOW_PATTERNS.iter().any(|p| lower.contains(p));

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

    /// The exact wire body that triggered the original failure:
    /// LiteLLM wrapping a Vertex/Gemini 400 INVALID_ARGUMENT with a
    /// `code = "400"` outer field. The OpenAI vendor classifier
    /// returns None on this; `refine` must catch it.
    #[test]
    fn litellm_vertex_overflow_classifies_as_context_window() {
        let body = r#"{"error":{"message":"litellm.ContextWindowExceededError: litellm.BadRequestError: ContextWindowExceededError: Vertex_ai_betaException - b'{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"The input token count exceeds the maximum number of tokens allowed 1048576.\",\n    \"status\": \"INVALID_ARGUMENT\"\n  }\n}\n'","type":null,"param":null,"code":"400"}}"#;
        assert!(matches!(
            refine(400, body, None),
            Some(ProviderError::ContextWindowExceeded { .. })
        ));
    }

    /// LiteLLM wrapping a Vertex RESOURCE_EXHAUSTED behind its own
    /// `MidStreamFallbackError`. The outer status IS 429 so the
    /// fallback path would handle it correctly, but `refine` must
    /// also catch the rate-limit signal so a wrapped 429 returned with
    /// any other outer status still classifies correctly, AND so the
    /// `retry_delay` from headers propagates.
    #[test]
    fn litellm_rate_limit_wrap_classifies_as_rate_limited() {
        let body = r#"{"error":{"message":"litellm.MidStreamFallbackError: litellm.RateLimitError: litellm.RateLimitError: vertex_ai_betaException - b'{\n  \"error\": {\n    \"code\": 429,\n    \"message\": \"Resource exhausted. Please try again later.\",\n    \"status\": \"RESOURCE_EXHAUSTED\"\n  }\n}\n'. Received Model Group=gemini-3-flash-preview","type":null,"param":null,"code":"429"}}"#;
        let delay = Some(Duration::from_secs(2));
        match refine(429, body, delay) {
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

    /// Rule "rate-limit beats overflow". A throttling body that also
    /// contains "maximum context length" must NOT be classified as
    /// overflow: compacting would not help and the next request would
    /// hit the same throttle.
    #[test]
    fn throttling_with_overflow_wording_classifies_as_rate_limited() {
        let body = r#"{"message":"Throttling error: maximum context length per minute reached"}"#;
        match refine(400, body, None) {
            Some(ProviderError::RateLimited { .. }) => {}
            other => panic!("expected RateLimited (throttling wins), got {other:?}"),
        }
    }

    /// First-party Anthropic overflow phrasing. The Anthropic vendor
    /// classifier normally catches this, and the test confirms the
    /// shared bank is a correct defense-in-depth backstop if the
    /// vendor classifier ever regresses or misses a variant.
    #[test]
    fn anthropic_prompt_too_long_classifies_as_context_window() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#;
        assert!(matches!(
            refine(400, body, None),
            Some(ProviderError::ContextWindowExceeded { .. })
        ));
    }

    /// Genuinely unrecognised 4xx: an auth error has nothing in common
    /// with overflow or rate-limit wording, so `refine` must return
    /// None and let `fallback_http_error` handle classification.
    #[test]
    fn unrelated_400_returns_none() {
        let body = r#"{"error":{"message":"invalid_api_key: incorrect API key provided","code":"invalid_api_key"}}"#;
        assert!(refine(400, body, None).is_none());
    }
}
