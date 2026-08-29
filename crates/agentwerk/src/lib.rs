//! Run agentic workflows where many agents work in parallel on a shared
//! task queue. An [`Agent`] picks up tasks from a [`Queue`],
//! calls the LLM provider, runs the tools it requests, and writes results
//! back. Tasks are assigned to agents by name or label; the queue
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
//!     .tool(GrepTool);
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
//! use agentwerk::{Agent, Task, Queue};
//! use agentwerk::tools::FetchUrlTool;
//!
//! # async fn run() {
//! let tasks = Queue::new();
//!
//! for _ in 0..4 {
//!     tasks.agent(
//!         Agent::from_env()
//!             .label("research")
//!             .tool(FetchUrlTool::new()),
//!     );
//! }
//!
//! for url in [
//!     "https://canvascomputing.org",
//!     "https://canvascomputing.org/about",
//!     "https://canvascomputing.org/products",
//!     "https://canvascomputing.org/blog",
//! ] {
//!     tasks.task(Task::labeled("research", format!("Summarize {url}")));
//! }
//!
//! tasks.finish_all().await;
//!
//! for task in tasks.tasks() {
//!     if let Some(result) = task.result {
//!         println!("{}: {}", task.key, result);
//!     }
//! }
//! # }
//! ```
//!
//! # Main types
//!
//! - [`Agent`]: picks up tasks and produces results.
//! - [`Queue`]: coordinates complex work across agents.
//! - [`Task`]: a task plus the label and schema that assign and validate it.
//! - [`Knowledge`]: durable memory the agent shares across tasks and other agents.
//! - [`Event`]: requests, tool usage, failures and more.
//! - [`tools`]: the built-in tools agents call, for files, search, commands, web, knowledge, and tasks.

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

// Workshop: agents pull tasks from the queue
pub use agents::Agent;
pub use agents::Query;
pub use agents::Queue;
pub use agents::Reply;
pub use agents::Status;
pub use agents::Task;

// Tuning, telemetry, durable state
pub use agents::Knowledge;
pub use agents::Policy;
pub use agents::Trajectory;

// Validation
pub use schemas::Schema;
pub use schemas::SchemaStore;

// Observation
pub use event::Event;
pub use event::EventKind;
pub use event::FinishReason;

// The public faces of `prompts`: what agentwerk tells the model when it has to
// correct it, and the text a prompt is set from. The rest of the module
// assembles a role and stays internal.
pub use prompts::directives::Directive;
pub use prompts::text::Text;
