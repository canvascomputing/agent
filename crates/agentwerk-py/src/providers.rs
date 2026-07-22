//! Provider handles for Python. M1 covers the two paths the happy case needs:
//! environment detection, and passing an explicit provider to the agent
//! builder. Concrete per-vendor constructors and `Model` tuning arrive later.

use std::sync::Arc;

use agentwerk::providers::{provider_from_env as detect_provider, Provider};
use pyo3::prelude::*;

use crate::convert::runtime_error;

/// An LLM provider the agent builder can be pointed at with `.provider(...)`.
#[pyclass(name = "Provider")]
pub struct PyProvider {
    pub inner: Arc<dyn Provider>,
}

/// Detect and construct a provider from environment variables
/// (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `MISTRAL_API_KEY`, `LITELLM_API_KEY`).
#[pyfunction]
#[pyo3(name = "provider_from_env")]
fn provider_from_env() -> PyResult<PyProvider> {
    let inner = detect_provider().map_err(runtime_error)?;
    Ok(PyProvider { inner })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyProvider>()?;
    m.add_function(wrap_pyfunction!(provider_from_env, m)?)?;
    Ok(())
}
