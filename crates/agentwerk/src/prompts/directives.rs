//! The catalogue of texts agentwerk sends the model to report a failure or
//! correct its behavior, and the store a host decides them with.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

/// The catalogue, one file per area, each holding its entries under `## key`
/// headings. A `{name}` is bound by the call site; one with no value renders as
/// written.
const CATALOGUE: &[&str] = &[
    include_str!("directives/loop.md"),
    include_str!("directives/registry.md"),
    include_str!("directives/files.md"),
    include_str!("directives/command.md"),
    include_str!("directives/search.md"),
    include_str!("directives/fetch_url.md"),
    include_str!("directives/knowledge.md"),
    include_str!("directives/tasks.md"),
    include_str!("directives/schemas.md"),
];

/// Declare every directive once: the name a render site inside the crate
/// writes, the one a host matches on, and the entry in [`Directive::ALL`]. One
/// literal produces all three, so a key cannot be spelled two ways. A key with
/// no `## ` heading behind it is caught by the tests below.
macro_rules! directives {
    ($($name:ident = $key:literal),* $(,)?) => {
        $(
            #[doc = concat!("The `", $key, "` directive.")]
            pub(crate) const $name: &str = $key;
        )*

        impl Directive {
            $(
                #[doc = concat!("The `", $key, "` directive.")]
                pub const $name: &'static str = $key;
            )*

            /// Every key, in the order they are declared. The binding crate
            /// walks it to publish the same constants to Python.
            pub const ALL: &'static [&'static str] = &[$($key),*];
        }
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
    COMMAND_TIMED_OUT = "command_timed_out",
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
    GREP_TIMED_OUT = "grep_timed_out",
    GREP_FAILED = "grep_failed",
    GREP_GLOB_REJECTED = "grep_glob_rejected",
    GREP_FILE_TYPE_UNKNOWN = "grep_file_type_unknown",
    GREP_PATTERN_REJECTED = "grep_pattern_rejected",
    CODE_PATTERN_REJECTED = "code_pattern_rejected",
    CODE_CONSTRAINT_INCOMPLETE = "code_constraint_incomplete",
    CODE_CONSTRAINT_METAVARIABLE_UNKNOWN = "code_constraint_metavariable_unknown",
    CODE_CONSTRAINT_REGEX_REJECTED = "code_constraint_regex_rejected",
    FETCH_URL_TOO_LONG = "fetch_url_too_long",
    FETCH_URL_SCHEME_MISSING = "fetch_url_scheme_missing",
    FETCH_URL_SCHEME_UNSUPPORTED = "fetch_url_scheme_unsupported",
    FETCH_URL_CREDENTIALS_PRESENT = "fetch_url_credentials_present",
    FETCH_URL_HOST_MISSING = "fetch_url_host_missing",
    FETCH_URL_HOST_NOT_RESOLVABLE = "fetch_url_host_not_resolvable",
    FETCH_URL_TOO_MANY_REDIRECTS = "fetch_url_too_many_redirects",
    FETCH_URL_REQUEST_FAILED = "fetch_url_request_failed",
    FETCH_URL_BODY_NOT_READ = "fetch_url_body_not_read",
    FETCH_URL_RESPONSE_TOO_LARGE = "fetch_url_response_too_large",
    FETCH_URL_REDIRECT_LOCATION_MISSING = "fetch_url_redirect_location_missing",
    KNOWLEDGE_PAGE_NOT_FOUND = "knowledge_page_not_found",
    KNOWLEDGE_WRITE_FAILED = "knowledge_write_failed",
    KNOWLEDGE_REMOVE_FAILED = "knowledge_remove_failed",
    QUEUE_UNAVAILABLE = "queue_unavailable",
    TASK_KEY_MISSING = "task_key_missing",
    TASK_NOT_ASSIGNED = "task_not_assigned",
    TASK_NOT_FOUND = "task_not_found",
    TASK_RESULT_MISSING = "task_result_missing",
    TASK_QUERY_INVALID = "task_query_invalid",
    TASK_EDIT_INCOMPLETE = "task_edit_incomplete",
    TASK_TRANSITION_REJECTED = "task_transition_rejected",
    HANDOVER_RESULT_MISSING = "handover_result_missing",
    FINISH_ARGUMENT_BLANK = "finish_argument_blank",
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

/// Every directive agentwerk can send, one constant per key.
///
/// [`Agent::directives`](crate::Agent::directives) takes the
/// function deciding all of them. Match the key it hands you against these
/// constants, and answer `None` for the ones you leave as they are; the arms
/// are constants, so a misspelled one does not compile.
///
/// ```no_run
/// use agentwerk::{Agent, Directive};
///
/// let agent = Agent::from_env()
///     .directives(|key| match key {
///         Directive::GREP_CANCELLED => Some("Stop searching."),
///         _ => None,
///     });
/// ```
pub struct Directive;

/// The function an agent decides its directives with, as the agent holds it.
/// A host writes one for [`Agent::directives`](crate::Agent::directives)
/// and never names this type.
pub(crate) struct DirectiveStore {
    compute: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
}

impl DirectiveStore {
    /// Decide every directive's text with `compute`, keeping the catalogue text
    /// wherever it answers `None`. What it returns is a template, bound
    /// afterwards, so a `{name}` it carries still resolves.
    pub(crate) fn new<T: Into<String>>(
        compute: impl Fn(&str) -> Option<T> + Send + Sync + 'static,
    ) -> DirectiveStore {
        DirectiveStore {
            compute: Arc::new(move |key| compute(key).map(Into::into)),
        }
    }

    /// Render the directive `key` names, binding every `{name}` it holds from
    /// `values`. A key the catalogue does not carry renders as itself.
    pub(crate) fn render(&self, key: &'static str, values: &[(&str, &str)]) -> String {
        match (self.compute)(key) {
            Some(template) => bind(&template, values),
            None => built_in(key, values),
        }
    }
}

/// The catalogue text for `key`, with `values` bound and no store consulted.
/// Three groups render through this, each composed where no agent is in reach:
/// the schema violations, the knowledge index, and the result-schema block a
/// task appends to its own task.
pub(crate) fn built_in(key: &'static str, values: &[(&str, &str)]) -> String {
    bind(catalogue().get(key).copied().unwrap_or(key), values)
}

impl Default for DirectiveStore {
    /// The built-in text, unchanged.
    fn default() -> Self {
        DirectiveStore::new(|_| None::<&str>)
    }
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
        for key in Directive::ALL {
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
                Directive::ALL.contains(key),
                "`## {key}` in the catalogue names no key"
            );
        }
    }

    #[test]
    fn no_key_is_declared_twice() {
        let mut seen = Directive::ALL.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), Directive::ALL.len());
    }

    #[test]
    fn no_directive_body_is_empty() {
        for key in Directive::ALL {
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
        let store = DirectiveStore::new(|_| Some("Stop searching in {dir}."));

        assert_eq!(
            store.render(GREP_CANCELLED, &[("dir", "src")]),
            "Stop searching in src.",
        );
    }

    #[test]
    fn a_key_the_store_does_not_name_keeps_its_built_in_text() {
        let store = DirectiveStore::new(|key| match key {
            GREP_CANCELLED => Some("Stop searching."),
            _ => None,
        });

        assert_eq!(store.render(GREP_CANCELLED, &[]), "Stop searching.");
        assert_eq!(
            store.render(GREP_FAILED, &[]),
            built_in().render(GREP_FAILED, &[]),
        );
    }

    #[test]
    fn two_stores_render_the_same_key_differently() {
        let one = DirectiveStore::new(|_| Some("one"));
        let other = DirectiveStore::new(|_| Some("other"));

        assert_eq!(one.render(GREP_CANCELLED, &[]), "one");
        assert_eq!(other.render(GREP_CANCELLED, &[]), "other");
    }

    #[test]
    fn a_store_reads_the_key_it_is_rendering() {
        let store = DirectiveStore::new(|key: &str| Some(key.to_string()));

        assert_eq!(store.render(GREP_FAILED, &[]), "grep_failed");
    }
}
