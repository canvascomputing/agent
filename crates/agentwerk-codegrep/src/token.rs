//! Token types and tokenizer entry points for patterns and targets.
//!
//! Patterns recognise three forms on top of the input grammar: ellipsis
//! (`...`, `....`) and metavariable variants (`$NAME`, `$...NAME`,
//! `$....NAME`). Targets only see words, brackets, newlines, and other
//! characters.

use super::conf::Conf;

/// One unit produced by tokenizing a pattern or a target string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// `...`: pattern-only.
    Ellipsis,
    /// `....`: pattern-only. Crosses newlines in singleline mode.
    LongEllipsis,
    /// `$NAME`: pattern-only.
    Metavar(String),
    /// `$...NAME`: pattern-only.
    MetavarEllipsis(String),
    /// `$....NAME`: pattern-only.
    LongMetavarEllipsis(String),
    /// A run of `word_chars` from the source.
    Word { text: String, start: usize },
    /// An opening bracket; `close` is the expected matching close character.
    Open {
        open: char,
        close: char,
        start: usize,
    },
    /// A closing bracket.
    Close { close: char, start: usize },
    /// Singleline mode only: a literal newline.
    Newline { start: usize },
    /// Any single character not covered by the cases above.
    Other { text: String, start: usize },
}

impl Token {
    pub(crate) fn start(&self) -> usize {
        match self {
            Token::Word { start, .. }
            | Token::Open { start, .. }
            | Token::Close { start, .. }
            | Token::Newline { start }
            | Token::Other { start, .. } => *start,
            Token::Ellipsis
            | Token::LongEllipsis
            | Token::Metavar(_)
            | Token::MetavarEllipsis(_)
            | Token::LongMetavarEllipsis(_) => {
                unreachable!("pattern-only token in target stream")
            }
        }
    }
}

/// Tokenize a pattern string. Recognises ellipsis and metavariable forms
/// in addition to the input grammar.
pub fn tokenize_pattern(source: &str, conf: &Conf) -> Vec<Token> {
    scan(source, conf, true)
}

/// Tokenize a target string. Words, brackets, newlines, and other characters.
pub fn tokenize_target(source: &str, conf: &Conf) -> Vec<Token> {
    scan(source, conf, false)
}

fn scan(source: &str, conf: &Conf, pattern_mode: bool) -> Vec<Token> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut position = 0;
    let mut tokens = Vec::new();

    while position < len {
        if pattern_mode {
            if source[position..].starts_with("....") {
                tokens.push(Token::LongEllipsis);
                position += 4;
                continue;
            }
            if source[position..].starts_with("...") {
                tokens.push(Token::Ellipsis);
                position += 3;
                continue;
            }
            if source[position..].starts_with("$....") {
                if let Some((name, name_len)) = read_metavar_name(&source[position + 5..]) {
                    tokens.push(Token::LongMetavarEllipsis(name));
                    position += 5 + name_len;
                    continue;
                }
            }
            if source[position..].starts_with("$...") {
                if let Some((name, name_len)) = read_metavar_name(&source[position + 4..]) {
                    tokens.push(Token::MetavarEllipsis(name));
                    position += 4 + name_len;
                    continue;
                }
            }
            if bytes[position] == b'$' {
                if let Some((name, name_len)) = read_metavar_name(&source[position + 1..]) {
                    tokens.push(Token::Metavar(name));
                    position += 1 + name_len;
                    continue;
                }
            }
        }

        let ch = source[position..].chars().next().unwrap();

        if is_blank(ch, conf.multiline) {
            position += ch.len_utf8();
            continue;
        }

        if conf.word_chars.contains(&ch) {
            let start = position;
            let mut text = String::new();
            while position < len {
                let next = source[position..].chars().next().unwrap();
                if !conf.word_chars.contains(&next) {
                    break;
                }
                text.push(next);
                position += next.len_utf8();
            }
            tokens.push(Token::Word { text, start });
            continue;
        }

        if let Some((open, close)) = conf.brackets.iter().find(|(o, _)| *o == ch) {
            tokens.push(Token::Open {
                open: *open,
                close: *close,
                start: position,
            });
            position += ch.len_utf8();
            continue;
        }

        if let Some((_, close)) = conf.brackets.iter().find(|(_, c)| *c == ch) {
            tokens.push(Token::Close {
                close: *close,
                start: position,
            });
            position += ch.len_utf8();
            continue;
        }

        if !conf.multiline && (ch == '\n' || ch == '\r') {
            let start = position;
            if ch == '\r' && bytes.get(position + 1) == Some(&b'\n') {
                position += 2;
            } else if ch == '\n' {
                position += 1;
            } else {
                tokens.push(Token::Other {
                    text: ch.to_string(),
                    start,
                });
                position += 1;
                continue;
            }
            tokens.push(Token::Newline { start });
            continue;
        }

        tokens.push(Token::Other {
            text: ch.to_string(),
            start: position,
        });
        position += ch.len_utf8();
    }

    tokens
}

fn is_blank(ch: char, multiline: bool) -> bool {
    match ch {
        ' ' | '\t' => true,
        '\n' | '\r' => multiline,
        _ => false,
    }
}

fn read_metavar_name(rest: &str) -> Option<(String, usize)> {
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if !is_name_start(first) {
        return None;
    }
    let mut name = String::new();
    name.push(first);
    let mut consumed = first.len_utf8();
    for (idx, c) in chars {
        if !is_name_continue(c) {
            consumed = idx;
            return Some((name, consumed));
        }
        name.push(c);
        consumed = idx + c.len_utf8();
    }
    Some((name, consumed))
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn singleline() -> Conf {
        Conf::default_singleline()
    }

    fn multiline() -> Conf {
        Conf::default_multiline()
    }

    #[test]
    fn tokenize_pattern_emits_ellipsis_token_for_three_dots() {
        assert_eq!(
            tokenize_pattern("...", &singleline()),
            vec![Token::Ellipsis],
        );
    }

    #[test]
    fn tokenize_pattern_emits_long_ellipsis_token_for_four_dots() {
        assert_eq!(
            tokenize_pattern("....", &singleline()),
            vec![Token::LongEllipsis],
        );
    }

    #[test]
    fn tokenize_pattern_emits_metavar_token_for_dollar_followed_by_name() {
        assert_eq!(
            tokenize_pattern("$NAME", &singleline()),
            vec![Token::Metavar("NAME".to_string())],
        );
    }

    #[test]
    fn tokenize_pattern_emits_metavar_ellipsis_token_for_dollar_three_dots_name() {
        assert_eq!(
            tokenize_pattern("$...X", &singleline()),
            vec![Token::MetavarEllipsis("X".to_string())],
        );
    }

    #[test]
    fn tokenize_pattern_emits_long_metavar_ellipsis_for_dollar_four_dots_name() {
        assert_eq!(
            tokenize_pattern("$....X", &singleline()),
            vec![Token::LongMetavarEllipsis("X".to_string())],
        );
    }

    #[test]
    fn tokenize_pattern_treats_bare_dollar_without_name_as_other_char() {
        assert_eq!(
            tokenize_pattern("$", &singleline()),
            vec![Token::Other {
                text: "$".to_string(),
                start: 0
            }],
        );
    }

    #[test]
    fn tokenize_pattern_collapses_word_char_run_into_one_word_token() {
        let tokens = tokenize_pattern("hello", &singleline());
        assert_eq!(
            tokens,
            vec![Token::Word {
                text: "hello".to_string(),
                start: 0
            }],
        );
    }

    #[test]
    fn tokenize_pattern_skips_spaces_between_words() {
        let tokens = tokenize_pattern("a b", &singleline());
        assert_eq!(
            tokens,
            vec![
                Token::Word {
                    text: "a".to_string(),
                    start: 0
                },
                Token::Word {
                    text: "b".to_string(),
                    start: 2
                },
            ],
        );
    }

    #[test]
    fn tokenize_pattern_singleline_emits_newline_token_for_line_break() {
        let tokens = tokenize_pattern("a\nb", &singleline());
        assert_eq!(
            tokens,
            vec![
                Token::Word {
                    text: "a".to_string(),
                    start: 0
                },
                Token::Newline { start: 1 },
                Token::Word {
                    text: "b".to_string(),
                    start: 2
                },
            ],
        );
    }

    #[test]
    fn tokenize_pattern_singleline_treats_crlf_as_single_newline_token() {
        let tokens = tokenize_pattern("a\r\nb", &singleline());
        assert_eq!(
            tokens,
            vec![
                Token::Word {
                    text: "a".to_string(),
                    start: 0
                },
                Token::Newline { start: 1 },
                Token::Word {
                    text: "b".to_string(),
                    start: 3
                },
            ],
        );
    }

    #[test]
    fn tokenize_pattern_multiline_treats_newline_as_whitespace() {
        let tokens = tokenize_pattern("a\nb", &multiline());
        assert!(!tokens.iter().any(|t| matches!(t, Token::Newline { .. })));
    }

    #[test]
    fn tokenize_pattern_emits_open_and_close_token_for_balanced_parens() {
        let tokens = tokenize_pattern("(x)", &multiline());
        assert_eq!(
            tokens,
            vec![
                Token::Open {
                    open: '(',
                    close: ')',
                    start: 0
                },
                Token::Word {
                    text: "x".to_string(),
                    start: 1
                },
                Token::Close {
                    close: ')',
                    start: 2
                },
            ],
        );
    }

    #[test]
    fn tokenize_target_treats_dollar_name_as_other_chars_not_metavar() {
        let tokens = tokenize_target("$X", &multiline());
        assert_eq!(
            tokens,
            vec![
                Token::Other {
                    text: "$".to_string(),
                    start: 0
                },
                Token::Word {
                    text: "X".to_string(),
                    start: 1
                },
            ],
        );
    }

    #[test]
    fn tokenize_target_treats_three_dots_as_three_other_chars_not_ellipsis() {
        let tokens = tokenize_target("...", &multiline());
        assert_eq!(tokens.len(), 3);
        assert!(tokens
            .iter()
            .all(|t| matches!(t, Token::Other { text, .. } if text == ".")));
    }

    #[test]
    fn tokenize_target_preserves_byte_positions_of_multi_byte_chars() {
        let tokens = tokenize_target("a ñ b", &multiline());
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].start(), 0);
        assert_eq!(tokens[1].start(), 2);
        assert_eq!(tokens[2].start(), 5);
    }
}
