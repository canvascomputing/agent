//! Events as Python sees them: one object with `kind`, `agent_name`,
//! `ticket_key`, and a `data` dict, so a handler reads any event without a class
//! per kind.

use agentwerk::event::{Event, EventKind};
use pyo3::prelude::*;
use serde_json::{json, Value};

use crate::convert::value_to_py;

/// An `Event` reports one thing that happened as agents work.
#[pyclass(name = "Event")]
pub struct PyEvent {
    #[pyo3(get)]
    kind: String,
    #[pyo3(get)]
    pub(crate) agent_name: String,
    #[pyo3(get)]
    pub(crate) ticket_key: String,
    data: Value,
}

#[pymethods]
impl PyEvent {
    /// What the event carries: model, tokens, tool name, message.
    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.data)
    }

    fn __repr__(&self) -> String {
        format!(
            "Event(kind={:?}, ticket_key={:?})",
            self.kind, self.ticket_key
        )
    }
}

/// Build a `PyEvent` from a crate `Event`.
pub fn to_py_event(event: &Event) -> PyEvent {
    PyEvent {
        kind: event.kind.to_string(),
        agent_name: event.agent_name.clone(),
        ticket_key: event.ticket_key.clone(),
        data: payload(&event.kind),
    }
}

/// What the event carries, as JSON. A typed field renders through its `Display`,
/// so every string Python sees is snake_case and spelled in one place.
fn payload(kind: &EventKind) -> Value {
    use EventKind::*;
    match kind {
        RunStarted | TicketStarted | TicketFinished | TicketFailed | TurnStarted => json!({}),
        RunFinished { reason } => json!({ "reason": reason.to_string() }),
        RequestStarted { model } => json!({ "model": model }),
        RequestFinished { model, usage } => {
            json!({ "model": model, "usage": serde_json::to_value(usage).unwrap_or(Value::Null) })
        }
        RequestFailed {
            model,
            reason,
            message,
        } => json!({ "model": model, "reason": reason.to_string(), "message": message }),
        RequestRetried {
            model,
            attempt,
            max_attempts,
            reason,
            message,
        } => {
            json!({ "model": model, "attempt": attempt, "max_attempts": max_attempts, "reason": reason.to_string(), "message": message })
        }
        TextChunkReceived { content } => json!({ "content": content }),
        ToolCallStarted {
            tool_name,
            call_id,
            input,
        } => json!({ "tool_name": tool_name, "call_id": call_id, "input": input }),
        ToolCallFinished {
            tool_name,
            call_id,
            output,
        } => json!({ "tool_name": tool_name, "call_id": call_id, "output": output }),
        ToolCallFailed {
            tool_name,
            call_id,
            reason,
            message,
        } => {
            json!({ "tool_name": tool_name, "call_id": call_id, "reason": reason.to_string(), "message": message })
        }
        FileOpenFinished { path } | FileOpenFailed { path } => json!({ "path": path }),
        KnowledgeUsed { op } | KnowledgeFailed { op } => json!({ "op": op.to_string() }),
        PolicyViolated { policy, limit } => json!({ "policy": policy.to_string(), "limit": limit }),
        SchemaRetried {
            attempt,
            max_attempts,
            message,
        } => json!({ "attempt": attempt, "max_attempts": max_attempts, "message": message }),
        CompactionStarted { reason, total } => {
            json!({ "reason": reason.to_string(), "total": total })
        }
        CompactionProgress {
            reason,
            completed,
            total,
        } => json!({ "reason": reason.to_string(), "completed": completed, "total": total }),
        CompactionFinished { reason } => json!({ "reason": reason.to_string() }),
        CompactionFailed { reason, message } => {
            json!({ "reason": reason.to_string(), "message": message })
        }
    }
}
