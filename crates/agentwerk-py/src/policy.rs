//! What a run may spend, how it retries, and when it compacts, as Python sees it.

use std::time::Duration;

use agentwerk::Policy;
use pyo3::prelude::*;

/// A `Policy` limits the turns, tokens, and time a run may spend, and allows
/// configuring retries and compaction.
#[pyclass(name = "Policy")]
pub struct PyPolicy {
    pub inner: Policy,
}

#[pymethods]
impl PyPolicy {
    /// Create a configuration, taking the built-in default for every field
    /// left out. `max_time` and `request_retry_delay` are in seconds.
    #[new]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        *,
        max_turns = None,
        max_input_tokens = None,
        max_output_tokens = None,
        max_request_tokens = None,
        max_schema_retries = None,
        max_request_retries = None,
        request_retry_delay = None,
        max_time = None,
        compaction_threshold = None,
    ))]
    fn new(
        max_turns: Option<u32>,
        max_input_tokens: Option<u64>,
        max_output_tokens: Option<u64>,
        max_request_tokens: Option<u32>,
        max_schema_retries: Option<u32>,
        max_request_retries: Option<u32>,
        request_retry_delay: Option<f64>,
        max_time: Option<f64>,
        compaction_threshold: Option<f64>,
    ) -> Self {
        let defaults = Policy::default();
        PyPolicy {
            inner: Policy {
                max_turns,
                max_input_tokens,
                max_output_tokens,
                max_request_tokens,
                max_schema_retries: max_schema_retries.or(defaults.max_schema_retries),
                max_request_retries: max_request_retries.unwrap_or(defaults.max_request_retries),
                request_retry_delay: request_retry_delay
                    .map(Duration::from_secs_f64)
                    .unwrap_or(defaults.request_retry_delay),
                max_time: max_time.map(Duration::from_secs_f64),
                compaction_threshold,
            },
        }
    }

    /// Get the turn limit, or `None` when there is none.
    #[getter]
    fn max_turns(&self) -> Option<u32> {
        self.inner.max_turns
    }

    /// Get the input-token limit, or `None` when there is none.
    #[getter]
    fn max_input_tokens(&self) -> Option<u64> {
        self.inner.max_input_tokens
    }

    /// Get the output-token limit, or `None` when there is none.
    #[getter]
    fn max_output_tokens(&self) -> Option<u64> {
        self.inner.max_output_tokens
    }

    /// Get the per-request output-token limit, or `None` when there is none.
    #[getter]
    fn max_request_tokens(&self) -> Option<u32> {
        self.inner.max_request_tokens
    }

    /// Get the schema-retry limit, 10 until it is changed.
    #[getter]
    fn max_schema_retries(&self) -> Option<u32> {
        self.inner.max_schema_retries
    }

    /// Get the request-retry limit, 10 until it is changed.
    #[getter]
    fn max_request_retries(&self) -> u32 {
        self.inner.max_request_retries
    }

    /// Get the delay between retries, in seconds.
    #[getter]
    fn request_retry_delay(&self) -> f64 {
        self.inner.request_retry_delay.as_secs_f64()
    }

    /// Get the elapsed-duration limit in seconds, or `None` when there is none.
    #[getter]
    fn max_time(&self) -> Option<f64> {
        self.inner.max_time.map(|d| d.as_secs_f64())
    }

    /// Get how full the context window may get before compaction fires, or
    /// `None` when the built-in default applies.
    #[getter]
    fn compaction_threshold(&self) -> Option<f64> {
        self.inner.compaction_threshold
    }
}
