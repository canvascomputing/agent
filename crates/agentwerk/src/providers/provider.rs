//! Connects agents to LLMs, and the request they send.

use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::ProviderResult;
use super::types::{Message, ModelResponse, StreamEvent};

/// One tool as the model is told about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderToolDefinition {
    /// The name the model calls the tool by.
    pub name: String,
    /// What the tool does, in the words the model reads.
    pub description: String,
    /// What the tool accepts, as JSON Schema.
    pub input_schema: Value,
}

/// How much reasoning to ask the model for.
///
/// Each LLM provider has its own field for it. `Off` sends none, leaving the
/// model's own default. This shapes only the request: whatever reasoning comes
/// back is always kept as a `Thinking`
/// [`ContentBlock`](super::ContentBlock).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningEffort {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    /// The value sent with the request, `"low"`, `"medium"`, or `"high"`, or
    /// `None` when off. Every supported LLM provider takes these same words.
    pub(crate) fn label(self) -> Option<&'static str> {
        match self {
            ReasoningEffort::Off => None,
            ReasoningEffort::Low => Some("low"),
            ReasoningEffort::Medium => Some("medium"),
            ReasoningEffort::High => Some("high"),
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label().unwrap_or("off"))
    }
}

/// One request to an LLM provider, assembled from the agent's configuration and
/// the conversation so far.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    /// Which model to ask, such as `claude-sonnet-4-20250514`.
    pub model: String,
    /// The system prompt, assembled from the role, the behavior, and the
    /// facts of the moment.
    pub system_prompt: String,
    /// Everything said so far, ending with the latest input.
    pub messages: Vec<Message>,
    /// The tools the model may call this turn.
    pub tools: Vec<ProviderToolDefinition>,
    /// Limit on this request's output tokens, or `None` for the LLM provider's
    /// own default.
    pub max_request_tokens: Option<u32>,
    /// Which tool the model may pick this turn.
    pub tool_choice: Option<ToolChoice>,
    /// How much reasoning to ask for, taken from the [`Model`](super::Model).
    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,
}

/// Which tool the model may pick on one turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolChoice {
    /// The model picks freely, or replies without calling a tool.
    Auto,
    /// The model must call this tool.
    Specific { name: String },
}

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
            tool_choice: None,
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
