//! The cross-ticket knowledge store as Python sees it. Build one store, cap its
//! rendered index, and bind it to one or more agents so they share durable
//! facts across every ticket and across runs.

use std::sync::Arc;

use agentwerk::Knowledge;
use pyo3::prelude::*;

use crate::convert::runtime_error;

/// A shared knowledge store rooted at a directory.
#[pyclass(name = "Knowledge")]
pub struct PyKnowledge {
    pub inner: Arc<Knowledge>,
}

#[pymethods]
impl PyKnowledge {
    /// Open (or seed from) an Open Knowledge Format bundle at `dir/knowledge`.
    #[staticmethod]
    fn load(dir: &str) -> PyResult<Self> {
        let inner = Knowledge::load(dir).map_err(runtime_error)?;
        Ok(PyKnowledge { inner })
    }

    /// Cap the rendered index injected into the system prompt, in characters.
    fn index_char_limit<'py>(slf: PyRef<'py, Self>, n: usize) -> PyRef<'py, Self> {
        Arc::clone(&slf.inner).index_char_limit(n);
        slf
    }

    /// The current rendered index (the bullet list), or an empty string.
    fn index(&self) -> String {
        self.inner.index()
    }

    /// Remove every page from the store.
    fn clear(&self) -> PyResult<()> {
        self.inner.clear().map_err(runtime_error)
    }
}
