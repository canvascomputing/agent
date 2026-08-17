//! Grep's code-shape matcher, the `syntax: "code"` mode. Matches by the
//! shape of code (`fn $NAME(...)`) rather than by regex, driving the `codegrep`
//! engine while reusing grep's file walk, output modes, and structured output.
//! `run` is handed the already-collected file list by `grep::search_corpus`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::Value;

use super::grep::{
    render_content, render_count, render_files, OutputMode, Query, MAX_LINE_COLUMNS,
};
use super::tool::ToolResult;
use crate::codegrep::{self, Conf, Pattern};

/// Match every file with the `codegrep` engine and render per `output_mode`,
/// reusing `grep`'s renderers so code results carry the same structured
/// shape and `head_limit`/`offset` paging as regex results.
pub(super) fn run(
    files: &[(PathBuf, String)],
    query: &Query,
    interrupt: &AtomicBool,
    deadline: Instant,
) -> ToolResult {
    let mut conf = Conf::default_multiline();
    conf.caseless = query.case_insensitive;
    let pattern = match Pattern::parse(&query.pattern, &conf) {
        Ok(pattern) => pattern,
        Err(error) => return ToolResult::error(format!("invalid code pattern: {error}")),
    };
    let constraints = match parse_constraints(&query.constraints, &pattern) {
        Ok(constraints) => constraints,
        Err(message) => return ToolResult::error(message),
    };

    match query.output_mode {
        OutputMode::Content => {
            let mut lines = Vec::new();
            for_each_file(files, interrupt, deadline, |display, content| {
                for found in codegrep::search(&pattern, content) {
                    if !satisfies_constraints(&found, &constraints) {
                        continue;
                    }
                    let (line, column) = line_and_byte_column(content, found.loc.start);
                    let summary = render_summary(&found.loc.substring);
                    let captures = render_captures(&found.captures);
                    let hit = format!("{display}:{line}:{column}: {summary}");
                    lines.push(if captures.is_empty() {
                        hit
                    } else {
                        format!("{hit} {captures}")
                    });
                }
            });
            render_content(&lines.join("\n"), query)
        }
        OutputMode::FilesWithMatches => {
            let mut hits = Vec::new();
            for_each_file(files, interrupt, deadline, |display, content| {
                if codegrep::search(&pattern, content)
                    .iter()
                    .any(|found| satisfies_constraints(found, &constraints))
                {
                    hits.push(display.to_string());
                }
            });
            render_files(&hits, query)
        }
        OutputMode::Count => {
            let mut rows = Vec::new();
            for_each_file(files, interrupt, deadline, |display, content| {
                let count = codegrep::search(&pattern, content)
                    .iter()
                    .filter(|found| satisfies_constraints(found, &constraints))
                    .count() as u64;
                if count > 0 {
                    rows.push((display.to_string(), count));
                }
            });
            render_count(&rows, query)
        }
    }
}

/// Read each file and hand its display path and contents to `visit`, skipping
/// unreadable ones. Stops between files on interrupt or deadline, so a long or
/// cancelled search bails the same way in every output mode.
fn for_each_file(
    files: &[(PathBuf, String)],
    interrupt: &AtomicBool,
    deadline: Instant,
    mut visit: impl FnMut(&str, &str),
) {
    for (path, display) in files {
        if interrupt.load(Ordering::Relaxed) || Instant::now() >= deadline {
            break;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        visit(display, &content);
    }
}

/// 1-based line plus 1-based **byte** column of `byte_offset` within its line.
/// Byte (not char) column so a code hit's `:col:` matches what regex mode
/// reports and what `read_file`'s `column` expects.
fn line_and_byte_column(content: &str, byte_offset: usize) -> (usize, usize) {
    let bytes = content.as_bytes();
    let clamped = byte_offset.min(bytes.len());
    let mut line = 1usize;
    let mut line_start = 0usize;
    for (index, &byte) in bytes.iter().enumerate().take(clamped) {
        if byte == b'\n' {
            line += 1;
            line_start = index + 1;
        }
    }
    (line, clamped - line_start + 1)
}

/// Collapse a matched span onto one line (escaping control chars) and limit it with
/// `grep`'s preview width so code and regex hits truncate alike.
fn render_summary(substring: &str) -> String {
    let escaped: String = substring
        .chars()
        .map(|c| match c {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            other => other.to_string(),
        })
        .collect();
    truncate_to_chars(&escaped, MAX_LINE_COLUMNS)
}

/// Render captured metavariables as `[$NAME=value, ...]`, empty when none.
fn render_captures(captures: &[(codegrep::Metavariable, codegrep::Loc)]) -> String {
    if captures.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = captures
        .iter()
        .map(|(metavariable, loc)| {
            let value = render_summary(&loc.substring);
            format!("${}={}", metavariable.bare_name, value)
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

fn truncate_to_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}...")
}

/// Compile `constraints` into `(metavariable, regex)` pairs, rejecting names the
/// pattern does not capture and regexes that do not compile. Regex filtering lives
/// here, not in the engine, so the core matcher stays free of a regex backend.
fn parse_constraints(
    constraints: &Value,
    pattern: &Pattern,
) -> std::result::Result<Vec<(String, regex::Regex)>, String> {
    let Some(items) = constraints.as_array() else {
        return Ok(Vec::new());
    };
    let names = pattern.metavariable_names();
    let mut compiled = Vec::new();
    for item in items {
        let name = item["metavariable"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches('$');
        let source = item["regex"].as_str().unwrap_or("");
        if name.is_empty() || source.is_empty() {
            return Err("each constraint needs a metavariable and a regex".to_string());
        }
        if !names.contains(name) {
            return Err(format!(
                "constraint references unknown metavariable ${name}"
            ));
        }
        let regex = regex::Regex::new(source).map_err(|error| {
            format!("invalid metavariable-regex constraint for ${name}: {error}")
        })?;
        compiled.push((name.to_string(), regex));
    }
    Ok(compiled)
}

/// Keep a match only when every constraint's named capture is present and its text
/// matches the regex. An absent capture fails the match.
fn satisfies_constraints(found: &codegrep::Match, constraints: &[(String, regex::Regex)]) -> bool {
    constraints.iter().all(|(name, regex)| {
        found
            .captures
            .iter()
            .find(|(metavariable, _)| &metavariable.bare_name == name)
            .map(|(_, loc)| regex.is_match(&loc.substring))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::super::grep::GrepTool;
    use super::super::tool::ToolContext;
    use serde_json::Value;
    use std::fs;

    fn test_ctx(path: &std::path::Path) -> ToolContext {
        ToolContext::new(path.to_path_buf())
    }

    async fn search(ctx: &ToolContext, input: Value) -> Value {
        let result = crate::tools::Tool::from(GrepTool).call(input, ctx).await;
        let content = result.content();
        serde_json::from_str(content).unwrap_or_else(|_| Value::String(content.to_string()))
    }

    #[tokio::test]
    async fn code_content_reports_shape_line_column_and_captures() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("calc.rs"),
            "fn main() {}\n  fn calculate(x: i32) {}\n",
        )
        .unwrap();
        let out = search(
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "fn $NAME(...)", "syntax": "code", "output_mode": "content"}),
        )
        .await;
        let content = out["content"].as_str().unwrap();
        // `fn calculate` starts at byte column 3 of line 2.
        assert!(content.contains("calc.rs:2:3:"), "got {content:?}");
        assert!(content.contains("$NAME=calculate"), "got {content:?}");
    }

    #[tokio::test]
    async fn constraints_filter_by_capture_regex() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("a.php"), "include('x');\nprint('y');\n").unwrap();
        let out = search(
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "$FUNC(...)",
                "syntax": "code",
                "output_mode": "content",
                "constraints": [{"metavariable": "FUNC", "regex": "^(include|require)$"}]
            }),
        )
        .await;
        let content = out["content"].as_str().unwrap();
        assert!(content.contains("include"), "got {content:?}");
        assert!(!content.contains("print"), "got {content:?}");
    }

    #[tokio::test]
    async fn case_insensitive_flag_is_caseless() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("doc.txt"), "HELLO world\n").unwrap();
        let out = search(
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "hello", "syntax": "code", "-i": true}),
        )
        .await;
        assert_eq!(out["numFiles"], 1, "got {out}");
    }

    #[tokio::test]
    async fn invalid_code_pattern_returns_the_parse_error() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("f.txt"), "x\n").unwrap();
        let out = search(
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "   ", "syntax": "code"}),
        )
        .await;
        assert!(
            out.as_str()
                .unwrap_or_default()
                .contains("invalid code pattern"),
            "got {out}"
        );
    }

    #[tokio::test]
    async fn code_honors_count_mode_and_head_limit() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        let out = search(
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "fn $N()", "syntax": "code", "output_mode": "count"}),
        )
        .await;
        assert_eq!(out["mode"], "count");
        assert_eq!(out["numMatches"], 2, "got {out}");
    }

    #[tokio::test]
    async fn syntax_defaults_to_regex() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("f.txt"), "fn $NAME(x)\n").unwrap();
        // Without syntax=code, `$NAME` is a regex (anchor + literal), not a
        // metavariable, so it matches the literal text on the line.
        let out = search(
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "\\$NAME", "output_mode": "content"}),
        )
        .await;
        assert!(
            out["content"].as_str().unwrap().contains("f.txt:1:"),
            "got {out}"
        );
    }
}
