//! Provider and model handles for Python. Environment detection covers the
//! common case; explicit per-vendor constructors and a `Model` tuning object
//! cover local endpoints and non-default context windows or reasoning effort.

use std::sync::Arc;

use agentwerk::providers::{
    provider_from_env as detect_provider, AnthropicProvider, LiteLlmProvider, MistralProvider,
    Model, OpenAiProvider, Provider, ReasoningEffort,
};
use pyo3::prelude::*;

use crate::convert::runtime_error;

/// An LLM provider the agent builder can be pointed at with `.provider(...)`.
#[pyclass(name = "Provider")]
pub struct PyProvider {
    pub inner: Arc<dyn Provider>,
}

/// A model name plus optional context-window and reasoning-effort overrides,
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

    /// Override the model's context window, in tokens.
    fn context_window(mut slf: PyRefMut<'_, Self>, size: u64) -> PyRefMut<'_, Self> {
        slf.inner = slf.inner.clone().context_window(size);
        slf
    }

    /// Set reasoning effort: `"off"`, `"low"`, `"medium"`, or `"high"`.
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
}

/// Detect and construct a provider from environment variables
/// (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `MISTRAL_API_KEY`, `LITELLM_API_KEY`).
#[pyfunction]
#[pyo3(name = "provider_from_env")]
fn provider_from_env() -> PyResult<PyProvider> {
    let inner = detect_provider().map_err(runtime_error)?;
    Ok(PyProvider { inner })
}

#[pyfunction]
#[pyo3(name = "AnthropicProvider", signature = (api_key, base_url=None))]
fn anthropic_provider(api_key: &str, base_url: Option<&str>) -> PyProvider {
    let mut provider = AnthropicProvider::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    PyProvider {
        inner: Arc::new(provider),
    }
}

#[pyfunction]
#[pyo3(name = "OpenAiProvider", signature = (api_key, base_url=None))]
fn openai_provider(api_key: &str, base_url: Option<&str>) -> PyProvider {
    let mut provider = OpenAiProvider::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    PyProvider {
        inner: Arc::new(provider),
    }
}

#[pyfunction]
#[pyo3(name = "MistralProvider", signature = (api_key, base_url=None))]
fn mistral_provider(api_key: &str, base_url: Option<&str>) -> PyProvider {
    let mut provider = MistralProvider::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    PyProvider {
        inner: Arc::new(provider),
    }
}

#[pyfunction]
#[pyo3(name = "LiteLlmProvider", signature = (api_key, base_url=None))]
fn litellm_provider(api_key: &str, base_url: Option<&str>) -> PyProvider {
    let mut provider = LiteLlmProvider::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    PyProvider {
        inner: Arc::new(provider),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyProvider>()?;
    m.add_class::<PyModel>()?;
    m.add_function(wrap_pyfunction!(provider_from_env, m)?)?;
    m.add_function(wrap_pyfunction!(anthropic_provider, m)?)?;
    m.add_function(wrap_pyfunction!(openai_provider, m)?)?;
    m.add_function(wrap_pyfunction!(mistral_provider, m)?)?;
    m.add_function(wrap_pyfunction!(litellm_provider, m)?)?;
    Ok(())
}
