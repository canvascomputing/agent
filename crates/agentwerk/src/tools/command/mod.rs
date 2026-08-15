//! The command tool and the parsing behind it: a line becomes one program and
//! its arguments, and one argument becomes the flag a rule can name.

mod parse;
mod tool;

pub(crate) use parse::Command;
pub use tool::CommandTool;
