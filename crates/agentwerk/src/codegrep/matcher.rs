//! Searches tokenized source and returns structural matches and locations.

use super::ast::{MetavariableKind, Node, Pattern};
use super::token::{self, Token};
use std::collections::{HashMap, HashSet};

/// Byte-range location into the target string, plus the matched substring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loc {
    /// Zero-based byte offset where the match starts.
    pub start: usize,
    /// Length of the match in bytes.
    pub length: usize,
    /// Exact slice of the target covered by the match.
    pub substring: String,
}

/// A captured metavariable: name without the leading `$` and its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metavariable {
    /// Whether the metavariable captures one word or a span.
    pub kind: MetavariableKind,
    /// Name without the leading `$` or ellipsis.
    pub bare_name: String,
}

/// One match of a pattern against a target string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Location of the complete match.
    pub loc: Loc,
    /// Metavariables and the locations they captured, in token order.
    pub captures: Vec<(Metavariable, Loc)>,
}

#[derive(Debug, Clone)]
struct MatchParams {
    caseless: bool,
    multiline: bool,
    word_chars: HashSet<char>,
}

#[derive(Debug, Clone)]
struct Binding {
    kind: MetavariableKind,
    token_start: usize,
    byte_start: usize,
    byte_end: usize,
}

#[derive(Debug, Default, Clone)]
struct MetavarEnv {
    bindings: HashMap<String, Binding>,
}

/// Find all non-overlapping matches of `pattern` in `target`.
pub fn search(pattern: &Pattern, target: &str) -> Vec<Match> {
    let tokens = token::tokenize_target(target, pattern.conf());
    search_tokens(pattern, &tokens, target)
}

/// Like [`search`], but reuses tokens the caller already built. Running a whole
/// pattern catalogue over one file tokenizes it once here instead of once per
/// [`search`] call.
///
/// `tokens` must be [`token::tokenize_target`] of this `target`. Its `Conf` need
/// only match `pattern.conf()` in word and bracket rules, the parts tokenizing
/// depends on: patterns differing only in `caseless` or `multiline` share tokens
/// safely. Other word or bracket rules yield silently wrong matches.
pub fn search_tokens(pattern: &Pattern, tokens: &[Token], target: &str) -> Vec<Match> {
    let conf = pattern.conf();
    let params = MatchParams {
        caseless: conf.caseless,
        multiline: conf.multiline,
        word_chars: conf.word_chars.iter().copied().collect(),
    };
    let mut out = Vec::new();
    let mut pos = 0;
    while pos <= tokens.len() {
        let mut env = MetavarEnv::default();
        if let Some(end) = match_seq(
            pattern.nodes(),
            tokens,
            pos,
            target,
            &mut env,
            None,
            None,
            None,
            &params,
            None,
        ) {
            out.push(build_match(tokens, target, pos, end, &env));
            pos = if end == pos { pos + 1 } else { end };
        } else {
            pos += 1;
        }
    }
    out
}

fn build_match(
    tokens: &[Token],
    target: &str,
    start: usize,
    end: usize,
    env: &MetavarEnv,
) -> Match {
    let start_byte = byte_start_of(tokens, target, start);
    // An empty match (end == start) spans one point, but byte_end_of would read
    // the previous token's end. In "a  b" an empty match at "b" has start_byte 3
    // and byte_end_of 1, so 1 - 3 underflows; pin end_byte to start_byte instead.
    let end_byte = if end == start {
        start_byte
    } else {
        byte_end_of(tokens, target, end)
    };
    let mut entries: Vec<(&String, &Binding)> = env.bindings.iter().collect();
    entries.sort_by_key(|(_, b)| b.token_start);
    let captures = entries
        .into_iter()
        .map(|(name, binding)| {
            let loc = Loc {
                start: binding.byte_start,
                length: binding.byte_end - binding.byte_start,
                substring: target[binding.byte_start..binding.byte_end].to_string(),
            };
            (
                Metavariable {
                    kind: binding.kind.clone(),
                    bare_name: name.clone(),
                },
                loc,
            )
        })
        .collect();
    Match {
        loc: Loc {
            start: start_byte,
            length: end_byte - start_byte,
            substring: target[start_byte..end_byte].to_string(),
        },
        captures,
    }
}

fn byte_start_of(tokens: &[Token], target: &str, token_idx: usize) -> usize {
    if token_idx < tokens.len() {
        tokens[token_idx].start()
    } else {
        target.len()
    }
}

fn byte_end_of(tokens: &[Token], target: &str, token_idx_exclusive: usize) -> usize {
    if token_idx_exclusive == 0 {
        return 0;
    }
    let last = &tokens[token_idx_exclusive - 1];
    match last {
        Token::Word { start, text } | Token::Other { start, text } => start + text.len(),
        Token::Open { start, open, .. } => start + open.len_utf8(),
        Token::Close { start, close, .. } => start + close.len_utf8(),
        Token::Newline { start } => {
            if target.as_bytes().get(*start) == Some(&b'\r') {
                start + 2
            } else {
                start + 1
            }
        }
        Token::Ellipsis
        | Token::LongEllipsis
        | Token::Metavar(_)
        | Token::MetavarEllipsis(_)
        | Token::LongMetavarEllipsis(_) => {
            unreachable!("pattern-only token in target stream")
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn match_seq(
    nodes: &[Node],
    tokens: &[Token],
    position: usize,
    target: &str,
    env: &mut MetavarEnv,
    excluded_close: Option<char>,
    outer_prev: Option<&Node>,
    outer_next: Option<&Node>,
    params: &MatchParams,
    close_after: Option<char>,
) -> Option<usize> {
    if nodes.is_empty() {
        return consume_close_after(tokens, position, close_after);
    }
    let first = &nodes[0];
    let rest = &nodes[1..];
    let prev = outer_prev;
    let next_for_first = rest.first().or(outer_next);

    if !check_left_anchor(first, prev, tokens, target, position, params) {
        return None;
    }

    match first {
        Node::Ellipsis | Node::LongEllipsis => {
            let allow_newlines = matches!(first, Node::LongEllipsis) || params.multiline;
            return match_ellipsis(
                first,
                tokens,
                position,
                rest,
                target,
                env,
                excluded_close,
                next_for_first,
                outer_next,
                params,
                allow_newlines,
                None,
                close_after,
            );
        }
        Node::MetavarEllipsis(name) | Node::LongMetavarEllipsis(name) => {
            let allow_newlines = matches!(first, Node::LongMetavarEllipsis(_)) || params.multiline;
            if let Some(binding) = env.bindings.get(name).cloned() {
                let end = match_ellipsis_backref(tokens, target, position, &binding, params)?;
                if !check_right_anchor(first, next_for_first, tokens, target, end, params) {
                    return None;
                }
                return match_seq(
                    rest,
                    tokens,
                    end,
                    target,
                    env,
                    excluded_close,
                    Some(first),
                    outer_next,
                    params,
                    close_after,
                );
            }
            return match_ellipsis(
                first,
                tokens,
                position,
                rest,
                target,
                env,
                excluded_close,
                next_for_first,
                outer_next,
                params,
                allow_newlines,
                Some(name.clone()),
                close_after,
            );
        }
        _ => {}
    }

    let cursor = match_node(first, tokens, position, target, env, params)?;
    if !check_right_anchor(first, next_for_first, tokens, target, cursor, params) {
        return None;
    }
    match_seq(
        rest,
        tokens,
        cursor,
        target,
        env,
        excluded_close,
        Some(first),
        outer_next,
        params,
        close_after,
    )
}

fn consume_close_after(
    tokens: &[Token],
    position: usize,
    close_after: Option<char>,
) -> Option<usize> {
    match close_after {
        None => Some(position),
        Some(c) => {
            let tok = tokens.get(position)?;
            if is_closing_token(tok, c) {
                Some(position + 1)
            } else {
                None
            }
        }
    }
}

fn is_closing_token(token: &Token, close_char: char) -> bool {
    match token {
        Token::Close { close, .. } => *close == close_char,
        Token::Open { open, close, .. } => *open == *close && *close == close_char,
        _ => false,
    }
}

fn match_node(
    node: &Node,
    tokens: &[Token],
    cursor: usize,
    target: &str,
    env: &mut MetavarEnv,
    params: &MatchParams,
) -> Option<usize> {
    let token = tokens.get(cursor)?;
    match node {
        Node::Word(text) => {
            if let Token::Word { text: t, .. } = token {
                if word_eq(text, t, params.caseless) {
                    return Some(cursor + 1);
                }
            }
            None
        }
        Node::Other(text) => {
            if let Token::Other { text: t, .. } = token {
                if t == text {
                    return Some(cursor + 1);
                }
            }
            None
        }
        Node::Newline => {
            if matches!(token, Token::Newline { .. }) {
                Some(cursor + 1)
            } else {
                None
            }
        }
        Node::Bracket(open, inner, close) => {
            if let Token::Open {
                open: token_open,
                close: token_close,
                ..
            } = token
            {
                if token_open == open && token_close == close {
                    let bracket_node = node;
                    return match_seq(
                        inner,
                        tokens,
                        cursor + 1,
                        target,
                        env,
                        Some(*close),
                        Some(bracket_node),
                        Some(bracket_node),
                        params,
                        Some(*close),
                    );
                }
            }
            None
        }
        Node::Metavar(name) => {
            if let Token::Word { text, start } = token {
                if let Some(binding) = env.bindings.get(name) {
                    if word_eq(
                        &target[binding.byte_start..binding.byte_end],
                        text,
                        params.caseless,
                    ) {
                        return Some(cursor + 1);
                    }
                    return None;
                }
                let byte_start = *start;
                let byte_end = byte_start + text.len();
                env.bindings.insert(
                    name.clone(),
                    Binding {
                        kind: MetavariableKind::Plain,
                        token_start: cursor,
                        byte_start,
                        byte_end,
                    },
                );
                return Some(cursor + 1);
            }
            None
        }
        Node::Ellipsis
        | Node::LongEllipsis
        | Node::MetavarEllipsis(_)
        | Node::LongMetavarEllipsis(_) => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn match_ellipsis(
    current: &Node,
    tokens: &[Token],
    start: usize,
    rest: &[Node],
    target: &str,
    env: &mut MetavarEnv,
    excluded_close: Option<char>,
    next_for_anchor: Option<&Node>,
    outer_next: Option<&Node>,
    params: &MatchParams,
    allow_newlines: bool,
    bind_name: Option<String>,
    close_after: Option<char>,
) -> Option<usize> {
    let mut current_end = start;
    loop {
        if check_right_anchor(
            current,
            next_for_anchor,
            tokens,
            target,
            current_end,
            params,
        ) {
            let snapshot = env.clone();
            if let Some(name) = &bind_name {
                let (byte_start, byte_end) = if start == current_end {
                    let position = byte_start_of(tokens, target, start);
                    (position, position)
                } else {
                    (
                        byte_start_of(tokens, target, start),
                        byte_end_of(tokens, target, current_end),
                    )
                };
                env.bindings.insert(
                    name.clone(),
                    Binding {
                        kind: MetavariableKind::Ellipsis,
                        token_start: start,
                        byte_start,
                        byte_end,
                    },
                );
            }
            if let Some(final_end) = match_seq(
                rest,
                tokens,
                current_end,
                target,
                env,
                excluded_close,
                Some(current),
                outer_next,
                params,
                close_after,
            ) {
                return Some(final_end);
            }
            *env = snapshot;
        }
        let token = tokens.get(current_end)?;
        if is_excluded_close(token, excluded_close) {
            return None;
        }
        if matches!(token, Token::Newline { .. }) && !allow_newlines {
            return None;
        }
        let next_end = match token {
            Token::Open { close, .. } => {
                match find_matching_close(tokens, current_end, *close, allow_newlines) {
                    Some(end_idx) => end_idx + 1,
                    None => current_end + 1,
                }
            }
            _ => current_end + 1,
        };
        if next_end == current_end {
            return None;
        }
        current_end = next_end;
    }
}

fn is_excluded_close(token: &Token, excluded_close: Option<char>) -> bool {
    let Some(c) = excluded_close else {
        return false;
    };
    is_closing_token(token, c)
}

fn find_matching_close(
    tokens: &[Token],
    start: usize,
    target_close: char,
    allow_newlines: bool,
) -> Option<usize> {
    let mut stack: Vec<char> = vec![target_close];
    let mut i = start + 1;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Open { open, close, .. } => {
                if open == close && stack.last() == Some(close) {
                    stack.pop();
                    if stack.is_empty() {
                        return Some(i);
                    }
                } else {
                    stack.push(*close);
                }
            }
            Token::Close { close, .. } => {
                if stack.last() == Some(close) {
                    stack.pop();
                    if stack.is_empty() {
                        return Some(i);
                    }
                }
            }
            Token::Newline { .. } if !allow_newlines => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

fn match_ellipsis_backref(
    tokens: &[Token],
    target: &str,
    cursor: usize,
    binding: &Binding,
    params: &MatchParams,
) -> Option<usize> {
    let captured = &target[binding.byte_start..binding.byte_end];
    let here_start = byte_start_of(tokens, target, cursor);
    let here_end = here_start + captured.len();
    if here_end > target.len() {
        return None;
    }
    // here_end is byte arithmetic; if it splits a multibyte char the backref cannot
    // align here, and the &str slices below would panic on a non-boundary index.
    if !target.is_char_boundary(here_end) {
        return None;
    }
    if &target[here_start..here_end] != captured {
        return None;
    }
    if !byte_boundary_ok(target, here_start, &params.word_chars) {
        return None;
    }
    if !byte_boundary_ok(target, here_end, &params.word_chars) {
        return None;
    }
    let mut end_cursor = cursor;
    while end_cursor < tokens.len() && tokens[end_cursor].start() < here_end {
        end_cursor += 1;
    }
    if here_end != here_start && byte_end_of(tokens, target, end_cursor) != here_end {
        return None;
    }
    Some(end_cursor)
}

fn byte_boundary_ok(target: &str, byte_pos: usize, word_chars: &HashSet<char>) -> bool {
    let bytes = target.as_bytes();
    let before_is_word = if byte_pos == 0 {
        false
    } else {
        char_ending_at(target, byte_pos)
            .map(|c| word_chars.contains(&c))
            .unwrap_or(false)
    };
    let after_is_word = if byte_pos >= bytes.len() {
        false
    } else {
        target[byte_pos..]
            .chars()
            .next()
            .map(|c| word_chars.contains(&c))
            .unwrap_or(false)
    };
    !(before_is_word && after_is_word)
}

fn char_ending_at(target: &str, byte_pos: usize) -> Option<char> {
    if byte_pos == 0 {
        return None;
    }
    target[..byte_pos].chars().next_back()
}

fn check_left_anchor(
    current: &Node,
    prev: Option<&Node>,
    tokens: &[Token],
    target: &str,
    cursor: usize,
    params: &MatchParams,
) -> bool {
    let must_input = prev.is_none() && is_multiline_ellipsis(current, params);
    let must_line = (prev.is_none() || matches!(prev, Some(Node::Newline)))
        && is_singleline_ellipsis(current, params);
    let byte = byte_start_of(tokens, target, cursor);
    if must_input && byte != 0 {
        return false;
    }
    if must_line {
        if byte == 0 {
            return true;
        }
        if target.as_bytes().get(byte - 1) != Some(&b'\n') {
            return false;
        }
    }
    true
}

fn check_right_anchor(
    current: &Node,
    next: Option<&Node>,
    tokens: &[Token],
    target: &str,
    cursor: usize,
    params: &MatchParams,
) -> bool {
    let must_input = next.is_none() && is_multiline_ellipsis(current, params);
    let must_line = (next.is_none() || matches!(next, Some(Node::Newline)))
        && is_singleline_ellipsis(current, params);
    let byte = byte_start_of(tokens, target, cursor);
    if must_input && byte != target.len() {
        return false;
    }
    if must_line {
        if byte == target.len() {
            return true;
        }
        let bytes = target.as_bytes();
        let here = bytes.get(byte);
        if here != Some(&b'\n') && !(here == Some(&b'\r') && bytes.get(byte + 1) == Some(&b'\n')) {
            return false;
        }
    }
    true
}

fn is_singleline_ellipsis(node: &Node, params: &MatchParams) -> bool {
    matches!(node, Node::Ellipsis | Node::MetavarEllipsis(_)) && !params.multiline
}

fn is_multiline_ellipsis(node: &Node, params: &MatchParams) -> bool {
    matches!(node, Node::LongEllipsis | Node::LongMetavarEllipsis(_))
        || (matches!(node, Node::Ellipsis | Node::MetavarEllipsis(_)) && params.multiline)
}

fn word_eq(a: &str, b: &str, caseless: bool) -> bool {
    if caseless {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}
