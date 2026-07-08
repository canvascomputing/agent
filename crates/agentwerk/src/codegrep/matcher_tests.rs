//! Matcher behaviour tests.
//!
//! Each test parses one pattern, runs `search`, and asserts a single
//! observable outcome through the public `Match` / `Loc` / `Metavariable`
//! surface.

use super::ast::{MetavariableKind, Pattern};
use super::conf::Conf;
use super::matcher::{search, search_tokens, Match};
use super::token::tokenize_target;

fn parse_and_search(conf: &Conf, pattern: &str, target: &str) -> Vec<Match> {
    let parsed = Pattern::parse(pattern, conf).expect("pattern parses");
    search(&parsed, target)
}

fn capture_value<'a>(matches: &'a [Match], name: &str) -> Option<&'a str> {
    matches.iter().find_map(|m| {
        m.captures
            .iter()
            .find(|(metavariable, _)| metavariable.bare_name == name)
            .map(|(_, loc)| loc.substring.as_str())
    })
}

fn singleline() -> Conf {
    Conf::default_singleline()
}

fn multiline() -> Conf {
    Conf::default_multiline()
}

// Match location reporting.

#[test]
fn match_loc_reports_byte_start_and_length_of_substring() {
    let matches = parse_and_search(&singleline(), "world", "hello world");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.start, 6);
    assert_eq!(matches[0].loc.length, 5);
    assert_eq!(matches[0].loc.substring, "world");
}

#[test]
fn captures_are_returned_in_token_order() {
    let matches = parse_and_search(&singleline(), "$A $B $C", "one two three");
    assert_eq!(matches.len(), 1);
    let captures = &matches[0].captures;
    assert_eq!(captures.len(), 3);
    assert_eq!(captures[0].0.bare_name, "A");
    assert_eq!(captures[1].0.bare_name, "B");
    assert_eq!(captures[2].0.bare_name, "C");
}

// Word matching.

#[test]
fn matches_a_single_literal_word() {
    let matches = parse_and_search(&singleline(), "a", "a b c");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a");
}

#[test]
fn matches_the_same_word_at_two_non_overlapping_positions() {
    let matches = parse_and_search(&singleline(), "ab", "ab c ab");
    assert_eq!(matches.len(), 2);
    assert!(matches.iter().all(|m| m.loc.substring == "ab"));
}

#[test]
fn does_not_match_a_word_when_target_only_contains_it_as_a_substring() {
    let matches = parse_and_search(&singleline(), "ab", "xabx");
    assert_eq!(matches.len(), 0);
}

#[test]
fn does_not_match_a_shorter_word_against_a_longer_word_token() {
    let matches = parse_and_search(&singleline(), "ab", "ab abc");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "ab");
}

// Whitespace.

#[test]
fn singleline_collapses_multiple_spaces_when_matching_two_words() {
    let matches = parse_and_search(&singleline(), "a b", "a  b");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a  b");
}

#[test]
fn singleline_collapses_pattern_double_space_to_match_single_space_target() {
    let matches = parse_and_search(&singleline(), "a  b", "a b");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a b");
}

#[test]
fn singleline_pattern_with_newline_does_not_match_space_in_target() {
    let matches = parse_and_search(&singleline(), "a\nb", "a b");
    assert_eq!(matches.len(), 0);
}

#[test]
fn singleline_pattern_with_space_does_not_match_newline_in_target() {
    let matches = parse_and_search(&singleline(), "a b", "a\nb");
    assert_eq!(matches.len(), 0);
}

#[test]
fn multiline_treats_pattern_newline_as_whitespace_against_space_target() {
    let matches = parse_and_search(&multiline(), "a\nb", "a b");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a b");
}

#[test]
fn multiline_treats_target_newline_as_whitespace_against_space_pattern() {
    let matches = parse_and_search(&multiline(), "a b", "a\nb");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a\nb");
}

// Ellipsis.

#[test]
fn ellipsis_consumes_whitespace_between_anchor_words() {
    let matches = parse_and_search(&singleline(), "a...b", "a b");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a b");
}

#[test]
fn ellipsis_consumes_a_single_non_word_char_between_anchor_words() {
    let matches = parse_and_search(&singleline(), "a...b", "a/b");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a/b");
}

#[test]
fn ellipsis_consumes_multiple_word_tokens_between_anchor_words() {
    let matches = parse_and_search(&singleline(), "a...b", "a x y b");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a x y b");
}

#[test]
fn ellipsis_matches_lazily_stopping_at_the_first_anchor_word() {
    let matches = parse_and_search(&singleline(), "a...b", "a b b");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a b");
}

#[test]
fn ellipsis_backtracks_when_first_lazy_attempt_does_not_satisfy_trailing_anchors() {
    let matches = parse_and_search(&singleline(), "a...b b c", "a b b b c");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a b b b c");
}

#[test]
fn singleline_short_ellipsis_does_not_cross_newline() {
    let matches = parse_and_search(&singleline(), "a...b", "a\nb");
    assert_eq!(matches.len(), 0);
}

#[test]
fn multiline_short_ellipsis_crosses_newlines() {
    let matches = parse_and_search(&multiline(), "a...b", "a\nx\nx\nb");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a\nx\nx\nb");
}

// Long ellipsis.

#[test]
fn singleline_long_ellipsis_crosses_a_newline() {
    let matches = parse_and_search(&singleline(), "a....b", "a\nb");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a\nb");
}

#[test]
fn multiline_long_ellipsis_crosses_multiple_newlines() {
    let matches = parse_and_search(&multiline(), "a....b", "a\nx\nx\nb");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a\nx\nx\nb");
}

// Plain metavariables.

#[test]
fn plain_metavar_captures_one_word_token() {
    let matches = parse_and_search(&singleline(), "a $X b", "a xy b");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a xy b");
    assert_eq!(capture_value(&matches, "X"), Some("xy"));
    assert_eq!(matches[0].captures[0].0.kind, MetavariableKind::Plain);
}

#[test]
fn metavar_name_accepts_digits_and_underscore_after_first_letter() {
    let matches = parse_and_search(&singleline(), "a $AB_4! b", "a xy! b");
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "AB_4"), Some("xy"));
}

#[test]
fn bare_dollar_followed_by_space_is_treated_as_literal_dollar() {
    let matches = parse_and_search(&singleline(), "$ X", "$ X");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "$ X");
    assert!(matches[0].captures.is_empty());
}

#[test]
fn two_adjacent_metavars_match_two_consecutive_words_non_overlapping() {
    let matches = parse_and_search(&singleline(), "$A $B", "1 2 3 4");
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].loc.substring, "1 2");
    assert_eq!(matches[1].loc.substring, "3 4");
}

// Structural balanced brackets inside an ellipsis.

#[test]
fn ellipsis_steps_over_a_balanced_bracket_in_the_target() {
    let matches = parse_and_search(&singleline(), "x...x", "x [x] x");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "x [x] x");
}

#[test]
fn ellipsis_treats_mismatched_close_inside_unbalanced_open_as_content() {
    let matches = parse_and_search(&singleline(), "x...x", "x ([)x])x");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "x ([)x])x");
}

#[test]
fn ellipsis_inside_bracket_pattern_steps_over_nested_brackets() {
    let matches = parse_and_search(&singleline(), "f(...)", "f(((x)))");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "f(((x)))");
}

#[test]
fn singleline_ellipsis_inside_quoted_pattern_steps_over_balanced_quotes() {
    let matches = parse_and_search(&singleline(), "\"...\"", "\"(\"\")\"");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "\"(\"\")\"");
}

#[test]
fn singleline_ellipsis_inside_paren_pattern_steps_over_quoted_close_paren() {
    let matches = parse_and_search(&singleline(), "(...)", "(\")\")");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "(\")\")");
}

#[test]
fn singleline_ellipsis_treats_mixed_balanced_quotes_as_one_unit() {
    let matches = parse_and_search(&singleline(), "x...x", "x \"'x' x\" x x");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "x \"'x' x\" x");
}

#[test]
fn multiline_paren_pattern_does_not_treat_quotes_as_brackets() {
    let matches = parse_and_search(&multiline(), "(...)", "(\")\")");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "(\")");
}

// Explicit bracket nodes.

#[test]
fn bracket_pattern_matches_the_first_balanced_pair_in_the_target() {
    let matches = parse_and_search(&singleline(), "(...)", "())");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "()");
}

#[test]
fn bracket_pattern_rejects_when_inner_ellipsis_cannot_leave_room_for_close() {
    let matches = parse_and_search(&singleline(), "(... x)", "()x)");
    assert_eq!(matches.len(), 0);
}

#[test]
fn bracket_pattern_matches_when_inner_is_a_balanced_subbracket() {
    let matches = parse_and_search(&singleline(), "(...)", "([])");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "([])");
}

#[test]
fn bracket_pattern_treats_an_unbalanced_inner_open_as_content() {
    let matches = parse_and_search(&singleline(), "(...)", "([)");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "([)");
}

#[test]
fn bracket_pattern_treats_an_orphan_inner_close_as_content() {
    let matches = parse_and_search(&singleline(), "(...)", "(])");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "(])");
}

#[test]
fn bracket_pattern_rejects_when_inner_subbracket_balances_only_via_outer_close() {
    let matches = parse_and_search(&singleline(), "(...)", "([)]");
    assert_eq!(matches.len(), 0);
}

#[test]
fn bracket_pattern_rejects_when_outer_brackets_are_themselves_unbalanced_overall() {
    let matches = parse_and_search(&singleline(), "(...)", "[([)]");
    assert_eq!(matches.len(), 0);
}

// Custom bracket configurations.

#[test]
fn default_paren_bracket_pattern_steps_over_nested_parens() {
    let matches = parse_and_search(&multiline(), "(...)", "((x))");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "((x))");
}

#[test]
fn custom_angle_brackets_match_nested_angle_pair() {
    let mut conf = Conf::default_multiline();
    conf.brackets = vec![('<', '>')];
    let matches = parse_and_search(&conf, "<...>", "<<x>>");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "<<x>>");
}

#[test]
fn mixed_paren_and_angle_brackets_nest_independently() {
    let mut conf = Conf::default_multiline();
    conf.brackets = vec![('(', ')'), ('<', '>')];
    let matches = parse_and_search(&conf, "<...>", "<(<x>)>");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "<(<x>)>");
}

// Plain metavariable backreferences.

#[test]
fn plain_backref_matches_only_when_second_word_equals_first() {
    let matches = parse_and_search(&singleline(), "$A ... $A", "a, b, c, a, d");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a, b, c, a");
    assert_eq!(capture_value(&matches, "A"), Some("a"));
}

#[test]
fn plain_backref_finds_non_overlapping_runs_of_three_repeats() {
    let matches = parse_and_search(
        &singleline(),
        "$A ... $A ... $A",
        "a x x x a x x x a x x x x a",
    );
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].loc.substring, "a x x x a x x x a");
}

#[test]
fn plain_backref_does_not_match_inside_a_longer_word_token() {
    let matches = parse_and_search(&singleline(), "$A ... $A", "ab abc");
    assert_eq!(matches.len(), 0);
}

#[test]
fn plain_backref_does_not_match_a_shorter_word_that_is_a_prefix_substring() {
    let matches = parse_and_search(&singleline(), "$A ... $A", "abc bc");
    assert_eq!(matches.len(), 0);
}

// Ellipsis-metavariable backreference word boundaries.

#[test]
fn ellipsis_backref_with_non_word_capture_may_touch_words_at_extremities() {
    let matches = parse_and_search(&singleline(), "... $...A : $...A ...", "x+ : +x");
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "A"), Some("+"));
}

#[test]
fn ellipsis_backref_with_word_capture_collapses_to_empty_when_neighbours_are_words() {
    let matches = parse_and_search(&singleline(), "... $...A : $...A ...", "xy : yx");
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "A"), Some(""));
}

#[test]
fn ellipsis_backref_word_extremity_may_not_partially_match_a_longer_word_token() {
    let matches = parse_and_search(&singleline(), "... $...A : $...A ...", "x : xx");
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "A"), Some(""));
}

#[test]
fn ellipsis_backref_non_word_extremity_may_match_adjacent_to_word() {
    let matches = parse_and_search(&singleline(), "... $...A : $...A ...", "+ : ++");
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "A"), Some("+"));
}

#[test]
fn ellipsis_backref_with_explicit_word_anchors_matches_symmetric_input() {
    let matches = parse_and_search(&singleline(), "x $...A : $...A x", "x+ : +x");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "x+ : +x");
    assert_eq!(capture_value(&matches, "A"), Some("+"));
}

// Ellipsis metavariable.

#[test]
fn ellipsis_metavar_captures_the_span_between_brackets() {
    let matches = parse_and_search(&singleline(), "[$...ITEMS]", "a, [ b, c ], d");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "[ b, c ]");
    assert_eq!(capture_value(&matches, "ITEMS"), Some("b, c"));
}

#[test]
fn singleline_short_ellipsis_metavar_does_not_cross_newline_inside_bracket() {
    let matches = parse_and_search(&singleline(), "[$...ITEMS]", "a, [ b,\nc ], d");
    assert_eq!(matches.len(), 0);
}

#[test]
fn singleline_long_ellipsis_metavar_crosses_newline_inside_bracket() {
    let matches = parse_and_search(&singleline(), "[$....ITEMS]", "a, [ b,\nc ], d");
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "ITEMS"), Some("b,\nc"));
}

#[test]
fn ellipsis_metavar_backtracks_to_satisfy_a_following_backref() {
    let matches = parse_and_search(&singleline(), "[$...A $...A]", "[a b a b]");
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "A"), Some("a b"));
}

#[test]
fn ellipsis_metavar_backref_requires_exact_byte_match_including_whitespace() {
    let matches = parse_and_search(&singleline(), "[$...A $...A]", "[a b a  b]");
    assert_eq!(matches.len(), 0);
}

// Multi-line ellipsis with metavar.

#[test]
fn literal_newline_pattern_matches_a_target_with_the_same_newlines() {
    let matches = parse_and_search(&singleline(), "\na\nb\n", "\na\nb\n");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "\na\nb\n");
}

#[test]
fn long_ellipsis_in_singleline_binds_two_metavars_across_many_lines() {
    let pattern = "\nvar $ORIG = ...;\n....\nvar $COPY = $ORIG;\n";
    let target =
        "\n/* sample code */\nvar a = 17;\nvar b = 42;\nvar c = 77;\nvar d = b;\nvar e = \"xx\";\n";
    let matches = parse_and_search(&singleline(), pattern, target);
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "ORIG"), Some("b"));
    assert_eq!(capture_value(&matches, "COPY"), Some("d"));
}

// Anchored ellipses.

#[test]
fn singleline_leading_ellipsis_anchors_to_start_of_line() {
    let matches = parse_and_search(&singleline(), "... $A", "!!!\n!!!hello world");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "!!!hello");
}

#[test]
fn singleline_long_leading_ellipsis_anchors_to_start_of_input_and_crosses_newlines() {
    let matches = parse_and_search(&singleline(), ".... $A", "!!!\n!!!hello world");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "!!!\n!!!hello");
}

#[test]
fn multiline_leading_ellipsis_anchors_to_start_of_input() {
    let matches = parse_and_search(&multiline(), "... $A", "!!!\n!!!hello world");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "!!!\n!!!hello");
}

#[test]
fn singleline_trailing_ellipsis_anchors_to_end_of_line() {
    let matches = parse_and_search(&singleline(), "$A ...", "hello!!!\n!!!");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "hello!!!");
}

#[test]
fn singleline_long_trailing_ellipsis_anchors_to_end_of_input() {
    let matches = parse_and_search(&singleline(), "$A ....", "hello!!!\n!!!");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "hello!!!\n!!!");
}

#[test]
fn multiline_trailing_ellipsis_anchors_to_end_of_input() {
    let matches = parse_and_search(&multiline(), "$A ...", "hello!!!\n!!!");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "hello!!!\n!!!");
}

// Pure ellipsis patterns.

#[test]
fn singleline_pure_ellipsis_matches_each_line_separately() {
    let matches = parse_and_search(&singleline(), "...", "hello\nworld");
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].loc.substring, "hello");
    assert_eq!(matches[1].loc.substring, "world");
}

#[test]
fn singleline_pure_long_ellipsis_matches_input_across_newlines_as_one_match() {
    let matches = parse_and_search(&singleline(), "....", "hello\nworld");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "hello\nworld");
}

#[test]
fn multiline_pure_ellipsis_matches_whole_input_as_one_match() {
    let matches = parse_and_search(&multiline(), "...", "hello\nworld");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "hello\nworld");
}

#[test]
fn singleline_ellipsis_newline_ellipsis_matches_two_consecutive_lines() {
    let matches = parse_and_search(&singleline(), "...\n...", "a\nb");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "a\nb");
}

#[test]
fn singleline_ellipsis_newline_ellipsis_finds_non_overlapping_pairs_of_lines() {
    let matches = parse_and_search(&singleline(), "...\n...", "a\nb\nc\n");
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].loc.substring, "a\nb");
    assert_eq!(matches[1].loc.substring, "c\n");
}

// Case sensitivity.

#[test]
fn default_match_is_case_sensitive() {
    let matches = parse_and_search(&multiline(), "hello", "HeLLo, world");
    assert_eq!(matches.len(), 0);
}

#[test]
fn caseless_conf_matches_word_regardless_of_letter_case() {
    let mut conf = Conf::default_multiline();
    conf.caseless = true;
    let matches = parse_and_search(&conf, "hello", "HeLLo, world");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].loc.substring, "HeLLo");
}

// Malware-shape patterns. These tests pin behaviour relied on by the
// malware-scanner catalogues.

#[test]
fn packer_signature_captures_five_positional_args_and_consumes_body() {
    let matches = parse_and_search(
        &multiline(),
        "eval(function($A, $B, $C, $D, $E, ....)....)",
        "eval(function(p, a, c, k, e, r) { return 'body'; }('args'))",
    );
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "A"), Some("p"));
    assert_eq!(capture_value(&matches, "E"), Some("e"));
}

#[test]
fn decode_then_eval_chain_matches_across_three_lines() {
    let target =
        "const raw = Buffer.from('aGVsbG8=', 'base64');\nconst code = raw.toString();\neval(code);\n";
    let matches = parse_and_search(
        &multiline(),
        "Buffer.from(....)....toString()....eval(....)",
        target,
    );
    assert_eq!(matches.len(), 1);
    assert!(matches[0].loc.substring.contains("Buffer.from"));
    assert!(matches[0].loc.substring.contains("toString()"));
    assert!(matches[0].loc.substring.contains("eval("));
}

#[test]
fn hyphen_split_prefix_pattern_captures_the_token_tail() {
    let matches = parse_and_search(
        &multiline(),
        "xoxb-$REST",
        "const token = \"xoxb-AB12CD34\";\n",
    );
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "REST"), Some("AB12CD34"));
}

#[test]
fn alphanumeric_hex_address_matches_as_one_word_token() {
    let matches = parse_and_search(
        &multiline(),
        "0xFc4a4858bafef54D1b1d7697bfb5c52F4c166976",
        "const drainTo = 0xFc4a4858bafef54D1b1d7697bfb5c52F4c166976;\n",
    );
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0].loc.substring,
        "0xFc4a4858bafef54D1b1d7697bfb5c52F4c166976"
    );
}

#[test]
fn bracket_balanced_call_pattern_fires_on_call_and_skips_assignment_to_same_name() {
    let target = "const execSync = wrapper;\nexecSync('rm -rf /tmp/x');\n";
    let matches = parse_and_search(&multiline(), "execSync(....)", target);
    assert_eq!(matches.len(), 1);
    assert!(matches[0].loc.substring.starts_with("execSync("));
}

// Contract pins for the argument and quoted-value gotchas: a bare `$X` binds one
// word, so it misses string literals and multi-token values; `(...)` and `$...X`
// catch them.

#[test]
fn bare_metavar_argument_misses_a_string_literal_call() {
    let matches = parse_and_search(&multiline(), "Command::new($X)", "Command::new(\"curl\")");
    assert_eq!(matches.len(), 0);
}

#[test]
fn ellipsis_argument_matches_a_string_literal_call() {
    let matches = parse_and_search(&multiline(), "Command::new(...)", "Command::new(\"curl\")");
    assert_eq!(matches.len(), 1);
}

#[test]
fn bare_metavar_misses_quoted_multi_token_value() {
    let matches = parse_and_search(&multiline(), "\"k\": $V", "\"k\": \"node x\"");
    assert_eq!(matches.len(), 0);
}

#[test]
fn span_metavar_in_quotes_matches_quoted_multi_token_value() {
    let matches = parse_and_search(&multiline(), "\"k\": \"$...V\"", "\"k\": \"node x\"");
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "V"), Some("node x"));
}

// Gotchas the prompt teaches, across quote styles and config formats.

#[test]
fn bare_metavar_argument_misses_string_literals_across_quote_styles() {
    for source in ["system(\"curl\")", "system('curl')", "system(`curl`)"] {
        let matches = parse_and_search(&multiline(), "system($X)", source);
        assert_eq!(matches.len(), 0, "should miss the literal: {source}");
    }
}

#[test]
fn ellipsis_argument_matches_string_literals_across_quote_styles() {
    for source in ["system(\"curl\")", "system('curl')", "system(`curl`)"] {
        let matches = parse_and_search(&multiline(), "system(...)", source);
        assert_eq!(matches.len(), 1, "should match: {source}");
    }
}

#[test]
fn bare_metavar_misses_a_quoted_config_value_but_a_span_matches() {
    let source = "\"postinstall\": \"node build.js\"";
    assert_eq!(
        parse_and_search(&multiline(), "\"postinstall\": $CMD", source).len(),
        0
    );
    let span = parse_and_search(&multiline(), "\"postinstall\": \"$...CMD\"", source);
    assert_eq!(span.len(), 1);
    assert_eq!(capture_value(&span, "CMD"), Some("node build.js"));
}

#[test]
fn two_dot_typo_matches_nothing_while_three_dots_match() {
    assert_eq!(
        parse_and_search(&multiline(), "fn $NAME(..)", "fn handler(arg) {}").len(),
        0
    );
    assert_eq!(
        parse_and_search(&multiline(), "$X.decode(..)", "data.decode(blob)").len(),
        0
    );
    assert_eq!(
        parse_and_search(&multiline(), "fn $NAME(...)", "fn handler(arg) {}").len(),
        1
    );
}

// Zero-width matches.

#[test]
fn ellipsis_reports_empty_line_as_a_zero_width_match() {
    let matches = parse_and_search(&singleline(), "...", "a\n\nb");
    let empty = matches
        .iter()
        .find(|m| m.loc.start == 2)
        .expect("empty line match");
    assert_eq!(empty.loc.length, 0);
    assert_eq!(empty.loc.substring, "");
}

// UTF-8 / multibyte encoding.

#[test]
fn span_backref_skips_a_candidate_whose_length_splits_a_multibyte_char() {
    // Re-checking the backref starts at '→' with a 1-byte capture, so here_end lands
    // inside the 3-byte char; the matcher must skip the split, not slice mid-char.
    let matches = parse_and_search(&multiline(), "[$...A $...A]", "[x →x]");
    assert_eq!(matches.len(), 0);
}

#[test]
fn span_backref_matches_identical_multibyte_content() {
    let matches = parse_and_search(&multiline(), "[$...A $...A]", "[é é]");
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "A"), Some("é"));
}

#[test]
fn span_backref_rejects_content_differing_in_a_multibyte_char() {
    let matches = parse_and_search(&multiline(), "[$...A $...A]", "[é à]");
    assert_eq!(matches.len(), 0);
}

#[test]
fn span_metavar_captures_multibyte_content_between_parens() {
    let matches = parse_and_search(&multiline(), "($...A)", "(café)");
    assert_eq!(matches.len(), 1);
    assert_eq!(capture_value(&matches, "A"), Some("café"));
}

#[test]
fn match_loc_reports_byte_offset_after_a_multibyte_prefix() {
    let matches = parse_and_search(&multiline(), "x", "é x");
    assert_eq!(matches.len(), 1);
    // 'é' is two bytes, so the match starts at byte 3, not char index 2.
    assert_eq!(matches[0].loc.start, 3);
    assert_eq!(matches[0].loc.length, 1);
    assert_eq!(matches[0].loc.substring, "x");
}

#[test]
fn caseless_does_not_fold_non_ascii_case() {
    let mut conf = multiline();
    conf.caseless = true;
    assert_eq!(parse_and_search(&conf, "É", "é").len(), 0);
}

// Shared-token catalogue contract.

#[test]
fn search_tokens_over_one_shared_stream_equals_per_pattern_search() {
    // The malware scanner tokenizes a file once and runs a whole catalogue over
    // those tokens; each pattern must see exactly what a standalone search would.
    let conf = multiline();
    let target = "fn handler(arg) {} Command::new(\"curl\")";
    let tokens = tokenize_target(target, &conf);
    for source in ["fn $NAME(...)", "Command::new(...)"] {
        let pattern = Pattern::parse(source, &conf).expect("pattern parses");
        assert_eq!(
            search_tokens(&pattern, &tokens, target),
            search(&pattern, target),
            "shared-token result diverged for {source}"
        );
    }
}
