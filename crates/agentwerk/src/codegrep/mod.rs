//! A language-agnostic structural pattern matcher used inside the workspace.
//!
//! Patterns combine literal text, balanced brackets, metavariables
//! (`$NAME`), and ellipsis (`...` and `....`) to express structural
//! queries over arbitrary source files. Brackets, ellipsis, and
//! metavariable consistency are handled in Rust code directly; no
//! regex backend is used.

pub mod ast;
pub mod conf;
pub mod matcher;
pub mod token;

pub use ast::Pattern;
pub use conf::Conf;
pub use matcher::{search, search_tokens, Loc, Match, Metavariable};
pub use token::tokenize_target;

#[cfg(test)]
mod parser_tests;

#[cfg(test)]
mod matcher_tests;
