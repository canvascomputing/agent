//! Python bindings for agentwerk: a thin PyO3 layer wrapping the Rust crate,
//! which stays the single source of truth. This module exposes its builder,
//! tools, providers, and ticket system to Python.

use pyo3::prelude::*;

mod agent;
mod convert;
mod event;
mod knowledge;
mod providers;
mod reply;
mod schema;
mod stats;
mod ticket;
mod ticket_system;
mod tools;
mod trajectory;

#[pymodule]
fn _agentwerk(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<agent::PyAgent>()?;
    m.add_class::<ticket_system::PyTicketSystem>()?;
    m.add_class::<ticket::PyTicket>()?;
    m.add_class::<reply::PyReply>()?;
    m.add_class::<reply::PyReplyContent>()?;
    m.add_class::<trajectory::PyTrajectory>()?;
    m.add_class::<schema::PySchema>()?;
    m.add_class::<event::PyEvent>()?;
    m.add_class::<knowledge::PyKnowledge>()?;
    m.add_class::<knowledge::PyPages>()?;
    m.add_class::<knowledge::PyPage>()?;
    m.add_class::<stats::PyStats>()?;
    m.add_class::<stats::PyToolStat>()?;
    m.add_class::<stats::PyFileStat>()?;
    m.add_class::<stats::PyKnowledgeStat>()?;
    m.add_class::<stats::PyModelStat>()?;
    providers::register(m)?;
    tools::register(m)?;
    Ok(())
}
