//! The schema as Python sees it: built from a dict or a JSON string, then
//! attached to a ticket or registered for a label.

use agentwerk::Schema;
use pyo3::prelude::*;

use crate::convert::{py_to_value, runtime_error, value_to_py};

/// A `Schema` constrains the result an agent produces for a ticket. Copying it
/// is cheap, so a ticket and a label can share one.
#[pyclass(name = "Schema")]
pub struct PySchema {
    pub inner: Schema,
}

#[pymethods]
impl PySchema {
    /// Create a schema from a dict, or from any JSON-like value.
    #[new]
    fn new(document: &Bound<'_, PyAny>) -> PyResult<Self> {
        let inner = Schema::parse(py_to_value(document)?).map_err(runtime_error)?;
        Ok(PySchema { inner })
    }

    /// Validate content and give back the value to keep. A value the agent
    /// wrote as a JSON string comes back decoded. Raises on a violation.
    fn validate<'py>(
        &self,
        py: Python<'py>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let kept = self
            .inner
            .validate(py_to_value(value)?)
            .map_err(runtime_error)?;
        value_to_py(py, &kept)
    }
}
