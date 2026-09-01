//! Exposes finished task conversations as savable training examples through Python.

use agentwerk::agents::Trajectory;
use pyo3::prelude::*;

use crate::convert::runtime_error;
use crate::task::PyTask;

/// A `Trajectory` is one finished task kept as a training example.
#[pyclass(name = "Trajectory")]
pub struct PyTrajectory {
    inner: Trajectory,
}

#[pymethods]
impl PyTrajectory {
    /// Capture `task`'s messages as an example produced by `agent` using
    /// `model`, whose name `Werk.get_model_for_agent` gives you.
    #[staticmethod]
    fn from_task(agent_id: &str, model: Option<&str>, task: PyRef<'_, PyTask>) -> Self {
        PyTrajectory {
            inner: Trajectory::from_task(agent_id, model, &task.inner),
        }
    }

    /// Save the example under `dir` as `trajectories/<id>.json`, with an
    /// `.html` beside it for reading.
    fn save(&self, dir: &str) -> PyResult<()> {
        self.inner.save(dir).map_err(runtime_error)
    }

    /// The example's identifier, `<agent>-<task>`, which is also its file
    /// name.
    fn get_id(&self) -> &str {
        self.inner.get_id()
    }

    /// Name of the model that produced the replies, when it was known.
    fn get_model(&self) -> Option<&str> {
        self.inner.get_model()
    }

    fn get_replies(&self) -> Vec<crate::reply::PyReply> {
        crate::reply::replies_to_py(self.inner.get_replies())
    }

    fn __repr__(&self) -> String {
        format!(
            "Trajectory(id={:?}, replies={})",
            self.inner.get_id(),
            self.inner.get_replies().len()
        )
    }
}
