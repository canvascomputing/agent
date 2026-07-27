//! Run statistics as Python sees them. The Rust accessors are computed, not
//! stored, so serialising the struct would hand Python the field layout instead
//! of the numbers callers ask for. This class exposes the accessors themselves,
//! under their Rust names, with every duration in seconds.

use std::collections::BTreeMap;
use std::sync::Arc;

use agentwerk::agents::stats::{FileStat, KnowledgeStat, ModelStat, ToolStat};
use agentwerk::{Stats, TicketSystem};
use pyo3::prelude::*;

use crate::convert::{runtime_error, value_to_py};

/// Where the numbers come from. The run-wide statistics live inside the ticket
/// system, so holding the system is what keeps them alive; a label slice is its
/// own shared value.
enum Source {
    Run(Arc<TicketSystem>),
    Label(Arc<Stats>),
}

/// Statistics for a run, or for one label within it.
#[pyclass(name = "Stats")]
pub struct PyStats {
    source: Source,
}

impl PyStats {
    pub(crate) fn for_run(system: Arc<TicketSystem>) -> Self {
        PyStats {
            source: Source::Run(system),
        }
    }

    fn get(&self) -> &Stats {
        match &self.source {
            Source::Run(system) => system.stats(),
            Source::Label(stats) => stats,
        }
    }
}

#[pymethods]
impl PyStats {
    /// Statistics scoped to one ticket label. `run_duration()` is always `None`
    /// on a slice, since run timing stays global.
    fn stats_for_label(&self, label: &str) -> PyStats {
        PyStats {
            source: Source::Label(self.get().stats_for_label(label)),
        }
    }

    /// The token usage recorded for one ticket, oldest first.
    fn usage_history<'py>(&self, py: Python<'py>, ticket_key: &str) -> PyResult<Bound<'py, PyAny>> {
        let history = self.get().usage_history(ticket_key);
        let value = serde_json::to_value(&history).map_err(runtime_error)?;
        value_to_py(py, &value)
    }

    /// Per-tool call and failure tallies, keyed by tool name.
    fn tool_stats(&self) -> BTreeMap<String, PyToolStat> {
        self.get()
            .tool_stats()
            .into_iter()
            .map(|(name, stat)| (name, PyToolStat { inner: stat }))
            .collect()
    }

    /// Per-path open and failure counts for the files tools opened.
    fn file_stats(&self) -> BTreeMap<String, PyFileStat> {
        self.get()
            .file_stats()
            .into_iter()
            .map(|(path, stat)| (path, PyFileStat { inner: stat }))
            .collect()
    }

    /// Knowledge-store usage across the run.
    fn knowledge_stats(&self) -> PyKnowledgeStat {
        PyKnowledgeStat {
            inner: self.get().knowledge_stats(),
        }
    }

    /// Per-model request and token totals, keyed by model name.
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

    fn errors(&self) -> u64 {
        self.get().errors()
    }

    /// Per-event counts, keyed by event name.
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

    /// How long the run has been going, in seconds. `None` before it starts.
    fn run_duration(&self) -> Option<f64> {
        self.get().run_duration().map(|d| d.as_secs_f64())
    }

    /// `finished / (finished + failed)`, or `None` when nothing resolved.
    fn tickets_success_rate(&self) -> Option<f64> {
        self.get().tickets_success_rate()
    }

    /// Total seconds tickets spent between creation and a terminal status.
    fn ticket_duration(&self) -> f64 {
        self.get().ticket_duration().as_secs_f64()
    }

    fn avg_ticket_duration(&self) -> Option<f64> {
        self.get().avg_ticket_duration().map(|d| d.as_secs_f64())
    }

    /// Total seconds tickets spent between being claimed and a terminal status.
    fn work_duration(&self) -> f64 {
        self.get().work_duration().as_secs_f64()
    }

    fn avg_work_duration(&self) -> Option<f64> {
        self.get().avg_work_duration().map(|d| d.as_secs_f64())
    }

    /// The same numbers as one dict, matching the on-disk `stats.json`.
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

/// Call and failure tallies for one tool.
#[pyclass(name = "ToolStat")]
pub struct PyToolStat {
    inner: ToolStat,
}

#[pymethods]
impl PyToolStat {
    /// Every attempt, including calls naming a tool that is not registered.
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

    /// The three failure counts added together.
    fn errors(&self) -> u64 {
        self.inner.errors()
    }

    /// Failures over calls, or `None` when the tool was never called.
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

/// Open and failure counts for one path.
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

    fn __repr__(&self) -> String {
        format!(
            "FileStat(opens={}, failed={})",
            self.inner.opens, self.inner.failed
        )
    }
}

/// Knowledge-store operation counts.
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

    /// Reads that found no page.
    #[getter]
    fn misses(&self) -> u64 {
        self.inner.misses
    }

    fn __repr__(&self) -> String {
        format!(
            "KnowledgeStat(writes={}, reads={})",
            self.inner.writes, self.inner.reads
        )
    }
}

/// Request and token totals for one model.
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

    #[getter]
    fn input_tokens(&self) -> u64 {
        self.inner.input_tokens
    }

    #[getter]
    fn output_tokens(&self) -> u64 {
        self.inner.output_tokens
    }

    fn __repr__(&self) -> String {
        format!(
            "ModelStat(requests={}, input_tokens={}, output_tokens={})",
            self.inner.requests, self.inner.input_tokens, self.inner.output_tokens
        )
    }
}
