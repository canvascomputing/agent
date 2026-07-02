//! Tool system: the `ToolLike` trait, the ad-hoc `Tool` struct, and the
//! registry agentwerk consults before each provider call.

mod error;
mod tool;
mod tool_file;
pub(crate) mod util;

mod bash;
mod codegrep;
mod edit_file;
mod fetch_url;
mod find_tools;
mod glob;
mod grep;
mod list_directory;
mod manage_knowledge;
mod read_file;
mod tickets;
mod write_file;

pub use error::ToolError;
pub use tool::{Tool, ToolContext, ToolLike, ToolResult};
pub(crate) use tool::{ToolCall, ToolRegistry};

pub use bash::BashTool;
pub use codegrep::CodegrepTool;
pub use edit_file::EditFileTool;
pub use fetch_url::FetchUrlTool;
pub use find_tools::FindToolsTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use list_directory::ListDirectoryTool;
pub use manage_knowledge::ManageKnowledgeTool;
pub use read_file::ReadFileTool;
pub(crate) use tickets::{finish_tool_input_schema, TICKET_FINISH_TOOLS};
pub use tickets::{FinishTicketTool, HandoverTicketTool, ManageTicketsTool, ReadTicketsTool};
pub use write_file::WriteFileTool;
