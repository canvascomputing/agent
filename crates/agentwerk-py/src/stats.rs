//! The statistics as Python sees them: the same accessors under the same names,
//! with every duration in seconds.
//!
//! The numbers are computed rather than stored, so handing over the struct
//! itself would expose the layout instead of the figures.

use std::collections::BTreeMap;
use std::sync::Arc;

use agentwerk::agents::stats::{FileStat, KnowledgeStat, ModelStat, ToolStat};
use agentwerk::{Stats, TicketQueue};
use pyo3::prelude::*;

use crate::convert::{runtime_error, value_to_py};

/// Where the numbers come from. The run-wide statistics live inside the ticket
/// queue, so holding the queue is what keeps them alive. A slice scoped to one
/// label or agent is its own shared value.
enum Source {
    Run(Arc<TicketQueue>),
    Slice(Arc<Stats>),
}

/// `Stats` holds the metrics about tickets, tokens, and time, for the whole
/// execution or for one label or agent within it.
#[pyclass(name = "Stats")]
pub struct PyStats {
    source: Source,
}

impl PyStats {
    pub(crate) fn for_run(queue: Arc<TicketQueue>) -> Self {
        PyStats {
            source: Source::Run(queue),
        }
    }

    fn get(&self) -> &Stats {
        match &self.source {
            Source::Run(queue) => queue.stats(),
            Source::Slice(stats) => stats,
        }
    }
}

#[pymethods]
impl PyStats {
    /// Get statistics scoped to one label. `run_duration()` is always `None`
    /// here, since timing stays global.
    fn stats_for_label(&self, label: &str) -> PyStats {
        PyStats {
            source: Source::Slice(self.get().stats_for_label(label)),
        }
    }

    /// Get statistics scoped to one agent, by the name it was added under.
    ///
    /// `tickets_created()` counts the tickets that agent filed; the rest count
    /// the tickets it claimed.
    fn stats_for_agent(&self, agent_name: &str) -> PyStats {
        PyStats {
            source: Source::Slice(self.get().stats_for_agent(agent_name)),
        }
    }

    /// Get a ticket's token usage, oldest first.
    fn token_usage<'py>(&self, py: Python<'py>, ticket_key: &str) -> PyResult<Bound<'py, PyAny>> {
        let history = self.get().token_usage(ticket_key);
        let value = serde_json::to_value(&history).map_err(runtime_error)?;
        value_to_py(py, &value)
    }

    /// Get per-tool call and failure counts, keyed by tool name.
    fn tool_stats(&self) -> BTreeMap<String, PyToolStat> {
        self.get()
            .tool_stats()
            .into_iter()
            .map(|(name, stat)| (name, PyToolStat { inner: stat }))
            .collect()
    }

    /// Get per-path open and failure counts for the files tools opened.
    fn file_stats(&self) -> BTreeMap<String, PyFileStat> {
        self.get()
            .file_stats()
            .into_iter()
            .map(|(path, stat)| (path, PyFileStat { inner: stat }))
            .collect()
    }

    /// Get knowledge usage: write, read, remove, list, and miss counts.
    fn knowledge_stats(&self) -> PyKnowledgeStat {
        PyKnowledgeStat {
            inner: self.get().knowledge_stats(),
        }
    }

    /// Get per-model requests and token usage, keyed by model name.
    fn model_stats(&self) -> BTreeMap<String, PyModelStat> {
        self.get()
            .model_stats()
            .into_iter()
            .map(|(model, stat)| (model, PyModelStat { inner: stat }))
            .collect()
    }

    fn turns(&self) -> u64 {
        self.get().turns()
    }

    fn requests(&self) -> u64 {
        self.get().requests()
    }

    fn tool_calls(&self) -> u64 {
        self.get().tool_calls()
    }

    fn requests_failed(&self) -> u64 {
        self.get().requests_failed()
    }

    /// Get per-event counts, keyed by event name.
    fn event_counts(&self) -> BTreeMap<String, u64> {
        self.get().event_counts()
    }

    fn input_tokens(&self) -> u64 {
        self.get().input_tokens()
    }

    fn output_tokens(&self) -> u64 {
        self.get().output_tokens()
    }

    fn tickets_created(&self) -> u64 {
        self.get().tickets_created()
    }

    fn tickets_finished(&self) -> u64 {
        self.get().tickets_finished()
    }

    fn tickets_failed(&self) -> u64 {
        self.get().tickets_failed()
    }

    /// Get the elapsed duration in seconds, or `None` before execution starts.
    fn run_duration(&self) -> Option<f64> {
        self.get().run_duration().map(|d| d.as_secs_f64())
    }

    /// Get `finished / (finished + failed)`, or `None` before any ticket is
    /// resolved.
    fn tickets_success_rate(&self) -> Option<f64> {
        self.get().tickets_success_rate()
    }

    /// Get the total time from creation to resolution, in seconds.
    fn total_ticket_duration(&self) -> f64 {
        self.get().total_ticket_duration().as_secs_f64()
    }

    fn avg_ticket_duration(&self) -> Option<f64> {
        self.get().avg_ticket_duration().map(|d| d.as_secs_f64())
    }

    /// Get the total time agents held tickets, in seconds.
    fn total_work_duration(&self) -> f64 {
        self.get().total_work_duration().as_secs_f64()
    }

    fn avg_work_duration(&self) -> Option<f64> {
        self.get().avg_work_duration().map(|d| d.as_secs_f64())
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
            stats.requests(),
            stats.tickets_finished()
        )
    }
}

/// A `ToolStat` counts one tool's calls and failures.
#[pyclass(name = "ToolStat")]
pub struct PyToolStat {
    inner: ToolStat,
}

#[pymethods]
impl PyToolStat {
    /// Every call, including calls naming a tool that is not registered.
    #[getter]
    fn calls(&self) -> u64 {
        self.inner.calls
    }

    #[getter]
    fn not_found(&self) -> u64 {
        self.inner.not_found
    }

    #[getter]
    fn execution_failed(&self) -> u64 {
        self.inner.execution_failed
    }

    #[getter]
    fn schema_failed(&self) -> u64 {
        self.inner.schema_failed
    }

    /// Get the total failures across the three kinds.
    fn errors(&self) -> u64 {
        self.inner.errors()
    }

    /// Get `errors / calls`, or `None` when the tool was never called.
    fn error_rate(&self) -> Option<f64> {
        self.inner.error_rate()
    }

    fn __repr__(&self) -> String {
        format!(
            "ToolStat(calls={}, errors={})",
            self.inner.calls,
            self.inner.errors()
        )
    }
}

/// A `FileStat` counts how often a tool opened one path, and how often it could
/// not.
#[pyclass(name = "FileStat")]
pub struct PyFileStat {
    inner: FileStat,
}

#[pymethods]
impl PyFileStat {
    #[getter]
    fn opens(&self) -> u64 {
        self.inner.opens
    }

    #[getter]
    fn failed(&self) -> u64 {
        self.inner.failed
    }

    /// Get the total failures.
    fn errors(&self) -> u64 {
        self.inner.errors()
    }

    /// Get `errors / opens`, or `None` when the path was never named.
    fn error_rate(&self) -> Option<f64> {
        self.inner.error_rate()
    }

    fn __repr__(&self) -> String {
        format!(
            "FileStat(opens={}, failed={})",
            self.inner.opens, self.inner.failed
        )
    }
}

/// A `KnowledgeStat` counts what agents did to the knowledge pages.
#[pyclass(name = "KnowledgeStat")]
pub struct PyKnowledgeStat {
    inner: KnowledgeStat,
}

#[pymethods]
impl PyKnowledgeStat {
    #[getter]
    fn writes(&self) -> u64 {
        self.inner.writes
    }

    #[getter]
    fn reads(&self) -> u64 {
        self.inner.reads
    }

    #[getter]
    fn removes(&self) -> u64 {
        self.inner.removes
    }

    #[getter]
    fn lists(&self) -> u64 {
        self.inner.lists
    }

    /// Attempts that did not go through, across the other four counts.
    #[getter]
    fn failed(&self) -> u64 {
        self.inner.failed
    }

    /// Get the total failures.
    fn errors(&self) -> u64 {
        self.inner.errors()
    }

    /// Get `errors / attempts`, or `None` when the pages were never touched.
    fn error_rate(&self) -> Option<f64> {
        self.inner.error_rate()
    }

    fn __repr__(&self) -> String {
        format!(
            "KnowledgeStat(writes={}, reads={})",
            self.inner.writes, self.inner.reads
        )
    }
}

/// A `ModelStat` counts one model's requests and token usage.
#[pyclass(name = "ModelStat")]
pub struct PyModelStat {
    inner: ModelStat,
}

#[pymethods]
impl PyModelStat {
    #[getter]
    fn requests(&self) -> u64 {
        self.inner.requests
    }

    /// Requests that came back as a failure and were not retried.
    #[getter]
    fn failed(&self) -> u64 {
        self.inner.failed
    }

    #[getter]
    fn input_tokens(&self) -> u64 {
        self.inner.input_tokens
    }

    #[getter]
    fn output_tokens(&self) -> u64 {
        self.inner.output_tokens
    }

    /// Get the total failures.
    fn errors(&self) -> u64 {
        self.inner.errors()
    }

    /// Get `errors / requests`, or `None` when the model was never asked.
    fn error_rate(&self) -> Option<f64> {
        self.inner.error_rate()
    }

    fn __repr__(&self) -> String {
        format!(
            "ModelStat(requests={}, input_tokens={}, output_tokens={})",
            self.inner.requests, self.inner.input_tokens, self.inner.output_tokens
        )
    }
}
