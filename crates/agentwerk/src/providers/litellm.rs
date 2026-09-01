//! The LiteLLM provider, which sends OpenAI-shaped requests to a local LiteLLM
//! instance, so switching LLM providers changes no agent code.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::endpoint::Endpoint;
use super::error::ProviderResult;
use super::openai::OpenAiChat;
use super::provider::{self, ProviderLike};
use super::types::{ModelRequest, ModelResponse, StreamEvent};

const DEFAULT_BASE_URL: &str = "http://localhost:4000";

/// LLM provider for a LiteLLM proxy. Sends the same request shape as OpenAI's
/// chat completions to a local or remote LiteLLM instance, so the model name on
/// the request decides which upstream backend handles it.
///
/// Build it through [`Provider::from_env`] to read the optional `LITELLM_API_KEY` and `LITELLM_BASE_URL`, which defaults to `http://localhost:4000`. Override the endpoint with [`base_url`] and the per-request timeout with [`timeout`].
///
/// # Examples
///
/// Direct construction pointed at a local proxy:
///
/// ```no_run
/// use agentwerk::providers::LiteLlm;
///
/// let _provider = LiteLlm::new("").base_url("http://localhost:4000");
/// ```
///
/// Read configuration from the environment:
///
/// ```no_run
/// use agentwerk::providers::Provider;
///
/// let _provider = Provider::from_env().expect("LLM provider required");
/// ```
///
/// [`Provider::from_env`]: crate::providers::Provider::from_env
/// [`base_url`]: LiteLlm::base_url
/// [`timeout`]: LiteLlm::timeout
pub struct LiteLlm(Endpoint);

impl LiteLlm {
    /// Create an endpoint using the API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self(Endpoint::new(api_key, DEFAULT_BASE_URL))
    }

    /// Replace the provider API base URL.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.0 = self.0.base_url(url);
        self
    }

    /// Set the request timeout.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.0 = self.0.timeout(duration);
        self
    }

    pub(crate) fn from_env() -> ProviderResult<Self> {
        use super::environment::env_or;
        Ok(Self::new(env_or("LITELLM_API_KEY", ""))
            .base_url(env_or("LITELLM_BASE_URL", DEFAULT_BASE_URL)))
    }
}

impl ProviderLike for LiteLlm {
    fn respond(
        &self,
        request: ModelRequest,
        on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
        provider::respond::<OpenAiChat>(&self.0, request, on_event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_provider_points_at_a_local_proxy() {
        assert_eq!(LiteLlm::new("").0.get_base_url(), DEFAULT_BASE_URL);
    }

    #[test]
    fn a_proxy_that_wants_no_key_is_built_with_an_empty_one() {
        // A local proxy commonly runs unauthenticated, so blank is legitimate here.
        assert_eq!(LiteLlm::new("").0.api_key(), "");
    }

    #[test]
    fn the_base_url_builder_replaces_the_local_default() {
        let provider = LiteLlm::new("").base_url("https://proxy.example.test");
        assert_eq!(provider.0.get_base_url(), "https://proxy.example.test");
    }

    #[test]
    fn the_timeout_builder_reaches_the_endpoint() {
        let provider = LiteLlm::new("").timeout(Duration::from_secs(42));
        assert_eq!(provider.0.get_timeout(), Duration::from_secs(42));
    }
}
