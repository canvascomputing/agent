//! A ticket's messages captured as a training example, as Python sees it.
//! Mirrors `Trajectory::from_ticket(agent, ticket).save(dir)`.

use agentwerk::agents::Trajectory;
use pyo3::prelude::*;

use crate::convert::{runtime_error, value_to_py};
use crate::ticket::PyTicket;

/// A finished agent run reduced to its messages.
#[pyclass(name = "Trajectory")]
pub struct PyTrajectory {
    inner: Trajectory,
}

#[pymethods]
impl PyTrajectory {
    /// Capture `ticket`'s messages as an example produced by `agent` using
    /// `model`. Read the model name from
    /// `TicketSystem.model_for_agent(event.agent_name)`.
    #[staticmethod]
    fn from_ticket(agent: &str, model: Option<&str>, ticket: PyRef<'_, PyTicket>) -> Self {
        PyTrajectory {
            inner: Trajectory::from_ticket(agent, model, &ticket.inner),
        }
    }

    /// Write the example under `dir` as `trajectories/<key>.json`, plus an
    /// `.html` sibling rendering the messages for reading.
    fn save(&self, dir: &str) -> PyResult<()> {
        self.inner.save(dir).map_err(runtime_error)
    }

    /// Example id `<agent>-<ticket>`; also the file name.
    #[getter]
    fn key(&self) -> &str {
        &self.inner.key
    }

    /// Name of the model that produced the messages, when it was known.
    #[getter]
    fn model(&self) -> Option<&str> {
        self.inner.model.as_deref()
    }

    #[getter]
    fn messages<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let value = serde_json::to_value(&self.inner.messages).map_err(runtime_error)?;
        value_to_py(py, &value)
    }

    fn __repr__(&self) -> String {
        format!(
            "Trajectory(key={:?}, messages={})",
            self.inner.key,
            self.inner.messages.len()
        )
    }
}
