//! The Python bindings wrap the Rust crate, which stays the one source of
//! truth, and expose its agents, tools, LLM providers, and ticket queue.

use pyo3::prelude::*;

mod agent;
mod compaction;
mod convert;
mod directives;
mod event;
mod knowledge;
mod providers;
mod reply;
mod schema;
mod ticket;
mod ticket_queue;
mod tools;
mod trajectory;

#[pymodule]
fn _agentwerk(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<agent::PyAgent>()?;
    m.add_class::<ticket_queue::PyTicketQueue>()?;
    m.add_class::<ticket::PyTicket>()?;
    m.add_class::<reply::PyReply>()?;
    m.add_class::<reply::PyReplyContent>()?;
    m.add_class::<trajectory::PyTrajectory>()?;
    m.add_class::<compaction::PyCompaction>()?;
    m.add_class::<schema::PySchema>()?;
    m.add_class::<schema::PySchemaStore>()?;
    m.add_class::<event::PyEvent>()?;
    m.add_function(wrap_pyfunction!(event::event_names, m)?)?;
    directives::register(m)?;
    m.add_class::<knowledge::PyKnowledge>()?;
    m.add_class::<knowledge::PyPages>()?;
    m.add_class::<knowledge::PyPage>()?;
    providers::register(m)?;
    tools::register(m)?;
    Ok(())
}
