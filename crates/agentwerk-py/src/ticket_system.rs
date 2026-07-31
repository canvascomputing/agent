//! The ticket system as Python sees it: register agents, enqueue work, set
//! policies, install callbacks, drive the run, and read results. The Rust
//! surface is `&self -> &Self` on a shared `Arc`, so these methods take
//! `PyRef<Self>` and return it for chaining.

use std::sync::Arc;
use std::time::Duration;

use agentwerk::agents::tickets::Reply;
use agentwerk::event::Event;
use agentwerk::{Ticket, TicketSystem};
use pyo3::prelude::*;
use serde_json::Value;

use crate::agent::PyAgent;
use crate::convert::{py_to_value, runtime_error, value_to_py};
use crate::event::to_py_event;
use crate::reply::{py_to_replies, replies_to_py};
use crate::schema::PySchema;
use crate::stats::PyStats;
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
    fn agent<'py>(slf: PyRef<'py, Self>, agent: PyRef<'_, PyAgent>) -> PyResult<PyRef<'py, Self>> {
        slf.inner.agent(agent.built()?.clone());
        Ok(slf)
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

    /// Attach a result and finish a ticket from outside the run. Raises when
    /// the key is unknown or the result misses the ticket's schema.
    fn set_finished(&self, key: &str, result: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner
            .set_finished(key, py_to_value(result)?)
            .map_err(runtime_error)
    }

    /// Fail a ticket from outside the run. Raises when the key is unknown.
    fn set_failed(&self, key: &str) -> PyResult<()> {
        self.inner.set_failed(key).map_err(runtime_error)
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

    /// Turn cap in force, or `None` when the run is unlimited.
    fn get_max_turns(&self) -> Option<u32> {
        self.inner.get_max_turns()
    }

    /// Total input-token cap in force, or `None`.
    fn get_max_input_tokens(&self) -> Option<u64> {
        self.inner.get_max_input_tokens()
    }

    /// Total output-token cap in force, or `None`.
    fn get_max_output_tokens(&self) -> Option<u64> {
        self.inner.get_max_output_tokens()
    }

    /// Per-request output-token cap in force, or `None`.
    fn get_max_request_tokens(&self) -> Option<u32> {
        self.inner.get_max_request_tokens()
    }

    /// Schema-retry cap in force, 10 until it is overridden.
    fn get_max_schema_retries(&self) -> Option<u32> {
        self.inner.get_max_schema_retries()
    }

    /// Request-retry cap in force, 10 until it is overridden.
    fn get_max_request_retries(&self) -> u32 {
        self.inner.get_max_request_retries()
    }

    /// Total elapsed-time cap in force, in seconds, or `None`.
    fn get_max_time(&self) -> Option<f64> {
        self.inner.get_max_time().map(|d| d.as_secs_f64())
    }

    /// Delay between request retries in force, in seconds.
    fn get_request_retry_delay(&self) -> f64 {
        self.inner.get_request_retry_delay().as_secs_f64()
    }

    /// Directory the session's logs and state are written to.
    fn dir<'py>(slf: PyRef<'py, Self>, dir: &str) -> PyRef<'py, Self> {
        slf.inner.dir(dir);
        slf
    }

    /// Directory the system writes to, `./.agentwerk` until `dir` overrides it.
    fn get_dir(&self) -> String {
        self.inner.get_dir().display().to_string()
    }

    /// Register a default result schema for every ticket carrying `label`.
    fn schema_for_label<'py>(
        slf: PyRef<'py, Self>,
        label: &str,
        schema: PyRef<'_, PySchema>,
    ) -> PyRef<'py, Self> {
        slf.inner.schema_for_label(label, schema.inner.clone());
        slf
    }

    /// Install an event handler. Replaces the default stderr logger.
    fn on_event<'py>(slf: PyRef<'py, Self>, callback: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_event(move |event: &Event| {
            Python::attach(|py| {
                let handled = callback.bind(py).call1((to_py_event(event),));
                if let Err(err) = handled {
                    err.print(py);
                }
            });
        });
        slf
    }

    /// Call `callback(ticket, result)` for every finished ticket, with the
    /// result already validated against the ticket's schema.
    fn on_result<'py>(slf: PyRef<'py, Self>, callback: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_result(move |ticket: &Ticket, result: &Value| {
            Python::attach(|py| {
                if let Err(err) = call_with_result(py, &callback, ticket, result) {
                    err.print(py);
                }
            });
        });
        slf
    }

    /// Call `callback(event, ticket)` for every failure: a failed ticket, a
    /// failed tool call, a failed request, a file that would not open, or
    /// compaction that could not finish. Read `event.kind` to tell them apart.
    fn on_failure<'py>(slf: PyRef<'py, Self>, callback: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_failure(move |event: &Event, ticket: &Ticket| {
            Python::attach(|py| {
                if let Err(err) = call_with_ticket(py, &callback, event, ticket) {
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

    /// Cancel the run when `predicate(ticket, result)` first returns truthy
    /// for a finished ticket.
    fn cancel_on_result<'py>(slf: PyRef<'py, Self>, predicate: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .cancel_on_result(move |ticket: &Ticket, result: &Value| {
                Python::attach(|py| {
                    call_with_result(py, &predicate, ticket, result)
                        .and_then(|value| value.is_truthy())
                        .unwrap_or(false)
                })
            });
        slf
    }

    /// Cancel the run when `predicate(event, ticket)` first returns truthy
    /// for a failure.
    fn cancel_on_failure<'py>(slf: PyRef<'py, Self>, predicate: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .cancel_on_failure(move |event: &Event, ticket: &Ticket| {
                Python::attach(|py| {
                    call_with_ticket(py, &predicate, event, ticket)
                        .and_then(|value| value.is_truthy())
                        .unwrap_or(false)
                })
            });
        slf
    }

    /// Cancel the run when `awaitable` resolves. Its result is discarded;
    /// only completion matters.
    fn cancel_on<'py>(slf: PyRef<'py, Self>, awaitable: Py<PyAny>) -> PyResult<PyRef<'py, Self>> {
        let future = Python::attach(|py| {
            pyo3_async_runtimes::tokio::into_future(awaitable.bind(py).clone())
        })?;
        // `TicketSystem::cancel_on` spawns onto the ambient Tokio runtime; a
        // pymethod call has no runtime entered on its own thread, so enter
        // the shared one pyo3-async-runtimes already uses for `finish()`.
        let _guard = pyo3_async_runtimes::tokio::get_runtime().enter();
        slf.inner.cancel_on(future);
        Ok(slf)
    }

    /// After every event, call `make(event)`; a returned `Ticket` is
    /// enqueued, `None` enqueues nothing.
    fn create_ticket_on_event<'py>(slf: PyRef<'py, Self>, make: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.create_ticket_on_event(move |event: &Event| {
            Python::attach(|py| {
                let produced = make.bind(py).call1((to_py_event(event),)).ok()?;
                built_ticket(&produced)
            })
        });
        slf
    }

    /// After each finished ticket, call `make(ticket, result)`; a returned
    /// `Ticket` is enqueued, `None` enqueues nothing.
    fn create_ticket_on_result<'py>(slf: PyRef<'py, Self>, make: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .create_ticket_on_result(move |ticket: &Ticket, result: &Value| {
                Python::attach(|py| {
                    let produced = call_with_result(py, &make, ticket, result).ok()?;
                    built_ticket(&produced)
                })
            });
        slf
    }

    /// After each failure, call `make(event, ticket)`; a returned `Ticket` is
    /// enqueued, `None` enqueues nothing. The retry path: count the attempts
    /// yourself, or a ticket that always fails re-queues itself forever.
    fn create_ticket_on_failure<'py>(slf: PyRef<'py, Self>, make: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .create_ticket_on_failure(move |event: &Event, ticket: &Ticket| {
                Python::attach(|py| {
                    let produced = call_with_ticket(py, &make, event, ticket).ok()?;
                    built_ticket(&produced)
                })
            });
        slf
    }

    /// Cancel every ticket carrying `label`.
    fn cancel_label<'py>(slf: PyRef<'py, Self>, label: &str) -> PyRef<'py, Self> {
        slf.inner.cancel_label(label);
        slf
    }

    /// True when `label` names a pool called off via `cancel_label`. Ask before
    /// minting follow-up work: a ticket carrying a cancelled label is never
    /// claimed.
    fn label_cancelled(&self, label: &str) -> bool {
        self.inner.label_cancelled(label)
    }

    /// Call off `label`'s agents when `predicate(event)` first returns
    /// truthy. Only that pool stops; other labels keep going.
    fn cancel_label_on_event<'py>(
        slf: PyRef<'py, Self>,
        label: &str,
        predicate: Py<PyAny>,
    ) -> PyRef<'py, Self> {
        slf.inner
            .cancel_label_on_event(label, move |event: &Event| {
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

    /// Call off `label`'s agents when a finished ticket makes
    /// `predicate(ticket, result)` truthy. Only that pool stops; other labels
    /// keep going.
    fn cancel_label_on_result<'py>(
        slf: PyRef<'py, Self>,
        label: &str,
        predicate: Py<PyAny>,
    ) -> PyRef<'py, Self> {
        slf.inner
            .cancel_label_on_result(label, move |ticket: &Ticket, result: &Value| {
                Python::attach(|py| {
                    call_with_result(py, &predicate, ticket, result)
                        .and_then(|value| value.is_truthy())
                        .unwrap_or(false)
                })
            });
        slf
    }

    /// Call off `label`'s agents when a failure makes `predicate(event,
    /// ticket)` truthy. Only that pool stops; other labels keep going.
    fn cancel_label_on_failure<'py>(
        slf: PyRef<'py, Self>,
        label: &str,
        predicate: Py<PyAny>,
    ) -> PyRef<'py, Self> {
        slf.inner
            .cancel_label_on_failure(label, move |event: &Event, ticket: &Ticket| {
                Python::attach(|py| {
                    call_with_ticket(py, &predicate, event, ticket)
                        .and_then(|value| value.is_truthy())
                        .unwrap_or(false)
                })
            });
        slf
    }

    /// Name of the model the bound agent named `agent_name` runs, or `None`.
    /// `Trajectory.from_ticket` wants it.
    fn model_for_agent(&self, agent_name: &str) -> Option<String> {
        self.inner.model_for_agent(agent_name)
    }

    /// The ticket with `key`, or `None`.
    fn get_ticket(&self, py: Python<'_>, key: &str) -> PyResult<Option<Py<PyTicket>>> {
        match self.inner.get_ticket(key) {
            Some(ticket) => Ok(Some(Py::new(py, PyTicket::from_ticket(&ticket))?)),
            None => Ok(None),
        }
    }

    /// Every ticket, in creation order.
    fn tickets(&self, py: Python<'_>) -> PyResult<Vec<Py<PyTicket>>> {
        self.inner
            .tickets()
            .iter()
            .map(|ticket| Py::new(py, PyTicket::from_ticket(ticket)))
            .collect()
    }

    /// Every ticket carrying `label`, in creation order, whatever its status.
    fn tickets_for_label(&self, py: Python<'_>, label: &str) -> PyResult<Vec<Py<PyTicket>>> {
        self.inner
            .tickets_for_label(label)
            .iter()
            .map(|ticket| Py::new(py, PyTicket::from_ticket(ticket)))
            .collect()
    }

    /// Every ticket for which `predicate(ticket)` is truthy.
    fn find_tickets(&self, py: Python<'_>, predicate: Py<PyAny>) -> PyResult<Vec<Py<PyTicket>>> {
        self.inner
            .find_tickets(|ticket| ticket_predicate(&predicate, ticket))
            .iter()
            .map(|ticket| Py::new(py, PyTicket::from_ticket(ticket)))
            .collect()
    }

    /// The first ticket for which `predicate(ticket)` is truthy, or `None`.
    fn find_ticket(&self, py: Python<'_>, predicate: Py<PyAny>) -> PyResult<Option<Py<PyTicket>>> {
        match self
            .inner
            .find_ticket(|ticket| ticket_predicate(&predicate, ticket))
        {
            Some(ticket) => Ok(Some(Py::new(py, PyTicket::from_ticket(&ticket))?)),
            None => Ok(None),
        }
    }

    /// Await the first ticket for which `predicate(ticket)` is truthy. Resolves
    /// to the ticket, or `None` if the run ends first.
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
                Some(ticket) => Ok(Some(Py::new(py, PyTicket::from_ticket(&ticket))?)),
                None => Ok::<_, PyErr>(None),
            })
        })
    }

    /// Call `callback(event, ticket)` when a ticket starts, finishes, or
    /// fails. The ticket arrives with its messages, so a handler can hand it
    /// straight to `Trajectory.from_ticket`.
    fn on_ticket<'py>(slf: PyRef<'py, Self>, callback: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_ticket(move |event: &Event, ticket: &Ticket| {
            Python::attach(|py| {
                let handled = Py::new(py, PyTicket::from_ticket(ticket))
                    .and_then(|view| callback.bind(py).call1((to_py_event(event), view)));
                if let Err(err) = handled {
                    err.print(py);
                }
            });
        });
        slf
    }

    /// Rewrite or drop a ticket's replies before its next request.
    /// `editor(events, replies)` receives the events since the ticket's
    /// previous request and the current `Reply` list, and returns the new
    /// list (or `None` to leave it unchanged). The editor must keep
    /// tool_use/tool_result pairs matched. The edit persists across
    /// resumption. An editor that raises prints its traceback and leaves the
    /// replies untouched: this runs on an agent's worker thread, with no
    /// Python frame to raise into.
    fn edit_replies_on_event<'py>(slf: PyRef<'py, Self>, editor: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .edit_replies_on_event(move |events: &[Event], replies: &mut Vec<Reply>| {
                Python::attach(|py| {
                    let outcome = (|| -> PyResult<Option<Vec<Reply>>> {
                        let py_events: Vec<_> = events.iter().map(to_py_event).collect();
                        let returned =
                            editor.bind(py).call1((py_events, replies_to_py(replies)))?;
                        if returned.is_none() {
                            return Ok(None);
                        }
                        Ok(Some(py_to_replies(&returned)?))
                    })();
                    match outcome {
                        Ok(Some(edited)) => *replies = edited,
                        Ok(None) => {}
                        Err(err) => err.print(py),
                    }
                });
            });
        slf
    }

    /// Rewrite or drop a ticket's replies now, without triggering a
    /// request. `editor(replies)` receives the current `Reply` list and
    /// returns the new one (or `None` to leave it unchanged). Persists the
    /// edit in place; a missing ticket is a no-op. An editor that raises,
    /// or returns something other than a list of `Reply`, raises here: this
    /// call has a Python frame to unwind into, so it does not guess.
    fn edit_replies<'py>(
        slf: PyRef<'py, Self>,
        key: &str,
        editor: Py<PyAny>,
    ) -> PyResult<PyRef<'py, Self>> {
        let mut failure: Option<PyErr> = None;
        slf.inner.edit_replies(key, |replies: &mut Vec<Reply>| {
            Python::attach(|py| {
                let outcome = (|| -> PyResult<Option<Vec<Reply>>> {
                    let returned = editor.bind(py).call1((replies_to_py(replies),))?;
                    if returned.is_none() {
                        return Ok(None);
                    }
                    Ok(Some(py_to_replies(&returned)?))
                })();
                match outcome {
                    Ok(Some(edited)) => *replies = edited,
                    Ok(None) => {}
                    Err(err) => failure = Some(err),
                }
            });
        });
        match failure {
            Some(err) => Err(err),
            None => Ok(slf),
        }
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

    /// How the run ended (`"drained"`, `"cancelled"`, `"policy_violated(..)"`),
    /// or `None` if it has not finished.
    fn finish_reason(&self) -> Option<String> {
        self.inner.finish_reason().map(|reason| reason.to_string())
    }

    /// Run statistics: requests, tokens, ticket counts, and the per-tool,
    /// per-file, per-label, and per-model breakdowns.
    fn stats(&self) -> PyStats {
        PyStats::for_run(Arc::clone(&self.inner))
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

/// Call a Python callable with the `(ticket, result)` pair every `_on_result`
/// hook hands over.
fn call_with_result<'py>(
    py: Python<'py>,
    callable: &Py<PyAny>,
    ticket: &Ticket,
    result: &Value,
) -> PyResult<Bound<'py, PyAny>> {
    let view = Py::new(py, PyTicket::from_ticket(ticket))?;
    let value = value_to_py(py, result)?;
    callable.bind(py).call1((view, value))
}

/// Call a Python callable with the `(event, ticket)` pair every `_on_failure`
/// hook hands over.
fn call_with_ticket<'py>(
    py: Python<'py>,
    callable: &Py<PyAny>,
    event: &Event,
    ticket: &Ticket,
) -> PyResult<Bound<'py, PyAny>> {
    let view = Py::new(py, PyTicket::from_ticket(ticket))?;
    callable.bind(py).call1((to_py_event(event), view))
}

/// Read a ticket back out of what a `create_ticket_*` callable returned.
/// `None`, or anything that is not a `Ticket`, enqueues nothing.
fn built_ticket(produced: &Bound<'_, PyAny>) -> Option<Ticket> {
    if produced.is_none() {
        return None;
    }
    Some(produced.extract::<PyRef<PyTicket>>().ok()?.to_ticket())
}

/// Call a Python predicate with a ticket, reading back its truthiness. A
/// conversion or Python error reads as `false` so a bad predicate never panics
/// a worker thread.
fn ticket_predicate(predicate: &Py<PyAny>, ticket: &Ticket) -> bool {
    Python::attach(|py| {
        Py::new(py, PyTicket::from_ticket(ticket))
            .and_then(|view| predicate.bind(py).call1((view,)))
            .and_then(|value| value.is_truthy())
            .unwrap_or(false)
    })
}
