//! Shared `#[cfg(test)]` helpers for the inline `tickets::*` test modules.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::ticket_queue::TicketQueue;
use crate::agents::agent::Agent;
use crate::event::{EventKind, FinishReason};

/// Collect the reason from every `RunFinished`, since the queue keeps none.
pub(super) fn collect_finish_reasons(queue: &TicketQueue) -> Arc<Mutex<Vec<FinishReason>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    queue.on_event(move |event| {
        if let EventKind::RunFinished { reason } = event.kind {
            sink.lock().unwrap().push(reason);
        }
    });
    seen
}

pub(super) fn minimal_agent(label: &str) -> Agent {
    use crate::agents::r#loop::test_util::MockProvider;
    Agent::new()
        .label(label)
        .provider(MockProvider::with_results(vec![]))
        .model("mock")
        .build()
}

/// Build a `TicketQueue` rooted at a fresh `TempDir` so the default
/// `.agentwerk` directory never lands in the source tree during tests.
/// Hold the returned `TempDir` for the test's lifetime.
pub(super) fn test_queue() -> (Arc<TicketQueue>, crate::test_util::TempDir) {
    let dir = crate::test_util::TempDir::new().unwrap();
    let built = TicketQueue::new();
    built.dir(dir.path().to_path_buf());
    (built, dir)
}

pub(super) fn attach_done_result(queue: &TicketQueue, key: &str, result: &str) {
    queue
        .set_result(key, serde_json::Value::String(result.into()))
        .unwrap();
    queue.set_finished_by(key, "agent").unwrap();
}

pub(super) fn read_tickets_log(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("tickets.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
