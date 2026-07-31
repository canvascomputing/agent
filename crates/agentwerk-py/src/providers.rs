//! The LLM providers and models as Python sees them. Reading the environment
//! covers the common case; the per-vendor constructors and `Model` cover a local
//! endpoint, a different context window, or a different reasoning level.

use std::sync::Arc;
use std::time::Duration;

use agentwerk::providers::{
    context_window_from_env as detect_context_window, model_from_env as detect_model,
    provider_from_env as detect_provider, AnthropicProvider, LiteLlmProvider, MistralProvider,
    Model, OpenAiProvider, Provider, ReasoningEffort,
};
use pyo3::prelude::*;

use crate::convert::runtime_error;

/// An LLM provider, passed to `.provider(...)`.
#[pyclass(name = "Provider")]
pub struct PyProvider {
    pub inner: Arc<dyn Provider>,
}

/// A model name, with an optional context window size and reasoning level,
/// passed to `.model(...)`.
#[pyclass(name = "Model")]
pub struct PyModel {
    pub inner: Model,
}

#[pymethods]
impl PyModel {
    #[new]
    fn new(name: &str) -> Self {
        PyModel {
            inner: Model::from_name(name),
        }
    }

    /// Set the context window size for a model, in tokens.
    fn context_window(mut slf: PyRefMut<'_, Self>, size: u64) -> PyRefMut<'_, Self> {
        slf.inner = slf.inner.clone().context_window(size);
        slf
    }

    /// Set the reasoning level: `"off"`, `"low"`, `"medium"`, or `"high"`.
    fn reasoning_effort<'py>(
        mut slf: PyRefMut<'py, Self>,
        effort: &str,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let parsed = match effort.to_ascii_lowercase().as_str() {
            "off" => ReasoningEffort::Off,
            "low" => ReasoningEffort::Low,
            "medium" => ReasoningEffort::Medium,
            "high" => ReasoningEffort::High,
            other => {
                return Err(runtime_error(format!(
                    "unknown reasoning effort {other:?}: use off, low, medium, or high"
                )))
            }
        };
        slf.inner = slf.inner.clone().reasoning_effort(parsed);
        Ok(slf)
    }

    /// Get the configured window size, in tokens.
    fn get_context_window(&self) -> Option<u64> {
        self.inner.get_context_window()
    }

    /// Get the configured effort.
    fn get_reasoning_effort(&self) -> String {
        self.inner.get_reasoning_effort().to_string()
    }
}

/// Read the LLM provider from `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`,
/// `MISTRAL_API_KEY`, or `LITELLM_API_KEY`.
#[pyfunction]
#[pyo3(name = "provider_from_env")]
fn provider_from_env() -> PyResult<PyProvider> {
    let inner = detect_provider().map_err(runtime_error)?;
    Ok(PyProvider { inner })
}

/// Read the model name from `MODEL`, from `<PROVIDER>_MODEL`, or from the
/// detected provider's own default.
#[pyfunction]
#[pyo3(name = "model_from_env")]
fn model_from_env() -> PyResult<String> {
    detect_model().map_err(runtime_error)
}

/// Read the context window size from `MODEL_CONTEXT_WINDOW`, or `None` when it
/// is unset or not a positive integer.
#[pyfunction]
#[pyo3(name = "context_window_from_env")]
fn context_window_from_env() -> Option<u64> {
    detect_context_window()
}

#[pyfunction]
#[pyo3(name = "AnthropicProvider", signature = (api_key, base_url=None, timeout=None))]
fn anthropic_provider(api_key: &str, base_url: Option<&str>, timeout: Option<f64>) -> PyProvider {
    let mut provider = AnthropicProvider::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    if let Some(seconds) = timeout {
        provider = provider.timeout(Duration::from_secs_f64(seconds));
    }
    PyProvider {
        inner: Arc::new(provider),
    }
}

#[pyfunction]
#[pyo3(name = "OpenAiProvider", signature = (api_key, base_url=None, timeout=None))]
fn openai_provider(api_key: &str, base_url: Option<&str>, timeout: Option<f64>) -> PyProvider {
    let mut provider = OpenAiProvider::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    if let Some(seconds) = timeout {
        provider = provider.timeout(Duration::from_secs_f64(seconds));
    }
    PyProvider {
        inner: Arc::new(provider),
    }
}

#[pyfunction]
#[pyo3(name = "MistralProvider", signature = (api_key, base_url=None, timeout=None))]
fn mistral_provider(api_key: &str, base_url: Option<&str>, timeout: Option<f64>) -> PyProvider {
    let mut provider = MistralProvider::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    if let Some(seconds) = timeout {
        provider = provider.timeout(Duration::from_secs_f64(seconds));
    }
    PyProvider {
        inner: Arc::new(provider),
    }
}

#[pyfunction]
#[pyo3(name = "LiteLlmProvider", signature = (api_key, base_url=None, timeout=None))]
fn litellm_provider(api_key: &str, base_url: Option<&str>, timeout: Option<f64>) -> PyProvider {
    let mut provider = LiteLlmProvider::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    if let Some(seconds) = timeout {
        provider = provider.timeout(Duration::from_secs_f64(seconds));
    }
    PyProvider {
        inner: Arc::new(provider),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyProvider>()?;
    m.add_class::<PyModel>()?;
    m.add_function(wrap_pyfunction!(provider_from_env, m)?)?;
    m.add_function(wrap_pyfunction!(model_from_env, m)?)?;
    m.add_function(wrap_pyfunction!(context_window_from_env, m)?)?;
    m.add_function(wrap_pyfunction!(anthropic_provider, m)?)?;
    m.add_function(wrap_pyfunction!(openai_provider, m)?)?;
    m.add_function(wrap_pyfunction!(mistral_provider, m)?)?;
    m.add_function(wrap_pyfunction!(litellm_provider, m)?)?;
    Ok(())
}
