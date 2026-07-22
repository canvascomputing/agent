//! JSON-Schema handles for Python. A `Schema` validates a ticket's result; it
//! is built from a Python dict (or JSON string) and attached to a ticket or a
//! label default.

use agentwerk::Schema;
use pyo3::prelude::*;
use serde_json::Value;

use crate::convert::{py_to_value, runtime_error};

/// A parsed result schema. Keeps its source document so a ticket builder can
/// re-derive an owned `Schema` without a second parse path.
#[pyclass(name = "Schema")]
pub struct PySchema {
    pub inner: Schema,
    pub source: Value,
}

#[pymethods]
impl PySchema {
    /// Parse a schema from a Python dict (or any JSON-like value).
    #[new]
    fn new(document: &Bound<'_, PyAny>) -> PyResult<Self> {
        let source = py_to_value(document)?;
        let inner = Schema::parse(source.clone()).map_err(runtime_error)?;
        Ok(PySchema { inner, source })
    }
}
