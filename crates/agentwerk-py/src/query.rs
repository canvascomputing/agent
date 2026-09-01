//! Exposes AQL queries over tasks and events through one Python class.

use agentwerk::agents::{Matcher, QueryError};
use agentwerk::event::Event;
use agentwerk::{Query, Task};
use pyo3::prelude::*;

use crate::event::to_py_event;
use crate::task::PyTask;

/// Selects tasks or recorded events by field values, compiled from AQL.
#[pyclass(name = "Query")]
pub struct PyQuery {
    source: String,
    tasks: Result<Query<Task>, QueryError>,
    events: Result<Query<Event>, QueryError>,
}

#[pymethods]
impl PyQuery {
    /// Compile an AQL string, the same syntax a string argument carries.
    ///
    /// A string the task fields reject still selects events, and the other
    /// way round. Only one the two field sets both reject raises here.
    #[new]
    fn new(query: &str) -> PyResult<Self> {
        let compiled = PyQuery {
            source: query.to_string(),
            tasks: Query::<Task>::new(query),
            events: Query::<Event>::new(query),
        };
        match (&compiled.tasks, &compiled.events) {
            (Err(over_tasks), Err(over_events)) => Err(rejected(over_tasks, over_events)),
            _ => Ok(compiled),
        }
    }

    fn __repr__(&self) -> String {
        format!("Query({:?})", self.source)
    }
}

/// The error a string neither field set accepts raises. One message where both
/// answered the same, so a malformed query is not reported twice.
fn rejected(over_tasks: &QueryError, over_events: &QueryError) -> PyErr {
    let over_tasks = over_tasks.to_string();
    let over_events = over_events.to_string();
    match over_tasks == over_events {
        true => value_error(over_tasks),
        false => value_error(format!(
            "Over tasks: {over_tasks} Over events: {over_events}"
        )),
    }
}

fn value_error(message: impl Into<String>) -> PyErr {
    pyo3::exceptions::PyValueError::new_err(message.into())
}

/// Read a Python argument as a task query: a `Query`, a string in AQL, or a
/// callable as a condition of its own. A string that does not compile raises
/// `ValueError` rather than panicking across the binding.
pub fn to_task_matcher(py: Python<'_>, arg: &Py<PyAny>) -> PyResult<Query<Task>> {
    if let Ok(query) = arg.extract::<PyRef<'_, PyQuery>>(py) {
        return match &query.tasks {
            Ok(compiled) => Ok(compiled.clone()),
            Err(error) => Err(value_error(error.to_string())),
        };
    }
    if let Ok(query) = arg.extract::<String>(py) {
        return Query::<Task>::new(&query).map_err(|error| value_error(error.to_string()));
    }
    let callable = arg.clone_ref(py);
    Ok(Matcher::into_query(move |task: &Task| {
        task_predicate(&callable, task)
    }))
}

/// Read a Python argument as an event query, the way a task filter is read.
pub fn to_event_matcher(py: Python<'_>, arg: &Py<PyAny>) -> PyResult<Query<Event>> {
    if let Ok(query) = arg.extract::<PyRef<'_, PyQuery>>(py) {
        return match &query.events {
            Ok(compiled) => Ok(compiled.clone()),
            Err(error) => Err(value_error(error.to_string())),
        };
    }
    if let Ok(query) = arg.extract::<String>(py) {
        return Query::<Event>::new(&query).map_err(|error| value_error(error.to_string()));
    }
    let callable = arg.clone_ref(py);
    Ok(Matcher::into_query(move |event: &Event| {
        event_predicate(&callable, event)
    }))
}

/// Ask a Python condition about a task. A conversion or Python error reads as
/// false, so a broken condition never brings down an agent's thread.
fn task_predicate(predicate: &Py<PyAny>, task: &Task) -> bool {
    Python::attach(|py| {
        Py::new(py, PyTask::from_task(task))
            .and_then(|view| predicate.bind(py).call1((view,)))
            .and_then(|value| value.is_truthy())
            .unwrap_or(false)
    })
}

/// Ask a Python condition about an event. A Python error reads as false, so a
/// broken condition never stops the read.
fn event_predicate(predicate: &Py<PyAny>, event: &Event) -> bool {
    Python::attach(|py| {
        predicate
            .bind(py)
            .call1((to_py_event(event),))
            .and_then(|value| value.is_truthy())
            .unwrap_or(false)
    })
}
