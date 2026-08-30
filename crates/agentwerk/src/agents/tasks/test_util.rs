//! Shared `#[cfg(test)]` helpers for the inline `tasks::*` test modules.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::queue::Queue;
use crate::agents::agent::Agent;
use crate::event::Event;

use super::FinishReason;

/// Collect the reason from every `RunFinished`, since the queue keeps none.
pub(super) fn collect_finish_reasons(queue: &Queue) -> Arc<Mutex<Vec<FinishReason>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    queue.on_event(move |_, event| {
        if event.get_name() == Event::RUN_FINISHED {
            if let Some(reason) = event
                .get_data()
                .get("reason")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
            {
                sink.lock().unwrap().push(reason);
            }
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
}

/// Build a `Queue` rooted at a fresh `TempDir` so the default
/// `.agentwerk` directory never lands in the source tree during tests.
/// Hold the returned `TempDir` for the test's lifetime.
pub(super) fn test_queue() -> (Arc<Queue>, crate::test_util::TempDir) {
    let dir = crate::test_util::TempDir::new().unwrap();
    let built = Queue::new();
    built.set_dir(dir.path().to_path_buf());
    (built, dir)
}

pub(super) fn attach_done_result(queue: &Queue, id: &str, result: &str) {
    queue
        .set_result(id, serde_json::Value::String(result.into()))
        .unwrap();
    queue.set_finished_by(id, "agent").unwrap();
}

pub(super) fn read_events_log(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
