//! Defines agents and the execution loop they use to claim tasks.

pub mod agent;
pub(crate) mod compaction;
pub mod knowledge;
pub(crate) mod r#loop;
pub mod policy;
mod query;
pub(crate) mod retry;
pub(crate) mod stats;
pub mod tasks;

pub use agent::Agent;
pub use knowledge::Knowledge;
pub use policy::{Policy, PolicyViolation};
pub use query::{Matcher, Query, QueryError};
pub use tasks::{Reply, Status, Task, TaskError, Trajectory, Werk};
