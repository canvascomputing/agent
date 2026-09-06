#![warn(missing_docs)]

//! Run agentic workflows where many agents work in parallel on a shared
//! [`Werk`]. An [`Agent`] picks up tasks from a Werk,
//! calls the LLM provider, runs the tools it requests, and writes results
//! back. Tasks are assigned to agents by label; the Werk
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
//! let task = agent.add_task("Find every `pub trait` defined under src/ and explain each in one sentence.");
//! let result = agent.finish_task(task).await.unwrap();
//!
//! println!("{}", result.as_str().unwrap_or_default());
//! # }
//! ```
//!
//! # Many agents working together
//!
//! ```no_run
//! use agentwerk::{Agent, Task, Werk};
//! use agentwerk::tools::FetchTool;
//!
//! # async fn run() {
//! let werk = Werk::new();
//!
//! for _ in 0..4 {
//!     werk.add_agent(
//!         Agent::from_env()
//!             .label("research")
//!             .tool(FetchTool::new()),
//!     );
//! }
//!
//! for url in [
//!     "https://canvascomputing.org",
//!     "https://canvascomputing.org/about",
//!     "https://canvascomputing.org/products",
//!     "https://canvascomputing.org/blog",
//! ] {
//!     werk.add_task(Task::labeled("research", format!("Summarize {url}")));
//! }
//!
//! werk.finish().await;
//!
//! for task in werk.get_tasks() {
//!     if let Some(result) = task.get_result() {
//!         println!("{}: {}", task.get_id(), result);
//!     }
//! }
//! # }
//! ```
//!
//! # Main types
//!
//! - [`Agent`]: picks up tasks and produces results.
//! - [`Werk`]: stores tasks and runs agents.
//! - [`Task`]: defines work with an optional label and schema.
//! - [`Query`]: a reusable AQL selection over tasks, events, or joined task-event pairs.
//! - [`Knowledge`]: durable memory the agent shares across tasks and other agents.
//! - [`Event`]: records requests, tool usage, failures, and other activity.
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

pub use agents::Agent;
pub use agents::Query;
pub use agents::Reply;
pub use agents::Status;
pub use agents::Task;
pub use agents::Werk;

pub use agents::Knowledge;
pub use agents::Policy;
pub use agents::PolicyViolation;
pub use agents::Trajectory;

pub use schemas::Schema;

pub use agents::tasks::FinishReason;
pub use event::Event;

// The public face of `prompts`: text a prompt is set from. The rest of the
// module assembles prompts and stays internal.
pub use prompts::text::Text;
