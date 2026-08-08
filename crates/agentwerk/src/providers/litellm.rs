//! The LiteLLM provider, which sends OpenAI-shaped requests to a local LiteLLM
//! instance, so switching LLM providers changes no agent code.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::error::ProviderResult;
use super::openai::OpenAiProvider;
use super::provider::{ModelRequest, ProviderLike};
use super::types::{ModelResponse, StreamEvent};

/// LLM provider for a LiteLLM proxy. Speaks the OpenAI-compatible wire
/// format against a local or remote LiteLLM instance so the model name on
/// the request decides which upstream backend handles it.
///
/// Reads `LITELLM_API_KEY` (optional) and `LITELLM_BASE_URL` (defaults to
/// `http://localhost:4000`) when built via [`Provider::from_env`]. Override
/// the endpoint with [`base_url`] and the per-request timeout with
/// [`timeout`].
///
/// # Examples
///
/// Direct construction pointed at a local proxy:
///
/// ```no_run
/// use agentwerk::providers::LiteLlmProvider;
///
/// let _provider = LiteLlmProvider::new("").base_url("http://localhost:4000");
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
/// [`base_url`]: LiteLlmProvider::base_url
/// [`timeout`]: LiteLlmProvider::timeout
pub struct LiteLlmProvider(OpenAiProvider);

const DEFAULT_BASE_URL: &str = "http://localhost:4000";

impl LiteLlmProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self(OpenAiProvider::raw(api_key, DEFAULT_BASE_URL))
    }

    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.0 = self.0.base_url(url);
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.0 = self.0.timeout(d);
        self
    }

    pub(crate) fn from_env() -> ProviderResult<Self> {
        use super::environment::env_or;
        Ok(Self::new(env_or("LITELLM_API_KEY", ""))
            .base_url(env_or("LITELLM_BASE_URL", DEFAULT_BASE_URL)))
    }
}

impl ProviderLike for LiteLlmProvider {
    fn respond(
        &self,
        request: ModelRequest,
        on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
        self.0.respond(request, on_event)
    }

    fn prewarm(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.0.prewarm()
    }
}
