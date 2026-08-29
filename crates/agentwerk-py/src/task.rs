//! The task as Python sees it. One class in both directions: you set the
//! fields you own, and the same class comes back with its status and messages.
//!
//! Rust sets those fields with chained methods. A Python class cannot carry a
//! `label` method and a `label` attribute, so they are keyword arguments here.

use agentwerk::Task;
use pyo3::prelude::*;

use crate::convert::{py_to_text, py_to_value, value_to_py};
use crate::schema::PySchema;

/// Read a Python argument as a task: a `Task`, an `os.PathLike` naming the
/// file holding the task, or any value as the task itself.
///
/// A `str` stays the task, since a task naming a file is still a task.
pub fn to_task(arg: &Bound<'_, PyAny>) -> PyResult<Task> {
    if let Ok(task) = arg.extract::<PyRef<'_, PyTask>>() {
        return Ok(task.to_task());
    }
    if arg.hasattr("__fspath__")? {
        return Ok(Task::new(py_to_text(arg)?));
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
    /// `label` assigns it to agents, `schema` is what the result must satisfy,
    /// and `parent` names the task it came from.
    #[new]
    #[pyo3(signature = (task, *, label=None, schema=None, parent=None))]
    fn new(
        task: &Bound<'_, PyAny>,
        label: Option<String>,
        schema: Option<PyRef<'_, PySchema>>,
        parent: Option<String>,
    ) -> PyResult<Self> {
        let mut inner = Task::new(py_to_value(task)?);
        if let Some(label) = label {
            inner = inner.label(label);
        }
        if let Some(schema) = schema {
            inner = inner.schema(schema.inner.clone());
        }
        if let Some(parent) = parent {
            inner = inner.parent(parent);
        }
        Ok(PyTask { inner })
    }

    /// Check whether the task carries `label`.
    fn has_label(&self, label: &str) -> bool {
        self.inner.has_label(label)
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

    /// Check whether this run has taken the task off the queue.
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    #[getter]
    fn key(&self) -> &str {
        &self.inner.key
    }

    /// `"todo"`, `"in_progress"`, `"finished"`, or `"failed"`.
    #[getter]
    fn status(&self) -> String {
        self.inner.status.to_string()
    }

    #[getter]
    fn task<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.inner.task)
    }

    #[getter]
    fn result<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.inner.result {
            Some(result) => Ok(Some(value_to_py(py, result)?)),
            None => Ok(None),
        }
    }

    #[getter]
    fn label(&self) -> Option<String> {
        self.inner.label.clone()
    }

    /// Optional schema the result must satisfy.
    #[getter]
    fn schema(&self) -> Option<PySchema> {
        self.inner.schema.as_ref().map(|schema| PySchema {
            inner: schema.clone(),
        })
    }

    #[getter]
    fn parent(&self) -> Option<String> {
        self.inner.parent.clone()
    }

    /// Name of the agent that created the task.
    #[getter]
    fn reporter(&self) -> &str {
        &self.inner.reporter
    }

    /// Name of the agent that claimed the task.
    #[getter]
    fn assignee(&self) -> Option<String> {
        self.inner.assignee.clone()
    }

    #[getter]
    fn created_at(&self) -> u64 {
        self.inner.created_at
    }

    #[getter]
    fn started_at(&self) -> Option<u64> {
        self.inner.started_at
    }

    #[getter]
    fn finished_at(&self) -> Option<u64> {
        self.inner.finished_at
    }

    #[getter]
    fn failed_at(&self) -> Option<u64> {
        self.inner.failed_at
    }

    /// The messages exchanged with the model, built on access so a handler that
    /// never asks never pays for them.
    #[getter]
    fn replies(&self) -> Vec<crate::reply::PyReply> {
        crate::reply::replies_to_py(&self.inner.replies)
    }

    /// The failures recorded against the task, as events, in the order they
    /// happened. A failed tool call or request does not fail the task, so a
    /// finished task can carry some.
    #[getter]
    fn errors(&self) -> Vec<crate::event::PyEvent> {
        self.inner
            .errors
            .iter()
            .map(crate::event::to_py_event)
            .collect()
    }

    fn __repr__(&self) -> String {
        format!("Task(key={:?}, status={:?})", self.inner.key, self.status())
    }
}

impl PyTask {
    /// Hand over a task the queue owns, messages included.
    pub fn from_task(task: &Task) -> Self {
        PyTask {
            inner: task.clone(),
        }
    }

    /// Build the task to submit, copying only the fields you own.
    ///
    /// Submitting sets key, status, reporter, and result, but leaves the
    /// messages and timestamps, so a task that came back out of the queue
    /// would otherwise carry its messages into the new one.
    pub fn to_task(&self) -> Task {
        let mut task = Task::new(self.inner.task.clone());
        if let Some(label) = &self.inner.label {
            task = task.label(label.clone());
        }
        if let Some(schema) = &self.inner.schema {
            task = task.schema(schema.clone());
        }
        if let Some(parent) = &self.inner.parent {
            task = task.parent(parent.clone());
        }
        task
    }
}
