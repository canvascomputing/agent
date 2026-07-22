//! The ticket system as Python sees it. M1 exposes the read side a caller needs
//! after `finish()`: results and run outcome. M3 grows the config, lifecycle,
//! and query surface on this same handle.

use std::sync::Arc;

use agentwerk::TicketSystem;
use pyo3::prelude::*;

use crate::convert::value_to_py;

/// Wraps the shared `Arc<TicketSystem>` returned by a finished run.
#[pyclass(name = "TicketSystem")]
pub struct PyTicketSystem {
    pub inner: Arc<TicketSystem>,
}

#[pymethods]
impl PyTicketSystem {
    /// The most recent finished ticket's result, or `None` if nothing finished.
    fn last_result<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.inner.last_result() {
            Some(value) => Ok(Some(value_to_py(py, &value)?)),
            None => Ok(None),
        }
    }

    /// Every finished ticket's result, in creation order.
    fn results<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyAny>>> {
        self.inner
            .results()
            .iter()
            .map(|value| value_to_py(py, value))
            .collect()
    }

    /// Finished results scoped to one label.
    fn results_for_label<'py>(
        &self,
        py: Python<'py>,
        label: &str,
    ) -> PyResult<Vec<Bound<'py, PyAny>>> {
        self.inner
            .results_for_label(label)
            .iter()
            .map(|value| value_to_py(py, value))
            .collect()
    }

    /// How the run ended (`"Drained"`, `"Cancelled"`, `"PolicyViolated(..)"`),
    /// or `None` if it has not finished.
    fn finish_reason(&self) -> Option<String> {
        self.inner.finish_reason().map(|reason| format!("{reason:?}"))
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// Request cooperative cancellation. Synchronous; safe to call at any time.
    fn cancel(&self) {
        self.inner.cancel();
    }
}
