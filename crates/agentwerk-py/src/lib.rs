//! Python bindings for agentwerk: a thin PyO3 veneer over the Rust agent loop.
//! The Rust crate stays the single source of truth; this module exposes its
//! builder, tools, providers, and ticket system to Python.

use pyo3::prelude::*;

mod agent;
mod convert;
mod event;
mod knowledge;
mod providers;
mod schema;
mod ticket;
mod ticket_system;
mod tools;
mod trajectory;

#[pymodule]
fn _agentwerk(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<agent::PyAgentBuilder>()?;
    m.add_class::<agent::PyAgent>()?;
    m.add_class::<ticket_system::PyTicketSystem>()?;
    m.add_class::<ticket::PyTicket>()?;
    m.add_class::<trajectory::PyTrajectory>()?;
    m.add_class::<schema::PySchema>()?;
    m.add_class::<event::PyEvent>()?;
    m.add_class::<knowledge::PyKnowledge>()?;
    providers::register(m)?;
    tools::register(m)?;
    Ok(())
}
