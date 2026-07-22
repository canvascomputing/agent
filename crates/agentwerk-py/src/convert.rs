//! One JSON conversion path between `serde_json::Value` and Python objects,
//! plus the error mappings the bindings reuse. Tool inputs, task bodies, and
//! results all cross the boundary here so there is a single place to keep the
//! encoding honest.

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use serde_json::Value;

/// Render a `serde_json::Value` as a native Python object (`dict`/`list`/`str`/...).
pub fn value_to_py<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    pythonize::pythonize(py, value).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Read a native Python object back into a `serde_json::Value`.
pub fn py_to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    pythonize::depythonize(obj).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Map a crate-side error (provider, tool, IO) to a Python exception. The crate
/// never hands us a `From` blanket, so the mapping stays explicit here too.
pub fn runtime_error(message: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(message.to_string())
}
