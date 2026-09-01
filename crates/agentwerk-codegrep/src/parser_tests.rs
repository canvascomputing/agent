//! Tests for the pattern parser.
//!
//! Each test parses a pattern string under a configuration and asserts
//! the resulting node sequence.

use super::ast::{Node, Pattern};
use super::conf::Conf;

fn parse_nodes(conf: &Conf, source: &str) -> Vec<Node> {
    Pattern::parse(source, conf)
        .expect("pattern parses")
        .nodes()
        .to_vec()
}

fn word(text: &str) -> Node {
    Node::Word(text.to_string())
}

fn other(text: &str) -> Node {
    Node::Other(text.to_string())
}

fn metavar(name: &str) -> Node {
    Node::Metavar(name.to_string())
}

fn bracket(open: char, inner: Vec<Node>, close: char) -> Node {
    Node::Bracket(open, inner, close)
}

#[test]
fn parses_literal_word_followed_by_other_char() {
    let conf = Conf::default_singleline();
    assert_eq!(
        parse_nodes(&conf, "a bc!"),
        vec![word("a"), word("bc"), other("!")],
    );
}

#[test]
fn parses_dollar_followed_by_name_as_metavar_node() {
    let conf = Conf::default_singleline();
    assert_eq!(
        parse_nodes(&conf, "$A $A $BB"),
        vec![metavar("A"), metavar("A"), metavar("BB")],
    );
}

#[test]
fn parses_three_dots_as_ellipsis_node() {
    let conf = Conf::default_singleline();
    assert_eq!(
        parse_nodes(&conf, "a ... b"),
        vec![word("a"), Node::Ellipsis, word("b")],
    );
}

#[test]
fn parses_four_dots_as_long_ellipsis_node() {
    let conf = Conf::default_singleline();
    assert_eq!(
        parse_nodes(&conf, "a .... b"),
        vec![word("a"), Node::LongEllipsis, word("b")],
    );
}

#[test]
fn parses_nested_brackets_into_bracket_tree() {
    let conf = Conf::default_singleline();
    assert_eq!(
        parse_nodes(&conf, "([x])"),
        vec![bracket('(', vec![bracket('[', vec![word("x")], ']')], ')')],
    );
}

#[test]
fn parses_orphan_close_inside_bracket_as_other_child() {
    let conf = Conf::default_singleline();
    assert_eq!(
        parse_nodes(&conf, "(})"),
        vec![bracket('(', vec![other("}")], ')')],
    );
}

#[test]
fn parses_unclosed_open_paren_as_other() {
    let conf = Conf::default_singleline();
    assert_eq!(parse_nodes(&conf, "("), vec![other("(")]);
}

#[test]
fn parses_bare_close_brace_as_other() {
    let conf = Conf::default_singleline();
    assert_eq!(parse_nodes(&conf, "}"), vec![other("}")]);
}

#[test]
fn parses_unmatched_open_and_close_as_two_other_nodes() {
    let conf = Conf::default_singleline();
    assert_eq!(parse_nodes(&conf, "(}"), vec![other("("), other("}")]);
}

#[test]
fn parses_mismatched_braces_inside_outer_bracket_as_other_children() {
    let conf = Conf::default_singleline();
    assert_eq!(
        parse_nodes(&conf, "[(}]"),
        vec![bracket('[', vec![other("("), other("}")], ']')],
    );
}

#[test]
fn singleline_parses_empty_quoted_string_as_self_closing_bracket() {
    let conf = Conf::default_singleline();
    assert_eq!(parse_nodes(&conf, "''"), vec![bracket('\'', vec![], '\'')]);
}

#[test]
fn singleline_parses_quoted_word_as_bracket_with_inner_word() {
    let conf = Conf::default_singleline();
    assert_eq!(
        parse_nodes(&conf, "'ab'"),
        vec![bracket('\'', vec![word("ab")], '\'')],
    );
}

#[test]
fn singleline_parses_quote_inside_quote_as_nested_brackets() {
    let conf = Conf::default_singleline();
    assert_eq!(
        parse_nodes(&conf, "'a\"b\"'"),
        vec![bracket(
            '\'',
            vec![word("a"), bracket('"', vec![word("b")], '"')],
            '\'',
        )],
    );
}

#[test]
fn multiline_treats_quotes_as_ordinary_chars_not_brackets() {
    let conf = Conf::default_multiline();
    assert_eq!(
        parse_nodes(&conf, "'a\"b\"'"),
        vec![
            other("'"),
            word("a"),
            other("\""),
            word("b"),
            other("\""),
            other("'"),
        ],
    );
}

#[test]
fn parse_returns_error_when_metavar_name_is_used_with_two_kinds() {
    let conf = Conf::default_singleline();
    assert!(Pattern::parse("$X ... $...X", &conf).is_err());
}

#[test]
fn parse_rejects_empty_pattern_with_no_matchable_nodes() {
    let conf = Conf::default_singleline();
    assert!(Pattern::parse("", &conf).is_err());
    assert!(Pattern::parse("   ", &conf).is_err());
}

#[test]
fn pattern_conf_returns_the_passed_configuration() {
    let conf = Conf::default_multiline();
    let pattern = Pattern::parse("hello", &conf).expect("parses");
    assert!(pattern.conf().multiline);
    assert!(!pattern.conf().caseless);
}
