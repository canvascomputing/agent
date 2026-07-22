//! The ticket builder as Python sees it. Mirrors `Ticket::new(...).label(...)
//! .schema(...).parent(...)`. Query results and callback inputs are plain dicts
//! (a `Ticket` serializes cleanly), so this type is only the write/build side.

use agentwerk::Ticket;
use pyo3::prelude::*;
use serde_json::Value;

use crate::convert::py_to_value;
use crate::schema::PySchema;

/// A ticket to enqueue on a `TicketSystem`.
#[pyclass(name = "Ticket")]
pub struct PyTicket {
    task: Value,
    labels: Vec<String>,
    schema: Option<Value>,
    parent: Option<String>,
}

#[pymethods]
impl PyTicket {
    /// Create a ticket carrying `task` (any JSON-serializable value).
    #[new]
    fn new(task: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(PyTicket {
            task: py_to_value(task)?,
            labels: Vec::new(),
            schema: None,
            parent: None,
        })
    }

    fn label(mut slf: PyRefMut<'_, Self>, label: String) -> PyRefMut<'_, Self> {
        slf.labels.push(label);
        slf
    }

    fn labels(mut slf: PyRefMut<'_, Self>, labels: Vec<String>) -> PyRefMut<'_, Self> {
        slf.labels.extend(labels);
        slf
    }

    /// Attach a result schema the finisher validates against.
    fn schema<'py>(
        mut slf: PyRefMut<'py, Self>,
        schema: PyRef<'_, PySchema>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.schema = Some(schema.source.clone());
        Ok(slf)
    }

    /// Record a parent ticket key, forming a handover audit trail.
    fn parent(mut slf: PyRefMut<'_, Self>, key: String) -> PyRefMut<'_, Self> {
        slf.parent = Some(key);
        slf
    }
}

impl PyTicket {
    /// Build the real `Ticket`. Schema re-parsing cannot fail here: the value
    /// came from an already-validated `PySchema`.
    pub fn to_ticket(&self) -> Ticket {
        let mut ticket = Ticket::new(self.task.clone()).labels(self.labels.clone());
        if let Some(schema) = &self.schema {
            if let Ok(parsed) = agentwerk::Schema::parse(schema.clone()) {
                ticket = ticket.schema(parsed);
            }
        }
        if let Some(parent) = &self.parent {
            ticket = ticket.parent(parent.clone());
        }
        ticket
    }
}
