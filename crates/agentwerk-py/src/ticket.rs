//! The ticket as Python sees it. One class in both directions: `with_*` builds
//! a ticket to enqueue, mirroring `Ticket::new(...).label(...).schema(...)
//! .parent(...)`, and the same class comes back out of queries and callbacks
//! with its recorded state and messages as attributes. The prefix is what
//! keeps the two apart: Python cannot carry a `labels` method and a `labels`
//! attribute on one class.

use agentwerk::{Status, Ticket};
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
    /// Create a ticket carrying `task` (any JSON-serializable value).
    #[new]
    fn new(task: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(PyTicket {
            inner: Ticket::new(py_to_value(task)?),
        })
    }

    fn with_label(mut slf: PyRefMut<'_, Self>, label: String) -> PyRefMut<'_, Self> {
        slf.inner.labels.push(label);
        slf
    }

    fn with_labels(mut slf: PyRefMut<'_, Self>, labels: Vec<String>) -> PyRefMut<'_, Self> {
        slf.inner.labels.extend(labels);
        slf
    }

    /// Attach a result schema the finisher validates against.
    fn with_schema<'py>(
        mut slf: PyRefMut<'py, Self>,
        schema: PyRef<'_, PySchema>,
    ) -> PyRefMut<'py, Self> {
        slf.inner.schema = Some(schema.inner.clone());
        slf
    }

    /// Record a parent ticket key, forming a handover audit trail.
    fn with_parent(mut slf: PyRefMut<'_, Self>, key: String) -> PyRefMut<'_, Self> {
        slf.inner.parent = Some(key);
        slf
    }

    /// True when the ticket carries `label`.
    fn has_label(&self, label: &str) -> bool {
        self.inner.has_label(label)
    }

    #[getter]
    fn key(&self) -> &str {
        &self.inner.key
    }

    /// `"Todo"`, `"InProgress"`, `"Finished"`, or `"Failed"`.
    #[getter]
    fn status(&self) -> &'static str {
        match self.inner.status {
            Status::Todo => "Todo",
            Status::InProgress => "InProgress",
            Status::Finished => "Finished",
            Status::Failed => "Failed",
        }
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

    /// The messages exchanged with the model, as dicts. Converted on access,
    /// so a callback that never asks never pays for the transcript.
    #[getter]
    fn replies<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let value =
            serde_json::to_value(&self.inner.replies).map_err(crate::convert::runtime_error)?;
        value_to_py(py, &value)
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
    /// Wrap a ticket the system owns, transcript included.
    pub fn from_ticket(ticket: &Ticket) -> Self {
        PyTicket {
            inner: ticket.clone(),
        }
    }

    /// Build the ticket to enqueue. Copies the caller-settable fields only:
    /// `insert` stamps key, status, reporter, and result but leaves the
    /// messages and lifecycle timestamps, so a ticket that came back out of
    /// the system would otherwise carry its transcript into the new one.
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
