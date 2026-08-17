//! The actions agents can take to perform their work.

mod tool;
mod tool_file;
pub(crate) mod util;

mod code;
mod command;
mod edit_file;
mod fetch_url;
mod glob;
mod grep;
mod knowledge;
mod list_directory;
mod read_file;
mod tickets;
mod write_file;

pub(crate) use tool::{cap_results, ToolCall, ToolRegistry};
pub use tool::{Tool, ToolContext, ToolResult};

pub use command::CommandTool;
pub use edit_file::EditFileTool;
pub use fetch_url::FetchUrlTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use knowledge::KnowledgeTool;
pub use list_directory::ListDirectoryTool;
pub use read_file::ReadFileTool;
pub use tickets::{FinishTool, TicketsTool};
pub use write_file::WriteFileTool;
