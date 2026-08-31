//! The schema as Python sees it: built from a dict or another JSON-like value,
//! then attached to a task.

use agentwerk::Schema;
use pyo3::prelude::*;

use crate::convert::{py_to_value, runtime_error, value_to_py};

/// A `Schema` constrains the result an agent produces for a task. Copying it
/// is cheap, so tasks can share one.
#[pyclass(name = "Schema")]
pub struct PySchema {
    pub inner: Schema,
}

#[pymethods]
impl PySchema {
    /// Create a schema from a dict, or from any JSON-like value.
    #[new]
    fn new(document: &Bound<'_, PyAny>) -> PyResult<Self> {
        let inner = Schema::new(py_to_value(document)?).map_err(runtime_error)?;
        Ok(PySchema { inner })
    }

    /// Validate content and give back the value to keep, plus the JSON pointer
    /// of every value it repaired. A value the agent quoted or wrote as JSON
    /// text comes back retyped. Raises on a violation.
    fn validate<'py>(
        &self,
        py: Python<'py>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<(Bound<'py, PyAny>, Vec<String>)> {
        let (kept, repaired) = self
            .inner
            .validate(py_to_value(value)?)
            .map_err(runtime_error)?;
        Ok((value_to_py(py, &kept)?, repaired))
    }
}
