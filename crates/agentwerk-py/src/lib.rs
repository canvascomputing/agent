//! Exposes the Rust agentwerk API through Python.

use pyo3::prelude::*;

mod agent;
mod convert;
mod event;
mod knowledge;
mod policy;
mod providers;
mod query;
mod reply;
mod schema;
mod task;
mod tools;
mod trajectory;
mod werk;

#[pymodule]
fn _agentwerk(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<agent::PyAgent>()?;
    m.add_class::<werk::PyWerk>()?;
    m.add_class::<policy::PyPolicy>()?;
    m.add_class::<task::PyTask>()?;
    m.add_class::<query::PyQuery>()?;
    m.add_class::<reply::PyReply>()?;
    m.add_class::<reply::PyReplyContent>()?;
    m.add_class::<trajectory::PyTrajectory>()?;
    m.add_class::<schema::PySchema>()?;
    m.add_class::<event::PyEvent>()?;
    m.add_class::<knowledge::PyKnowledge>()?;
    m.add_class::<knowledge::PyPages>()?;
    m.add_class::<knowledge::PyPage>()?;
    providers::register(m)?;
    tools::register(m)?;
    Ok(())
}
