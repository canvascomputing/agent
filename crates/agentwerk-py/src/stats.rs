//! The statistics as Python sees them: the same accessors under the same names,
//! with every duration in seconds.
//!
//! The numbers are computed rather than stored, so handing over the struct
//! itself would expose the layout instead of the figures.

use std::collections::BTreeMap;
use std::sync::Arc;

use agentwerk::event::EventName;
use agentwerk::{Stats, TicketQueue};
use pyo3::prelude::*;

use crate::convert::{runtime_error, value_to_py};

/// Resolve the name Python passed. Rust names the kind through `EventName`, so
/// a misspelling is a compile error there; here it has to be an error at the
/// call rather than a silent zero.
fn event_name(name: &str) -> PyResult<EventName> {
    serde_json::from_value(serde_json::Value::String(name.to_string()))
        .map_err(|_| runtime_error(format!("unknown event name: {name}")))
}

/// `Stats` holds the run-wide counts: the events, the tokens they cost, and how
/// long execution took. Anything finer is a fold over the events an `on_event`
/// handler receives.
#[pyclass(name = "Stats")]
pub struct PyStats {
    /// The run-wide statistics live inside the ticket queue, so holding the
    /// queue is what keeps them alive.
    queue: Arc<TicketQueue>,
}

impl PyStats {
    pub(crate) fn for_run(queue: Arc<TicketQueue>) -> Self {
        PyStats { queue }
    }

    fn get(&self) -> &Stats {
        self.queue.stats()
    }
}

#[pymethods]
impl PyStats {
    /// Get how many events of one kind were recorded. `name` is spelled the way
    /// `Event.kind` reports it, such as `"turn_started"`.
    fn event_count(&self, name: &str) -> PyResult<u64> {
        Ok(self.get().event_count(event_name(name)?))
    }

    /// Get per-event counts, keyed by event name.
    fn event_counts(&self) -> BTreeMap<String, u64> {
        self.get()
            .event_counts()
            .into_iter()
            .map(|(event, count)| (event.as_str().to_string(), count))
            .collect()
    }

    fn input_tokens(&self) -> u64 {
        self.get().input_tokens()
    }

    fn output_tokens(&self) -> u64 {
        self.get().output_tokens()
    }

    /// Get the elapsed duration in seconds, or `None` before execution starts.
    fn execution_duration(&self) -> Option<f64> {
        self.get().execution_duration().map(|d| d.as_secs_f64())
    }

    /// Get every figure as one dict, the same shape as `stats.json`.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let value = serde_json::to_value(self.get()).map_err(runtime_error)?;
        value_to_py(py, &value)
    }

    fn __repr__(&self) -> String {
        let stats = self.get();
        format!(
            "Stats(requests={}, tickets_finished={})",
            stats.event_count(EventName::RequestFinished),
            stats.event_count(EventName::TicketFinished)
        )
    }
}
