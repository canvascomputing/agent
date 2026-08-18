//! Run agentic workflows where many agents work in parallel on a shared
//! ticket queue. An [`Agent`] picks up tickets from a [`TicketQueue`],
//! calls the LLM provider, runs the tools it requests, and writes results
//! back. Tickets are assigned to agents by name or label; the queue
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
//! let agent = Agent::from_env()
//!     .role("You are a Rust developer who explores source files to answer questions.")
//!     .tool(ReadFileTool)
//!     .tool(GrepTool)
//!     .build();
//!
//! agent.task("Find every `pub trait` defined under src/ and explain each in one sentence.");
//! let work = agent.start();
//! let result = work.finish_last().await.unwrap();
//!
//! println!("{}", result.as_str().unwrap_or_default());
//! # }
//! ```
//!
//! # Many agents working together
//!
//! ```no_run
//! use agentwerk::{Agent, Ticket, TicketQueue};
//! use agentwerk::tools::FetchUrlTool;
//!
//! # async fn run() {
//! let tickets = TicketQueue::new();
//!
//! for _ in 0..4 {
//!     tickets.agent(
//!         Agent::from_env()
//!             .label("research")
//!             .tool(FetchUrlTool::new())
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
//! tickets.finish_all().await;
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
//! - [`TicketQueue`]: coordinates complex work across agents.
//! - [`Ticket`]: a task plus the label and schema that assign and validate it.
//! - [`Knowledge`]: durable memory the agent shares across tickets and other agents.
//! - [`Event`]: requests, tool usage, failures and more.
//! - [`tools`]: the built-in tools agents call, for files, search, commands, web, knowledge, and tickets.

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

// Workshop: agents pull tickets from the queue
pub use agents::Agent;
pub use agents::AgentBuilder;
pub use agents::Reply;
pub use agents::Status;
pub use agents::Ticket;
pub use agents::TicketQueue;

// Tuning, telemetry, durable state
pub use agents::Compaction;
pub use agents::Knowledge;
pub use agents::Trajectory;

// Validation
pub use schemas::Schema;
pub use schemas::SchemaStore;

// Observation
pub use event::Event;
pub use event::EventKind;
pub use event::FinishReason;

// The one public face of `prompts`: what agentwerk tells the model when it has
// to correct it. The rest of the module assembles a role and stays internal.
pub use prompts::directives::Directive;
