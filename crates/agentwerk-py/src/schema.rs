//! Exposes result schemas built from Python JSON-like values.

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
    /// Create a schema from a dict declaring top-level `type: object`.
    #[new]
    fn new(document: &Bound<'_, PyAny>) -> PyResult<Self> {
        let inner = Schema::new(py_to_value(document)?).map_err(runtime_error)?;
        Ok(PySchema { inner })
    }

    /// Validate content and give back the value to keep, plus the JSON pointer
    /// of every nested value it repaired. Quoted JSON values come back retyped;
    /// string enums may be corrected for case or outer whitespace, but never
    /// converted to another JSON type. Raises on a violation.
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
