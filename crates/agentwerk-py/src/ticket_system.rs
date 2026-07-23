//! The ticket system as Python sees it: register agents, enqueue work, set
//! policies, install callbacks, drive the run, and read results. The Rust
//! surface is `&self -> &Self` on a shared `Arc`, so these methods take
//! `PyRef<Self>` and return it for chaining.

use std::sync::Arc;
use std::time::Duration;

use agentwerk::event::Event;
use agentwerk::{Ticket, TicketSystem};
use pyo3::prelude::*;
use serde_json::Value;

use crate::agent::PyAgent;
use crate::convert::{py_to_value, runtime_error, value_to_py};
use crate::event::to_py_event;
use crate::schema::PySchema;
use crate::ticket::PyTicket;

/// A shared queue one or more agents work. Wraps `Arc<TicketSystem>`.
#[pyclass(name = "TicketSystem")]
pub struct PyTicketSystem {
    pub inner: Arc<TicketSystem>,
}

#[pymethods]
impl PyTicketSystem {
    #[new]
    fn new() -> Self {
        PyTicketSystem {
            inner: TicketSystem::new(),
        }
    }

    /// Reopen a session directory written by a prior run.
    #[staticmethod]
    fn load(dir: &str) -> PyResult<Self> {
        let inner = TicketSystem::load(dir).map_err(runtime_error)?;
        Ok(PyTicketSystem { inner })
    }

    /// Register an agent. Drains any work it had queued privately into this
    /// system and adds it to the dispatch set.
    fn agent<'py>(slf: PyRef<'py, Self>, agent: PyRef<'_, PyAgent>) -> PyRef<'py, Self> {
        slf.inner.agent(agent.inner.clone());
        slf
    }

    /// Enqueue a task (any JSON-serializable value) and return its ticket key.
    fn task(slf: PyRef<'_, Self>, task: &Bound<'_, PyAny>) -> PyResult<String> {
        Ok(slf.inner.task(py_to_value(task)?))
    }

    /// Enqueue a fully-built ticket and return its key.
    fn ticket(slf: PyRef<'_, Self>, ticket: PyRef<'_, PyTicket>) -> String {
        slf.inner.ticket(ticket.to_ticket())
    }

    /// Append a reply to a paused ticket, driving its next turn.
    fn reply<'py>(slf: PyRef<'py, Self>, key: &str, content: &str) -> PyRef<'py, Self> {
        slf.inner.reply(key, content);
        slf
    }

    fn max_turns(slf: PyRef<'_, Self>, n: u32) -> PyRef<'_, Self> {
        slf.inner.max_turns(n);
        slf
    }

    fn max_input_tokens(slf: PyRef<'_, Self>, n: u64) -> PyRef<'_, Self> {
        slf.inner.max_input_tokens(n);
        slf
    }

    fn max_output_tokens(slf: PyRef<'_, Self>, n: u64) -> PyRef<'_, Self> {
        slf.inner.max_output_tokens(n);
        slf
    }

    fn max_request_tokens(slf: PyRef<'_, Self>, n: u32) -> PyRef<'_, Self> {
        slf.inner.max_request_tokens(n);
        slf
    }

    fn max_schema_retries(slf: PyRef<'_, Self>, n: u32) -> PyRef<'_, Self> {
        slf.inner.max_schema_retries(n);
        slf
    }

    fn max_request_retries(slf: PyRef<'_, Self>, n: u32) -> PyRef<'_, Self> {
        slf.inner.max_request_retries(n);
        slf
    }

    /// Total elapsed-time cap, in seconds.
    fn max_time(slf: PyRef<'_, Self>, seconds: f64) -> PyRef<'_, Self> {
        slf.inner.max_time(Duration::from_secs_f64(seconds));
        slf
    }

    /// Delay between request retries, in seconds.
    fn request_retry_delay(slf: PyRef<'_, Self>, seconds: f64) -> PyRef<'_, Self> {
        slf.inner
            .request_retry_delay(Duration::from_secs_f64(seconds));
        slf
    }

    /// Directory the session's logs and state are written to.
    fn dir<'py>(slf: PyRef<'py, Self>, dir: &str) -> PyRef<'py, Self> {
        slf.inner.dir(dir);
        slf
    }

    /// Register a default result schema for every ticket carrying `label`.
    fn schema_for_label<'py>(
        slf: PyRef<'py, Self>,
        label: &str,
        schema: PyRef<'_, PySchema>,
    ) -> PyResult<PyRef<'py, Self>> {
        let parsed = agentwerk::Schema::parse(schema.source.clone()).map_err(runtime_error)?;
        slf.inner.schema_for_label(label, parsed);
        Ok(slf)
    }

    /// Install an event handler. Replaces the default stderr logger.
    fn on_event<'py>(slf: PyRef<'py, Self>, callback: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_event(move |event: Event| {
            Python::attach(|py| {
                let handled = callback.bind(py).call1((to_py_event(&event),));
                if let Err(err) = handled {
                    err.print(py);
                }
            });
        });
        slf
    }

    /// Cancel the run when `predicate(event)` first returns truthy.
    fn cancel_on_event<'py>(slf: PyRef<'py, Self>, predicate: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.cancel_on_event(move |event: &Event| {
            Python::attach(|py| {
                predicate
                    .bind(py)
                    .call1((to_py_event(event),))
                    .and_then(|value| value.is_truthy())
                    .unwrap_or(false)
            })
        });
        slf
    }

    /// Cancel the run when `predicate(result)` first returns truthy.
    fn cancel_on_result<'py>(slf: PyRef<'py, Self>, predicate: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.cancel_on_result(move |result: &Value| {
            Python::attach(|py| {
                value_to_py(py, result)
                    .and_then(|value| predicate.bind(py).call1((value,)))
                    .and_then(|value| value.is_truthy())
                    .unwrap_or(false)
            })
        });
        slf
    }

    /// After each finished ticket, call `make(ticket)`; a returned `Ticket`
    /// is enqueued, `None` enqueues nothing.
    fn create_ticket_on_result<'py>(slf: PyRef<'py, Self>, make: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.create_ticket_on_result(move |ticket: &Ticket| {
            Python::attach(|py| {
                let view = value_to_py(py, &ticket_to_value(ticket)).ok()?;
                let produced = make.bind(py).call1((view,)).ok()?;
                if produced.is_none() {
                    return None;
                }
                let built = produced.extract::<PyRef<PyTicket>>().ok()?;
                Some(built.to_ticket())
            })
        });
        slf
    }

    /// Cancel every ticket carrying `label`.
    fn cancel_label<'py>(slf: PyRef<'py, Self>, label: &str) -> PyRef<'py, Self> {
        slf.inner.cancel_label(label);
        slf
    }

    /// The ticket with `key`, as a dict, or `None`.
    fn get_ticket<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.inner.get_ticket(key) {
            Some(ticket) => Ok(Some(value_to_py(py, &ticket_to_value(&ticket))?)),
            None => Ok(None),
        }
    }

    /// Every ticket, as dicts.
    fn tickets<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyAny>>> {
        self.inner
            .tickets()
            .iter()
            .map(|ticket| value_to_py(py, &ticket_to_value(ticket)))
            .collect()
    }

    /// Every ticket for which `predicate(ticket)` is truthy, as dicts.
    fn find_tickets<'py>(
        &self,
        py: Python<'py>,
        predicate: Py<PyAny>,
    ) -> PyResult<Vec<Bound<'py, PyAny>>> {
        self.inner
            .find_tickets(|ticket| ticket_predicate(&predicate, ticket))
            .iter()
            .map(|ticket| value_to_py(py, &ticket_to_value(ticket)))
            .collect()
    }

    /// The first ticket for which `predicate(ticket)` is truthy, or `None`.
    fn find_ticket<'py>(
        &self,
        py: Python<'py>,
        predicate: Py<PyAny>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self
            .inner
            .find_ticket(|ticket| ticket_predicate(&predicate, ticket))
        {
            Some(ticket) => Ok(Some(value_to_py(py, &ticket_to_value(&ticket))?)),
            None => Ok(None),
        }
    }

    /// Await the first ticket for which `predicate(ticket)` is truthy. Resolves
    /// to the ticket dict, or `None` if the run ends first.
    fn wait_for_ticket<'py>(
        &self,
        py: Python<'py>,
        predicate: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let found = inner
                .wait_for_ticket(|ticket| ticket_predicate(&predicate, ticket))
                .await;
            Python::attach(|py| match found {
                Some(ticket) => Ok(Some(value_to_py(py, &ticket_to_value(&ticket))?.unbind())),
                None => Ok::<_, PyErr>(None),
            })
        })
    }

    /// Write a ticket's trajectory to disk when `predicate(event)` is truthy.
    fn save_trajectory_on_event<'py>(
        slf: PyRef<'py, Self>,
        predicate: Py<PyAny>,
    ) -> PyRef<'py, Self> {
        slf.inner.save_trajectory_on_event(move |event: &Event| {
            Python::attach(|py| {
                predicate
                    .bind(py)
                    .call1((to_py_event(event),))
                    .and_then(|value| value.is_truthy())
                    .unwrap_or(false)
            })
        });
        slf
    }

    fn start<'py>(slf: PyRef<'py, Self>) -> PyRef<'py, Self> {
        slf.inner.start();
        slf
    }

    /// Run every queued ticket to completion. Awaitable.
    fn finish<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            inner.finish().await;
            Ok::<_, PyErr>(PyTicketSystem { inner })
        })
    }

    fn cancel(&self) {
        self.inner.cancel();
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// How the run ended (`"Drained"`, `"Cancelled"`, `"PolicyViolated(..)"`),
    /// or `None` if it has not finished.
    fn finish_reason(&self) -> Option<String> {
        self.inner
            .finish_reason()
            .map(|reason| format!("{reason:?}"))
    }

    /// The most recent finished ticket's result, or `None`.
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
}

/// Serialize a ticket to JSON for the Python side. `replies` is `#[serde(skip)]`,
/// so this carries only the observable state (key, status, task, result, ...).
fn ticket_to_value(ticket: &Ticket) -> Value {
    serde_json::to_value(ticket).unwrap_or(Value::Null)
}

/// Call a Python predicate with a ticket dict, reading back its truthiness. A
/// conversion or Python error reads as `false` so a bad predicate never panics
/// a worker thread.
fn ticket_predicate(predicate: &Py<PyAny>, ticket: &Ticket) -> bool {
    Python::attach(|py| {
        value_to_py(py, &ticket_to_value(ticket))
            .and_then(|view| predicate.bind(py).call1((view,)))
            .and_then(|value| value.is_truthy())
            .unwrap_or(false)
    })
}
