//! Connects agents to LLMs: the trait a provider implements, the handle callers
//! hold it by, and the two protocols the built-in four are configurations of.
//!
//! What a request and a reply are made of lives in [`types`](super::types).

use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use super::endpoint::Endpoint;
use super::error::{ProviderError, ProviderResult};
use super::stream::{read_reply, ResponseBuilder};
use super::types::{Message, ModelRequest, ModelResponse, ReasoningEffort, StreamEvent};

/// What every LLM provider implements.
///
/// Implement it on any type that can answer a [`ModelRequest`]. Callers hold
/// the finished thing as a [`Provider`], which any implementer converts into.
pub trait ProviderLike: Send + Sync {
    /// Run one turn: send the request, forward each [`StreamEvent`] as it
    /// arrives, and give back the assembled reply.
    ///
    /// Pass a handler that does nothing to wait only for the final
    /// [`ModelResponse`].
    fn respond(
        &self,
        request: ModelRequest,
        on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>>;
}

/// Lets a caller who already shares an implementer through an `Arc` hand it
/// over without unwrapping it.
impl<T: ProviderLike + ?Sized> ProviderLike for Arc<T> {
    fn respond(
        &self,
        request: ModelRequest,
        on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
        (**self).respond(request, on_event)
    }
}

/// Connect to a `Provider` to give agents access to LLMs.
///
/// agentwerk supports [`Anthropic`](crate::providers::Anthropic),
/// [`OpenAi`](crate::providers::OpenAi),
/// [`Mistral`](crate::providers::Mistral), and
/// [`LiteLlm`](crate::providers::LiteLlm). Each converts into a
/// `Provider`, so `.provider(Anthropic::new(key))` needs no wrapping.
/// Implement [`ProviderLike`] for anything else.
///
/// Cloning shares one connection pool, so several agents can hold the same
/// provider.
///
/// ```no_run
/// use agentwerk::Agent;
/// use agentwerk::providers::{Anthropic, Provider};
///
/// # fn run(key: &str) -> Result<(), Box<dyn std::error::Error>> {
/// let shared = Provider::from_env()?;
/// let reader = Agent::new().provider(shared.clone()).model("claude-sonnet-4-20250514");
/// let writer = Agent::new().provider(Anthropic::new(key)).model("claude-sonnet-4-20250514");
/// # let _ = (reader, writer);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Provider(Arc<dyn ProviderLike>);

impl Provider {
    /// Wrap anything that implements [`ProviderLike`].
    pub fn new(provider: impl ProviderLike + 'static) -> Self {
        Self(Arc::new(provider))
    }

    /// Detect the LLM provider from environment variables.
    pub fn from_env() -> ProviderResult<Self> {
        super::environment::provider_from_env()
    }

    /// Confirm the API key, model, and endpoint work by sending one minimal
    /// request before any real traffic.
    ///
    /// On failure the classified [`ProviderError`](super::ProviderError)
    /// (`AuthenticationFailed`, `ModelNotFound`, `ConnectionFailed`, ...) lets a
    /// caller stop with a clear message instead of failing every downstream
    /// request. One probe is enough: a wrong key, model, or endpoint fails the
    /// same way on every call.
    pub async fn verify(&self, model: &str) -> ProviderResult<()> {
        let request = ModelRequest {
            model: model.to_string(),
            system_prompt: String::new(),
            messages: vec![Message::user("ping")],
            tools: Vec::new(),
            max_request_tokens: None,
            reasoning_effort: ReasoningEffort::Off,
        };
        self.respond(request, Arc::new(|_| {})).await.map(|_| ())
    }
}

impl<P: ProviderLike + 'static> From<P> for Provider {
    fn from(provider: P) -> Self {
        Self::new(provider)
    }
}

/// Lets `provider.respond(..)` reach the trait without naming it.
impl Deref for Provider {
    type Target = dyn ProviderLike;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

/// One LLM request shape. Two exist: Anthropic's Messages API, and OpenAI's
/// chat completions, which Mistral and LiteLLM also speak. A provider is one
/// `Endpoint` plus one of these, so the four built-ins are four configurations
/// of two protocols.
pub(crate) trait Protocol {
    const PATH: &'static str;

    /// Add this vendor's authentication headers. Everything else about the call
    /// is [`Endpoint`]'s.
    fn authenticate(posted: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder;

    /// The request body, including however this protocol asks to be streamed.
    fn serialize(request: &ModelRequest) -> Value;

    /// Read the 400 bodies only this vendor words its own way. Statuses every
    /// vendor reports alike are mapped by `Endpoint::send`, and anything left
    /// falls through to `error::recover_wrapped_error`.
    fn classify_error(status: u16, body: &str) -> Option<ProviderError>;

    /// Read one payload of the reply, naming which [`ResponseBuilder`] call it is.
    fn decode(payload: &Value, reply: &mut ResponseBuilder);

    /// Only a model that writes its calls as text rather than emitting them
    /// needs this.
    fn recover(_reply: &mut ModelResponse, _on_event: &Arc<dyn Fn(StreamEvent) + Send + Sync>) {}
}

/// Run one turn of `P` against `endpoint`. Every provider answers through here,
/// so no two can disagree about the order a turn happens in.
pub(crate) fn respond<P: Protocol>(
    endpoint: &Endpoint,
    request: ModelRequest,
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
    let body = P::serialize(&request);
    Box::pin(async move {
        let posted = P::authenticate(endpoint.post(P::PATH, &body), endpoint.api_key());
        let streamed = endpoint.send(posted, P::classify_error).await?;
        let mut reply = read_reply(streamed, &on_event, P::decode).await?;
        P::recover(&mut reply, &on_event);
        Ok(reply)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::error::ProviderError;
    use crate::providers::types::{ResponseStatus, TokenUsage};

    /// Answers every request with a fixed outcome, so `Provider::verify` can be
    /// exercised without a live endpoint.
    struct FixedProvider(fn() -> ProviderResult<ModelResponse>);

    impl ProviderLike for FixedProvider {
        fn respond(
            &self,
            _request: ModelRequest,
            _on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = ProviderResult<ModelResponse>> + Send + '_>> {
            let outcome = (self.0)();
            Box::pin(async move { outcome })
        }
    }

    #[tokio::test]
    async fn a_clone_reaches_the_same_implementer() {
        let provider = Provider::new(FixedProvider(|| {
            Err(ProviderError::ProviderUnrecognized {
                message: "probe".into(),
            })
        }));
        let clone = provider.clone();

        let error = clone.verify("mock").await.unwrap_err();
        assert!(error.to_string().contains("probe"));
    }

    #[tokio::test]
    async fn an_implementer_converts_without_wrapping() {
        let provider: Provider = FixedProvider(|| {
            Err(ProviderError::ProviderUnrecognized {
                message: "probe".into(),
            })
        })
        .into();

        assert!(provider.verify("mock").await.is_err());
    }

    #[tokio::test]
    async fn verify_passes_a_bad_key_through_as_authentication_failed() {
        let provider = Provider::new(FixedProvider(|| {
            Err(ProviderError::AuthenticationFailed {
                message: "invalid api key".into(),
            })
        }));
        let error = provider.verify("any-model").await.unwrap_err();
        assert!(error.to_string().contains("Authentication failed"));
    }

    #[tokio::test]
    async fn verify_passes_a_wrong_model_through_as_model_not_found() {
        let provider = Provider::new(FixedProvider(|| {
            Err(ProviderError::ModelNotFound {
                message: "no such model".into(),
            })
        }));
        let error = provider.verify("does-not-exist").await.unwrap_err();
        assert!(error.to_string().contains("Model not found"));
    }

    #[tokio::test]
    async fn verify_returns_ok_when_the_probe_succeeds() {
        let provider = Provider::new(FixedProvider(|| {
            Ok(ModelResponse {
                content: Vec::new(),
                status: ResponseStatus::EndTurn,
                usage: TokenUsage::default(),
                model: "mock".into(),
            })
        }));
        assert!(provider.verify("any-model").await.is_ok());
    }
}
