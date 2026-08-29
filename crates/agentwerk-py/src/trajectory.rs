//! A task's messages captured as a training example, as Python sees it.
//! Mirrors `Trajectory::from_task(agent_id, task).save(dir)`.

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
    /// `model`, whose name `Queue.get_model_for_agent` gives you.
    #[staticmethod]
    fn from_task(agent_id: &str, model: Option<&str>, task: PyRef<'_, PyTask>) -> Self {
        PyTrajectory {
            inner: Trajectory::from_task(agent_id, model, &task.inner),
        }
    }

    /// Save the example under `dir` as `trajectories/<key>.json`, with an
    /// `.html` beside it for reading.
    fn save(&self, dir: &str) -> PyResult<()> {
        self.inner.save(dir).map_err(runtime_error)
    }

    /// The example's identifier, `<agent>-<task>`, which is also its file
    /// name.
    fn get_key(&self) -> &str {
        self.inner.get_key()
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
            "Trajectory(key={:?}, replies={})",
            self.inner.get_key(),
            self.inner.get_replies().len()
        )
    }
}
