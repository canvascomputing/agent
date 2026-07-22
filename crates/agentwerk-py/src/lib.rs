//! Python bindings for agentwerk: a thin PyO3 veneer over the Rust agent loop.
//! The Rust crate stays the single source of truth; this module exposes its
//! builder, tools, providers, and ticket system to Python.

use pyo3::prelude::*;

mod agent;
mod convert;
mod providers;
mod ticket_system;
mod tools;

#[pymodule]
fn _agentwerk(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<agent::PyAgentBuilder>()?;
    m.add_class::<agent::PyAgent>()?;
    m.add_class::<ticket_system::PyTicketSystem>()?;
    providers::register(m)?;
    tools::register(m)?;
    Ok(())
}
