//! The ticket queue as Python sees it: add agents, submit work, set limits,
//! install handlers, drive execution, and read results.

use std::sync::Arc;
use std::time::Duration;

use agentwerk::agents::tickets::Reply;
use agentwerk::event::Event;
use agentwerk::{Ticket, TicketQueue};
use pyo3::prelude::*;
use serde_json::Value;

use crate::agent::PyAgent;
use crate::compaction::invoke_editor;
use crate::convert::{py_to_value, runtime_error, value_to_py};
use crate::event::to_py_event;
use crate::reply::{py_to_replies, replies_to_py};
use crate::schema::PySchema;
use crate::stats::PyStats;
use crate::ticket::PyTicket;

/// The core data structure of agentwerk, coordinating complex work across
/// agents.
#[pyclass(name = "TicketQueue")]
pub struct PyTicketQueue {
    pub inner: Arc<TicketQueue>,
}

#[pymethods]
impl PyTicketQueue {
    #[new]
    fn new() -> Self {
        PyTicketQueue {
            inner: TicketQueue::new(),
        }
    }

    /// Continue a session from a directory written earlier.
    #[staticmethod]
    fn load(dir: &str) -> PyResult<Self> {
        let inner = TicketQueue::load(dir).map_err(runtime_error)?;
        Ok(PyTicketQueue { inner })
    }

    /// Add an agent to this ticket queue, moving any tickets it queued on its
    /// own across first.
    fn agent<'py>(slf: PyRef<'py, Self>, agent: PyRef<'_, PyAgent>) -> PyResult<PyRef<'py, Self>> {
        slf.inner.agent(agent.built()?.clone());
        Ok(slf)
    }

    /// Submit a task and return its ticket key.
    fn task(slf: PyRef<'_, Self>, task: &Bound<'_, PyAny>) -> PyResult<String> {
        Ok(slf.inner.task(py_to_value(task)?))
    }

    /// Submit a `Ticket` with custom labels or schema, and return its key.
    fn ticket(slf: PyRef<'_, Self>, ticket: PyRef<'_, PyTicket>) -> String {
        slf.inner.ticket(ticket.to_ticket())
    }

    /// Add a reply to a ticket, which drives its next turn.
    fn reply<'py>(slf: PyRef<'py, Self>, key: &str, content: &str) -> PyRef<'py, Self> {
        slf.inner.reply(key, content);
        slf
    }

    /// Finish a ticket with a result, from outside the execution.
    ///
    /// Raises when the key is unknown, or when the result misses the ticket's
    /// schema.
    fn set_finished(&self, key: &str, result: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner
            .set_finished(key, py_to_value(result)?)
            .map_err(runtime_error)
    }

    /// Fail a ticket, from outside the execution. Raises when the key is unknown.
    fn set_failed(&self, key: &str) -> PyResult<()> {
        self.inner.set_failed(key).map_err(runtime_error)
    }

    /// Limit the total number of turns.
    fn max_turns(slf: PyRef<'_, Self>, n: u32) -> PyRef<'_, Self> {
        slf.inner.max_turns(n);
        slf
    }

    /// Limit the total input tokens.
    fn max_input_tokens(slf: PyRef<'_, Self>, n: u64) -> PyRef<'_, Self> {
        slf.inner.max_input_tokens(n);
        slf
    }

    /// Limit the total output tokens.
    fn max_output_tokens(slf: PyRef<'_, Self>, n: u64) -> PyRef<'_, Self> {
        slf.inner.max_output_tokens(n);
        slf
    }

    /// Limit the output tokens of a single request.
    fn max_request_tokens(slf: PyRef<'_, Self>, n: u32) -> PyRef<'_, Self> {
        slf.inner.max_request_tokens(n);
        slf
    }

    /// Limit how often a result may fail its schema before the ticket fails.
    fn max_schema_retries(slf: PyRef<'_, Self>, n: u32) -> PyRef<'_, Self> {
        slf.inner.max_schema_retries(n);
        slf
    }

    /// Limit how often a failing request is retried.
    fn max_request_retries(slf: PyRef<'_, Self>, n: u32) -> PyRef<'_, Self> {
        slf.inner.max_request_retries(n);
        slf
    }

    /// Limit the total elapsed duration, in seconds.
    fn max_time(slf: PyRef<'_, Self>, seconds: f64) -> PyRef<'_, Self> {
        slf.inner.max_time(Duration::from_secs_f64(seconds));
        slf
    }

    /// Compact once the model's context window is this full.
    ///
    /// Unset, a built-in fraction applies. The value is clamped to `0.0` through
    /// `1.0`, and a fraction near zero compacts every turn. Nothing happens on a
    /// model whose window is unknown.
    fn compact_at(slf: PyRef<'_, Self>, fraction: f64) -> PyRef<'_, Self> {
        slf.inner.compact_at(fraction);
        slf
    }

    /// Wait this long between retries, in seconds.
    fn request_retry_delay(slf: PyRef<'_, Self>, seconds: f64) -> PyRef<'_, Self> {
        slf.inner
            .request_retry_delay(Duration::from_secs_f64(seconds));
        slf
    }

    /// Get the turn limit, or `None` when there is none.
    fn get_max_turns(&self) -> Option<u32> {
        self.inner.get_max_turns()
    }

    /// Get the input-token limit, or `None` when there is none.
    fn get_max_input_tokens(&self) -> Option<u64> {
        self.inner.get_max_input_tokens()
    }

    /// Get the output-token limit, or `None` when there is none.
    fn get_max_output_tokens(&self) -> Option<u64> {
        self.inner.get_max_output_tokens()
    }

    /// Get the per-request output-token limit, or `None` when there is none.
    fn get_max_request_tokens(&self) -> Option<u32> {
        self.inner.get_max_request_tokens()
    }

    /// Get the schema-retry limit, 10 until it is changed.
    fn get_max_schema_retries(&self) -> Option<u32> {
        self.inner.get_max_schema_retries()
    }

    /// Get the request-retry limit, 10 until it is changed.
    fn get_max_request_retries(&self) -> u32 {
        self.inner.get_max_request_retries()
    }

    /// Get the elapsed-duration limit in seconds, or `None` when there is none.
    fn get_max_time(&self) -> Option<f64> {
        self.inner.get_max_time().map(|d| d.as_secs_f64())
    }

    /// Get how full the context window may get before compaction fires, or
    /// `None` when the built-in default applies.
    fn get_compact_at(&self) -> Option<f64> {
        self.inner.get_compact_at()
    }

    /// Get the delay between retries, in seconds.
    fn get_request_retry_delay(&self) -> f64 {
        self.inner.get_request_retry_delay().as_secs_f64()
    }

    /// Define where a session is stored.
    fn dir<'py>(slf: PyRef<'py, Self>, dir: &str) -> PyRef<'py, Self> {
        slf.inner.dir(dir);
        slf
    }

    /// Get the session directory, `./.agentwerk` until `dir` changes it.
    fn get_dir(&self) -> String {
        self.inner.get_dir().display().to_string()
    }

    /// Register a schema every ticket of that label validates against.
    fn schema_for_label<'py>(
        slf: PyRef<'py, Self>,
        label: &str,
        schema: PyRef<'_, PySchema>,
    ) -> PyRef<'py, Self> {
        slf.inner.schema_for_label(label, schema.inner.clone());
        slf
    }

    /// Read every event as it is emitted. It replaces the handler that prints to
    /// stderr.
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

    /// Read every finished ticket together with its result, already validated
    /// against the ticket's schema.
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

    /// Read every failure together with the ticket it happened in: a failed
    /// ticket, tool call, or request, a file that would not open, or compaction
    /// that could not finish. Read `event.kind` to tell them apart.
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

    /// Stop execution when an event matches.
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

    /// Stop execution when a finished result matches.
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

    /// Stop execution when a failure matches.
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

    /// Stop execution when another task you supply finishes. Its result is
    /// discarded; only finishing matters.
    fn cancel_on<'py>(slf: PyRef<'py, Self>, awaitable: Py<PyAny>) -> PyResult<PyRef<'py, Self>> {
        let future = Python::attach(|py| {
            pyo3_async_runtimes::tokio::into_future(awaitable.bind(py).clone())
        })?;
        // `TicketQueue::cancel_on` spawns onto the ambient Tokio runtime; a
        // pymethod call has no runtime entered on its own thread, so enter
        // the shared one pyo3-async-runtimes already uses for `finish()`.
        let _guard = pyo3_async_runtimes::tokio::get_runtime().enter();
        slf.inner.cancel_on(future);
        Ok(slf)
    }

    /// Enqueue a follow-up ticket from any event. Returning `None` adds
    /// nothing.
    fn create_ticket_on_event<'py>(slf: PyRef<'py, Self>, make: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.create_ticket_on_event(move |event: &Event| {
            Python::attach(|py| {
                let produced = make.bind(py).call1((to_py_event(event),)).ok()?;
                built_ticket(&produced)
            })
        });
        slf
    }

    /// Enqueue a follow-up ticket from a finished ticket. Returning `None` adds
    /// nothing.
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

    /// Enqueue a retry for a ticket that failed. Returning `None` adds nothing.
    ///
    /// Count the attempts yourself, or a ticket that fails every time re-queues
    /// itself forever.
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

    /// Stop one label's agents while the rest keep working.
    fn cancel_label<'py>(slf: PyRef<'py, Self>, label: &str) -> PyRef<'py, Self> {
        slf.inner.cancel_label(label);
        slf
    }

    /// Check whether one label's agents have been stopped.
    ///
    /// Ask before creating follow-up work: a ticket carrying a stopped label is
    /// never claimed.
    fn is_label_cancelled(&self, label: &str) -> bool {
        self.inner.is_label_cancelled(label)
    }

    /// Stop one label's agents when an event matches, while the rest keep
    /// working.
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

    /// Stop one label's agents when a finished result matches, while the rest
    /// keep working.
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

    /// Stop one label's agents when a failure matches, while the rest keep
    /// working.
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

    /// Get the model that agent runs, or `None` when no agent of that name is
    /// added. `Trajectory.from_ticket` needs it.
    fn model_for_agent(&self, agent_name: &str) -> Option<String> {
        self.inner.model_for_agent(agent_name)
    }

    /// Get one ticket by key.
    fn get_ticket(&self, py: Python<'_>, key: &str) -> PyResult<Option<Py<PyTicket>>> {
        match self.inner.get_ticket(key) {
            Some(ticket) => Ok(Some(Py::new(py, PyTicket::from_ticket(&ticket))?)),
            None => Ok(None),
        }
    }

    /// Get every ticket in creation order.
    fn tickets(&self, py: Python<'_>) -> PyResult<Vec<Py<PyTicket>>> {
        self.inner
            .tickets()
            .iter()
            .map(|ticket| Py::new(py, PyTicket::from_ticket(ticket)))
            .collect()
    }

    /// Get every ticket carrying a label, in any status.
    fn tickets_for_label(&self, py: Python<'_>, label: &str) -> PyResult<Vec<Py<PyTicket>>> {
        self.inner
            .tickets_for_label(label)
            .iter()
            .map(|ticket| Py::new(py, PyTicket::from_ticket(ticket)))
            .collect()
    }

    /// Get every ticket claimed by an agent, in any status.
    fn tickets_for_agent(&self, py: Python<'_>, agent_name: &str) -> PyResult<Vec<Py<PyTicket>>> {
        self.inner
            .tickets_for_agent(agent_name)
            .iter()
            .map(|ticket| Py::new(py, PyTicket::from_ticket(ticket)))
            .collect()
    }

    /// Get every ticket matching a condition.
    fn find_tickets(&self, py: Python<'_>, predicate: Py<PyAny>) -> PyResult<Vec<Py<PyTicket>>> {
        self.inner
            .find_tickets(|ticket| ticket_predicate(&predicate, ticket))
            .iter()
            .map(|ticket| Py::new(py, PyTicket::from_ticket(ticket)))
            .collect()
    }

    /// Get the earliest ticket matching a condition.
    fn find_ticket(&self, py: Python<'_>, predicate: Py<PyAny>) -> PyResult<Option<Py<PyTicket>>> {
        match self
            .inner
            .find_ticket(|ticket| ticket_predicate(&predicate, ticket))
        {
            Some(ticket) => Ok(Some(Py::new(py, PyTicket::from_ticket(&ticket))?)),
            None => Ok(None),
        }
    }

    /// Get the first ticket that matches, and execution carries on. Gives back
    /// `None` when execution ends first.
    fn finish_on_ticket<'py>(
        &self,
        py: Python<'py>,
        condition: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let found = inner
                .finish_on_ticket(|ticket| ticket_predicate(&condition, ticket))
                .await;
            Python::attach(|py| match found {
                Some(ticket) => Ok(Some(Py::new(py, PyTicket::from_ticket(&ticket))?)),
                None => Ok::<_, PyErr>(None),
            })
        })
    }

    /// Get the first event that matches, and execution carries on. Gives back
    /// `None` when execution ends first.
    fn finish_on_event<'py>(
        &self,
        py: Python<'py>,
        condition: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let found = inner
                .finish_on_event(|event| event_predicate(&condition, event))
                .await;
            Ok::<_, PyErr>(found.as_ref().map(to_py_event))
        })
    }

    /// Get the first finished result that matches, and execution carries on.
    /// Gives back `None` when execution ends first.
    fn finish_on_result<'py>(
        &self,
        py: Python<'py>,
        condition: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let found = inner
                .finish_on_result(|ticket, result| {
                    Python::attach(|py| {
                        call_with_result(py, &condition, ticket, result)
                            .and_then(|value| value.is_truthy())
                            .unwrap_or(false)
                    })
                })
                .await;
            Python::attach(|py| match found {
                Some((ticket, result)) => Ok(Some((
                    Py::new(py, PyTicket::from_ticket(&ticket))?,
                    value_to_py(py, &result)?.unbind(),
                ))),
                None => Ok::<_, PyErr>(None),
            })
        })
    }

    /// Get the first failure that matches, and execution carries on. Gives back
    /// `None` when execution ends first.
    fn finish_on_failure<'py>(
        &self,
        py: Python<'py>,
        condition: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let found = inner
                .finish_on_failure(|event, ticket| {
                    Python::attach(|py| {
                        call_with_ticket(py, &condition, event, ticket)
                            .and_then(|value| value.is_truthy())
                            .unwrap_or(false)
                    })
                })
                .await;
            Python::attach(|py| match found {
                Some((event, ticket)) => Ok(Some((
                    to_py_event(&event),
                    Py::new(py, PyTicket::from_ticket(&ticket))?,
                ))),
                None => Ok::<_, PyErr>(None),
            })
        })
    }

    /// Read a ticket as it starts, finishes, or fails.
    ///
    /// It arrives with its messages, so a handler can pass it straight to
    /// `Trajectory.from_ticket`.
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

    /// Rewrite a ticket's replies before its next request.
    ///
    /// Your function receives the events since the ticket's previous request and
    /// the current replies, and returns the new list, or `None` to change
    /// nothing. Keep each `tool_use` paired with its `tool_result`. The edit is
    /// permanent and survives the session being continued.
    ///
    /// Raising prints the traceback and leaves the replies alone: this runs on
    /// an agent's own thread, with no Python frame to raise into.
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

    /// Decide what compaction does with a ticket's replies.
    ///
    /// Your function receives the `Compaction` and the current replies, and
    /// returns the new list, or `None` to leave them alone. Define it with
    /// `async def` to await `compaction.summarize(replies)`; a plain function
    /// cannot await and has to rewrite the replies on its own.
    ///
    /// Handing the replies back unchanged says compaction found nothing to
    /// drop, and after an overflow the ticket then fails. One editor is held at
    /// a time, and installing a second replaces the first.
    ///
    /// Raising prints the traceback and leaves the replies alone: this runs on
    /// an agent's own thread, with no Python frame to raise into.
    fn edit_replies_on_compaction<'py>(
        slf: PyRef<'py, Self>,
        editor: Py<PyAny>,
    ) -> PyRef<'py, Self> {
        slf.inner
            .edit_replies_on_compaction(move |compaction, replies: Vec<Reply>| {
                let editor = Python::attach(|py| editor.clone_ref(py));
                let unchanged = replies.clone();
                async move {
                    // The editor may await `Compaction.summarize`, whose future
                    // the tokio runtime drives. Running `asyncio.run` on an
                    // async worker would block the thread that has to poll it.
                    let edited = tokio::task::spawn_blocking(move || {
                        Python::attach(|py| {
                            match invoke_editor(py, &editor, compaction, &replies) {
                                Ok(Some(edited)) => edited,
                                Ok(None) => replies,
                                Err(err) => {
                                    err.print(py);
                                    replies
                                }
                            }
                        })
                    })
                    .await;
                    // A panicked editor thread carries no replies back, so hand
                    // over the originals: compaction changed nothing.
                    Ok(edited.unwrap_or(unchanged))
                }
            });
        slf
    }

    /// Rewrite one ticket's replies now, without sending a request.
    ///
    /// Your function receives the current replies and returns the new ones, or
    /// `None` to change nothing. A ticket that does not exist changes nothing.
    /// Raising, or returning anything but a list of `Reply`, raises here: this
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

    /// Process every queued ticket, then return. Awaitable.
    fn finish<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            inner.finish().await;
            Ok::<_, PyErr>(PyTicketQueue { inner })
        })
    }

    fn cancel(&self) {
        self.inner.cancel();
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// Get the execution statistics: requests, tokens, ticket counts, and the
    /// per-tool, per-file, per-label, and per-model figures.
    fn stats(&self) -> PyStats {
        PyStats::for_run(Arc::clone(&self.inner))
    }

    /// Get the result of every finished ticket, in creation order.
    fn results<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyAny>>> {
        self.inner
            .results()
            .iter()
            .map(|value| value_to_py(py, value))
            .collect()
    }

    /// Get the result of every finished ticket carrying a label.
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

    /// Get the result of every finished ticket claimed by an agent.
    fn results_for_agent<'py>(
        &self,
        py: Python<'py>,
        agent_name: &str,
    ) -> PyResult<Vec<Bound<'py, PyAny>>> {
        self.inner
            .results_for_agent(agent_name)
            .iter()
            .map(|value| value_to_py(py, value))
            .collect()
    }

    /// Get one ticket's result by key.
    fn result_for_ticket<'py>(
        &self,
        py: Python<'py>,
        key: &str,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.inner.result_for_ticket(key) {
            Some(value) => Ok(Some(value_to_py(py, &value)?)),
            None => Ok(None),
        }
    }
}

/// Call a Python function with the ticket and result every `_on_result` hook
/// hands over.
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

/// Call a Python function with the event and ticket every `_on_failure` hook
/// hands over.
fn call_with_ticket<'py>(
    py: Python<'py>,
    callable: &Py<PyAny>,
    event: &Event,
    ticket: &Ticket,
) -> PyResult<Bound<'py, PyAny>> {
    let view = Py::new(py, PyTicket::from_ticket(ticket))?;
    callable.bind(py).call1((to_py_event(event), view))
}

/// Read a ticket back out of what a `create_ticket_*` function returned. `None`,
/// or anything that is not a `Ticket`, adds nothing.
fn built_ticket(produced: &Bound<'_, PyAny>) -> Option<Ticket> {
    if produced.is_none() {
        return None;
    }
    Some(produced.extract::<PyRef<PyTicket>>().ok()?.to_ticket())
}

/// Ask a Python condition about an event, on the same terms as
/// [`ticket_predicate`].
fn event_predicate(predicate: &Py<PyAny>, event: &Event) -> bool {
    Python::attach(|py| {
        predicate
            .bind(py)
            .call1((to_py_event(event),))
            .and_then(|value| value.is_truthy())
            .unwrap_or(false)
    })
}

/// Ask a Python condition about a ticket. A conversion or Python error reads as
/// false, so a broken condition never brings down an agent's thread.
fn ticket_predicate(predicate: &Py<PyAny>, ticket: &Ticket) -> bool {
    Python::attach(|py| {
        Py::new(py, PyTicket::from_ticket(ticket))
            .and_then(|view| predicate.bind(py).call1((view,)))
            .and_then(|value| value.is_truthy())
            .unwrap_or(false)
    })
}
