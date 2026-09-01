//! The Werk as Python sees it: add agents, submit work, set limits,
//! install handlers, drive execution, and read results.

use std::future::Future;
use std::sync::Arc;

use agentwerk::agents::tasks::Reply;
use agentwerk::event::Event;
use agentwerk::{Task, Werk};
use pyo3::prelude::*;
use serde_json::Value;

use crate::agent::PyAgent;
use crate::convert::{py_to_value, runtime_error, value_to_py};
use crate::event::{to_py_event, PyEvent};
use crate::policy::PyPolicy;
use crate::query::{to_event_matcher, to_task_matcher};
use crate::reply::{py_to_replies, replies_to_py};
use crate::task::{to_task, PyTask};

/// The core data structure of agentwerk, coordinating complex work across
/// agents.
#[pyclass(name = "Werk")]
pub struct PyWerk {
    pub inner: Arc<Werk>,
}

#[pymethods]
impl PyWerk {
    #[new]
    fn new() -> Self {
        PyWerk { inner: Werk::new() }
    }

    /// Continue a session from a directory written earlier.
    #[staticmethod]
    fn load(werk_dir: &str) -> PyResult<Self> {
        let inner = Werk::load(werk_dir).map_err(runtime_error)?;
        Ok(PyWerk { inner })
    }

    /// Add an agent to this Werk, moving any tasks it queued on its
    /// own across first.
    fn add_agent<'py>(
        slf: PyRef<'py, Self>,
        agent: PyRef<'_, PyAgent>,
    ) -> PyResult<PyRef<'py, Self>> {
        slf.inner.add_agent(agent.ready()?.clone());
        Ok(slf)
    }

    /// Submit a task and return its task ID.
    ///
    /// A `str` is the task itself, and an `os.PathLike` names the file holding
    /// it. A `Task` carries a custom label or schema with it.
    fn add_task(slf: PyRef<'_, Self>, task: &Bound<'_, PyAny>) -> PyResult<String> {
        Ok(slf.inner.add_task(to_task(task)?))
    }

    /// Add a reply to a task, which drives its next turn.
    fn add_reply<'py>(slf: PyRef<'py, Self>, id: &str, content: &str) -> PyRef<'py, Self> {
        slf.inner.add_reply(id, content);
        slf
    }

    /// Publish an event and return what every observer saw.
    fn emit_event(&self, event: PyRef<'_, PyEvent>) -> PyEvent {
        to_py_event(&self.inner.emit_event(event.inner.clone()))
    }

    /// Finish a task with a result, from outside the execution.
    ///
    /// Raises when the ID is unknown, or when the result misses the task's
    /// schema.
    fn set_task_finished(&self, id: &str, result: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner
            .set_task_finished(id, py_to_value(result)?)
            .map_err(runtime_error)
    }

    /// Fail a task, from outside the execution. Raises when the ID is unknown.
    fn set_task_failed(&self, id: &str) -> PyResult<()> {
        self.inner.set_task_failed(id).map_err(runtime_error)
    }

    /// Set the execution limits and retry tuning.
    fn set_policy<'py>(slf: PyRef<'py, Self>, policy: PyRef<'_, PyPolicy>) -> PyRef<'py, Self> {
        slf.inner.set_policy(policy.inner.clone());
        slf
    }

    /// Get the execution limits and retry tuning in force.
    fn get_policy(&self) -> PyPolicy {
        PyPolicy {
            inner: self.inner.get_policy(),
        }
    }

    /// Define where a session is stored.
    fn set_dir<'py>(slf: PyRef<'py, Self>, dir: &str) -> PyRef<'py, Self> {
        slf.inner.set_dir(dir);
        slf
    }

    /// Get the session directory, `./.agentwerk` until `dir` changes it.
    fn get_dir(&self) -> String {
        self.inner.get_dir().display().to_string()
    }

    /// Read every event as it is emitted. It replaces the handler that prints to
    /// stderr.
    ///
    /// The Werk arrives first, so a handler files follow-up work with
    /// `werk.add_task(..)` and selects tasks and results with `werk.find_*`.
    fn on_event<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_event(move |werk, event: &Event| {
            Python::attach(|py| {
                let handled = as_py_werk(py, werk)
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
    /// Handlers run only while `finish_tasks` or `finish_all_tasks` is awaited, and MUST
    /// NOT call either themselves: that waits forever on the handover the
    /// handler is running inside.
    fn on_event_async<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_event_async(move |werk, event: Event| {
            let coroutine = Python::attach(|py| {
                let view = as_py_werk(py, &werk)?;
                let produced = handler.bind(py).call1((view, to_py_event(&event)))?;
                pyo3_async_runtimes::tokio::into_future(produced)
            });
            await_coroutine(coroutine)
        });
        slf
    }

    /// Read every finished task together with its result, already validated
    /// against the task's schema.
    fn on_result<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_result(move |werk, task: &Task, result: &Value| {
                Python::attach(|py| {
                    if let Err(err) = call_with_result(py, &handler, werk, task, result) {
                        err.print(py);
                    }
                });
            });
        slf
    }

    /// Read every finished task together with its result, in an `async def`
    /// that `finish` waits for before it returns, on the terms
    /// `on_event_async` sets.
    ///
    /// Handlers run only while `finish_tasks` or `finish_all_tasks` is awaited, and MUST
    /// NOT call either themselves: that waits forever on the handover the
    /// handler is running inside.
    fn on_result_async<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_result_async(move |werk, task: Task, result: Value| {
                let coroutine = Python::attach(|py| {
                    let produced = call_with_result(py, &handler, &werk, &task, &result)?;
                    pyo3_async_runtimes::tokio::into_future(produced)
                });
                await_coroutine(coroutine)
            });
        slf
    }

    /// Read every failure together with the task it happened in: a failed
    /// task, tool call, or request, a file that would not open, or compaction
    /// that could not finish. Read `event.get_name()` to tell them apart.
    fn on_failure<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_failure(move |werk, event: &Event, task: &Task| {
                Python::attach(|py| {
                    if let Err(err) = call_with_task(py, &handler, werk, event, task) {
                        err.print(py);
                    }
                });
            });
        slf
    }

    /// Read every failure together with the task it happened in, in an
    /// `async def` that `finish` waits for before it returns, on the terms
    /// `on_event_async` sets.
    ///
    /// Handlers run only while `finish_tasks` or `finish_all_tasks` is awaited, and MUST
    /// NOT call either themselves: that waits forever on the handover the
    /// handler is running inside.
    fn on_failure_async<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_failure_async(move |werk, event: Event, task: Task| {
                let coroutine = Python::attach(|py| {
                    let produced = call_with_task(py, &handler, &werk, &event, &task)?;
                    pyo3_async_runtimes::tokio::into_future(produced)
                });
                await_coroutine(coroutine)
            });
        slf
    }

    /// Get the model that agent runs, or `None` when no agent of that name is
    /// added. `Trajectory.from_task` needs it.
    fn get_model_for_agent(&self, agent_id: &str) -> Option<String> {
        self.inner.get_model_for_agent(agent_id)
    }

    /// Get one task by ID.
    fn get_task(&self, py: Python<'_>, id: &str) -> PyResult<Option<Py<PyTask>>> {
        match self.inner.get_task(id) {
            Some(task) => Ok(Some(Py::new(py, PyTask::from_task(&task))?)),
            None => Ok(None),
        }
    }

    /// Get every task in creation order.
    fn get_tasks(&self, py: Python<'_>) -> PyResult<Vec<Py<PyTask>>> {
        self.inner
            .get_tasks()
            .iter()
            .map(|task| Py::new(py, PyTask::from_task(task)))
            .collect()
    }

    /// Get every task matching a Query or callable.
    fn find_tasks(&self, py: Python<'_>, predicate: Py<PyAny>) -> PyResult<Vec<Py<PyTask>>> {
        let tasks = self.inner.find_tasks(to_task_matcher(py, &predicate)?);
        tasks
            .iter()
            .map(|task| Py::new(py, PyTask::from_task(task)))
            .collect()
    }

    /// Get the first task matching a Query or callable.
    fn find_task(&self, py: Python<'_>, predicate: Py<PyAny>) -> PyResult<Option<Py<PyTask>>> {
        let task = self.inner.find_task(to_task_matcher(py, &predicate)?);
        match task {
            Some(task) => Ok(Some(Py::new(py, PyTask::from_task(&task))?)),
            None => Ok(None),
        }
    }

    /// Read a task as it starts, finishes, or fails.
    ///
    /// It arrives with its messages, so a handler can pass it straight to
    /// `Trajectory.from_task`.
    fn on_task<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner.on_task(move |werk, event: &Event, task: &Task| {
            Python::attach(|py| {
                if let Err(err) = call_with_task(py, &handler, werk, event, task) {
                    err.print(py);
                }
            });
        });
        slf
    }

    /// Read a task as it starts, finishes, or fails, in an `async def` that
    /// `finish` waits for before it returns, on the terms `on_event_async`
    /// sets.
    ///
    /// Handlers run only while `finish_tasks` or `finish_all_tasks` is awaited, and MUST
    /// NOT call either themselves: that waits forever on the handover the
    /// handler is running inside.
    fn on_task_async<'py>(slf: PyRef<'py, Self>, handler: Py<PyAny>) -> PyRef<'py, Self> {
        slf.inner
            .on_task_async(move |werk, event: Event, task: Task| {
                let coroutine = Python::attach(|py| {
                    let produced = call_with_task(py, &handler, &werk, &event, &task)?;
                    pyo3_async_runtimes::tokio::into_future(produced)
                });
                await_coroutine(coroutine)
            });
        slf
    }

    /// Rewrite one task's replies now, without sending a request.
    ///
    /// Your function receives the current replies and returns the new ones, or
    /// `None` to change nothing. A task that does not exist changes nothing.
    /// Raising, or returning anything but a list of `Reply`, raises here: this
    /// call has a Python frame to unwind into, so it does not guess.
    fn edit_replies<'py>(
        slf: PyRef<'py, Self>,
        id: &str,
        editor: Py<PyAny>,
    ) -> PyResult<PyRef<'py, Self>> {
        let mut failure: Option<PyErr> = None;
        slf.inner.edit_replies(id, |replies: &mut Vec<Reply>| {
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

    /// Begin processing tasks, on a background task. An empty Werk keeps the
    /// run alive; calling this while one is under way does nothing.
    fn start(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        // `Werk::start` spawns onto the ambient Tokio runtime; a pymethod
        // call has no runtime entered on its own thread, so enter the shared
        // one pyo3-async-runtimes already uses for `finish_tasks()`.
        let _guard = pyo3_async_runtimes::tokio::get_runtime().enter();
        slf.inner.start();
        slf
    }

    /// Wait for the matching tasks to be done, then give back their results
    /// in query order. Accepts a Query or callable. Awaitable.
    fn finish_tasks<'py>(
        &self,
        py: Python<'py>,
        matches: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        let query = to_task_matcher(py, &matches)?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let results = inner.finish_tasks(query).await;
            Python::attach(|py| {
                results
                    .iter()
                    .map(|value| Ok(value_to_py(py, value)?.unbind()))
                    .collect::<PyResult<Vec<_>>>()
            })
        })
    }

    /// Wait for every task to be done, then give back every result in
    /// creation order. Awaitable.
    fn finish_all_tasks<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let results = inner.finish_all_tasks().await;
            Python::attach(|py| {
                results
                    .iter()
                    .map(|value| Ok(value_to_py(py, value)?.unbind()))
                    .collect::<PyResult<Vec<_>>>()
            })
        })
    }

    /// Wait for the matching tasks to be done, then give back the first result
    /// in query order. `None` means no matching task finished with a result.
    /// Awaitable.
    fn finish_task<'py>(&self, py: Python<'py>, matches: Py<PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        let query = to_task_matcher(py, &matches)?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = inner.finish_task(query).await;
            Python::attach(|py| {
                result
                    .map(|value| Ok(value_to_py(py, &value)?.unbind()))
                    .transpose()
            })
        })
    }

    /// Get why the last run ended, or `None` while one is still going.
    fn get_finish_reason(&self) -> Option<String> {
        self.inner
            .get_finish_reason()
            .map(|reason| reason.to_string())
    }

    /// Take every matching task off the Werk. Accepts a Query or callable.
    fn cancel_tasks<'py>(slf: PyRef<'py, Self>, matches: Py<PyAny>) -> PyResult<PyRef<'py, Self>> {
        slf.inner.cancel_tasks(to_task_matcher(slf.py(), &matches)?);
        Ok(slf)
    }

    /// Take every task off the Werk, which ends the run.
    fn cancel_all_tasks(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf.inner.cancel_all_tasks();
        slf
    }

    /// Get every recorded event matching a `Query`, an AQL string, or a
    /// callable, oldest first. Counting is `len()`, and a total is a fold over
    /// the events themselves.
    fn find_events(&self, matches: Py<PyAny>, py: Python<'_>) -> PyResult<Vec<PyEvent>> {
        let found = self.inner.find_events(to_event_matcher(py, &matches)?);
        Ok(found.iter().map(to_py_event).collect())
    }

    /// Get the earliest recorded event matching a `Query`, an AQL string, or a
    /// callable, or the first in the order an `ORDER BY` names.
    fn find_event(&self, matches: Py<PyAny>, py: Python<'_>) -> PyResult<Option<PyEvent>> {
        let found = self.inner.find_event(to_event_matcher(py, &matches)?);
        Ok(found.as_ref().map(to_py_event))
    }

    /// Get the input tokens across the run's finished requests.
    fn get_input_tokens(&self) -> u64 {
        self.inner.get_input_tokens()
    }

    /// Get the output tokens across the run's finished requests.
    fn get_output_tokens(&self) -> u64 {
        self.inner.get_output_tokens()
    }

    /// Get the elapsed execution duration in seconds, or `None` before the
    /// first task starts.
    fn get_duration(&self) -> Option<f64> {
        self.inner.get_duration().map(|d| d.as_secs_f64())
    }

    /// Get the result of every finished task, in creation order.
    fn get_results<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyAny>>> {
        self.inner
            .get_results()
            .iter()
            .map(|value| value_to_py(py, value))
            .collect()
    }

    /// Get every result whose task matches the Query or callable, in query
    /// order. Status defaults to `"finished"`.
    fn find_results<'py>(
        &self,
        py: Python<'py>,
        query: Py<PyAny>,
    ) -> PyResult<Vec<Bound<'py, PyAny>>> {
        let results = self.inner.find_results(to_task_matcher(py, &query)?);
        results.iter().map(|value| value_to_py(py, value)).collect()
    }

    /// Get the first result whose task matches the Query or callable.
    /// Status defaults to `"finished"`.
    fn find_result<'py>(
        &self,
        py: Python<'py>,
        query: Py<PyAny>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let result = self.inner.find_result(to_task_matcher(py, &query)?);
        match result {
            Some(value) => Ok(Some(value_to_py(py, &value)?)),
            None => Ok(None),
        }
    }
}

/// Hand a hook the Werk it is registered on. Built per call: a cached view
/// would hold the Werk that holds the handler, and neither would ever be freed.
fn as_py_werk<'py>(py: Python<'py>, werk: &Arc<Werk>) -> PyResult<Bound<'py, PyAny>> {
    let view = Py::new(
        py,
        PyWerk {
            inner: Arc::clone(werk),
        },
    )?;
    Ok(view.into_bound(py).into_any())
}

/// Call a Python function with the Werk, task, and result every `on_result`
/// hook hands over.
fn call_with_result<'py>(
    py: Python<'py>,
    callable: &Py<PyAny>,
    werk: &Arc<Werk>,
    task: &Task,
    result: &Value,
) -> PyResult<Bound<'py, PyAny>> {
    let view = Py::new(py, PyTask::from_task(task))?;
    let value = value_to_py(py, result)?;
    callable
        .bind(py)
        .call1((as_py_werk(py, werk)?, view, value))
}

/// Call a Python function with the Werk, event, and task the `on_task` and
/// `on_failure` hooks hand over.
fn call_with_task<'py>(
    py: Python<'py>,
    callable: &Py<PyAny>,
    werk: &Arc<Werk>,
    event: &Event,
    task: &Task,
) -> PyResult<Bound<'py, PyAny>> {
    let view = Py::new(py, PyTask::from_task(task))?;
    callable
        .bind(py)
        .call1((as_py_werk(py, werk)?, to_py_event(event), view))
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
