//! The one place JSON crosses between Rust and Python, and the errors that
//! crossing can raise. Tool inputs, task bodies, and results all pass here, and
//! so does the text a prompt is set from.

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use serde_json::Value;

use agentwerk::Text;

/// Hand a `serde_json::Value` to Python as a `dict`, `list`, or scalar.
pub fn value_to_py<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    pythonize::pythonize(py, value).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Read a Python object back into a `serde_json::Value`.
pub fn py_to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    pythonize::depythonize(obj).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Read a Python object as prompt text: a `str` is the text itself, an
/// `os.PathLike` names the file holding it.
///
/// `str` is tried first because `PathBuf` extraction accepts one too, which
/// would turn every prompt into a filename.
pub fn py_to_text(obj: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(text) = obj.extract::<String>() {
        return Ok(Text::from(text).into_string());
    }
    let file = obj.extract::<std::path::PathBuf>()?;
    Ok(Text::from_file(file).map_err(runtime_error)?.into_string())
}

/// Turn a crate error into a Python exception. The crate maps every conversion
/// explicitly, and so does this.
pub fn runtime_error(message: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(message.to_string())
}
