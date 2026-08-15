//! The LLM providers and models as Python sees them. Reading the environment
//! covers the common case; the per-vendor constructors and `Model` cover a local
//! endpoint, a different context window, or a different reasoning level.

use std::time::Duration;

use agentwerk::providers::{Anthropic, LiteLlm, Mistral, Model, OpenAi, Provider, ReasoningEffort};
use pyo3::prelude::*;

use crate::convert::runtime_error;

/// An LLM provider, passed to `.provider(...)`.
#[pyclass(name = "Provider")]
pub struct PyProvider {
    pub inner: Provider,
}

#[pymethods]
impl PyProvider {
    /// Detect the LLM provider from environment variables.
    #[staticmethod]
    fn from_env() -> PyResult<Self> {
        let inner = Provider::from_env().map_err(runtime_error)?;
        Ok(PyProvider { inner })
    }

    /// Fill the environment from a `.env` file in the current directory, then
    /// detect the LLM provider. An exported value wins over the file.
    #[staticmethod]
    fn from_dot_env() -> PyResult<Self> {
        let inner = Provider::from_dot_env().map_err(runtime_error)?;
        Ok(PyProvider { inner })
    }
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

    /// Read the model name from the environment, plus the context window when
    /// `MODEL_CONTEXT_WINDOW` is set.
    #[staticmethod]
    fn from_env() -> PyResult<Self> {
        let inner = Model::from_env().map_err(runtime_error)?;
        Ok(PyModel { inner })
    }

    /// Fill the environment from a `.env` file in the current directory, then
    /// read the model name from it. An exported value wins over the file.
    #[staticmethod]
    fn from_dot_env() -> PyResult<Self> {
        let inner = Model::from_dot_env().map_err(runtime_error)?;
        Ok(PyModel { inner })
    }

    /// The resolved model name.
    #[getter]
    fn name(&self) -> String {
        self.inner.name.clone()
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

#[pyfunction]
#[pyo3(name = "Anthropic", signature = (api_key, base_url=None, timeout=None))]
fn anthropic_provider(api_key: &str, base_url: Option<&str>, timeout: Option<f64>) -> PyProvider {
    let mut provider = Anthropic::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    if let Some(seconds) = timeout {
        provider = provider.timeout(Duration::from_secs_f64(seconds));
    }
    PyProvider {
        inner: Provider::new(provider),
    }
}

#[pyfunction]
#[pyo3(name = "OpenAi", signature = (api_key, base_url=None, timeout=None))]
fn openai_provider(api_key: &str, base_url: Option<&str>, timeout: Option<f64>) -> PyProvider {
    let mut provider = OpenAi::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    if let Some(seconds) = timeout {
        provider = provider.timeout(Duration::from_secs_f64(seconds));
    }
    PyProvider {
        inner: Provider::new(provider),
    }
}

#[pyfunction]
#[pyo3(name = "Mistral", signature = (api_key, base_url=None, timeout=None))]
fn mistral_provider(api_key: &str, base_url: Option<&str>, timeout: Option<f64>) -> PyProvider {
    let mut provider = Mistral::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    if let Some(seconds) = timeout {
        provider = provider.timeout(Duration::from_secs_f64(seconds));
    }
    PyProvider {
        inner: Provider::new(provider),
    }
}

#[pyfunction]
#[pyo3(name = "LiteLlm", signature = (api_key, base_url=None, timeout=None))]
fn litellm_provider(api_key: &str, base_url: Option<&str>, timeout: Option<f64>) -> PyProvider {
    let mut provider = LiteLlm::new(api_key);
    if let Some(url) = base_url {
        provider = provider.base_url(url);
    }
    if let Some(seconds) = timeout {
        provider = provider.timeout(Duration::from_secs_f64(seconds));
    }
    PyProvider {
        inner: Provider::new(provider),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyProvider>()?;
    m.add_class::<PyModel>()?;
    m.add_function(wrap_pyfunction!(anthropic_provider, m)?)?;
    m.add_function(wrap_pyfunction!(openai_provider, m)?)?;
    m.add_function(wrap_pyfunction!(mistral_provider, m)?)?;
    m.add_function(wrap_pyfunction!(litellm_provider, m)?)?;
    Ok(())
}
