//! Agents, the tickets they work from, and the loop that drives them.

pub mod agent;
pub(crate) mod compaction;
pub mod knowledge;
pub mod r#loop;
pub mod policy;
mod query;
pub(crate) mod retry;
pub(crate) mod stats;
pub mod tickets;

pub use agent::{Agent, AgentBuilder};
pub use knowledge::Knowledge;
pub use policy::Policy;
pub use query::{EventMatcher, EventQuery, Query, QueryError, TicketMatcher};
pub use tickets::{Reply, Status, Ticket, TicketError, TicketQueue, Trajectory};
