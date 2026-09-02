//! Defines corrective model instructions and lets the host override their text.

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

/// The catalogue, one file per area, each holding its entries under `## key`
/// headings. A `{name}` is bound by the call site; one with no value renders as
/// written.
const CATALOGUE: &[&str] = &[
    include_str!("directives/loop.md"),
    include_str!("directives/registry.md"),
    include_str!("directives/files.md"),
    include_str!("directives/command.md"),
    include_str!("directives/search.md"),
    include_str!("directives/fetch.md"),
    include_str!("directives/knowledge.md"),
    include_str!("directives/task.md"),
    include_str!("directives/schemas.md"),
];

/// Declare every directive once: the constant a render site writes and its
/// catalogue key. A key with no `## ` heading behind it is caught by the tests
/// below.
macro_rules! directives {
    ($($name:ident = $key:literal),* $(,)?) => {
        $(
            pub(crate) const $name: &str = $key;
        )*

        #[cfg(test)]
        const ALL: &[&str] = &[$($key),*];
    };
}

directives! {
    REPLY_REJECTED = "reply_rejected",
    NO_TOOL_CALLED = "no_tool_called",
    ARGUMENTS_REJECTED = "arguments_rejected",
    ARGUMENTS_EXPECTED = "arguments_expected",
    RESULT_SCHEMA_REQUIRED = "result_schema_required",
    SUMMARY_REQUESTED = "summary_requested",
    KNOWLEDGE_INDEX_TRUNCATED = "knowledge_index_truncated",
    TOOL_NOT_FOUND = "tool_not_found",
    NO_TOOLS_REGISTERED = "no_tools_registered",
    TOOL_PANICKED = "tool_panicked",
    TOOL_TIMED_OUT = "tool_timed_out",
    TOOL_OUTPUT_EMPTY = "tool_output_empty",
    TOOL_OUTPUT_OFFLOADED = "tool_output_offloaded",
    EDIT_FILE_READ_FAILED = "edit_file_read_failed",
    EDIT_FILE_OLD_STRING_NOT_FOUND = "edit_file_old_string_not_found",
    EDIT_FILE_OLD_STRING_NOT_UNIQUE = "edit_file_old_string_not_unique",
    EDIT_FILE_WRITE_FAILED = "edit_file_write_failed",
    WRITE_FILE_PARENT_NOT_CREATED = "write_file_parent_not_created",
    WRITE_FILE_FAILED = "write_file_failed",
    READ_FILE_PATH_IS_DIRECTORY = "read_file_path_is_directory",
    READ_FILE_PATH_IS_DIRECTORY_WITH_ENTRIES = "read_file_path_is_directory_with_entries",
    READ_FILE_IS_BINARY = "read_file_is_binary",
    READ_FILE_NOT_FOUND = "read_file_not_found",
    READ_FILE_FAILED = "read_file_failed",
    LIST_DIRECTORY_PATH_IS_FILE = "list_directory_path_is_file",
    LIST_DIRECTORY_NOT_FOUND = "list_directory_not_found",
    LIST_DIRECTORY_FAILED = "list_directory_failed",
    PATH_HINT_DIRECTORY_LISTED = "path_hint_directory_listed",
    PATH_HINT_SUGGESTION = "path_hint_suggestion",
    PATH_HINT_WORKING_DIRECTORY = "path_hint_working_directory",
    COMMAND_CANCELLED = "command_cancelled",
    COMMAND_NOT_STARTED = "command_not_started",
    COMMAND_MISSING = "command_missing",
    COMMAND_SHELL_OPERATOR_FOUND = "command_shell_operator_found",
    COMMAND_QUOTE_UNTERMINATED = "command_quote_unterminated",
    COMMAND_CONTROL_CHARACTER_FOUND = "command_control_character_found",
    COMMAND_ASSIGNMENT_FOUND = "command_assignment_found",
    COMMAND_FLAG_DENIED = "command_flag_denied",
    COMMAND_PATTERN_DENIED = "command_pattern_denied",
    COMMAND_NOT_ALLOWED = "command_not_allowed",
    COMMAND_FLAG_NOT_ALLOWED = "command_flag_not_allowed",
    GREP_CANCELLED = "grep_cancelled",
    GREP_FAILED = "grep_failed",
    GREP_GLOB_REJECTED = "grep_glob_rejected",
    GREP_FILE_TYPE_UNKNOWN = "grep_file_type_unknown",
    GREP_PATTERN_REJECTED = "grep_pattern_rejected",
    CODE_PATTERN_REJECTED = "code_pattern_rejected",
    CODE_CONSTRAINT_INCOMPLETE = "code_constraint_incomplete",
    CODE_CONSTRAINT_METAVARIABLE_UNKNOWN = "code_constraint_metavariable_unknown",
    CODE_CONSTRAINT_REGEX_REJECTED = "code_constraint_regex_rejected",
    FETCH_TOO_LONG = "fetch_too_long",
    FETCH_SCHEME_MISSING = "fetch_scheme_missing",
    FETCH_SCHEME_UNSUPPORTED = "fetch_scheme_unsupported",
    FETCH_CREDENTIALS_PRESENT = "fetch_credentials_present",
    FETCH_HOST_MISSING = "fetch_host_missing",
    FETCH_HOST_NOT_RESOLVABLE = "fetch_host_not_resolvable",
    FETCH_TOO_MANY_REDIRECTS = "fetch_too_many_redirects",
    FETCH_REQUEST_FAILED = "fetch_request_failed",
    FETCH_BODY_NOT_READ = "fetch_body_not_read",
    FETCH_RESPONSE_TOO_LARGE = "fetch_response_too_large",
    FETCH_REDIRECT_LOCATION_MISSING = "fetch_redirect_location_missing",
    KNOWLEDGE_PAGE_NOT_FOUND = "knowledge_page_not_found",
    KNOWLEDGE_WRITE_FAILED = "knowledge_write_failed",
    KNOWLEDGE_REMOVE_FAILED = "knowledge_remove_failed",
    WERK_UNAVAILABLE = "werk_unavailable",
    TASK_ID_MISSING = "task_id_missing",
    TASK_NOT_ASSIGNED = "task_not_assigned",
    TASK_NOT_FOUND = "task_not_found",
    TASK_RESULT_MISSING = "task_result_missing",
    TASK_QUERY_INVALID = "task_query_invalid",
    TASK_EDIT_INCOMPLETE = "task_edit_incomplete",
    TASK_TRANSITION_REJECTED = "task_transition_rejected",
    HANDOVER_RESULT_MISSING = "handover_result_missing",
    HANDOVER_SCHEMA_INVALID = "handover_schema_invalid",
    SCHEMA_FALSE_REJECTED = "schema_false_rejected",
    SCHEMA_TYPE_MISMATCHED = "schema_type_mismatched",
    SCHEMA_CONST_MISMATCHED = "schema_const_mismatched",
    SCHEMA_ENUM_MISMATCHED = "schema_enum_mismatched",
    SCHEMA_ANY_OF_UNMATCHED = "schema_any_of_unmatched",
    SCHEMA_ONE_OF_AMBIGUOUS = "schema_one_of_ambiguous",
    SCHEMA_NOT_MATCHED = "schema_not_matched",
    SCHEMA_PROPERTY_MISSING = "schema_property_missing",
    SCHEMA_PROPERTY_UNEXPECTED = "schema_property_unexpected",
    SCHEMA_ARRAY_TOO_SHORT = "schema_array_too_short",
    SCHEMA_ARRAY_TOO_LONG = "schema_array_too_long",
    SCHEMA_STRING_TOO_SHORT = "schema_string_too_short",
    SCHEMA_STRING_TOO_LONG = "schema_string_too_long",
    SCHEMA_PATTERN_UNMATCHED = "schema_pattern_unmatched",
    SCHEMA_NUMBER_TOO_SMALL = "schema_number_too_small",
    SCHEMA_NUMBER_TOO_LARGE = "schema_number_too_large",
    SCHEMA_HINT_UNQUOTE = "schema_hint_unquote",
    SCHEMA_HINT_JSON = "schema_hint_json",
    SCHEMA_HINT_QUOTE = "schema_hint_quote",
}

/// Holds one agent's explicit directive overrides.
#[derive(Clone, Default)]
pub(crate) struct DirectiveStore {
    overrides: HashMap<String, String>,
}

impl DirectiveStore {
    pub(crate) fn insert(&mut self, key: impl Into<String>, template: impl Into<String>) {
        self.overrides.insert(key.into(), template.into());
    }

    /// Render the directive `key` names, binding every `{name}` it holds from
    /// `values`. A key the catalogue does not carry renders as itself.
    pub(crate) fn render(&self, key: &str, values: &[(&str, &str)]) -> String {
        self.render_override(key, values)
            .unwrap_or_else(|| built_in(key, values))
    }

    /// Render only an explicit override, without falling back to the built-in
    /// catalogue. Custom event names use this path.
    pub(crate) fn render_override(&self, key: &str, values: &[(&str, &str)]) -> Option<String> {
        self.overrides
            .get(key)
            .map(|template| bind(template, values))
    }
}

/// The catalogue text for `key`, with `values` bound and no store consulted.
/// Three groups render through this, each composed where no agent is in reach:
/// the schema violations, the knowledge index, and the result-schema block a
/// task appends to its own task.
pub(crate) fn built_in(key: &str, values: &[(&str, &str)]) -> String {
    bind(catalogue().get(key).copied().unwrap_or(key), values)
}

impl fmt::Debug for DirectiveStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirectiveStore").finish_non_exhaustive()
    }
}

/// Substitute in one pass, so a value carrying `{` is never read as a
/// placeholder of its own: a bound value is a path, an error, or text the
/// model wrote, any of which can hold braces.
fn bind(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        match values.iter().find(|(key, _)| *key == name) {
            Some((_, value)) => out.push_str(value),
            None => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Walk the `## key` headings of one catalogue file. Whatever precedes the
/// first heading is the file's own comment, which is not an entry.
fn entries(markdown: &str) -> impl Iterator<Item = (&str, &str)> {
    markdown
        .split("\n## ")
        .skip(1)
        .filter_map(|entry| entry.split_once('\n'))
        .map(|(key, body)| (key.trim(), body.trim_matches('\n')))
}

/// The built-in texts, parsed once off the `##` headings of every catalogue
/// file.
fn catalogue() -> &'static HashMap<&'static str, &'static str> {
    static PARSED: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    PARSED.get_or_init(|| CATALOGUE.iter().flat_map(|file| entries(file)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_a_heading() {
        for key in ALL {
            assert!(
                catalogue().contains_key(key),
                "no `## {key}` heading in the catalogue",
            );
        }
    }

    #[test]
    fn every_heading_has_a_key() {
        for key in catalogue().keys() {
            assert!(
                ALL.contains(key),
                "`## {key}` in the catalogue names no key"
            );
        }
    }

    #[test]
    fn no_key_is_declared_twice() {
        let mut seen = ALL.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), ALL.len());
    }

    #[test]
    fn no_directive_body_is_empty() {
        for key in ALL {
            assert!(
                !built_in().render(key, &[]).is_empty(),
                "{key} renders empty"
            );
        }
    }

    #[test]
    fn binding_substitutes_every_placeholder() {
        let rendered = built_in().render(EDIT_FILE_OLD_STRING_NOT_FOUND, &[("path", "src/lib.rs")]);
        assert!(rendered.contains("src/lib.rs"));
        assert!(!rendered.contains("{path}"));
    }

    #[test]
    fn an_unbound_placeholder_renders_as_written() {
        assert_eq!(bind("read {path} again", &[]), "read {path} again");
    }

    #[test]
    fn a_bound_value_is_never_read_as_a_placeholder() {
        let rendered = bind("at {path}", &[("path", "{name}"), ("name", "expanded")]);
        assert_eq!(rendered, "at {name}");
    }

    #[test]
    fn an_unclosed_brace_renders_as_written() {
        assert_eq!(bind("at {path", &[("path", "x")]), "at {path");
    }

    fn built_in() -> DirectiveStore {
        DirectiveStore::default()
    }

    #[test]
    fn what_a_store_returns_is_bound_afterwards() {
        let mut store = DirectiveStore::default();
        store.insert(GREP_CANCELLED, "Stop searching in {dir}.");

        assert_eq!(
            store.render(GREP_CANCELLED, &[("dir", "src")]),
            "Stop searching in src.",
        );
    }

    #[test]
    fn a_key_the_store_does_not_name_keeps_its_built_in_text() {
        let mut store = DirectiveStore::default();
        store.insert(GREP_CANCELLED, "Stop searching.");

        assert_eq!(store.render(GREP_CANCELLED, &[]), "Stop searching.");
        assert_eq!(
            store.render(GREP_FAILED, &[]),
            built_in().render(GREP_FAILED, &[]),
        );
    }

    #[test]
    fn two_stores_render_the_same_key_differently() {
        let mut one = DirectiveStore::default();
        one.insert(GREP_CANCELLED, "one");
        let mut other = DirectiveStore::default();
        other.insert(GREP_CANCELLED, "other");

        assert_eq!(one.render(GREP_CANCELLED, &[]), "one");
        assert_eq!(other.render(GREP_CANCELLED, &[]), "other");
    }

    #[test]
    fn a_later_override_replaces_an_earlier_one() {
        let mut store = DirectiveStore::default();
        store.insert(GREP_FAILED, "one");
        store.insert(GREP_FAILED, "two");

        assert_eq!(store.render(GREP_FAILED, &[]), "two");
    }

    #[test]
    fn an_explicit_custom_key_has_no_catalogue_fallback() {
        let mut store = DirectiveStore::default();
        store.insert("cache_miss", "No cache entry for {path}.");

        assert_eq!(
            store.render_override("cache_miss", &[("path", "index")]),
            Some("No cache entry for index.".to_string()),
        );
        assert_eq!(store.render_override("another_event", &[]), None);
    }
}
