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
use crate::event::{to_py_event, PyEvent};
use crate::reply::{py_to_replies, replies_to_py};
use crate::schema::PySchemaStore;
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
    fn load(tickets_dir: &str) -> PyResult<Self> {
        let inner = TicketQueue::load(tickets_dir).map_err(runtime_error)?;
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

    /// Submit a `Ticket` with a custom label or schema, and return its key.
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
    fn max_turns(slf: PyRef<'_, Self>, count: u32) -> PyRef<'_, Self> {
        slf.inner.max_turns(count);
        slf
    }

    /// Limit the total input tokens.
    fn max_input_tokens(slf: PyRef<'_, Self>, count: u64) -> PyRef<'_, Self> {
        slf.inner.max_input_tokens(count);
        slf
    }

    /// Limit the total output tokens.
    fn max_output_tokens(slf: PyRef<'_, Self>, count: u64) -> PyRef<'_, Self> {
        slf.inner.max_output_tokens(count);
        slf
    }

    /// Limit the output tokens of a single request.
    fn max_request_tokens(slf: PyRef<'_, Self>, count: u32) -> PyRef<'_, Self> {
        slf.inner.max_request_tokens(count);
        slf
    }

    /// Limit the consecutive turns without a valid tool call; any successful
    /// call resets the count.
    fn max_schema_retries(slf: PyRef<'_, Self>, count: u32) -> PyRef<'_, Self> {
        slf.inner.max_schema_retries(count);
        slf
    }

    /// Limit how often a failing request is retried.
    fn max_request_retries(slf: PyRef<'_, Self>, count: u32) -> PyRef<'_, Self> {
        slf.inner.max_request_retries(count);
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

    /// Enforce schemas for ticket results. A ticket claimed under a label the
    /// store knows takes that schema, unless it already carries one of its own.
    fn schemas<'py>(slf: PyRef<'py, Self>, store: PyRef<'_, PySchemaStore>) -> PyRef<'py, Self> {
        slf.inner.schemas(&store.inner);
        slf
    }

    /// Read every event as it is emitted. It replaces the handler that prints to
    /// stderr.
    fn on_event<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_event(move |event: &Event| {
            Python::attach(|py| {
                let handled = handler.bind(py).call1((to_py_event(event),));
                if let Err(err) = handled {
                    err.print(py);
                }
            });
        });
        slf
    }

    /// Read every finished ticket together with its result, already validated
    /// against the ticket's schema.
    fn on_result<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_result(move |ticket: &Ticket, result: &Value| {
            Python::attach(|py| {
                if let Err(err) = call_with_result(py, &handler, ticket, result) {
                    err.print(py);
                }
            });
        });
        slf
    }

    /// Read every finished ticket together with its result, in an `async def`
    /// that `finish` waits for before it returns. It runs on the event loop
    /// awaiting `finish`, so work that has to stay serialized against the
    /// caller's own, such as a commit, can be; `on_result` runs on an agent
    /// thread and cannot await.
    ///
    /// Handlers run only while `finish` or `finish_all` is awaited, and MUST
    /// NOT call either themselves: that waits forever on the handover the
    /// handler is running inside.
    fn on_result_async<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_result_async(move |ticket: Ticket, result: Value| {
                let coroutine = Python::attach(|py| {
                    let produced = call_with_result(py, &handler, &ticket, &result)?;
                    pyo3_async_runtimes::tokio::into_future(produced)
                });
                async move {
                    match coroutine {
                        Ok(future) => {
                            if let Err(err) = future.await {
                                Python::attach(|py| err.print(py));
                            }
                        }
                        Err(err) => Python::attach(|py| err.print(py)),
                    }
                }
            });
        slf
    }

    /// Read every result the run has produced so far, each time one lands, in
    /// an `async def` that `finish` waits for before it returns. Use it to act
    /// on a condition across results with work that has to be awaited.
    ///
    /// Handlers run only while `finish` or `finish_all` is awaited, and MUST
    /// NOT call either themselves: that waits forever on the handover the
    /// handler is running inside.
    fn on_results_async<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_results_async(move |results: Vec<Value>| {
            let coroutine = Python::attach(|py| {
                let produced = call_with_results(py, &handler, &results)?;
                pyo3_async_runtimes::tokio::into_future(produced)
            });
            async move {
                match coroutine {
                    Ok(future) => {
                        if let Err(err) = future.await {
                            Python::attach(|py| err.print(py));
                        }
                    }
                    Err(err) => Python::attach(|py| err.print(py)),
                }
            }
        });
        slf
    }

    /// Read every result the run has produced so far, each time one lands. The
    /// same list `results()` gives after the run, in creation order, delivered
    /// while it is still going.
    fn on_results<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_results(move |results: &[Value]| {
            Python::attach(|py| {
                if let Err(err) = call_with_results(py, &handler, results) {
                    err.print(py);
                }
            });
        });
        slf
    }

    /// Read every failure together with the ticket it happened in: a failed
    /// ticket, tool call, or request, a file that would not open, or compaction
    /// that could not finish. Read `event.kind` to tell them apart.
    fn on_failure<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_failure(move |event: &Event, ticket: &Ticket| {
            Python::attach(|py| {
                if let Err(err) = call_with_ticket(py, &handler, event, ticket) {
                    err.print(py);
                }
            });
        });
        slf
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

    /// Enqueue follow-up tickets once a condition across every result holds.
    /// Your function is the condition: return an empty list or `None` until
    /// the results call for the work, which is also what stops a follow-up
    /// whose own result triggers it again.
    fn create_tickets_on_results<'py>(slf: PyRef<'py, Self>, make: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .create_tickets_on_results(move |results: &[Value]| {
                Python::attach(|py| match call_with_results(py, &make, results) {
                    Ok(produced) => built_tickets(&produced),
                    Err(err) => {
                        err.print(py);
                        Vec::new()
                    }
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

    /// Get the model that agent runs, or `None` when no agent of that name is
    /// added. `Trajectory.from_ticket` needs it.
    fn model_for_agent(&self, agent_id: &str) -> Option<String> {
        self.inner.model_for_agent(agent_id)
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

    /// Read a ticket as it starts, finishes, or fails.
    ///
    /// It arrives with its messages, so a handler can pass it straight to
    /// `Trajectory.from_ticket`.
    fn on_ticket<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_ticket(move |event: &Event, ticket: &Ticket| {
            Python::attach(|py| {
                let handled = Py::new(py, PyTicket::from_ticket(ticket))
                    .and_then(|view| handler.bind(py).call1((to_py_event(event), view)));
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

    /// Rewrite the prompt that corrects an agent's behavior.
    ///
    /// Your function receives the `SchemaRetried` event that says why the retry
    /// happened, in whose ticket, and for which agent, together with the
    /// built-in prompt, and returns the replacement, or `None` to keep it. It
    /// runs once per retry, so keep it cheap. One editor is held at a time, and
    /// installing a second replaces the first.
    ///
    /// Raising prints the traceback and keeps the built-in prompt: this runs on
    /// an agent's own thread, with no Python frame to raise into.
    fn edit_directive_on_retry<'py>(slf: PyRef<'py, Self>, editor: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .edit_directive_on_retry(move |event: &Event, directive: &mut String| {
                Python::attach(|py| {
                    let outcome = (|| -> PyResult<Option<String>> {
                        let returned = editor
                            .bind(py)
                            .call1((to_py_event(event), directive.as_str()))?;
                        if returned.is_none() {
                            return Ok(None);
                        }
                        Ok(Some(returned.extract::<String>()?))
                    })();
                    match outcome {
                        Ok(Some(replacement)) => *directive = replacement,
                        Ok(None) => {}
                        Err(err) => err.print(py),
                    }
                });
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

    /// Begin processing tickets, on a background task. An empty queue keeps the
    /// run alive; calling this while one is under way does nothing.
    fn start(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        // `TicketQueue::start` spawns onto the ambient Tokio runtime; a pymethod
        // call has no runtime entered on its own thread, so enter the shared
        // one pyo3-async-runtimes already uses for `finish()`.
        let _guard = pyo3_async_runtimes::tokio::get_runtime().enter();
        slf.inner.start();
        slf
    }

    /// Wait for the matching tickets to be done, then give back their results
    /// in creation order. Name a label to wait for one pool, or a key to wait
    /// for one ticket; `finish_all()` waits for the whole run. Awaitable.
    fn finish<'py>(&self, py: Python<'py>, matches: Py<PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let results = inner
                .finish(|ticket| ticket_predicate(&matches, ticket))
                .await;
            Python::attach(|py| {
                results
                    .iter()
                    .map(|value| Ok(value_to_py(py, value)?.unbind()))
                    .collect::<PyResult<Vec<_>>>()
            })
        })
    }

    /// Wait for every ticket to be done, then give back every result in
    /// creation order. Awaitable.
    fn finish_all<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let results = inner.finish_all().await;
            Python::attach(|py| {
                results
                    .iter()
                    .map(|value| Ok(value_to_py(py, value)?.unbind()))
                    .collect::<PyResult<Vec<_>>>()
            })
        })
    }

    /// Wait for every ticket to be done, then give back the last result in
    /// creation order. `None` means no ticket finished with a result.
    /// Awaitable.
    fn finish_last<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = inner.finish_last().await;
            Python::attach(|py| {
                result
                    .map(|value| Ok(value_to_py(py, &value)?.unbind()))
                    .transpose()
            })
        })
    }

    /// Get why the last run ended, or `None` while one is still going.
    fn finish_reason(&self) -> Option<String> {
        self.inner.finish_reason().map(|reason| reason.to_string())
    }

    /// Take every matching ticket off the queue.
    fn cancel<'py>(slf: PyRef<'py, Self>, matches: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .cancel(move |ticket| ticket_predicate(&matches, ticket));
        slf
    }

    /// Take every ticket off the queue, which ends the run.
    fn cancel_all(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf.inner.cancel_all();
        slf
    }

    /// Check whether a ticket has been cancelled. Ask before creating follow-up
    /// work: a cancelled ticket is never claimed.
    fn is_cancelled(&self, ticket: &PyTicket) -> bool {
        self.inner.is_cancelled(&ticket.to_ticket())
    }

    /// Get every recorded event matching a condition, oldest first. Counting
    /// is `len()`, and a total is a fold over the events themselves.
    fn find_events(&self, predicate: Py<PyAny>) -> Vec<PyEvent> {
        self.inner
            .find_events(|event| event_predicate(&predicate, event))
            .iter()
            .map(to_py_event)
            .collect()
    }

    /// Get the earliest recorded event matching a condition.
    fn find_event(&self, predicate: Py<PyAny>) -> Option<PyEvent> {
        self.inner
            .find_event(|event| event_predicate(&predicate, event))
            .as_ref()
            .map(to_py_event)
    }

    /// Get the input tokens across the run's finished requests.
    fn input_tokens(&self) -> u64 {
        self.inner.input_tokens()
    }

    /// Get the output tokens across the run's finished requests.
    fn output_tokens(&self) -> u64 {
        self.inner.output_tokens()
    }

    /// Get the elapsed execution duration in seconds, or `None` before the
    /// first ticket starts.
    fn execution_duration(&self) -> Option<f64> {
        self.inner.execution_duration().map(|d| d.as_secs_f64())
    }

    /// Get the result of every finished ticket, in creation order.
    fn results<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyAny>>> {
        self.inner
            .results()
            .iter()
            .map(|value| value_to_py(py, value))
            .collect()
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

/// Call a Python function with the parent and its children's results every
/// `_on_results` hook hands over.
fn call_with_results<'py>(
    py: Python<'py>,
    callable: &Py<PyAny>,
    results: &[Value],
) -> PyResult<Bound<'py, PyAny>> {
    let values = results
        .iter()
        .map(|result| value_to_py(py, result))
        .collect::<PyResult<Vec<_>>>()?;
    callable.bind(py).call1((values,))
}

/// Read a ticket back out of what a `create_ticket_*` function returned. `None`,
/// or anything that is not a `Ticket`, adds nothing.
fn built_ticket(produced: &Bound<'_, PyAny>) -> Option<Ticket> {
    if produced.is_none() {
        return None;
    }
    Some(produced.extract::<PyRef<PyTicket>>().ok()?.to_ticket())
}

/// Read every ticket back out of what `create_tickets_on_results` returned.
/// Anything that is not a sequence, `None` included, and any element that is
/// not a `Ticket`, adds nothing.
fn built_tickets(produced: &Bound<'_, PyAny>) -> Vec<Ticket> {
    let Ok(items) = produced.try_iter() else {
        return Vec::new();
    };
    items
        .flatten()
        .filter_map(|item| built_ticket(&item))
        .collect()
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
