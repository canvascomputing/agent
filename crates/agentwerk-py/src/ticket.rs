//! The ticket as Python sees it. One class in both directions: the constructor
//! takes the caller-settable fields, and the same class comes back out of
//! queries and callbacks with its recorded state and messages as attributes.
//! Rust spells those fields as chained builders (`Ticket::new(...).label(...)
//! .schema(...).parent(...)`); a Python class cannot carry a `labels` method
//! and a `labels` attribute, so they arrive as keyword arguments instead.

use agentwerk::Ticket;
use pyo3::prelude::*;

use crate::convert::{py_to_value, value_to_py};
use crate::schema::PySchema;

/// A ticket to enqueue on a `TicketSystem`, and the ticket the system hands
/// back.
#[pyclass(name = "Ticket")]
pub struct PyTicket {
    pub inner: Ticket,
}

#[pymethods]
impl PyTicket {
    /// Create a ticket carrying `task` (any JSON-serializable value). `labels`
    /// assign it to agents, `schema` is validated against the result, and
    /// `parent` records a handover audit trail.
    #[new]
    #[pyo3(signature = (task, *, labels=None, schema=None, parent=None))]
    fn new(
        task: &Bound<'_, PyAny>,
        labels: Option<Vec<String>>,
        schema: Option<PyRef<'_, PySchema>>,
        parent: Option<String>,
    ) -> PyResult<Self> {
        let mut inner = Ticket::new(py_to_value(task)?);
        if let Some(labels) = labels {
            inner = inner.labels(labels);
        }
        if let Some(schema) = schema {
            inner = inner.schema(schema.inner.clone());
        }
        if let Some(parent) = parent {
            inner = inner.parent(parent);
        }
        Ok(PyTicket { inner })
    }

    /// True when the ticket carries `label`.
    fn has_label(&self, label: &str) -> bool {
        self.inner.has_label(label)
    }

    /// True while the ticket is created but not yet claimed.
    fn is_todo(&self) -> bool {
        self.inner.is_todo()
    }

    /// True once the ticket produced an accepted result.
    fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }

    /// True once the ticket gave up without a result.
    fn is_failed(&self) -> bool {
        self.inner.is_failed()
    }

    /// True while an agent is working the ticket.
    fn is_in_progress(&self) -> bool {
        self.inner.is_in_progress()
    }

    /// True while the ticket still has work left: todo or in progress.
    fn is_pending(&self) -> bool {
        self.inner.is_pending()
    }

    /// True once the ticket reached a terminal status: finished or failed.
    fn is_resolved(&self) -> bool {
        self.inner.is_resolved()
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
    fn labels(&self) -> Vec<String> {
        self.inner.labels.clone()
    }

    /// The schema the result is validated against, when the ticket carries one.
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

    /// Name of the agent that opened the ticket.
    #[getter]
    fn reporter(&self) -> &str {
        &self.inner.reporter
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

    /// The messages exchanged with the model. Converted on access, so a
    /// callback that never asks never pays for them.
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
    /// Wrap a ticket the system owns, messages included.
    pub fn from_ticket(ticket: &Ticket) -> Self {
        PyTicket {
            inner: ticket.clone(),
        }
    }

    /// Build the ticket to enqueue. Copies the caller-settable fields only:
    /// `insert` stamps key, status, reporter, and result but leaves the
    /// messages and lifecycle timestamps, so a ticket that came back out of
    /// the system would otherwise carry its messages into the new one.
    pub fn to_ticket(&self) -> Ticket {
        let mut ticket = Ticket::new(self.inner.task.clone()).labels(self.inner.labels.clone());
        if let Some(schema) = &self.inner.schema {
            ticket = ticket.schema(schema.clone());
        }
        if let Some(parent) = &self.inner.parent {
            ticket = ticket.parent(parent.clone());
        }
        ticket
    }
}
