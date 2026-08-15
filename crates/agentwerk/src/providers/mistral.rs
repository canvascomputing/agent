//! The Mistral provider, which sends OpenAI-shaped requests to api.mistral.ai.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::endpoint::Endpoint;
use super::error::ProviderResult;
use super::openai;
use super::provider::{ModelRequest, ProviderLike};
use super::types::{ModelResponse, StreamEvent};

const DEFAULT_BASE_URL: &str = "https://api.mistral.ai";

/// LLM provider for the Mistral API. Sends the same request shape as OpenAI's
/// chat completions against `api.mistral.ai`.
///
/// Reads `MISTRAL_API_KEY` (and optional `MISTRAL_BASE_URL`) when built
/// via [`Provider::from_env`]. Override the endpoint with [`base_url`] and
/// the per-request timeout with [`timeout`].
///
/// # Examples
///
/// Direct construction with an API key:
///
/// ```no_run
/// use agentwerk::providers::Mistral;
///
/// let _provider = Mistral::new("...");
/// ```
///
/// Read the API key from the environment:
///
/// ```no_run
/// use agentwerk::providers::Provider;
///
/// let _provider = Provider::from_env().expect("LLM provider required");
/// ```
///
/// [`Provider::from_env`]: crate::providers::Provider::from_env
/// [`base_url`]: Mistral::base_url
/// [`timeout`]: Mistral::timeout
pub struct Mistral(Endpoint);

impl Mistral {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self(Endpoint::new(api_key, DEFAULT_BASE_URL))
    }

    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.0 = self.0.base_url(url);
        self
    }

    pub fn timeout(mut self, duration: Duration) -> Self {
        self.0 = self.0.timeout(duration);
        self
    }

    pub(crate) fn from_env() -> ProviderResult<Self> {
        use super::environment::{env_or, env_required};
        Ok(Self::new(env_required("MISTRAL_API_KEY")?)
            .base_url(env_or("MISTRAL_BASE_URL", DEFAULT_BASE_URL)))
    }
}

impl ProviderLike for Mistral {
    fn respond(
        &self,
        request: ModelRequest,
        on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
        openai::respond(&self.0, request, on_event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::endpoint::DEFAULT_REQUEST_TIMEOUT;

    #[test]
    fn a_new_provider_points_at_the_mistral_api() {
        let provider = Mistral::new("test-key");
        assert_eq!(provider.0.api_key(), "test-key");
        assert_eq!(provider.0.get_timeout(), DEFAULT_REQUEST_TIMEOUT);
    }

    #[test]
    fn the_timeout_builder_reaches_the_endpoint() {
        let provider = Mistral::new("test-key").timeout(Duration::from_secs(42));
        assert_eq!(provider.0.get_timeout(), Duration::from_secs(42));
    }
}
