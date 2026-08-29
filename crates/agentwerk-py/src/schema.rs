//! The schema as Python sees it: built from a dict or a JSON string, then
//! attached to a task or bound to a label through a `SchemaStore`.

use std::sync::Arc;

use agentwerk::{Schema, SchemaStore};
use pyo3::prelude::*;

use crate::convert::{py_to_value, runtime_error, value_to_py};

/// A `Schema` constrains the result an agent produces for a task. Copying it
/// is cheap, so a task and a label can share one.
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

/// `SchemaStore` holds one schema per label and hands it to every task claimed
/// under that label that carries no schema of its own. Give it to a task
/// queue with `queue.set_schemas(store)`.
#[pyclass(name = "SchemaStore")]
pub struct PySchemaStore {
    pub inner: Arc<SchemaStore>,
}

#[pymethods]
impl PySchemaStore {
    /// Create an empty store.
    #[new]
    fn new() -> Self {
        PySchemaStore {
            inner: SchemaStore::new(),
        }
    }

    /// Bind a schema to a label, creating or replacing the entry. Raises on a
    /// document that is not a schema.
    fn label<'py>(
        slf: PyRef<'py, Self>,
        label: &str,
        document: &Bound<'_, PyAny>,
    ) -> PyResult<PyRef<'py, Self>> {
        slf.inner
            .label(label, py_to_value(document)?)
            .map_err(runtime_error)?;
        Ok(slf)
    }

    /// Read back the schema bound to a label, or `None` when there is none.
    fn get(&self, label: &str) -> Option<PySchema> {
        self.inner.get(label).map(|inner| PySchema { inner })
    }
}
