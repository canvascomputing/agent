//! JSON-Schema handles for Python. A `Schema` validates a ticket's result; it
//! is built from a Python dict (or JSON string) and attached to a ticket or a
//! label default.

use agentwerk::Schema;
use pyo3::prelude::*;

use crate::convert::{py_to_value, runtime_error, value_to_py};

/// A parsed result schema. Cheap to clone, so a ticket or a label default can
/// hold the same compiled schema without a second parse.
#[pyclass(name = "Schema")]
pub struct PySchema {
    pub inner: Schema,
}

#[pymethods]
impl PySchema {
    /// Parse a schema from a Python dict (or any JSON-like value).
    #[new]
    fn new(document: &Bound<'_, PyAny>) -> PyResult<Self> {
        let inner = Schema::parse(py_to_value(document)?).map_err(runtime_error)?;
        Ok(PySchema { inner })
    }

    /// Validate `value` and return the value to keep: a value the agent
    /// double-encoded as a JSON string comes back decoded. Raises on a
    /// violation.
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
