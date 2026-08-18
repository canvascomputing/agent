//! The ticket as Python sees it. One class in both directions: you set the
//! fields you own, and the same class comes back with its status and messages.
//!
//! Rust sets those fields with chained methods. A Python class cannot carry a
//! `label` method and a `label` attribute, so they are keyword arguments here.

use agentwerk::{Query, Status, Ticket};
use pyo3::prelude::*;

use crate::convert::{py_to_value, value_to_py};
use crate::schema::PySchema;

/// Selects tickets by field values.
#[pyclass(name = "Query")]
pub struct PyQuery {
    pub inner: Query,
}

#[pymethods]
impl PyQuery {
    #[new]
    #[pyo3(signature = (*, key=None, label=None, status=None, agent=None, parent_key=None))]
    fn new(
        key: Option<String>,
        label: Option<String>,
        status: Option<&str>,
        agent: Option<String>,
        parent_key: Option<String>,
    ) -> PyResult<Self> {
        let mut q = Query::new();
        if let Some(k) = key {
            q = q.key(k);
        }
        if let Some(l) = label {
            q = q.label(l);
        }
        if let Some(s) = status {
            q = q.status(parse_status(s)?);
        }
        if let Some(a) = agent {
            q = q.agent(a);
        }
        if let Some(p) = parent_key {
            q = q.parent_key(p);
        }
        Ok(PyQuery { inner: q })
    }

    fn __repr__(&self) -> String {
        format!("{:?}", self.inner)
    }
}

fn parse_status(s: &str) -> PyResult<Status> {
    match s {
        "todo" => Ok(Status::Todo),
        "in_progress" => Ok(Status::InProgress),
        "finished" => Ok(Status::Finished),
        "failed" => Ok(Status::Failed),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown status: {other}"
        ))),
    }
}

/// Read a Python argument as a query: a `Query`, or a string naming a label.
/// Falls back to `None` so the caller can try the callable path.
pub fn try_extract_query(py: Python<'_>, arg: &Py<PyAny>) -> Option<Query> {
    if let Ok(query) = arg.extract::<PyRef<'_, PyQuery>>(py) {
        return Some(query.inner.clone());
    }
    arg.extract::<String>(py).ok().map(Query::from)
}

/// Read a Python argument that must be a query, raising where a callable has
/// nothing to fall back to.
pub fn as_query(py: Python<'_>, arg: &Py<PyAny>) -> PyResult<Query> {
    try_extract_query(py, arg)
        .ok_or_else(|| pyo3::exceptions::PyTypeError::new_err("expected a Query or a label string"))
}

/// Read a Python argument as a ticket: a `Ticket`, or any value as the task.
pub fn to_ticket(arg: &Bound<'_, PyAny>) -> PyResult<Ticket> {
    match arg.extract::<PyRef<'_, PyTicket>>() {
        Ok(ticket) => Ok(ticket.to_ticket()),
        Err(_) => Ok(Ticket::new(py_to_value(arg)?)),
    }
}

/// A `Ticket` is a task plus what assigns and validates it.
#[pyclass(name = "Ticket")]
pub struct PyTicket {
    pub inner: Ticket,
}

#[pymethods]
impl PyTicket {
    /// Create a ticket carrying `task`.
    ///
    /// `label` assigns it to agents, `schema` is what the result must satisfy,
    /// and `parent` names the ticket it came from.
    #[new]
    #[pyo3(signature = (task, *, label=None, schema=None, parent=None))]
    fn new(
        task: &Bound<'_, PyAny>,
        label: Option<String>,
        schema: Option<PyRef<'_, PySchema>>,
        parent: Option<String>,
    ) -> PyResult<Self> {
        let mut inner = Ticket::new(py_to_value(task)?);
        if let Some(label) = label {
            inner = inner.label(label);
        }
        if let Some(schema) = schema {
            inner = inner.schema(schema.inner.clone());
        }
        if let Some(parent) = parent {
            inner = inner.parent(parent);
        }
        Ok(PyTicket { inner })
    }

    /// Check whether the ticket carries a label.
    fn has_label(&self, label: &str) -> bool {
        self.inner.has_label(label)
    }

    /// Check whether the ticket is waiting to be claimed.
    fn is_todo(&self) -> bool {
        self.inner.is_todo()
    }

    /// Check whether the ticket finished.
    fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }

    /// Check whether the ticket failed.
    fn is_failed(&self) -> bool {
        self.inner.is_failed()
    }

    /// Check whether an agent is working on the ticket.
    fn is_in_progress(&self) -> bool {
        self.inner.is_in_progress()
    }

    /// Check whether the ticket is still todo or in progress.
    fn is_pending(&self) -> bool {
        self.inner.is_pending()
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

    /// Name of the agent that created the ticket.
    #[getter]
    fn reporter(&self) -> &str {
        &self.inner.reporter
    }

    /// Name of the agent that claimed the ticket.
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

    fn __repr__(&self) -> String {
        format!(
            "Ticket(key={:?}, status={:?})",
            self.inner.key,
            self.status()
        )
    }
}

impl PyTicket {
    /// Hand over a ticket the queue owns, messages included.
    pub fn from_ticket(ticket: &Ticket) -> Self {
        PyTicket {
            inner: ticket.clone(),
        }
    }

    /// Build the ticket to submit, copying only the fields you own.
    ///
    /// Submitting sets key, status, reporter, and result, but leaves the
    /// messages and timestamps, so a ticket that came back out of the queue
    /// would otherwise carry its messages into the new one.
    pub fn to_ticket(&self) -> Ticket {
        let mut ticket = Ticket::new(self.inner.task.clone());
        if let Some(label) = &self.inner.label {
            ticket = ticket.label(label.clone());
        }
        if let Some(schema) = &self.inner.schema {
            ticket = ticket.schema(schema.clone());
        }
        if let Some(parent) = &self.inner.parent {
            ticket = ticket.parent(parent.clone());
        }
        ticket
    }
}
