//! Werk-owned prompt rendering.

use std::collections::HashMap;
use std::fmt;

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
        write!(f, "cannot render {{{}}}: {}", self.expression, self.message)
    }
}

impl std::error::Error for RenderError {}

impl Werk {
    /// Resolve a source string from runtime values, shared templates, and results.
    ///
    /// Runtime values override shared templates and remain literal after insertion.
    pub(crate) fn render_prompt(
        &self,
        source: &str,
        values: &[(&str, String)],
    ) -> Result<String, RenderError> {
        let values: Values<'_> = values
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let shared = self.template_values();
        render_source(
            source.trim(),
            |name| {
                values
                    .get(name)
                    .map(|value| (*value).to_string())
                    .or_else(|| shared.get(name).cloned())
            },
            self,
        )
    }
}

type Values<'a> = HashMap<&'a str, &'a str>;

/// Render only source expressions; replacement values are never scanned.
fn render_source(
    prompt: &str,
    mut named_value: impl FnMut(&str) -> Option<String>,
    werk: &Werk,
) -> Result<String, RenderError> {
    let mut output = String::with_capacity(prompt.len());
    let mut remaining = prompt;
    while let Some(character) = remaining.chars().next() {
        if remaining.starts_with("{{") || remaining.starts_with("}}") {
            output.push(character);
            remaining = &remaining[2..];
            continue;
        }
        let Some(body) = remaining.strip_prefix('{') else {
            output.push(character);
            remaining = &remaining[character.len_utf8()..];
            continue;
        };
        let Some(end) = expression_end(body) else {
            if result_expression(body).is_some() {
                return Err(RenderError {
                    expression: body.to_string(),
                    message: "unclosed expression or quoted value".into(),
                });
            }
            output.push('{');
            remaining = body;
            continue;
        };
        let expression = &body[..end];
        let literal = &remaining[..end + 2];
        let replacement = resolve_expression(werk, expression, &mut named_value)?;
        output.push_str(replacement.as_deref().unwrap_or(literal));
        remaining = &remaining[end + 2..];
    }
    Ok(output)
}

fn resolve_expression(
    werk: &Werk,
    expression: &str,
    named_value: &mut impl FnMut(&str) -> Option<String>,
) -> Result<Option<String>, RenderError> {
    let Some((kind, query)) = result_expression(expression) else {
        return Ok(named_value(expression));
    };
    resolve_result(werk, kind, query)
        .map(Some)
        .map_err(|message| RenderError {
            expression: expression.to_string(),
            message,
        })
}

fn result_expression(expression: &str) -> Option<(&str, &str)> {
    let (kind, query) = expression.split_once(':')?;
    let kind = kind.trim();
    matches!(kind, "result" | "results" | "result_path" | "result_paths")
        .then_some((kind, query.trim()))
}

/// Braces inside quoted AQL values and ordinary JSON do not end the expression.
fn expression_end(body: &str) -> Option<usize> {
    let mut depth = 0;
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in body.char_indices() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '{' => depth += 1,
            '}' if depth == 0 => return Some(index),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn resolve_result(werk: &Werk, kind: &str, query: &str) -> Result<String, String> {
    let query = Query::new(query).map_err(|error| error.to_string())?;
    let mut tasks = werk.result_tasks(query);
    let plural = matches!(kind, "results" | "result_paths");
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
        return Ok(serde_json::Value::Array(values).to_string());
    }
    match values
        .into_iter()
        .next()
        .expect("singular selection is nonempty")
    {
        serde_json::Value::String(text) => Ok(text),
        value => Ok(value.to_string()),
    }
}

fn result_value(
    werk: &Werk,
    task: &crate::Task,
    use_path: bool,
) -> Result<serde_json::Value, String> {
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
    Ok(serde_json::Value::String(
        absolute.to_string_lossy().into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_text_is_trimmed() {
        let werk = Werk::new();
        assert_eq!(
            werk.render_prompt("\n\nYou review code.\n", &[]).unwrap(),
            "You review code."
        );
    }

    use crate::{Event, Task, Werk};

    fn render(werk: &Werk, source: impl AsRef<str>) -> Result<String, RenderError> {
        werk.render_prompt(source.as_ref(), &[])
    }

    fn session() -> (std::sync::Arc<Werk>, crate::test_util::TempDir) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf()).on_event(|_, _| {});
        (werk, dir)
    }

    #[test]
    fn named_values_replace_previous_bindings_and_stay_literal() {
        let (werk, _dir) = session();
        werk.set_template("company", "old");
        werk.set_template("company", "Acme");
        werk.set_template("data", "{company} {result: missing}");
        assert_eq!(
            render(&werk, "{company}: {data}").unwrap(),
            "Acme: {company} {result: missing}"
        );
    }

    #[test]
    fn runtime_string_values_override_shared_templates_and_stay_literal() {
        let (werk, _dir) = session();
        werk.set_templates([("company", "Shared"), ("topic", "prompts")]);
        let values = [("company", "Local {topic}".to_string())];

        assert_eq!(
            werk.render_prompt("{company}: {topic}", &values).unwrap(),
            "Local {topic}: prompts"
        );
    }

    #[test]
    fn unknown_names_json_and_escaped_braces_remain_literal() {
        let (werk, _dir) = session();
        werk.set_template("company", "Acme");
        for (source, expected) in [
            ("{unknown}", "{unknown}"),
            (r#"{"nested":{"value":1}}"#, r#"{"nested":{"value":1}}"#),
            ("{{company}}", "{company}"),
            ("{{result: missing}}", "{result: missing}"),
            ("Unicode: 日本 {company}", "Unicode: 日本 Acme"),
        ] {
            assert_eq!(render(&werk, source).unwrap(), expected);
        }
    }

    #[test]
    fn aql_results_use_the_existing_status_order_and_join_rules() {
        let (werk, _dir) = session();
        let first = werk.add_task(Task::labeled("research", "first"));
        let second = werk.add_task(Task::labeled("research", "second"));
        werk.add_task(Task::labeled("research", "pending"));
        werk.set_task_finished(&first, serde_json::json!("first {company}"))
            .unwrap();
        werk.set_task_finished(&second, serde_json::json!({"answer": 42}))
            .unwrap();
        werk.emit_event(Event::new("selected").task_id(&second));
        werk.emit_event(Event::new("selected").task_id(&second));
        assert_eq!(
            render(&werk, "{result: research}").unwrap(),
            "first {company}"
        );
        assert_eq!(
            render(&werk, "{results: research ORDER BY task.id DESC}").unwrap(),
            r#"[{"answer":42},"first {company}"]"#
        );
        assert_eq!(
            render(
                &werk,
                "{results: task.label = research AND event.name = selected}",
            )
            .unwrap(),
            r#"[{"answer":42}]"#
        );
        assert_eq!(
            render(&werk, format!("{{result: {second}}}")).unwrap(),
            r#"{"answer":42}"#
        );
    }

    #[test]
    fn quoted_braces_inside_aql_do_not_end_the_expression() {
        let (werk, _dir) = session();
        let id = werk.add_task(Task::labeled("research}notes", "go"));
        werk.set_task_finished(&id, serde_json::json!("found"))
            .unwrap();
        assert_eq!(
            render(&werk, r#"{result: task.label = "research}notes"}"#).unwrap(),
            "found"
        );
    }

    #[test]
    fn empty_plural_results_are_arrays_and_singular_results_are_errors() {
        let (werk, _dir) = session();
        for kind in ["results", "result_paths"] {
            assert_eq!(render(&werk, format!("{{{kind}: missing}}")).unwrap(), "[]");
        }
        for kind in ["result", "result_path"] {
            assert!(render(&werk, format!("{{{kind}: missing}}"))
                .unwrap_err()
                .message
                .contains("no matching result"));
        }
    }

    #[test]
    fn malformed_aql_and_unclosed_result_expressions_report_the_expression() {
        let (werk, _dir) = session();
        for source in [
            "{result:}",
            "{results: task.label =}",
            "{result: research",
            "{result: task.label = \"oops}",
        ] {
            let error = render(&werk, source).unwrap_err();
            assert!(!error.expression.is_empty());
            assert!(!error.message.is_empty());
        }
    }

    #[test]
    fn result_paths_are_existing_absolute_files_and_never_write_missing_files() {
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
            render(&werk, format!("{{result_path: {id}}}")).unwrap(),
            path.to_str().unwrap()
        );
        let paths = render(&werk, format!("{{result_paths: {id}}}")).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&paths).unwrap(),
            [path.to_string_lossy()]
        );
        std::fs::remove_file(&path).unwrap();
        assert!(render(&werk, format!("{{result_path: {id}}}")).is_err());
        assert!(!path.exists());
        assert_eq!(render(&werk, format!("{{result: {id}}}")).unwrap(), "done");
    }
    #[test]
    fn direct_aql_resolves_but_aql_in_template_values_stays_literal() {
        let (werk, _dir) = session();
        let id = werk.add_task(Task::labeled("research", "go"));
        werk.set_task_finished(&id, serde_json::json!("Use {company}"))
            .unwrap();
        werk.set_template("research", "{result: research}");
        assert_eq!(
            render(&werk, "{research} | {result: research}").unwrap(),
            "{result: research} | Use {company}"
        );
    }
}
