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
    const TOOL_CALL_REPAIRED: &'static str = Event::TOOL_CALL_REPAIRED;
    #[classattr]
    const TOOL_CALL_DECLINED: &'static str = Event::TOOL_CALL_DECLINED;
    #[classattr]
    const TOOL_CALL_STARTED: &'static str = Event::TOOL_CALL_STARTED;
    #[classattr]
    const TOOL_CALL_FINISHED: &'static str = Event::TOOL_CALL_FINISHED;
    #[classattr]
    const TOOL_CALL_FAILED: &'static str = Event::TOOL_CALL_FAILED;
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

    #[staticmethod]
    fn run_started() -> Self {
        Self {
            inner: Event::run_started(),
        }
    }

    #[staticmethod]
    fn run_finished(outcome: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            inner: Event::new(Event::RUN_FINISHED)
                .data(serde_json::json!({ "outcome": py_to_value(outcome)? })),
        })
    }

    #[staticmethod]
    fn task_created() -> Self {
        Self {
            inner: Event::task_created(),
        }
    }

    #[staticmethod]
    fn task_started() -> Self {
        Self {
            inner: Event::task_started(),
        }
    }

    #[staticmethod]
    fn task_finished() -> Self {
        Self {
            inner: Event::task_finished(),
        }
    }

    #[staticmethod]
    fn task_failed() -> Self {
        Self {
            inner: Event::task_failed(),
        }
    }

    #[staticmethod]
    fn turn_started() -> Self {
        Self {
            inner: Event::turn_started(),
        }
    }

    #[staticmethod]
    fn request_started(model: &str) -> Self {
        Self {
            inner: Event::request_started(model),
        }
    }

    #[staticmethod]
    fn request_finished(model: &str, usage: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            inner: Event::new(Event::REQUEST_FINISHED).data(serde_json::json!({
                "model": model,
                "usage": py_to_value(usage)?,
            })),
        })
    }

    #[staticmethod]
    fn request_failed(model: &str, kind: &str, message: &str) -> Self {
        Self {
            inner: Event::new(Event::REQUEST_FAILED).data(serde_json::json!({
                "model": model,
                "kind": kind,
                "message": message,
            })),
        }
    }

    #[staticmethod]
    fn request_retried(
        model: &str,
        attempt: u32,
        max_attempts: u32,
        kind: &str,
        message: &str,
    ) -> Self {
        Self {
            inner: Event::new(Event::REQUEST_RETRIED).data(serde_json::json!({
                "model": model,
                "attempt": attempt,
                "max_attempts": max_attempts,
                "kind": kind,
                "message": message,
            })),
        }
    }

    #[staticmethod]
    fn text_chunk_received(content: &str) -> Self {
        Self {
            inner: Event::text_chunk_received(content),
        }
    }

    #[staticmethod]
    fn tool_call_repaired(tool_name: &str, call_id: &str, kind: &str, message: &str) -> Self {
        Self {
            inner: Event::tool_call_repaired(tool_name, call_id, kind, message),
        }
    }

    #[staticmethod]
    fn tool_call_declined(tool_name: &str, kind: &str) -> Self {
        Self {
            inner: Event::new(Event::TOOL_CALL_DECLINED)
                .data(serde_json::json!({ "tool_name": tool_name, "kind": kind })),
        }
    }

    #[staticmethod]
    fn tool_call_started(
        tool_name: &str,
        call_id: &str,
        input: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        Ok(Self {
            inner: Event::tool_call_started(tool_name, call_id, py_to_value(input)?),
        })
    }

    #[staticmethod]
    fn tool_call_finished(output: &str) -> Self {
        Self {
            inner: Event::tool_call_finished(output),
        }
    }

    #[staticmethod]
    fn tool_call_failed(message: &str) -> Self {
        Self {
            inner: Event::tool_call_failed(message),
        }
    }

    #[staticmethod]
    fn knowledge_written(slug: &str) -> Self {
        Self {
            inner: Event::knowledge_written(slug),
        }
    }

    #[staticmethod]
    fn knowledge_read(slug: &str) -> Self {
        Self {
            inner: Event::knowledge_read(slug),
        }
    }

    #[staticmethod]
    fn knowledge_removed(slug: &str) -> Self {
        Self {
            inner: Event::knowledge_removed(slug),
        }
    }

    #[staticmethod]
    fn knowledge_listed() -> Self {
        Self {
            inner: Event::knowledge_listed(),
        }
    }

    #[staticmethod]
    fn knowledge_failed(action: &str, slug: &str, kind: &str, message: &str) -> Self {
        Self {
            inner: Event::knowledge_failed(action, slug, kind, message),
        }
    }

    #[staticmethod]
    fn policy_violated(policy: &str, limit: u64) -> Self {
        Self {
            inner: Event::new(Event::POLICY_VIOLATED)
                .data(serde_json::json!({ "policy": policy, "limit": limit })),
        }
    }

    #[staticmethod]
    fn schema_retried(attempt: u32, max_attempts: u32, kind: &str, message: &str) -> Self {
        Self {
            inner: Event::schema_retried(attempt, max_attempts, kind, message),
        }
    }

    #[staticmethod]
    fn compaction_started(trigger: &str, total: u32) -> Self {
        Self {
            inner: Event::compaction_started(trigger, total),
        }
    }

    #[staticmethod]
    fn compaction_progress(trigger: &str, completed: u32, total: u32) -> Self {
        Self {
            inner: Event::compaction_progress(trigger, completed, total),
        }
    }

    #[staticmethod]
    fn compaction_finished(trigger: &str) -> Self {
        Self {
            inner: Event::compaction_finished(trigger),
        }
    }

    #[staticmethod]
    fn compaction_failed(trigger: &str, kind: &str, message: &str) -> Self {
        Self {
            inner: Event::compaction_failed(trigger, kind, message),
        }
    }

    fn data<'py>(
        mut slf: PyRefMut<'py, Self>,
        data: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.inner = slf.inner.clone().data(py_to_value(data)?);
        Ok(slf)
    }

    fn directive<'py>(mut slf: PyRefMut<'py, Self>, directive: &str) -> PyRefMut<'py, Self> {
        slf.inner = slf.inner.clone().directive(directive);
        slf
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

    fn get_directive(&self) -> Option<&str> {
        self.inner.get_directive()
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
