//! Pattern AST and its parser entry point.

use super::conf::Conf;
use super::token::{self, Token};
use std::collections::{HashMap, HashSet};

/// Whether a capture comes from `$X` (one word) or `$...X` / `$....X`
/// (a span). Short and long ellipsis variants share this kind because
/// their backreference semantics are the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetavariableKind {
    /// `$X`: binds one word token.
    Plain,
    /// `$...X` or `$....X`: binds a span.
    Ellipsis,
}

/// One node of a parsed codegrep pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A whole word like `hello`.
    Word(String),
    /// A single non-word, non-bracket character like `!`.
    Other(String),
    /// A literal newline. Only meaningful in singleline mode.
    Newline,
    /// `...`: short ellipsis.
    Ellipsis,
    /// `....`: long ellipsis. Crosses newlines in singleline mode.
    LongEllipsis,
    /// `$NAME`: captures one word.
    Metavar(String),
    /// `$...NAME`: captures a short-ellipsis span.
    MetavarEllipsis(String),
    /// `$....NAME`: captures a long-ellipsis span.
    LongMetavarEllipsis(String),
    /// A balanced bracket. `(open, inner, close)`.
    Bracket(char, Vec<Node>, char),
}

/// A parsed pattern carrying the configuration used to tokenize and match it.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub(crate) nodes: Vec<Node>,
    pub(crate) conf: Conf,
}

impl Pattern {
    /// Read-only view of the parsed nodes. Used by tests and the matcher.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The configuration carried with this pattern.
    pub fn conf(&self) -> &Conf {
        &self.conf
    }

    /// Bare names of every metavariable in the pattern.
    pub fn metavariable_names(&self) -> HashSet<String> {
        let mut seen = HashMap::new();
        // parse() already validated consistency, so this walk cannot error.
        let _ = walk_metavars(&self.nodes, &mut seen);
        seen.into_keys().collect()
    }
}

/// A pattern that mixes kinds for the same metavariable name (`$X`
/// and `$...X`) or contains a syntactic issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ParseError {}

impl Pattern {
    /// Parse a pattern string under the given configuration. Returns an
    /// error when the pattern is empty (no matchable nodes) or reuses a
    /// metavariable name across kinds (e.g. `$X` and `$...X`).
    pub fn parse(source: &str, conf: &Conf) -> Result<Pattern, ParseError> {
        let tokens = token::tokenize_pattern(source, conf);
        let mut cursor = 0;
        let nodes = match parse_seq_until(&tokens, &mut cursor, None) {
            Ok(nodes) => nodes,
            Err(_) => unreachable!("top-level parse has no expected close"),
        };
        if nodes.is_empty() {
            return Err(ParseError(
                "empty pattern: provide at least one literal word, metavariable, or ellipsis"
                    .to_string(),
            ));
        }
        validate_metavariable_consistency(&nodes)?;
        Ok(Pattern {
            nodes,
            conf: conf.clone(),
        })
    }
}

fn parse_seq_until(
    tokens: &[Token],
    cursor: &mut usize,
    expected_close: Option<char>,
) -> Result<Vec<Node>, ()> {
    let mut nodes = Vec::new();
    while *cursor < tokens.len() {
        match &tokens[*cursor] {
            Token::Ellipsis => {
                nodes.push(Node::Ellipsis);
                *cursor += 1;
            }
            Token::LongEllipsis => {
                nodes.push(Node::LongEllipsis);
                *cursor += 1;
            }
            Token::Metavar(name) => {
                nodes.push(Node::Metavar(name.clone()));
                *cursor += 1;
            }
            Token::MetavarEllipsis(name) => {
                nodes.push(Node::MetavarEllipsis(name.clone()));
                *cursor += 1;
            }
            Token::LongMetavarEllipsis(name) => {
                nodes.push(Node::LongMetavarEllipsis(name.clone()));
                *cursor += 1;
            }
            Token::Word { text, .. } => {
                nodes.push(Node::Word(text.clone()));
                *cursor += 1;
            }
            Token::Newline { .. } => {
                nodes.push(Node::Newline);
                *cursor += 1;
            }
            Token::Other { text, .. } => {
                nodes.push(Node::Other(text.clone()));
                *cursor += 1;
            }
            Token::Open { open, close, .. } => {
                if expected_close == Some(*open) {
                    *cursor += 1;
                    return Ok(nodes);
                }
                let open = *open;
                let close = *close;
                let saved = *cursor;
                *cursor += 1;
                match parse_seq_until(tokens, cursor, Some(close)) {
                    Ok(inner) => nodes.push(Node::Bracket(open, inner, close)),
                    Err(_) => {
                        *cursor = saved + 1;
                        nodes.push(Node::Other(open.to_string()));
                    }
                }
            }
            Token::Close { close, .. } => {
                if expected_close == Some(*close) {
                    *cursor += 1;
                    return Ok(nodes);
                }
                nodes.push(Node::Other(close.to_string()));
                *cursor += 1;
            }
        }
    }
    match expected_close {
        None => Ok(nodes),
        Some(_) => Err(()),
    }
}

fn validate_metavariable_consistency(nodes: &[Node]) -> Result<(), ParseError> {
    let mut seen: HashMap<String, MetavariableKind> = HashMap::new();
    walk_metavars(nodes, &mut seen)
}

fn walk_metavars(
    nodes: &[Node],
    seen: &mut HashMap<String, MetavariableKind>,
) -> Result<(), ParseError> {
    for node in nodes {
        match node {
            Node::Metavar(name) => record_kind(name, MetavariableKind::Plain, seen)?,
            Node::MetavarEllipsis(name) | Node::LongMetavarEllipsis(name) => {
                record_kind(name, MetavariableKind::Ellipsis, seen)?
            }
            Node::Bracket(_, inner, _) => walk_metavars(inner, seen)?,
            _ => {}
        }
    }
    Ok(())
}

fn record_kind(
    name: &str,
    kind: MetavariableKind,
    seen: &mut HashMap<String, MetavariableKind>,
) -> Result<(), ParseError> {
    match seen.get(name) {
        Some(existing) if existing != &kind => Err(ParseError(format!(
            "inconsistent use of metavariable ${name}"
        ))),
        _ => {
            seen.insert(name.to_string(), kind);
            Ok(())
        }
    }
}
