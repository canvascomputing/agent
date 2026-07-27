//! Events as Python sees them. An `Event` is flattened to a small object with
//! `kind`, `agent_name`, `ticket_key`, and a `data` dict carrying the variant's
//! payload, so a Python handler can inspect any event without a class per kind.

use agentwerk::event::{Event, EventKind};
use pyo3::prelude::*;
use serde_json::{json, Value};

use crate::convert::value_to_py;

/// One observation from a run.
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
    /// The variant payload (model, tokens, tool name, message, ...) as a dict.
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

/// The variant's payload as JSON. Enum-typed fields (error kinds, policy kinds,
/// reasons) render through their `Display` impl, so every string Python sees is
/// snake_case and named in one place on the crate side.
fn payload(kind: &EventKind) -> Value {
    use EventKind::*;
    match kind {
        RunStarted | TicketStarted | TicketFinished | TicketFailed | TurnStarted
        | KnowledgeMissed => json!({}),
        RunFinished { reason } => json!({ "reason": reason.to_string() }),
        RequestStarted { model } => json!({ "model": model }),
        RequestFinished { model, usage } => {
            json!({ "model": model, "usage": serde_json::to_value(usage).unwrap_or(Value::Null) })
        }
        RequestFailed {
            model,
            kind,
            message,
        } => json!({ "model": model, "kind": kind.to_string(), "message": message }),
        RequestRetried {
            model,
            attempt,
            max_attempts,
            kind,
            message,
        } => {
            json!({ "model": model, "attempt": attempt, "max_attempts": max_attempts, "kind": kind.to_string(), "message": message })
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
            kind,
            message,
        } => {
            json!({ "tool_name": tool_name, "call_id": call_id, "kind": kind.to_string(), "message": message })
        }
        FileOpenFinished { path } | FileOpenFailed { path } => json!({ "path": path }),
        KnowledgeUsed { op } => json!({ "op": op.to_string() }),
        PolicyViolated { kind, limit } => json!({ "kind": kind.to_string(), "limit": limit }),
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
