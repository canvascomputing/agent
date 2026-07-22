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
    agent_name: String,
    #[pyo3(get)]
    ticket_key: String,
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
        format!("Event(kind={:?}, ticket_key={:?})", self.kind, self.ticket_key)
    }
}

/// Build a `PyEvent` from a crate `Event`.
pub fn to_py_event(event: &Event) -> PyEvent {
    let (kind, data) = describe(&event.kind);
    PyEvent {
        kind: kind.to_string(),
        agent_name: event.agent_name.clone(),
        ticket_key: event.ticket_key.clone(),
        data,
    }
}

/// Map an `EventKind` to its stable name and a JSON payload. Enum-typed fields
/// (error kinds, policy kinds, reasons) are rendered as their debug string.
fn describe(kind: &EventKind) -> (&'static str, Value) {
    use EventKind::*;
    match kind {
        RunStarted => ("run_started", json!({})),
        RunFinished { reason } => ("run_finished", json!({ "reason": format!("{reason:?}") })),
        TicketStarted => ("ticket_started", json!({})),
        TicketFinished => ("ticket_finished", json!({})),
        TicketFailed => ("ticket_failed", json!({})),
        TurnStarted => ("turn_started", json!({})),
        RequestStarted { model } => ("request_started", json!({ "model": model })),
        RequestFinished { model, usage } => (
            "request_finished",
            json!({ "model": model, "usage": serde_json::to_value(usage).unwrap_or(Value::Null) }),
        ),
        RequestFailed {
            model,
            kind,
            message,
        } => (
            "request_failed",
            json!({ "model": model, "kind": format!("{kind:?}"), "message": message }),
        ),
        RequestRetried {
            model,
            attempt,
            max_attempts,
            kind,
            message,
        } => (
            "request_retried",
            json!({ "model": model, "attempt": attempt, "max_attempts": max_attempts, "kind": format!("{kind:?}"), "message": message }),
        ),
        TextChunkReceived { content } => ("text_chunk_received", json!({ "content": content })),
        ToolCallStarted {
            tool_name,
            call_id,
            input,
        } => (
            "tool_call_started",
            json!({ "tool_name": tool_name, "call_id": call_id, "input": input }),
        ),
        ToolCallFinished {
            tool_name,
            call_id,
            output,
        } => (
            "tool_call_finished",
            json!({ "tool_name": tool_name, "call_id": call_id, "output": output }),
        ),
        ToolCallFailed {
            tool_name,
            call_id,
            kind,
            message,
        } => (
            "tool_call_failed",
            json!({ "tool_name": tool_name, "call_id": call_id, "kind": format!("{kind:?}"), "message": message }),
        ),
        FileOpenFinished { path } => ("file_open_finished", json!({ "path": path })),
        FileOpenFailed { path } => ("file_open_failed", json!({ "path": path })),
        KnowledgeUsed { op } => ("knowledge_used", json!({ "op": format!("{op:?}") })),
        KnowledgeMissed => ("knowledge_missed", json!({})),
        PolicyViolated { kind, limit } => (
            "policy_violated",
            json!({ "kind": format!("{kind:?}"), "limit": limit }),
        ),
        SchemaRetried {
            attempt,
            max_attempts,
            message,
        } => (
            "schema_retried",
            json!({ "attempt": attempt, "max_attempts": max_attempts, "message": message }),
        ),
        CompactionStarted { reason, total } => (
            "compaction_started",
            json!({ "reason": format!("{reason:?}"), "total": total }),
        ),
        CompactionProgress {
            reason,
            completed,
            total,
        } => (
            "compaction_progress",
            json!({ "reason": format!("{reason:?}"), "completed": completed, "total": total }),
        ),
        CompactionFinished { reason } => (
            "compaction_finished",
            json!({ "reason": format!("{reason:?}") }),
        ),
        CompactionFailed { reason, message } => (
            "compaction_failed",
            json!({ "reason": format!("{reason:?}"), "message": message }),
        ),
    }
}
