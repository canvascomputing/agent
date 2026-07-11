//! Structural code search across files. Locates code by shape (function signatures, control structures, balanced brackets) rather than by literal substring.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use super::tool::{ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;
use super::util::glob_matches_file;
use crate::codegrep::{self, Conf, Pattern};
use crate::providers::ProviderResult as Result;

/// Search file contents by structural code pattern with metavariables, balanced brackets, and ellipses.
/// Returns one match per line with file path, line, column, captured substring, and metavariable values.
/// Read-only.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::CodegrepTool;
///
/// Agent::new().tool(CodegrepTool);
/// ```
pub struct CodegrepTool;

const MAX_RESULTS: usize = 100;
const MAX_LINE_DISPLAY: usize = 200;
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "vendor"];

/// Returned when a syntactically valid pattern matches nothing. Doubles as a
/// nudge: a model that reached for regex (the common miss) sees how to write a
/// structural query and can correct on its next turn instead of repeating it.
const NO_MATCH_HINT: &str = "No matches. This is structural search, not regex: write the literal code \
and use `$NAME` to capture an identifier, e.g. `fn $NAME(...)`. Regex syntax (`[a-z]`, `*`, `\\`) is \
matched literally, so it will not match code. Ellipsis is three dots `...` or four `....`; `..` is two \
literal dots.";

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("codegrep.tool.md")))
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

impl ToolLike for CodegrepTool {
    fn name(&self) -> &str {
        &tool_file().name
    }

    fn description(&self) -> &str {
        description()
    }

    fn input_schema(&self) -> Value {
        tool_file().input_schema.clone()
    }

    fn is_read_only(&self) -> bool {
        tool_file().read_only
    }

    fn call<'a>(
        &'a self,
        input: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
        Box::pin(async move {
            let pattern_source = match input["pattern"].as_str() {
                Some(p) => p,
                None => return Ok(ToolResult::error("Missing required parameter: pattern")),
            };

            let base = ctx.dir.clone();
            let glob_filter = input["glob"].as_str().map(|s| s.to_string());
            let caseless = input["caseless"].as_bool().unwrap_or(false);

            let mut conf = Conf::default_multiline();
            conf.caseless = caseless;

            let pattern = match Pattern::parse(pattern_source, &conf) {
                Ok(p) => p,
                Err(error) => {
                    return Ok(ToolResult::error(format!(
                        "invalid codegrep pattern: {error}"
                    )));
                }
            };

            let constraints = match parse_constraints(&input, &pattern) {
                Ok(constraints) => constraints,
                Err(message) => return Ok(ToolResult::error(message)),
            };

            let mut files = Vec::new();
            collect_files(&base, &base, &glob_filter, &mut files);
            files.sort();

            let mut output: Vec<String> = Vec::new();
            'outer: for file in &files {
                let content = match std::fs::read_to_string(file) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let matches = codegrep::search(&pattern, &content);
                let rel = relative_path(file, &base);
                for codegrep_match in matches {
                    if !satisfies_constraints(&codegrep_match, &constraints) {
                        continue;
                    }
                    let (line, col) = byte_to_line_col(&content, codegrep_match.loc.start);
                    let summary = render_summary(&codegrep_match.loc.substring);
                    let captures = render_captures(&codegrep_match.captures);
                    let row = if captures.is_empty() {
                        format!("{rel}:{line}:{col}: {summary}")
                    } else {
                        format!("{rel}:{line}:{col}: {summary} {captures}")
                    };
                    output.push(row);
                    if output.len() >= MAX_RESULTS {
                        break 'outer;
                    }
                }
            }

            if output.is_empty() {
                return Ok(ToolResult::success(
                    match placeholder_hint(pattern_source) {
                        Some(extra) => format!("{NO_MATCH_HINT}\n{extra}"),
                        None => NO_MATCH_HINT.to_string(),
                    },
                ));
            }
            Ok(ToolResult::success(output.join("\n")))
        })
    }
}

/// A pattern like `<call>(...)` is a placeholder the writer meant to fill with a
/// real identifier; codegrep matched the literal `<`, the word, and `>`, which no
/// code contains. When a no-match pattern carries such a token, name it so the
/// next attempt substitutes the identifier instead of retrying variants. Only
/// lowercase-led `<word>` tokens trigger it, so generics (`<T>`) are untouched
/// and a real `<script>` that matched never reaches this path.
fn placeholder_hint(pattern_source: &str) -> Option<String> {
    let chars: Vec<char> = pattern_source.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c != '<' {
            continue;
        }
        let start = i + 1;
        if chars
            .get(start)
            .is_none_or(|first| !first.is_ascii_lowercase())
        {
            continue;
        }
        let mut end = start;
        while chars
            .get(end)
            .is_some_and(|w| w.is_ascii_lowercase() || w.is_ascii_digit() || *w == '_')
        {
            end += 1;
        }
        if chars.get(end) == Some(&'>') {
            let word: String = chars[start..end].iter().collect();
            return Some(format!(
                "Note: `<{word}>` is not a placeholder. It matched the literal `<`, `{word}`, `>`, \
which no code contains. Write the real identifier instead, e.g. `{word}(...)`."
            ));
        }
    }
    None
}

fn collect_files(dir: &Path, base: &Path, glob_filter: &Option<String>, results: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                collect_files(&path, base, glob_filter, results);
            }
            continue;
        }
        if let Some(ref filter) = glob_filter {
            if !glob_matches_file(filter, &path, base) {
                continue;
            }
        }
        results.push(path);
    }
}

fn relative_path(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn byte_to_line_col(content: &str, byte_offset: usize) -> (usize, usize) {
    let bytes = content.as_bytes();
    let clamped = byte_offset.min(bytes.len());
    let mut line = 1usize;
    let mut line_start = 0usize;
    for (idx, byte) in bytes.iter().enumerate().take(clamped) {
        if *byte == b'\n' {
            line += 1;
            line_start = idx + 1;
        }
    }
    let col = content[line_start..clamped].chars().count() + 1;
    (line, col)
}

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
    truncate_to_chars(&escaped, MAX_LINE_DISPLAY)
}

fn render_captures(captures: &[(codegrep::Metavariable, codegrep::Loc)]) -> String {
    if captures.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = captures
        .iter()
        .map(|(metavariable, loc)| {
            let value = truncate_to_chars(&render_summary(&loc.substring), MAX_LINE_DISPLAY);
            format!("${}={}", metavariable.bare_name, value)
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

fn truncate_to_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}...")
}

/// Compile `constraints` into `(metavariable, regex)` pairs, rejecting names the
/// pattern does not capture and regexes that do not compile. Regex filtering lives
/// here, in the tool, so the core matcher stays free of a regex backend.
fn parse_constraints(
    input: &Value,
    pattern: &Pattern,
) -> std::result::Result<Vec<(String, regex::Regex)>, String> {
    let Some(items) = input["constraints"].as_array() else {
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
fn satisfies_constraints(
    codegrep_match: &codegrep::Match,
    constraints: &[(String, regex::Regex)],
) -> bool {
    constraints.iter().all(|(name, regex)| {
        codegrep_match
            .captures
            .iter()
            .find(|(metavariable, _)| &metavariable.bare_name == name)
            .map(|(_, loc)| regex.is_match(&loc.substring))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_ctx(path: &Path) -> ToolContext {
        ToolContext::new(path.to_path_buf())
    }

    async fn run(tool: &CodegrepTool, ctx: &ToolContext, input: serde_json::Value) -> String {
        let result = tool.call(input, ctx).await.unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = result;
        content
    }

    #[tokio::test]
    async fn finds_function_signature_capturing_function_name() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("calc.rs"),
            "pub fn calculate(items: Vec<i32>) -> i32 { 0 }\n",
        )
        .unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "fn $NAME(...)"}),
        )
        .await;
        assert!(output.contains("calc.rs"), "output: {output}");
        assert!(output.contains("$NAME=calculate"), "output: {output}");
    }

    #[tokio::test]
    async fn respects_glob_filter_when_walking_files() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("a.rs"), "fn foo() {}\n").unwrap();
        fs::write(tmp.path().join("b.md"), "fn foo() {}\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "fn $N()", "glob": "*.rs"}),
        )
        .await;
        assert!(output.contains("a.rs"), "output: {output}");
        assert!(!output.contains("b.md"), "output: {output}");
    }

    #[tokio::test]
    async fn caseless_flag_matches_uppercase_target() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("doc.txt"), "HELLO world\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "hello", "caseless": true}),
        )
        .await;
        assert!(output.contains("HELLO"), "output: {output}");
    }

    #[tokio::test]
    async fn case_sensitive_default_does_not_match_different_case() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("doc.txt"), "HELLO world\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "hello"}),
        )
        .await;
        assert!(
            output.starts_with("No matches"),
            "expected no match, got: {output}"
        );
    }

    #[tokio::test]
    async fn reports_no_match_with_a_structural_syntax_hint() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("file.txt"), "nothing here\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "absent"}),
        )
        .await;
        assert!(output.starts_with("No matches"), "output: {output}");
        // The miss should teach the metavariable form, not leave the model guessing.
        assert!(output.contains("$NAME"), "output: {output}");
    }

    #[tokio::test]
    async fn no_match_on_angle_bracket_placeholder_names_the_real_identifier() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("a.go"), "func handler() { run() }\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "<decode>(...)"}),
        )
        .await;
        assert!(output.starts_with("No matches"), "output: {output}");
        // The miss should point at the placeholder, not leave the model retrying it.
        assert!(
            output.contains("`<decode>` is not a placeholder"),
            "output: {output}"
        );
        assert!(output.contains("decode(...)"), "output: {output}");
    }

    #[tokio::test]
    async fn returns_parse_error_when_pattern_uses_metavar_with_two_kinds() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("file.txt"), "anything\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "$X ... $...X"}),
        )
        .await;
        assert!(
            output.starts_with("invalid codegrep pattern:"),
            "expected parse error, got: {output}"
        );
    }

    #[tokio::test]
    async fn returns_parse_error_when_pattern_is_empty_or_whitespace() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("file.py"), "import os\n  pass\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "   "}),
        )
        .await;
        assert!(
            output.starts_with("invalid codegrep pattern:"),
            "expected parse error, got: {output}"
        );
    }

    #[tokio::test]
    async fn returns_error_when_pattern_field_is_missing() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        let output = run(&CodegrepTool, &test_ctx(tmp.path()), serde_json::json!({})).await;
        assert!(output.contains("Missing required parameter"));
    }

    #[tokio::test]
    async fn caps_match_count_at_max_results() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        let mut content = String::new();
        for i in 0..(MAX_RESULTS + 10) {
            content.push_str(&format!("fn fn_{i}() {{}}\n"));
        }
        fs::write(tmp.path().join("many.rs"), content).unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "fn $N()"}),
        )
        .await;
        assert_eq!(output.lines().count(), MAX_RESULTS, "output: {output}");
    }

    #[tokio::test]
    async fn output_includes_one_based_line_and_column_for_first_byte_of_match() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("file.rs"), "fn main() {}\n  fn aux() {}\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "fn aux()"}),
        )
        .await;
        assert!(
            output.contains(":2:3:"),
            "expected line 2 col 3, got: {output}"
        );
    }

    #[tokio::test]
    async fn collapses_multi_line_match_substring_into_summary_line() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("file.rs"), "a\nb\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "a....b"}),
        )
        .await;
        assert!(
            output.contains("a\\nb"),
            "expected escaped newline, got: {output}"
        );
    }

    #[tokio::test]
    async fn omits_captures_section_when_match_has_no_metavariables() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("file.rs"), "hello world\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({"pattern": "hello"}),
        )
        .await;
        assert!(!output.contains("[$"), "output: {output}");
    }

    #[tokio::test]
    async fn constraint_keeps_only_matches_whose_capture_satisfies_regex() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("keys.rs"),
            "let good = AKIAIOSFODNN7EXAMPLE;\nlet bad = AKIAshort;\n",
        )
        .unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "$X",
                "constraints": [{"metavariable": "X", "regex": "^AKIA[0-9A-Z]{16}$"}]
            }),
        )
        .await;
        assert!(output.contains("AKIAIOSFODNN7EXAMPLE"), "output: {output}");
        assert!(!output.contains("AKIAshort"), "output: {output}");
    }

    #[tokio::test]
    async fn returns_error_when_constraint_names_unknown_metavariable() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("keys.rs"),
            "let k = AKIAIOSFODNN7EXAMPLE;\n",
        )
        .unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "$X",
                "constraints": [{"metavariable": "NOPE", "regex": "^.*$"}]
            }),
        )
        .await;
        assert!(output.contains("unknown metavariable"), "output: {output}");
    }

    #[tokio::test]
    async fn returns_error_when_constraint_regex_is_invalid() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("keys.rs"),
            "let k = AKIAIOSFODNN7EXAMPLE;\n",
        )
        .unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "$X",
                "constraints": [{"metavariable": "X", "regex": "["}]
            }),
        )
        .await;
        assert!(
            output.contains("invalid metavariable-regex"),
            "output: {output}"
        );
    }

    #[tokio::test]
    async fn constraint_filters_a_plain_metavar_capture() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("a.php"), "include('x.php');\nprint('y');\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "$FUNC(...)",
                "constraints": [{"metavariable": "FUNC", "regex": "^(include|require|require_once)$"}]
            }),
        )
        .await;
        assert!(output.contains("include"), "output: {output}");
        assert!(!output.contains("print"), "output: {output}");
    }

    #[tokio::test]
    async fn constraint_accepts_a_capture_named_inside_brackets() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("a.rs"), "foo(secret123);\nfoo(public);\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "foo($X)",
                "constraints": [{"metavariable": "X", "regex": "^secret"}]
            }),
        )
        .await;
        assert!(!output.contains("unknown metavariable"), "output: {output}");
        assert!(output.contains("secret123"), "output: {output}");
        assert!(!output.contains("public"), "output: {output}");
    }

    #[tokio::test]
    async fn constraint_metavariable_accepts_a_leading_dollar() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("keys.rs"),
            "let g = AKIAIOSFODNN7EXAMPLE;\nlet b = AKIAshort;\n",
        )
        .unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "$X",
                "constraints": [{"metavariable": "$X", "regex": "^AKIA[0-9A-Z]{16}$"}]
            }),
        )
        .await;
        assert!(output.contains("AKIAIOSFODNN7EXAMPLE"), "output: {output}");
        assert!(!output.contains("AKIAshort"), "output: {output}");
    }

    #[tokio::test]
    async fn multiple_constraints_all_must_match() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("c.txt"), "aaa = bbb\naaa = ccc\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "$L = $R",
                "constraints": [
                    {"metavariable": "L", "regex": "^aaa$"},
                    {"metavariable": "R", "regex": "^bbb$"}
                ]
            }),
        )
        .await;
        assert!(output.contains("bbb"), "output: {output}");
        assert!(!output.contains("ccc"), "output: {output}");
    }

    #[tokio::test]
    async fn caseless_match_does_not_relax_a_case_sensitive_constraint() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("k.rs"),
            "let u = AKIAIOSFODNN7EXAMPLE;\nlet l = akiaiosfodnn7example;\n",
        )
        .unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "$X",
                "caseless": true,
                "constraints": [{"metavariable": "X", "regex": "^AKIA[0-9A-Z]{16}$"}]
            }),
        )
        .await;
        assert!(output.contains("AKIAIOSFODNN7EXAMPLE"), "output: {output}");
        assert!(!output.contains("akiaiosfodnn7example"), "output: {output}");
    }

    #[tokio::test]
    async fn constraint_supports_regex_flags_classes_and_alternation() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("k.txt"), "id = SECRET123\nid = TOKEN45\n").unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "id = $V",
                "constraints": [{"metavariable": "V", "regex": "(?i)^(secret|token)\\d{3}$"}]
            }),
        )
        .await;
        assert!(output.contains("SECRET123"), "output: {output}");
        assert!(!output.contains("TOKEN45"), "output: {output}");
    }

    #[tokio::test]
    async fn constraint_filters_on_a_span_capture_text() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("a.js"),
            "exec(\"rm -rf /tmp\")\nexec(\"ls\")\n",
        )
        .unwrap();
        let output = run(
            &CodegrepTool,
            &test_ctx(tmp.path()),
            serde_json::json!({
                "pattern": "exec(\"$...ARG\")",
                "constraints": [{"metavariable": "ARG", "regex": "rm"}]
            }),
        )
        .await;
        assert!(output.contains("rm -rf"), "output: {output}");
        assert!(!output.contains("\"ls\""), "output: {output}");
    }
}
