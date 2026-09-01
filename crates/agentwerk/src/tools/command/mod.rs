//! Runs explicitly permitted commands without a shell.

mod parse;
mod tool;

pub(crate) use parse::Command;
pub use tool::CommandTool;
