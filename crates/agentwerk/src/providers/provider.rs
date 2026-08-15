//! Connects agents to LLMs, and the request they send.

use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::environment;
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

    /// Warm the TCP+TLS connection pool before the first request.
    ///
    /// Called automatically by the agent loop before the first turn. Sends
    /// a fire-and-forget HEAD request to the provider's base URL so the
    /// TLS handshake (~100-200 ms) overlaps with agent startup, letting
    /// the first real request reuse the already-established connection.
    ///
    /// Default implementation is a no-op: override in providers that own
    /// a `reqwest::Client` and call `prewarm_with` from there.
    fn prewarm(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }

    /// Confirm the API key, model, and endpoint work by sending one minimal
    /// request before any real traffic. On failure the classified `ProviderError`
    /// (`AuthenticationFailed`, `ModelNotFound`, `ConnectionFailed`, ...) lets a
    /// caller stop with a clear message instead of failing every downstream
    /// request. One probe is enough: a wrong key, model, or endpoint fails the
    /// same way on every call.
    fn verify(&self, model: &str) -> Pin<Box<dyn Future<Output = ProviderResult<()>> + Send + '_>> {
        let request = ModelRequest {
            model: model.to_string(),
            system_prompt: String::new(),
            messages: vec![Message::user("ping")],
            tools: Vec::new(),
            max_request_tokens: None,
            tool_choice: None,
            reasoning_effort: ReasoningEffort::Off,
        };
        Box::pin(async move { self.respond(request, Arc::new(|_| {})).await.map(|_| ()) })
    }
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

    fn prewarm(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        (**self).prewarm()
    }

    fn verify(&self, model: &str) -> Pin<Box<dyn Future<Output = ProviderResult<()>> + Send + '_>> {
        (**self).verify(model)
    }
}

/// Connect to a `Provider` to give agents access to LLMs.
///
/// agentwerk supports [`AnthropicProvider`](crate::providers::AnthropicProvider),
/// [`OpenAiProvider`](crate::providers::OpenAiProvider),
/// [`MistralProvider`](crate::providers::MistralProvider), and
/// [`LiteLlmProvider`](crate::providers::LiteLlmProvider). Each converts into a
/// `Provider`, so `.provider(AnthropicProvider::new(key))` needs no wrapping.
/// Implement [`ProviderLike`] for anything else.
///
/// Cloning shares one connection pool, so several agents can hold the same
/// provider.
///
/// ```no_run
/// use agentwerk::Agent;
/// use agentwerk::providers::{AnthropicProvider, Provider};
///
/// # fn run(key: &str) -> Result<(), Box<dyn std::error::Error>> {
/// let shared = Provider::from_env()?;
/// let reader = Agent::new().provider(shared.clone()).model("claude-sonnet-4-20250514");
/// let writer = Agent::new().provider(AnthropicProvider::new(key)).model("claude-sonnet-4-20250514");
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

pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Build the `reqwest::Client` every built-in provider uses. The
/// whole-request timeout makes a hung endpoint surface as a retryable
/// `ConnectionFailed` instead of blocking indefinitely. The 10-minute default
/// mirrors Anthropic's own reference agent (`claude-code`).
///
/// Trusts custom certificate authorities the way OpenSSL and curl do: when
/// `SSL_CERT_FILE` or `SSL_CERT_DIR` is set, the built-in root store is
/// replaced entirely by the certificates loaded from them.
pub(crate) fn build_client(timeout: Duration) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    let file = environment::env_opt("SSL_CERT_FILE");
    let dir = environment::env_opt("SSL_CERT_DIR");
    if file.is_some() || dir.is_some() {
        builder = builder.tls_built_in_root_certs(false);
        for cert in root_certificates_from(file.as_deref(), dir.as_deref()) {
            builder = builder.add_root_certificate(cert);
        }
    }
    builder
        .build()
        .expect("reqwest::Client with timeout should build")
}

/// Load CA certificates from a PEM bundle file, a directory of PEM files, or
/// both. Panics if either path is unreadable, contains invalid PEM data, or
/// if the combined result is empty: with the built-in roots off, an empty trust
/// store would quietly reject every connection.
fn root_certificates_from(file: Option<&str>, dir: Option<&str>) -> Vec<reqwest::Certificate> {
    let mut certs = Vec::new();
    if let Some(path) = file {
        let pem = std::fs::read(path).unwrap_or_else(|e| panic!("SSL_CERT_FILE {path}: {e}"));
        certs.extend(
            reqwest::Certificate::from_pem_bundle(&pem)
                .unwrap_or_else(|e| panic!("SSL_CERT_FILE {path}: {e}")),
        );
    }
    if let Some(path) = dir {
        let entries =
            std::fs::read_dir(path).unwrap_or_else(|e| panic!("SSL_CERT_DIR {path}: {e}"));
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| panic!("SSL_CERT_DIR {path}: {e}"));
            let Ok(pem) = std::fs::read(entry.path()) else {
                continue;
            };
            // c-rehash directories mix certs with symlinks/hashes; skip what doesn't parse
            if let Ok(found) = reqwest::Certificate::from_pem_bundle(&pem) {
                certs.extend(found);
            }
        }
    }
    assert!(
        !certs.is_empty(),
        "SSL_CERT_FILE/SSL_CERT_DIR set but no certificates loaded: every connection would be rejected"
    );
    certs
}

/// Fire-and-forget HEAD request to warm the TCP+TLS connection pool. Helper
/// for a `Provider::prewarm` of your own. Pass it that provider's
/// `reqwest::Client` and base URL.
pub(crate) async fn prewarm_with(client: &reqwest::Client, base_url: &str) {
    let _ = tokio::time::timeout(Duration::from_secs(10), client.head(base_url).send()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::error::ProviderError;
    use crate::providers::types::{ResponseStatus, TokenUsage};

    #[test]
    fn default_request_timeout_is_ten_minutes() {
        assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(600));
    }

    #[tokio::test]
    async fn a_clone_reaches_the_same_implementer() {
        let provider = Provider::new(FixedProvider(|| {
            Err(ProviderError::ProviderUnrecognized {
                message: "probe".into(),
            })
        }));
        let clone = provider.clone();

        let err = clone.verify("mock").await.unwrap_err();
        assert!(err.to_string().contains("probe"));
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

    /// Generates a throwaway self-signed cert at `cert_path` via the `openssl` CLI.
    /// Returns `false` when `openssl` isn't on `PATH`, so callers can skip
    /// gracefully instead of failing on a machine without it installed.
    fn generate_self_signed_cert(cert_path: &std::path::Path) -> bool {
        let key_path = cert_path.with_extension("key");
        let output = match std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=agentwerk-test",
                "-keyout",
                key_path.to_str().unwrap(),
                "-out",
                cert_path.to_str().unwrap(),
            ])
            .output()
        {
            Ok(output) => output,
            Err(_) => return false,
        };
        std::fs::remove_file(&key_path).ok();
        assert!(
            output.status.success(),
            "openssl failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    #[test]
    fn root_certificates_from_pem_bundle_loads_all_certs_in_file() {
        let path = std::env::temp_dir().join("agentwerk-test-ssl-cert-file.pem");
        if !generate_self_signed_cert(&path) {
            eprintln!("skipping: openssl not found on PATH");
            return;
        }
        let certs = root_certificates_from(Some(path.to_str().unwrap()), None);
        assert_eq!(certs.len(), 1);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn root_certificates_from_dir_skips_files_that_are_not_certificates() {
        let dir = std::env::temp_dir().join("agentwerk-test-ssl-cert-dir");
        std::fs::create_dir_all(&dir).unwrap();
        if !generate_self_signed_cert(&dir.join("cert.pem")) {
            eprintln!("skipping: openssl not found on PATH");
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        std::fs::write(dir.join("not-a-cert.txt"), "not pem data").unwrap();
        let certs = root_certificates_from(None, Some(dir.to_str().unwrap()));
        assert_eq!(certs.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[should_panic(expected = "no certificates loaded")]
    fn root_certificates_from_panics_when_nothing_loads() {
        let dir = std::env::temp_dir().join("agentwerk-test-ssl-cert-dir-empty");
        std::fs::create_dir_all(&dir).unwrap();
        root_certificates_from(None, Some(dir.to_str().unwrap()));
    }

    /// Answers every request with a fixed outcome, so `verify`'s default impl
    /// can be exercised without a live endpoint.
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
    async fn verify_passes_a_bad_key_through_as_authentication_failed() {
        let provider = FixedProvider(|| {
            Err(ProviderError::AuthenticationFailed {
                message: "invalid api key".into(),
            })
        });
        let error = provider.verify("any-model").await.unwrap_err();
        assert!(error.to_string().contains("Authentication failed"));
    }

    #[tokio::test]
    async fn verify_passes_a_wrong_model_through_as_model_not_found() {
        let provider = FixedProvider(|| {
            Err(ProviderError::ModelNotFound {
                message: "no such model".into(),
            })
        });
        let error = provider.verify("does-not-exist").await.unwrap_err();
        assert!(error.to_string().contains("Model not found"));
    }

    #[tokio::test]
    async fn verify_returns_ok_when_the_probe_succeeds() {
        let provider = FixedProvider(|| {
            Ok(ModelResponse {
                content: Vec::new(),
                status: ResponseStatus::EndTurn,
                usage: TokenUsage::default(),
                model: "mock".into(),
            })
        });
        assert!(provider.verify("any-model").await.is_ok());
    }
}
