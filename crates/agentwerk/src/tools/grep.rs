//! Content search across files. Gives a model a way to locate the relevant code before opening any single file.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use super::tool::{ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;
use super::util::glob_match;
use crate::providers::ProviderResult as Result;

/// Search file contents by substring under the working directory. Read-only.
/// Returns matching line snippets with file paths and line numbers; capped
/// at 100 hits by default.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::GrepTool;
///
/// Agent::new().tool(GrepTool);
/// ```
pub struct GrepTool;

const DEFAULT_MAX_RESULTS: u64 = 100;
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "vendor"];

/// Maximum bytes of a source line to include in content-mode output. Lines
/// longer than this (common in minified bundles) are sliced to a window
/// around the match column so a single hit never dumps megabytes into the
/// tool result. The agent can follow up with `read_file_tool` and
/// `col_offset`/`col_limit` for the full context.
const MAX_LINE_DISPLAY: usize = 200;

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("grep.tool.md")))
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

impl ToolLike for GrepTool {
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

    fn opened_paths(&self, input: &Value) -> Vec<String> {
        input
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn call<'a>(
        &'a self,
        input: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
        Box::pin(async move {
            let pattern = match input["pattern"].as_str() {
                Some(p) => p,
                None => return Ok(ToolResult::error("Missing required parameter: pattern")),
            };

            let base = ctx.dir.join(input["path"].as_str().unwrap_or("."));
            let glob_filter = input["glob"].as_str().map(|s| s.to_string());
            let output_mode = input["output_mode"].as_str().unwrap_or("files");
            let context_lines = input["context_lines"].as_u64().unwrap_or(0) as usize;
            let case_insensitive = input["case_insensitive"].as_bool().unwrap_or(false);
            let max_results = input["max_results"].as_u64().unwrap_or(DEFAULT_MAX_RESULTS) as usize;

            let needle = if case_insensitive {
                pattern.to_lowercase()
            } else {
                pattern.to_string()
            };

            let mut files = Vec::new();
            collect_files(&base, &glob_filter, &mut files);

            let result = match output_mode {
                "content" => search_content(
                    &files,
                    &base,
                    &needle,
                    case_insensitive,
                    context_lines,
                    max_results,
                ),
                "count" => search_count(&files, &base, &needle, case_insensitive, max_results),
                _ => search_files(&files, &base, &needle, case_insensitive, max_results),
            };

            Ok(ToolResult::success(result))
        })
    }
}

fn search_content(
    files: &[PathBuf],
    base: &Path,
    needle: &str,
    case_insensitive: bool,
    context_lines: usize,
    max_results: usize,
) -> String {
    let mut output = Vec::new();

    'outer: for file_path in files {
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let lines: Vec<&str> = content.lines().collect();
        let rel = relative_path(file_path, base);

        let match_map: std::collections::BTreeMap<usize, usize> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, line)| {
                line_matches(line, needle, case_insensitive).map(|col| (i, col))
            })
            .collect();

        let match_indices: Vec<usize> = match_map.keys().copied().collect();

        let mut emitted = std::collections::BTreeSet::new();
        for &idx in &match_indices {
            let start = idx.saturating_sub(context_lines);
            let end = (idx + context_lines + 1).min(lines.len());
            for (li, line) in lines.iter().enumerate().take(end).skip(start) {
                if !emitted.insert(li) {
                    continue;
                }
                if let Some(&col) = match_map.get(&li) {
                    let snippet = truncate_around(line, col.saturating_sub(1), MAX_LINE_DISPLAY);
                    output.push(format!("{}:{}:{}: {}", rel, li + 1, col, snippet));
                } else {
                    let snippet = truncate_line(line, MAX_LINE_DISPLAY);
                    output.push(format!("{}:{}: {}", rel, li + 1, snippet));
                }
                if output.len() >= max_results {
                    break 'outer;
                }
            }
        }
    }

    output.join("\n")
}

fn search_count(
    files: &[PathBuf],
    base: &Path,
    needle: &str,
    case_insensitive: bool,
    max_results: usize,
) -> String {
    let mut counts = Vec::new();

    for file_path in files {
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let n = content
            .lines()
            .filter(|line| line_matches(line, needle, case_insensitive).is_some())
            .count();
        if n > 0 {
            counts.push(format!("{}: {n} matches", relative_path(file_path, base)));
        }
        if counts.len() >= max_results {
            break;
        }
    }

    counts.join("\n")
}

fn search_files(
    files: &[PathBuf],
    base: &Path,
    needle: &str,
    case_insensitive: bool,
    max_results: usize,
) -> String {
    let mut matched = Vec::new();

    for file_path in files {
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if content
            .lines()
            .any(|line| line_matches(line, needle, case_insensitive).is_some())
        {
            matched.push(relative_path(file_path, base));
            if matched.len() >= max_results {
                break;
            }
        }
    }

    matched.join("\n")
}

/// Truncate a line to `max` bytes from the start, snapping to a char boundary.
fn truncate_line(line: &str, max: usize) -> &str {
    if line.len() <= max {
        return line;
    }
    let mut end = max;
    while end < line.len() && !line.is_char_boundary(end) {
        end += 1;
    }
    &line[..end]
}

/// Return a window of up to `max` bytes centred on `byte_offset`, snapping
/// to char boundaries. For short lines the full line is returned unchanged.
fn truncate_around(line: &str, byte_offset: usize, max: usize) -> &str {
    if line.len() <= max {
        return line;
    }
    let half = max / 2;
    let raw_start = byte_offset.saturating_sub(half);
    let mut start = raw_start;
    // Snap start forward to a char boundary.
    while start < line.len() && !line.is_char_boundary(start) {
        start += 1;
    }
    let mut end = (start + max).min(line.len());
    // Snap end forward to a char boundary.
    while end < line.len() && !line.is_char_boundary(end) {
        end += 1;
    }
    &line[start..end]
}

/// Returns the 1-based column of the first match, or `None` if there is no match.
fn line_matches(line: &str, needle: &str, case_insensitive: bool) -> Option<usize> {
    let byte_offset = if case_insensitive {
        line.to_lowercase().find(needle)
    } else {
        line.find(needle)
    };
    byte_offset.map(|off| off + 1)
}

fn relative_path(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn collect_files(dir: &Path, glob_filter: &Option<String>, results: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                collect_files(&path, glob_filter, results);
            }
            continue;
        }

        if let Some(ref filter) = glob_filter {
            if !glob_match(filter, &name) {
                continue;
            }
        }
        results.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_ctx(path: &std::path::Path) -> ToolContext {
        ToolContext::new(path.to_path_buf())
    }

    fn setup_test_dir() -> crate::test_util::TempDir {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(
            tmp.path().join("src/main.rs"),
            "fn main() {\n    println!(\"Hello world\");\n}\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("src/lib.rs"),
            "pub fn greet() {\n    println!(\"Hello\");\n}\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("readme.md"),
            "# Hello Project\nThis is a test.\n",
        )
        .unwrap();
        tmp
    }

    #[tokio::test]
    async fn substring_match_found() {
        let tmp = setup_test_dir();
        let tool = GrepTool;
        let ctx = test_ctx(tmp.path());

        let result = tool
            .call(
                serde_json::json!({"pattern": "Hello world", "output_mode": "files"}),
                &ctx,
            )
            .await
            .unwrap();

        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(content.contains("main.rs"));
        // lib.rs has "Hello" but not "Hello world"
        assert!(!content.contains("lib.rs"));
    }

    #[tokio::test]
    async fn case_insensitive_search() {
        let tmp = setup_test_dir();
        let tool = GrepTool;
        let ctx = test_ctx(tmp.path());

        let result = tool
            .call(
                serde_json::json!({
                    "pattern": "hello world",
                    "case_insensitive": true,
                    "output_mode": "files"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(content.contains("main.rs"));
    }

    #[tokio::test]
    async fn context_lines_included() {
        let tmp = setup_test_dir();
        let tool = GrepTool;
        let ctx = test_ctx(tmp.path());

        let result = tool
            .call(
                serde_json::json!({
                    "pattern": "Hello world",
                    "output_mode": "content",
                    "context_lines": 1
                }),
                &ctx,
            )
            .await
            .unwrap();

        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        // Should include the matching line with column info
        assert!(content.contains("Hello world"));
        // "Hello world" starts at column 15 in `    println!("Hello world");`
        assert!(
            content.contains(":2:15: "),
            "Expected match line to include :2:15: but got:\n{content}"
        );
        // With 1 context line, should also include fn main() line (line before)
        assert!(content.contains("fn main()"));
    }

    #[tokio::test]
    async fn content_mode_includes_column_of_first_match() {
        let tmp = setup_test_dir();
        let tool = GrepTool;
        let ctx = test_ctx(tmp.path());

        let result = tool
            .call(
                serde_json::json!({
                    "pattern": "Hello world",
                    "output_mode": "content"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        // `    println!("Hello world");` — "Hello world" starts at byte 15 (1-based)
        let line = content
            .lines()
            .next()
            .expect("should have at least one line");
        let parts: Vec<&str> = line.splitn(4, ':').collect();
        assert_eq!(parts.len(), 4, "Expected path:line:col: content");
        assert_eq!(parts[2], "15", "Column of 'Hello world' should be 15");
    }

    #[tokio::test]
    async fn context_lines_omit_column() {
        let tmp = setup_test_dir();
        let tool = GrepTool;
        let ctx = test_ctx(tmp.path());

        let result = tool
            .call(
                serde_json::json!({
                    "pattern": "Hello world",
                    "output_mode": "content",
                    "context_lines": 1
                }),
                &ctx,
            )
            .await
            .unwrap();

        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        for line in content.lines() {
            if line.contains("Hello world") {
                // Match line: 4-part format
                let parts: Vec<&str> = line.splitn(4, ':').collect();
                assert_eq!(
                    parts.len(),
                    4,
                    "Match line should be path:line:col: content, got: {line}"
                );
            } else {
                // Context line: 3-part format (path:line: content)
                let parts: Vec<&str> = line.splitn(3, ':').collect();
                assert_eq!(
                    parts.len(),
                    3,
                    "Context line should be path:line: content, got: {line}"
                );
                // The second part should be a valid line number
                assert!(
                    parts[1].trim().parse::<usize>().is_ok(),
                    "Second part of context line should be a line number, got: {}",
                    parts[1]
                );
            }
        }
    }

    #[tokio::test]
    async fn case_insensitive_column_matches_original_position() {
        let tmp = setup_test_dir();
        let tool = GrepTool;
        let ctx = test_ctx(tmp.path());

        let result = tool
            .call(
                serde_json::json!({
                    "pattern": "hello world",
                    "case_insensitive": true,
                    "output_mode": "content"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        let line = content
            .lines()
            .next()
            .expect("should have at least one line");
        let parts: Vec<&str> = line.splitn(4, ':').collect();
        assert_eq!(parts.len(), 4);
        // Column should match the position of "Hello world" in the original (not lowercased) line
        assert_eq!(
            parts[2], "15",
            "Column under case-insensitive search should be 15"
        );
    }

    #[tokio::test]
    async fn all_output_modes() {
        let tmp = setup_test_dir();
        let tool = GrepTool;
        let ctx = test_ctx(tmp.path());

        // files mode
        let result = tool
            .call(
                serde_json::json!({"pattern": "Hello", "output_mode": "files"}),
                &ctx,
            )
            .await
            .unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        let file_lines: Vec<&str> = content.lines().collect();
        // Should find matches in main.rs, lib.rs, and readme.md
        assert!(file_lines.len() >= 2);

        // content mode
        let result = tool
            .call(
                serde_json::json!({"pattern": "Hello", "output_mode": "content"}),
                &ctx,
            )
            .await
            .unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        // Match lines (no context) have format "file:line_no:col: content"
        for line in content.lines() {
            let parts: Vec<&str> = line.splitn(4, ':').collect();
            assert!(
                parts.len() == 4,
                "Expected 4-part format (path:line:col: content) but got: {line}"
            );
        }

        // count mode
        let result = tool
            .call(
                serde_json::json!({"pattern": "Hello", "output_mode": "count"}),
                &ctx,
            )
            .await
            .unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        // Count lines have format "file: N matches"
        for line in content.lines() {
            assert!(
                line.contains("matches"),
                "Expected 'matches' in count line: {line}"
            );
        }
    }

    #[tokio::test]
    async fn content_mode_truncates_long_lines() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        // Build a single-line file with a needle buried deep inside.
        let prefix = "x".repeat(1000);
        let suffix = "y".repeat(1000);
        let content = format!("{prefix}NEEDLE{suffix}");
        fs::write(tmp.path().join("big.txt"), &content).unwrap();

        let tool = GrepTool;
        let ctx = test_ctx(tmp.path());

        let result = tool
            .call(
                serde_json::json!({"pattern": "NEEDLE", "output_mode": "content"}),
                &ctx,
            )
            .await
            .unwrap();

        let (ToolResult::Success(output)
        | ToolResult::Error(output)
        | ToolResult::SchemaError(output)) = &result;
        let line = output.lines().next().expect("should have a match");
        // The output line (including path:line:col: prefix) must be much
        // shorter than the 2006-byte source line.
        assert!(
            line.len() < 300,
            "grep content output should truncate long lines, got {} bytes",
            line.len()
        );
        // The snippet should still contain the needle.
        assert!(
            line.contains("NEEDLE"),
            "truncated output should contain the needle; got: {line}"
        );
    }
}
