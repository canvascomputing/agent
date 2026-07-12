//! Run agentic workflows where many agents work in parallel on a shared
//! ticket queue. An [`Agent`] picks up tickets from a [`TicketSystem`],
//! calls the LLM provider, runs the tools it requests, and writes results
//! back. Tickets are assigned to agents by name or label; the system
//! handles concurrency, automatic context compaction, schema validation,
//! retries, and limits.
//!
//! # Quick start
//!
//! ```no_run
//! use agentwerk::Agent;
//! use agentwerk::tools::{GrepTool, ReadFileTool};
//!
//! # async fn run() {
//! let agent = Agent::new()
//!     .from_env()
//!     .role("You are a Rust developer who explores source files to answer questions.")
//!     .tool(ReadFileTool)
//!     .tool(GrepTool)
//!     .build();
//!
//! agent.task("Find every `pub trait` defined under src/ and explain each in one sentence.");
//! let work = agent.finish().await;
//!
//! let result = work.last_result().unwrap();
//! println!("{}", result.as_str().unwrap_or_default());
//! # }
//! ```
//!
//! # Many agents working together
//!
//! ```no_run
//! use agentwerk::{Agent, Ticket, TicketSystem};
//! use agentwerk::tools::FetchUrlTool;
//!
//! # async fn run() {
//! let tickets = TicketSystem::new();
//!
//! for i in 0..4 {
//!     tickets.agent(
//!         Agent::new()
//!             .name(format!("agent_{i}"))
//!             .label("research")
//!             .from_env()
//!             .tool(FetchUrlTool)
//!             .build(),
//!     );
//! }
//!
//! for url in [
//!     "https://canvascomputing.org",
//!     "https://canvascomputing.org/about",
//!     "https://canvascomputing.org/products",
//!     "https://canvascomputing.org/blog",
//! ] {
//!     tickets.ticket(Ticket::new(format!("Summarize {url}")).label("research"));
//! }
//!
//! tickets.finish().await;
//!
//! for ticket in tickets.tickets() {
//!     if let Some(result) = ticket.result {
//!         println!("{}: {}", ticket.key, result);
//!     }
//! }
//! # }
//! ```
//!
//! # Main types
//!
//! - [`Agent`]: picks up tickets and produces results.
//! - [`TicketSystem`]: orchestrates work across one or more agents.
//! - [`Ticket`]: a task plus labels and schema for assignment and validation.
//! - [`Knowledge`]: durable memory the agent curates and shares across tickets and other agents.
//! - [`Stats`]: statistics for tickets, tokens, and activity.
//! - [`Event`]: lifecycle events emitted as agents work.
//! - [`tools`]: built-in tools agents call: file I/O, search, shell, web, knowledge, ticket finish tools.

pub mod agents;
pub mod codegrep;
pub mod event;
pub(crate) mod persistence;
pub(crate) mod prompts;
pub mod providers;
pub mod schemas;
pub mod tools;

#[cfg(test)]
pub(crate) mod test_util;

// Workshop: agents pull tickets from the system
pub use agents::Agent;
pub use agents::Status;
pub use agents::Ticket;
pub use agents::TicketSystem;

// Tuning, telemetry, durable state
pub use agents::Knowledge;
pub use agents::Stats;

// Validation
pub use schemas::Schema;

// Observation
pub use event::Event;
pub use event::EventKind;
pub use event::FinishReason;
