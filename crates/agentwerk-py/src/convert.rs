//! The one place JSON crosses between Rust and Python, and the errors that
//! crossing can raise. Tool inputs, task bodies, and results all pass here.

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use serde_json::Value;

/// Hand a `serde_json::Value` to Python as a `dict`, `list`, or scalar.
pub fn value_to_py<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    pythonize::pythonize(py, value).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Read a Python object back into a `serde_json::Value`.
pub fn py_to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    pythonize::depythonize(obj).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Turn a crate error into a Python exception. The crate maps every conversion
/// explicitly, and so does this.
pub fn runtime_error(message: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(message.to_string())
}
