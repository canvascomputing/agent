//! Werk-owned prompt rendering.

use std::collections::HashMap;
use std::fmt;

use serde_json::Value;

use crate::{Query, Werk};

/// An expression that could not be rendered.
#[derive(Debug, Clone)]
pub(crate) struct RenderError {
    /// Expression that failed, without its surrounding braces.
    pub(crate) expression: String,
    /// Why the expression could not be resolved.
    pub(crate) message: String,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cannot render {{")?;
        f.write_str(&self.expression)?;
        f.write_str("}}: ")?;
        f.write_str(&self.message)
    }
}

impl std::error::Error for RenderError {}

impl Werk {
    /// Resolve a prompt from runtime values, shared templates, and results.
    ///
    /// Runtime values override shared templates and remain literal after insertion.
    pub(crate) fn render_prompt(
        &self,
        prompt: &str,
        values: &[(&str, String)],
    ) -> Result<String, RenderError> {
        let values: Values<'_> = values
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let shared = self.template_values();
        let mut named_value = |name: &str| {
            values
                .get(name)
                .map(|value| (*value).to_string())
                .or_else(|| shared.get(name).cloned())
        };
        render_template(prompt.trim(), |expression| {
            resolve_expression(self, expression, &mut named_value)
        })
    }
}

type Values<'a> = HashMap<&'a str, &'a str>;

const EXPRESSION_OPEN: &str = "{{";
const EXPRESSION_CLOSE: &str = "}}";
const ESCAPED_EXPRESSION_OPEN: &str = "{{{{";
const ESCAPED_EXPRESSION_CLOSE: &str = "}}}}";

/// Render a template's expressions; replacement values are never scanned.
fn render_template(
    template: &str,
    mut resolve: impl FnMut(&str) -> Result<Option<String>, RenderError>,
) -> Result<String, RenderError> {
    let mut output = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(character) = remaining.chars().next() {
        if let Some(escaped) = remaining.strip_prefix(ESCAPED_EXPRESSION_OPEN) {
            let Some(end) = escaped.find(ESCAPED_EXPRESSION_CLOSE) else {
                return Err(RenderError {
                    expression: escaped.to_string(),
                    message: "unclosed escaped expression".into(),
                });
            };
            output.push_str(EXPRESSION_OPEN);
            output.push_str(&escaped[..end]);
            output.push_str(EXPRESSION_CLOSE);
            remaining = &escaped[end + ESCAPED_EXPRESSION_CLOSE.len()..];
            continue;
        }
        let Some(body) = remaining.strip_prefix(EXPRESSION_OPEN) else {
            output.push(character);
            remaining = &remaining[character.len_utf8()..];
            continue;
        };
        let end = match expression_end(body) {
            Ok(Some(end)) => end,
            Ok(None) => {
                return Err(RenderError {
                    expression: body.to_string(),
                    message: "unclosed expression or quoted value".into(),
                });
            }
            Err(message) => {
                return Err(RenderError {
                    expression: body.to_string(),
                    message: message.into(),
                });
            }
        };
        let expression = &body[..end];
        let expression_end = EXPRESSION_OPEN.len() + end + EXPRESSION_CLOSE.len();
        let literal = &remaining[..expression_end];
        let replacement = resolve(expression)?;
        output.push_str(replacement.as_deref().unwrap_or(literal));
        remaining = &remaining[expression_end..];
    }
    Ok(output)
}

/// Resolve only named values, preserving an unknown or malformed template.
pub(super) fn render_values(
    template: &str,
    mut named_value: impl FnMut(&str) -> Option<String>,
) -> String {
    render_template(template, |expression| Ok(named_value(expression.trim())))
        .unwrap_or_else(|_| template.to_string())
}

fn resolve_expression(
    werk: &Werk,
    expression: &str,
    named_value: &mut impl FnMut(&str) -> Option<String>,
) -> Result<Option<String>, RenderError> {
    let expression = expression.trim();
    let (expanded, nested) =
        expand_nested(expression, named_value).map_err(|message| RenderError {
            expression: expression.to_string(),
            message,
        })?;
    let expanded = expanded.trim();

    if let Some(inner) = readable_expression(expanded).map_err(|message| RenderError {
        expression: expression.to_string(),
        message,
    })? {
        let Some((kind, query)) = result_expression(inner) else {
            return Err(RenderError {
                expression: expression.to_string(),
                message: "readable expects a result: or results: expression".into(),
            });
        };
        if !matches!(kind, "result" | "results") {
            return Err(RenderError {
                expression: expression.to_string(),
                message: "readable expects a result: or results: expression".into(),
            });
        }
        return select_result(werk, kind, query)
            .map(|value| Some(readable(&value)))
            .map_err(|message| RenderError {
                expression: expression.to_string(),
                message,
            });
    }

    if let Some((kind, query)) = result_expression(expanded) {
        return select_result(werk, kind, query)
            .map(|value| Some(result_text(value, is_plural(kind))))
            .map_err(|message| RenderError {
                expression: expression.to_string(),
                message,
            });
    }

    if nested {
        return Err(RenderError {
            expression: expression.to_string(),
            message: "nested values are only supported inside result expressions".into(),
        });
    }
    Ok(named_value(expanded))
}

fn expand_nested(
    expression: &str,
    named_value: &mut impl FnMut(&str) -> Option<String>,
) -> Result<(String, bool), String> {
    let mut output = String::with_capacity(expression.len());
    let mut remaining = expression;
    let mut expanded = false;
    while let Some(open) = remaining.find(EXPRESSION_OPEN) {
        output.push_str(&remaining[..open]);
        let body = &remaining[open + EXPRESSION_OPEN.len()..];
        let Some(close) = body.find(EXPRESSION_CLOSE) else {
            return Err("unclosed nested expression".into());
        };
        let name = body[..close].trim();
        if name.is_empty()
            || name.contains(EXPRESSION_OPEN)
            || result_expression(name).is_some()
            || name.starts_with("readable(")
        {
            return Err("nested expressions must name a template value".into());
        }
        let Some(value) = named_value(name) else {
            return Err(format!("unknown nested template value `{name}`"));
        };
        output.push_str(&value);
        remaining = &body[close + EXPRESSION_CLOSE.len()..];
        expanded = true;
    }
    output.push_str(remaining);
    Ok((output, expanded))
}

fn readable_expression(expression: &str) -> Result<Option<&str>, String> {
    let Some(body) = expression.strip_prefix("readable(") else {
        return Ok(None);
    };
    let Some(body) = body.strip_suffix(')') else {
        return Err("unclosed readable call".into());
    };
    Ok(Some(body.trim()))
}

fn result_expression(expression: &str) -> Option<(&str, &str)> {
    let (kind, query) = expression.split_once(':')?;
    let kind = kind.trim();
    matches!(kind, "result" | "results" | "result_path" | "result_paths")
        .then_some((kind, query.trim()))
}

/// Nested placeholders may appear in quoted AQL; ordinary braces remain data.
fn expression_end(body: &str) -> Result<Option<usize>, &'static str> {
    let mut brace_depth = 0;
    let mut nested = false;
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;
    while index < body.len() {
        let remaining = &body[index..];
        if nested {
            if remaining.starts_with(EXPRESSION_OPEN) {
                return Err("nested expressions may only be one level deep");
            }
            if remaining.starts_with(EXPRESSION_CLOSE) {
                nested = false;
                index += EXPRESSION_CLOSE.len();
                continue;
            }
            index += remaining
                .chars()
                .next()
                .expect("remaining is nonempty")
                .len_utf8();
            continue;
        }
        if remaining.starts_with(EXPRESSION_OPEN) {
            nested = true;
            index += EXPRESSION_OPEN.len();
            continue;
        }

        let ch = remaining.chars().next().expect("remaining is nonempty");
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
            index += ch.len_utf8();
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '{' => brace_depth += 1,
            '}' if brace_depth > 0 => brace_depth -= 1,
            '}' if remaining.starts_with(EXPRESSION_CLOSE) => return Ok(Some(index)),
            _ => {}
        }
        index += ch.len_utf8();
    }
    if nested {
        Err("unclosed nested expression")
    } else {
        Ok(None)
    }
}

fn is_plural(kind: &str) -> bool {
    matches!(kind, "results" | "result_paths")
}

fn select_result(werk: &Werk, kind: &str, query: &str) -> Result<Value, String> {
    let query = Query::new(query).map_err(|error| error.to_string())?;
    let mut tasks = werk.result_tasks(query);
    let plural = is_plural(kind);
    if !plural && tasks.is_empty() {
        return Err("no matching result".into());
    }
    if !plural {
        tasks.truncate(1);
    }
    let use_paths = matches!(kind, "result_path" | "result_paths");
    let values = tasks
        .iter()
        .map(|task| result_value(werk, task, use_paths))
        .collect::<Result<Vec<_>, _>>()?;
    if plural {
        return Ok(Value::Array(values));
    }
    Ok(values
        .into_iter()
        .next()
        .expect("singular selection is nonempty"))
}

fn result_text(value: Value, plural: bool) -> String {
    match value {
        Value::String(text) if !plural => text,
        value => value.to_string(),
    }
}

fn readable(value: &Value) -> String {
    readable_lines(value, 0).join("\n")
}

fn readable_lines(value: &Value, indent: usize) -> Vec<String> {
    let padding = " ".repeat(indent);
    match value {
        Value::Null => Vec::new(),
        Value::Bool(_) | Value::Number(_) => vec![format!("{padding}{value}")],
        Value::String(text) => {
            if text.is_empty() {
                return vec![padding];
            }
            text.lines()
                .map(|line| format!("{padding}{line}"))
                .collect()
        }
        Value::Array(values) => {
            let mut lines = Vec::new();
            for value in values {
                let mut nested = readable_lines(value, indent + 2);
                if nested.is_empty() {
                    continue;
                }
                if value.is_array() {
                    lines.push(format!("{padding}-"));
                    lines.extend(nested);
                    continue;
                }
                let first = nested.remove(0);
                lines.push(format!("{padding}- {}", &first[indent + 2..]));
                lines.extend(nested);
            }
            lines
        }
        Value::Object(fields) => {
            let mut lines = Vec::new();
            for (key, value) in fields {
                if let Value::String(text) = value {
                    let mut text_lines = text.lines();
                    let first = text_lines.next().unwrap_or_default();
                    lines.push(format!("{padding}{key}: {first}"));
                    let continuation = " ".repeat(indent + key.chars().count() + 2);
                    lines.extend(text_lines.map(|line| format!("{continuation}{line}")));
                    continue;
                }
                let nested = readable_lines(value, indent + 2);
                if nested.is_empty() {
                    continue;
                }
                if matches!(value, Value::Bool(_) | Value::Number(_)) {
                    lines.push(format!("{padding}{key}: {}", &nested[0][indent + 2..]));
                } else {
                    lines.push(format!("{padding}{key}:"));
                    lines.extend(nested);
                }
            }
            lines
        }
    }
}

fn result_value(werk: &Werk, task: &crate::Task, use_path: bool) -> Result<Value, String> {
    if !use_path {
        return Ok(task
            .get_result()
            .cloned()
            .expect("result selector requires a result"));
    }
    let path = werk.result_path(task.get_id());
    let absolute = path
        .canonicalize()
        .map_err(|error| format!("cannot access result file `{}`: {error}", path.display()))?;
    if !absolute.is_file() {
        return Err(format!(
            "result path `{}` is not a file",
            absolute.display()
        ));
    }
    Ok(Value::String(absolute.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_text_is_trimmed() {
        let werk = Werk::new();
        assert_eq!(
            werk.render_prompt("\n\nYou review code.\n", &[]).unwrap(),
            "You review code."
        );
    }

    use crate::{Event, Task, Werk};

    fn render(werk: &Werk, prompt: impl AsRef<str>) -> Result<String, RenderError> {
        werk.render_prompt(prompt.as_ref(), &[])
    }

    fn session() -> (std::sync::Arc<Werk>, crate::test_util::TempDir) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf()).on_event(|_, _| {});
        (werk, dir)
    }

    #[test]
    fn later_shared_values_replace_previous_bindings() {
        let (werk, _dir) = session();
        werk.set_template("company", "old");
        werk.set_template("company", "Acme");

        assert_eq!(render(&werk, "{{ company }}").unwrap(), "Acme");
    }

    #[test]
    fn shared_values_are_inserted_without_rendering_their_contents() {
        let (werk, _dir) = session();
        werk.set_template("company", "Acme");
        werk.set_template("data", "{{ company }} {{ result: missing }}");
        assert_eq!(
            render(&werk, "{{ company }}: {{ data }}").unwrap(),
            "Acme: {{ company }} {{ result: missing }}"
        );
    }

    #[test]
    fn runtime_string_values_override_shared_templates_and_stay_literal() {
        let (werk, _dir) = session();
        werk.set_templates([("company", "Shared"), ("topic", "prompts")]);
        let values = [("company", "Local {{ topic }}".to_string())];

        assert_eq!(
            werk.render_prompt("{{ company }}: {{ topic }}", &values)
                .unwrap(),
            "Local {{ topic }}: prompts"
        );
    }

    #[test]
    fn value_rendering_replaces_known_names_once_and_preserves_unknown_names() {
        let values = [("name", "{{ other }}"), ("other", "expanded")];
        let rendered = render_values("{{ name }} {{name}} {{ missing }}", |name| {
            values
                .iter()
                .find_map(|(key, value)| (*key == name).then(|| (*value).to_string()))
        });

        assert_eq!(rendered, "{{ other }} {{ other }} {{ missing }}");
    }

    #[test]
    fn value_rendering_leaves_non_template_braces_literal() {
        let value = |name: &str| (name == "name").then(|| "expanded".to_string());
        let json = r#"{"one":{"two":{"three":{"value":1}}}}"#;

        assert_eq!(render_values("{name}", value), "{name}");
        assert_eq!(render_values(json, value), json);
    }

    #[test]
    fn value_rendering_unescapes_double_brace_expressions() {
        assert_eq!(render_values("{{{{ name }}}}", |_| None), "{{ name }}");
    }

    #[test]
    fn value_rendering_preserves_non_value_expressions() {
        let template = "{{ readable(result: x) }} {{ result: x }} {{ outer {{ name }} }}";

        assert_eq!(render_values(template, |_| None), template);
    }

    #[test]
    fn value_rendering_preserves_the_entire_malformed_template() {
        let value = |name: &str| (name == "name").then(|| "expanded".to_string());
        let malformed = "{{ name }} then {{ missing";

        assert_eq!(render_values(malformed, value), malformed);
    }

    #[test]
    fn prompt_rendering_leaves_non_template_braces_literal() {
        let werk = Werk::new();
        let json = r#"{"one":{"two":{"three":{"value":1}}}}"#;

        assert_eq!(render(&werk, "{company}").unwrap(), "{company}");
        assert_eq!(render(&werk, json).unwrap(), json);
        assert_eq!(
            render(&werk, "standalone }}}} braces").unwrap(),
            "standalone }}}} braces"
        );
    }

    #[test]
    fn prompt_rendering_preserves_unknown_expressions() {
        let werk = Werk::new();

        assert_eq!(render(&werk, "{{ unknown }}").unwrap(), "{{ unknown }}");
    }

    #[test]
    fn prompt_values_ignore_delimiter_whitespace() {
        let (werk, _dir) = session();
        werk.set_template("company", "Acme");

        assert_eq!(
            render(&werk, "{{company}} | {{ company }} | 日本 {{ company }}").unwrap(),
            "Acme | Acme | 日本 Acme"
        );
    }

    #[test]
    fn four_braces_emit_a_literal_double_brace_expression() {
        let (werk, _dir) = session();
        werk.set_template("company", "Acme");

        assert_eq!(render(&werk, "{{{{ company }}}}").unwrap(), "{{ company }}");
    }

    #[test]
    fn result_selectors_keep_strings_plain_and_structured_values_compact() {
        let (werk, _dir) = session();
        let first = werk.add_task(Task::labeled("research", "first"));
        let second = werk.add_task(Task::labeled("research", "second"));
        werk.set_task_finished(&first, serde_json::json!("first {{ company }}"))
            .unwrap();
        werk.set_task_finished(&second, serde_json::json!({"answer": 42}))
            .unwrap();
        assert_eq!(
            render(&werk, "{{ result: research }}").unwrap(),
            "first {{ company }}"
        );
        assert_eq!(
            render(&werk, format!("{{{{ result: {second} }}}}")).unwrap(),
            r#"{"answer":42}"#
        );
    }

    #[test]
    fn plural_result_selectors_follow_aql_order_and_skip_pending_tasks() {
        let (werk, _dir) = session();
        let first = werk.add_task(Task::labeled("research", "first"));
        let second = werk.add_task(Task::labeled("research", "second"));
        werk.add_task(Task::labeled("research", "pending"));
        werk.set_task_finished(&first, serde_json::json!("first"))
            .unwrap();
        werk.set_task_finished(&second, serde_json::json!("second"))
            .unwrap();

        assert_eq!(
            render(&werk, "{{ results: research ORDER BY task.id DESC }}").unwrap(),
            r#"["second","first"]"#
        );
    }

    #[test]
    fn joined_result_selectors_emit_each_matching_task_once() {
        let (werk, _dir) = session();
        let selected = werk.add_task(Task::labeled("research", "selected"));
        werk.set_task_finished(&selected, serde_json::json!({"answer": 42}))
            .unwrap();
        werk.emit_event(Event::new("selected").task_id(&selected));
        werk.emit_event(Event::new("selected").task_id(&selected));

        assert_eq!(
            render(
                &werk,
                "{{ results: task.label = research AND event.name = selected }}",
            )
            .unwrap(),
            r#"[{"answer":42}]"#
        );
    }

    #[test]
    fn quoted_braces_inside_aql_do_not_end_the_expression() {
        let (werk, _dir) = session();
        let id = werk.add_task(Task::labeled("research}notes", "go"));
        werk.set_task_finished(&id, serde_json::json!("found"))
            .unwrap();
        assert_eq!(
            render(&werk, r#"{{ result: task.label = "research}notes" }}"#).unwrap(),
            "found"
        );
    }

    #[test]
    fn empty_plural_result_selectors_render_empty_arrays() {
        let (werk, _dir) = session();
        for kind in ["results", "result_paths"] {
            assert_eq!(
                render(&werk, format!("{{{{ {kind}: missing }}}}")).unwrap(),
                "[]"
            );
        }
    }

    #[test]
    fn missing_singular_result_selectors_are_rejected() {
        let (werk, _dir) = session();
        for kind in ["result", "result_path"] {
            let error = render(&werk, format!("{{{{ {kind}: missing }}}}")).unwrap_err();
            assert_eq!(error.message, "no matching result");
        }
    }

    #[test]
    fn malformed_aql_reports_the_query_failure() {
        let (werk, _dir) = session();
        for (prompt, expression, message) in [
            (
                "{{ result: }}",
                "result:",
                "A query cannot be blank. Name an origin-qualified field or a task ID.",
            ),
            (
                "{{ results: task.label = }}",
                "results: task.label =",
                "The query ends in the middle of a term.",
            ),
        ] {
            let error = render(&werk, prompt).unwrap_err();
            assert_eq!(error.expression, expression);
            assert_eq!(error.message, message);
        }
    }

    #[test]
    fn unclosed_expressions_report_the_unclosed_construct() {
        let werk = Werk::new();
        for (prompt, message) in [
            ("{{ result: research", "unclosed expression or quoted value"),
            (
                "{{ result: task.label = \"oops }}",
                "unclosed expression or quoted value",
            ),
            ("{{ readable(result: research }}", "unclosed readable call"),
        ] {
            let error = render(&werk, prompt).unwrap_err();
            assert_eq!(error.message, message, "{prompt}");
            assert!(error.to_string().starts_with("cannot render {{"));
        }
    }

    #[test]
    fn result_path_selectors_return_existing_absolute_files() {
        let (werk, dir) = session();
        let id = werk.add_task("go");
        werk.set_task_finished(&id, serde_json::json!("done"))
            .unwrap();
        let path = dir
            .path()
            .join("tasks")
            .join(&id)
            .join("result.json")
            .canonicalize()
            .unwrap();
        assert_eq!(
            render(&werk, format!("{{{{ result_path: {id} }}}}")).unwrap(),
            path.to_str().unwrap()
        );
        let paths = render(&werk, format!("{{{{ result_paths: {id} }}}}")).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&paths).unwrap(),
            [path.to_string_lossy()]
        );
    }

    #[test]
    fn missing_result_paths_are_rejected_without_recreating_the_file() {
        let (werk, dir) = session();
        let id = werk.add_task("go");
        werk.set_task_finished(&id, serde_json::json!("done"))
            .unwrap();
        let path = dir.path().join("tasks").join(&id).join("result.json");
        std::fs::remove_file(&path).unwrap();

        assert!(render(&werk, format!("{{{{ result_path: {id} }}}}")).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn direct_aql_resolves_but_aql_in_template_values_stays_literal() {
        let (werk, _dir) = session();
        let id = werk.add_task(Task::labeled("research", "go"));
        werk.set_task_finished(&id, serde_json::json!("Use {{ company }}"))
            .unwrap();
        werk.set_template("research", "{{ result: research }}");
        assert_eq!(
            render(&werk, "{{ research }} | {{ result: research }}").unwrap(),
            "{{ result: research }} | Use {{ company }}"
        );
    }

    #[test]
    fn named_query_fragments_expand_before_aql_parsing() {
        let (werk, _dir) = session();
        let id = werk.add_task(Task::labeled("research", "go"));
        werk.set_task_finished(&id, serde_json::json!("found"))
            .unwrap();
        werk.set_template("selection", "research");

        assert_eq!(
            render(&werk, "{{ result: {{ selection }} }}").unwrap(),
            "found"
        );
    }

    #[test]
    fn multiple_named_values_expand_inside_quoted_aql_values() {
        let (werk, _dir) = session();
        let id = werk.add_task(Task::labeled("research", "go"));
        werk.set_task_finished(&id, serde_json::json!("found"))
            .unwrap();
        werk.set_templates([("field", "task.label"), ("label", "research")]);

        assert_eq!(
            render(&werk, r#"{{ result: {{ field }} = "{{ label }}" }}"#,).unwrap(),
            "found"
        );
    }

    #[test]
    fn nested_replacements_are_not_rendered_again() {
        let (werk, _dir) = session();
        let literal = werk.add_task(Task::labeled("{{ other }}", "go"));
        werk.set_task_finished(&literal, serde_json::json!("literal"))
            .unwrap();
        werk.set_templates([
            ("literal_query", r#"task.label = "{{ other }}""#),
            ("other", "research"),
        ]);

        assert_eq!(
            render(&werk, "{{ result: {{ literal_query }} }}").unwrap(),
            "literal"
        );
    }

    #[test]
    fn invalid_nested_expressions_report_why_they_are_rejected() {
        let (werk, _dir) = session();
        werk.set_templates([("name", "research"), ("outer", "name")]);
        for (prompt, message) in [
            (
                "{{ result: {{ missing }} }}",
                "unknown nested template value `missing`",
            ),
            (
                "{{ result: {{ result: research }} }}",
                "nested expressions must name a template value",
            ),
            (
                "{{ result: {{ outer {{ name }} }} }}",
                "nested expressions may only be one level deep",
            ),
            (
                "{{ prefix {{ name }} }}",
                "nested values are only supported inside result expressions",
            ),
        ] {
            assert_eq!(
                render(&werk, prompt).unwrap_err().message,
                message,
                "{prompt}"
            );
        }
    }

    #[test]
    fn nested_values_that_produce_invalid_aql_are_rejected() {
        let (werk, _dir) = session();
        werk.set_template("selection", "task.label =");

        let error = render(&werk, "{{ result: {{ selection }} }}").unwrap_err();

        assert_eq!(error.expression, "result: {{ selection }}");
        assert_eq!(error.message, "The query ends in the middle of a term.");
    }

    #[test]
    fn readable_objects_and_arrays_form_an_indented_outline() {
        let (werk, _dir) = session();
        let structured = werk.add_task(Task::labeled("detail", "go"));
        werk.set_task_finished(
            &structured,
            serde_json::json!({
                "name": "Acme",
                "details": {"active": true, "none": null},
                "findings": ["one", null, {}, [], "two\ncontinued"],
                "summary": "Executive\nsummary",
                "empty": [],
            }),
        )
        .unwrap();

        assert_eq!(
            render(&werk, "{{ readable(result: detail) }}").unwrap(),
            "details:\n  active: true\nfindings:\n  - one\n  - two\n    continued\nname: Acme\nsummary: Executive\n         summary"
        );
    }

    #[test]
    fn readable_root_scalars_are_plain_text() {
        let (werk, _dir) = session();
        for (label, value, expected) in [
            ("boolean", serde_json::json!(true), "true"),
            ("number", serde_json::json!(42), "42"),
            (
                "text",
                serde_json::json!("line one\nline two"),
                "line one\nline two",
            ),
            ("null", Value::Null, ""),
        ] {
            let id = werk.add_task(Task::labeled(label, "go"));
            werk.set_task_finished(&id, value).unwrap();
            assert_eq!(
                render(&werk, format!("{{{{ readable(result: {label}) }}}}")).unwrap(),
                expected,
            );
        }
    }

    #[test]
    fn readable_empty_root_collections_render_nothing() {
        let (werk, _dir) = session();
        for (label, value) in [
            ("array", serde_json::json!([])),
            ("object", serde_json::json!({})),
        ] {
            let id = werk.add_task(Task::labeled(label, "go"));
            werk.set_task_finished(&id, value).unwrap();
            assert_eq!(
                render(&werk, format!("{{{{ readable(result: {label}) }}}}")).unwrap(),
                "",
            );
        }
    }

    #[test]
    fn readable_arrays_render_objects_and_mixed_scalars_as_bullets() {
        let (werk, _dir) = session();
        let id = werk.add_task(Task::labeled("mixed", "go"));
        werk.set_task_finished(
            &id,
            serde_json::json!([
                {"name": "Acme", "active": true},
                42,
                false,
            ]),
        )
        .unwrap();

        assert_eq!(
            render(&werk, "{{ readable(result: mixed) }}").unwrap(),
            "- active: true\n  name: Acme\n- 42\n- false"
        );
    }

    #[test]
    fn readable_plural_results_omit_empty_values() {
        let (werk, _dir) = session();
        let text = werk.add_task(Task::labeled("mixed", "go"));
        werk.set_task_finished(&text, serde_json::json!("Executive\nsummary"))
            .unwrap();
        let empty = werk.add_task(Task::labeled("mixed", "go"));
        werk.set_task_finished(&empty, Value::Null).unwrap();
        assert_eq!(
            render(&werk, "{{ readable(results: mixed) }}").unwrap(),
            "- Executive\n  summary"
        );
    }

    #[test]
    fn readable_empty_selection_is_empty_text() {
        let (werk, _dir) = session();
        assert_eq!(
            render(&werk, "{{ readable(results: missing) }}").unwrap(),
            ""
        );
    }

    #[test]
    fn readable_accepts_only_result_selectors() {
        let (werk, _dir) = session();
        werk.set_template("name", "research");
        for prompt in [
            "{{ readable(name) }}",
            "{{ readable(result_path: research) }}",
            "{{ readable(results: {{ readable(result: research) }}) }}",
        ] {
            assert!(render(&werk, prompt).is_err(), "{prompt}");
        }
    }
}
