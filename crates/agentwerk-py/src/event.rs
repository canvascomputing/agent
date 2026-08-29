//! Events as Python sees them.

use agentwerk::event::Event;
use pyo3::prelude::*;

use crate::convert::{py_to_value, value_to_py};

/// An `Event` reports one thing that happened as agents work.
#[pyclass(name = "Event")]
pub struct PyEvent {
    pub(crate) inner: Event,
}

#[pymethods]
impl PyEvent {
    #[classattr]
    const RUN_STARTED: &'static str = Event::RUN_STARTED;
    #[classattr]
    const RUN_FINISHED: &'static str = Event::RUN_FINISHED;
    #[classattr]
    const TASK_CREATED: &'static str = Event::TASK_CREATED;
    #[classattr]
    const TASK_STARTED: &'static str = Event::TASK_STARTED;
    #[classattr]
    const TASK_FINISHED: &'static str = Event::TASK_FINISHED;
    #[classattr]
    const TASK_FAILED: &'static str = Event::TASK_FAILED;
    #[classattr]
    const TURN_STARTED: &'static str = Event::TURN_STARTED;
    #[classattr]
    const REQUEST_STARTED: &'static str = Event::REQUEST_STARTED;
    #[classattr]
    const REQUEST_FINISHED: &'static str = Event::REQUEST_FINISHED;
    #[classattr]
    const REQUEST_FAILED: &'static str = Event::REQUEST_FAILED;
    #[classattr]
    const REQUEST_RETRIED: &'static str = Event::REQUEST_RETRIED;
    #[classattr]
    const TEXT_CHUNK_RECEIVED: &'static str = Event::TEXT_CHUNK_RECEIVED;
    #[classattr]
    const RESPONSE_REPAIRED: &'static str = Event::RESPONSE_REPAIRED;
    #[classattr]
    const TOOL_CALL_DECLINED: &'static str = Event::TOOL_CALL_DECLINED;
    #[classattr]
    const TOOL_CALL_STARTED: &'static str = Event::TOOL_CALL_STARTED;
    #[classattr]
    const TOOL_CALL_FINISHED: &'static str = Event::TOOL_CALL_FINISHED;
    #[classattr]
    const TOOL_CALL_FAILED: &'static str = Event::TOOL_CALL_FAILED;
    #[classattr]
    const FILE_OPEN_FINISHED: &'static str = Event::FILE_OPEN_FINISHED;
    #[classattr]
    const FILE_OPEN_FAILED: &'static str = Event::FILE_OPEN_FAILED;
    #[classattr]
    const KNOWLEDGE_WRITTEN: &'static str = Event::KNOWLEDGE_WRITTEN;
    #[classattr]
    const KNOWLEDGE_READ: &'static str = Event::KNOWLEDGE_READ;
    #[classattr]
    const KNOWLEDGE_REMOVED: &'static str = Event::KNOWLEDGE_REMOVED;
    #[classattr]
    const KNOWLEDGE_LISTED: &'static str = Event::KNOWLEDGE_LISTED;
    #[classattr]
    const KNOWLEDGE_FAILED: &'static str = Event::KNOWLEDGE_FAILED;
    #[classattr]
    const POLICY_VIOLATED: &'static str = Event::POLICY_VIOLATED;
    #[classattr]
    const SCHEMA_RETRIED: &'static str = Event::SCHEMA_RETRIED;
    #[classattr]
    const COMPACTION_STARTED: &'static str = Event::COMPACTION_STARTED;
    #[classattr]
    const COMPACTION_PROGRESS: &'static str = Event::COMPACTION_PROGRESS;
    #[classattr]
    const COMPACTION_FINISHED: &'static str = Event::COMPACTION_FINISHED;
    #[classattr]
    const COMPACTION_FAILED: &'static str = Event::COMPACTION_FAILED;

    #[new]
    fn new(name: &str) -> Self {
        Self {
            inner: Event::new(name),
        }
    }

    fn data<'py>(
        mut slf: PyRefMut<'py, Self>,
        data: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.inner = slf.inner.clone().data(py_to_value(data)?);
        Ok(slf)
    }

    fn task_id<'py>(mut slf: PyRefMut<'py, Self>, task_id: &str) -> PyRefMut<'py, Self> {
        slf.inner = slf.inner.clone().task_id(task_id);
        slf
    }

    fn agent_id<'py>(mut slf: PyRefMut<'py, Self>, agent_id: &str) -> PyRefMut<'py, Self> {
        slf.inner = slf.inner.clone().agent_id(agent_id);
        slf
    }

    fn get_name(&self) -> &str {
        self.inner.get_name()
    }

    fn get_data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, self.inner.get_data())
    }

    fn get_task_id(&self) -> &str {
        self.inner.get_task_id()
    }

    fn get_agent_id(&self) -> &str {
        self.inner.get_agent_id()
    }

    fn get_label(&self) -> Option<&str> {
        self.inner.get_label()
    }

    fn get_created_at(&self) -> u64 {
        self.inner.get_created_at()
    }

    fn __repr__(&self) -> String {
        format!(
            "Event(name={:?}, task_id={:?})",
            self.inner.get_name(),
            self.inner.get_task_id()
        )
    }
}

/// Build a `PyEvent` from a crate `Event`.
pub fn to_py_event(event: &Event) -> PyEvent {
    PyEvent {
        inner: event.clone(),
    }
}
