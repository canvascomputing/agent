//! The ticket queue as Python sees it: add agents, submit work, set limits,
//! install handlers, drive execution, and read results.

use std::future::Future;
use std::sync::Arc;

use agentwerk::agents::tickets::Reply;
use agentwerk::event::Event;
use agentwerk::{Ticket, TicketQueue};
use pyo3::prelude::*;
use serde_json::Value;

use crate::agent::PyAgent;
use crate::convert::{py_to_value, runtime_error, value_to_py};
use crate::event::{to_py_event, PyEvent};
use crate::policy::PyPolicy;
use crate::reply::{py_to_replies, replies_to_py};
use crate::schema::PySchemaStore;
use crate::ticket::{to_ticket, try_extract_query, PyTicket};

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
    ///
    /// A `str` is the task itself, and an `os.PathLike` names the file holding
    /// it. A `Ticket` carries a custom label or schema with it.
    fn ticket(slf: PyRef<'_, Self>, ticket: &Bound<'_, PyAny>) -> PyResult<String> {
        Ok(slf.inner.ticket(to_ticket(ticket)?))
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

    /// Set the execution limits and retry tuning.
    fn policy<'py>(slf: PyRef<'py, Self>, policy: PyRef<'_, PyPolicy>) -> PyRef<'py, Self> {
        slf.inner.policy(policy.inner.clone());
        slf
    }

    /// Get the execution limits and retry tuning in force.
    fn get_policy(&self) -> PyPolicy {
        PyPolicy {
            inner: self.inner.get_policy(),
        }
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
    ///
    /// The queue arrives first, so a handler files follow-up work with
    /// `queue.ticket(..)` and selects tickets and results with `queue.find_*`.
    fn on_event<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_event(move |queue, event: &Event| {
            Python::attach(|py| {
                let handled = as_py_queue(py, queue)
                    .and_then(|view| handler.bind(py).call1((view, to_py_event(event))));
                if let Err(err) = handled {
                    err.print(py);
                }
            });
        });
        slf
    }

    /// Read every event as it is emitted, in an `async def` that `finish` waits
    /// for before it returns. It runs on the event loop awaiting `finish`, so
    /// work that has to stay serialized against the caller's own, such as a
    /// commit, can be; `on_event` runs on an agent thread and cannot await.
    ///
    /// Every kind reaches it, streamed reply chunks included, and each event
    /// waits in memory until a `finish` drains it.
    ///
    /// Handlers run only while `finish` or `finish_all` is awaited, and MUST
    /// NOT call either themselves: that waits forever on the handover the
    /// handler is running inside.
    fn on_event_async<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_event_async(move |queue, event: Event| {
            let coroutine = Python::attach(|py| {
                let view = as_py_queue(py, &queue)?;
                let produced = handler.bind(py).call1((view, to_py_event(&event)))?;
                pyo3_async_runtimes::tokio::into_future(produced)
            });
            await_coroutine(coroutine)
        });
        slf
    }

    /// Read every finished ticket together with its result, already validated
    /// against the ticket's schema.
    fn on_result<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_result(move |queue, ticket: &Ticket, result: &Value| {
                Python::attach(|py| {
                    if let Err(err) = call_with_result(py, &handler, queue, ticket, result) {
                        err.print(py);
                    }
                });
            });
        slf
    }

    /// Read every finished ticket together with its result, in an `async def`
    /// that `finish` waits for before it returns, on the terms
    /// `on_event_async` sets.
    ///
    /// Handlers run only while `finish` or `finish_all` is awaited, and MUST
    /// NOT call either themselves: that waits forever on the handover the
    /// handler is running inside.
    fn on_result_async<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_result_async(move |queue, ticket: Ticket, result: Value| {
                let coroutine = Python::attach(|py| {
                    let produced = call_with_result(py, &handler, &queue, &ticket, &result)?;
                    pyo3_async_runtimes::tokio::into_future(produced)
                });
                await_coroutine(coroutine)
            });
        slf
    }

    /// Read every failure together with the ticket it happened in: a failed
    /// ticket, tool call, or request, a file that would not open, or compaction
    /// that could not finish. Read `event.kind` to tell them apart.
    fn on_failure<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_failure(move |queue, event: &Event, ticket: &Ticket| {
                Python::attach(|py| {
                    if let Err(err) = call_with_ticket(py, &handler, queue, event, ticket) {
                        err.print(py);
                    }
                });
            });
        slf
    }

    /// Read every failure together with the ticket it happened in, in an
    /// `async def` that `finish` waits for before it returns, on the terms
    /// `on_event_async` sets.
    ///
    /// Handlers run only while `finish` or `finish_all` is awaited, and MUST
    /// NOT call either themselves: that waits forever on the handover the
    /// handler is running inside.
    fn on_failure_async<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_failure_async(move |queue, event: Event, ticket: Ticket| {
                let coroutine = Python::attach(|py| {
                    let produced = call_with_ticket(py, &handler, &queue, &event, &ticket)?;
                    pyo3_async_runtimes::tokio::into_future(produced)
                });
                await_coroutine(coroutine)
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

    /// Get every ticket matching a Query or callable.
    fn find_tickets(&self, py: Python<'_>, predicate: Py<PyAny>) -> PyResult<Vec<Py<PyTicket>>> {
        let tickets = match try_extract_query(py, &predicate)? {
            Some(query) => self.inner.find_tickets(query),
            None => self
                .inner
                .find_tickets(|ticket: &Ticket| ticket_predicate(&predicate, ticket)),
        };
        tickets
            .iter()
            .map(|ticket| Py::new(py, PyTicket::from_ticket(ticket)))
            .collect()
    }

    /// Get the first ticket matching a Query or callable.
    fn find_ticket(&self, py: Python<'_>, predicate: Py<PyAny>) -> PyResult<Option<Py<PyTicket>>> {
        let ticket = match try_extract_query(py, &predicate)? {
            Some(query) => self.inner.find_ticket(query),
            None => self
                .inner
                .find_ticket(|ticket: &Ticket| ticket_predicate(&predicate, ticket)),
        };
        match ticket {
            Some(ticket) => Ok(Some(Py::new(py, PyTicket::from_ticket(&ticket))?)),
            None => Ok(None),
        }
    }

    /// Read a ticket as it starts, finishes, or fails.
    ///
    /// It arrives with its messages, so a handler can pass it straight to
    /// `Trajectory.from_ticket`.
    fn on_ticket<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_ticket(move |queue, event: &Event, ticket: &Ticket| {
                Python::attach(|py| {
                    if let Err(err) = call_with_ticket(py, &handler, queue, event, ticket) {
                        err.print(py);
                    }
                });
            });
        slf
    }

    /// Read a ticket as it starts, finishes, or fails, in an `async def` that
    /// `finish` waits for before it returns, on the terms `on_event_async`
    /// sets.
    ///
    /// Handlers run only while `finish` or `finish_all` is awaited, and MUST
    /// NOT call either themselves: that waits forever on the handover the
    /// handler is running inside.
    fn on_ticket_async<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_ticket_async(move |queue, event: Event, ticket: Ticket| {
                let coroutine = Python::attach(|py| {
                    let produced = call_with_ticket(py, &handler, &queue, &event, &ticket)?;
                    pyo3_async_runtimes::tokio::into_future(produced)
                });
                await_coroutine(coroutine)
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
    /// in creation order. Accepts a Query or callable. Awaitable.
    fn finish<'py>(&self, py: Python<'py>, matches: Py<PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        let query = try_extract_query(py, &matches)?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let results = match query {
                Some(q) => inner.finish(q).await,
                None => {
                    inner
                        .finish(|ticket: &Ticket| ticket_predicate(&matches, ticket))
                        .await
                }
            };
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

    /// Take every matching ticket off the queue. Accepts a Query or callable.
    fn cancel<'py>(slf: PyRef<'py, Self>, matches: Py<PyAny>) -> PyResult<PyRef<'py, Self>> {
        match try_extract_query(slf.py(), &matches)? {
            Some(query) => slf.inner.cancel(query),
            None => slf
                .inner
                .cancel(move |ticket: &Ticket| ticket_predicate(&matches, ticket)),
        };
        Ok(slf)
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

    /// Get every result whose ticket matches the Query or callable, in creation
    /// order. Status defaults to `"finished"`.
    fn find_results<'py>(
        &self,
        py: Python<'py>,
        query: Py<PyAny>,
    ) -> PyResult<Vec<Bound<'py, PyAny>>> {
        let results = match try_extract_query(py, &query)? {
            Some(parsed) => self.inner.find_results(parsed),
            None => self
                .inner
                .find_results(|ticket: &Ticket| ticket_predicate(&query, ticket)),
        };
        results.iter().map(|value| value_to_py(py, value)).collect()
    }

    /// Get the first result whose ticket matches the Query or callable.
    /// Status defaults to `"finished"`.
    fn find_result<'py>(
        &self,
        py: Python<'py>,
        query: Py<PyAny>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let result = match try_extract_query(py, &query)? {
            Some(parsed) => self.inner.find_result(parsed),
            None => self
                .inner
                .find_result(|ticket: &Ticket| ticket_predicate(&query, ticket)),
        };
        match result {
            Some(value) => Ok(Some(value_to_py(py, &value)?)),
            None => Ok(None),
        }
    }
}

/// Hand a hook the queue it is registered on. Built per call: a cached view
/// would hold the queue that holds the handler, and neither would ever be freed.
fn as_py_queue<'py>(py: Python<'py>, queue: &Arc<TicketQueue>) -> PyResult<Bound<'py, PyAny>> {
    let view = Py::new(
        py,
        PyTicketQueue {
            inner: Arc::clone(queue),
        },
    )?;
    Ok(view.into_bound(py).into_any())
}

/// Call a Python function with the queue, ticket, and result every `on_result`
/// hook hands over.
fn call_with_result<'py>(
    py: Python<'py>,
    callable: &Py<PyAny>,
    queue: &Arc<TicketQueue>,
    ticket: &Ticket,
    result: &Value,
) -> PyResult<Bound<'py, PyAny>> {
    let view = Py::new(py, PyTicket::from_ticket(ticket))?;
    let value = value_to_py(py, result)?;
    callable
        .bind(py)
        .call1((as_py_queue(py, queue)?, view, value))
}

/// Call a Python function with the queue, event, and ticket the `on_ticket` and
/// `on_failure` hooks hand over.
fn call_with_ticket<'py>(
    py: Python<'py>,
    callable: &Py<PyAny>,
    queue: &Arc<TicketQueue>,
    event: &Event,
    ticket: &Ticket,
) -> PyResult<Bound<'py, PyAny>> {
    let view = Py::new(py, PyTicket::from_ticket(ticket))?;
    callable
        .bind(py)
        .call1((as_py_queue(py, queue)?, to_py_event(event), view))
}

/// Await what an `async def` handler returned, printing whatever it raised:
/// there is no Python frame behind a handover to raise into.
async fn await_coroutine(coroutine: PyResult<impl Future<Output = PyResult<Py<PyAny>>> + Send>) {
    match coroutine {
        Ok(future) => {
            if let Err(err) = future.await {
                Python::attach(|py| err.print(py));
            }
        }
        Err(err) => Python::attach(|py| err.print(py)),
    }
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
