//! Exposes origin-aware AQL queries through one Python class.

use agentwerk::agents::Matcher;
use agentwerk::event::Event;
use agentwerk::{Query, Task};
use pyo3::prelude::*;

use crate::event::to_py_event;
use crate::task::PyTask;

/// Selects tasks or events by origin-qualified field values.
#[pyclass(name = "Query")]
pub struct PyQuery {
    source: String,
    query: Query,
}

#[pymethods]
impl PyQuery {
    /// Compile an AQL string, the same syntax a string argument carries.
    ///
    #[new]
    fn new(query: &str) -> PyResult<Self> {
        Ok(PyQuery {
            source: query.to_string(),
            query: Query::new(query).map_err(|error| value_error(error.to_string()))?,
        })
    }

    fn __repr__(&self) -> String {
        format!("Query({:?})", self.source)
    }
}

fn value_error(message: impl Into<String>) -> PyErr {
    pyo3::exceptions::PyValueError::new_err(message.into())
}

/// Read a Python argument as a task query: a `Query`, a string in AQL, or a
/// callable as a condition of its own. A string that does not compile raises
/// `ValueError` rather than panicking across the binding.
pub fn to_task_matcher(py: Python<'_>, arg: &Py<PyAny>) -> PyResult<Query> {
    if let Ok(query) = arg.extract::<PyRef<'_, PyQuery>>(py) {
        query
            .query
            .expects_task()
            .map_err(|error| value_error(error.to_string()))?;
        return Ok(query.query.clone());
    }
    if let Ok(query) = arg.extract::<String>(py) {
        let query = Query::new(&query).map_err(|error| value_error(error.to_string()))?;
        query
            .expects_task()
            .map_err(|error| value_error(error.to_string()))?;
        return Ok(query);
    }
    let callable = arg.clone_ref(py);
    Ok(Matcher::into_query(move |task: &Task| {
        task_predicate(&callable, task)
    }))
}

/// Read a task or result finder's query. Named AQL may originate from tasks
/// or events; callables continue to receive the destination task.
pub fn to_task_finder(py: Python<'_>, arg: &Py<PyAny>) -> PyResult<Query> {
    if let Ok(query) = arg.extract::<PyRef<'_, PyQuery>>(py) {
        return Ok(query.query.clone());
    }
    if let Ok(query) = arg.extract::<String>(py) {
        return Query::new(&query).map_err(|error| value_error(error.to_string()));
    }
    let callable = arg.clone_ref(py);
    Ok(Matcher::into_query(move |task: &Task| {
        task_predicate(&callable, task)
    }))
}

/// Read an event finder's query. Named AQL may originate from tasks or events;
/// callables continue to receive the destination event.
pub fn to_event_matcher(py: Python<'_>, arg: &Py<PyAny>) -> PyResult<Query> {
    if let Ok(query) = arg.extract::<PyRef<'_, PyQuery>>(py) {
        return Ok(query.query.clone());
    }
    if let Ok(query) = arg.extract::<String>(py) {
        return Query::new(&query).map_err(|error| value_error(error.to_string()));
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
