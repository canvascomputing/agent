//! Exposes tasks for both creation and inspection through Python.
//!
//! Rust sets those fields with chained methods. A Python class cannot carry a
//! `label` method and a `label` attribute, so they are keyword arguments here.

use agentwerk::Task;
use pyo3::prelude::*;

use crate::convert::{py_to_value, value_to_py};
use crate::schema::PySchema;

/// Read a Python argument as a task or any JSON-compatible task value.
pub fn to_task(arg: &Bound<'_, PyAny>) -> PyResult<Task> {
    if let Ok(task) = arg.extract::<PyRef<'_, PyTask>>() {
        return Ok(task.to_task());
    }
    Ok(Task::new(py_to_value(arg)?))
}

/// A `Task` is a task plus what assigns and validates it.
#[pyclass(name = "Task")]
pub struct PyTask {
    pub inner: Task,
}

#[pymethods]
impl PyTask {
    /// Create a task carrying `task`.
    ///
    /// `label` assigns it to agents and `schema` is what the result must satisfy.
    #[new]
    #[pyo3(signature = (task, *, label=None, schema=None))]
    fn new(
        task: &Bound<'_, PyAny>,
        label: Option<String>,
        schema: Option<PyRef<'_, PySchema>>,
    ) -> PyResult<Self> {
        let mut inner = Task::new(py_to_value(task)?);
        if let Some(label) = label {
            inner = inner.label(label);
        }
        if let Some(schema) = schema {
            inner = inner.schema(schema.inner.clone());
        }
        Ok(PyTask { inner })
    }

    /// Check whether the task is waiting to be claimed.
    fn is_todo(&self) -> bool {
        self.inner.is_todo()
    }

    /// Check whether the task finished.
    fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }

    /// Check whether the task failed.
    fn is_failed(&self) -> bool {
        self.inner.is_failed()
    }

    /// Check whether an agent is working on the task.
    fn is_in_progress(&self) -> bool {
        self.inner.is_in_progress()
    }

    /// Check whether the task still has work for an agent in this run.
    fn is_pending(&self) -> bool {
        self.inner.is_pending()
    }

    /// Check whether this run has excluded the task from scheduling.
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn get_id(&self) -> &str {
        self.inner.get_id()
    }

    /// `"todo"`, `"in_progress"`, `"finished"`, or `"failed"`.
    fn get_status(&self) -> String {
        self.inner.get_status().to_string()
    }

    fn get_task<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, self.inner.get_task())
    }

    fn get_result<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.inner.get_result() {
            Some(result) => Ok(Some(value_to_py(py, result)?)),
            None => Ok(None),
        }
    }

    fn get_label(&self) -> Option<&str> {
        self.inner.get_label()
    }

    /// Optional schema the result must satisfy.
    fn get_schema(&self) -> Option<PySchema> {
        self.inner.get_schema().map(|schema| PySchema {
            inner: schema.clone(),
        })
    }

    /// Name of the agent that created the task.
    fn get_reporter(&self) -> &str {
        self.inner.get_reporter()
    }

    /// Name of the agent that claimed the task.
    fn get_assignee(&self) -> Option<&str> {
        self.inner.get_assignee()
    }

    fn get_created_at(&self) -> u64 {
        self.inner.get_created_at()
    }

    fn get_started_at(&self) -> Option<u64> {
        self.inner.get_started_at()
    }

    fn get_finished_at(&self) -> Option<u64> {
        self.inner.get_finished_at()
    }

    fn get_failed_at(&self) -> Option<u64> {
        self.inner.get_failed_at()
    }

    /// The messages exchanged with the model, built on access so a handler that
    /// never asks never pays for them.
    fn get_replies(&self) -> Vec<crate::reply::PyReply> {
        crate::reply::replies_to_py(self.inner.get_replies())
    }

    /// The failures recorded against the task, as events, in the order they
    /// happened. A failed tool call or request does not fail the task, so a
    /// finished task can carry some.
    fn get_errors(&self) -> Vec<crate::event::PyEvent> {
        self.inner
            .get_errors()
            .iter()
            .map(crate::event::to_py_event)
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "Task(id={:?}, status={:?})",
            self.inner.get_id(),
            self.get_status()
        )
    }
}

impl PyTask {
    /// Copy a task the Werk owns, messages included.
    pub fn from_task(task: &Task) -> Self {
        PyTask {
            inner: task.clone(),
        }
    }

    /// Preserve task input; `add_task` clears execution-owned fields in Rust.
    pub fn to_task(&self) -> Task {
        self.inner.clone()
    }
}
