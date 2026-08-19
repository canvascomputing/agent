//! Agents, the tickets they work from, and the loop that drives them.

pub mod agent;
pub(crate) mod compaction;
pub mod knowledge;
pub mod r#loop;
pub(crate) mod policy;
pub(crate) mod retry;
pub(crate) mod stats;
pub mod tickets;

pub use agent::{Agent, AgentBuilder};
pub use knowledge::Knowledge;
pub use tickets::{Query, QueryError, Reply, Status, Ticket, TicketError, TicketQueue, Trajectory};
