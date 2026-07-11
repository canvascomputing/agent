//! Content search across files. Gives a model a way to locate the relevant code before opening any single file.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use super::tool::{ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;
use super::util::glob_matches_file;
use crate::providers::ProviderResult as Result;

/// Search file contents by substring under the working directory. Read-only.
/// Returns matching line snippets with file paths and line numbers; capped
/// at 100 hits.
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

const MAX_RESULTS: usize = 100;
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "vendor"];

/// Maximum bytes of a source line to include in content-mode output. Lines
/// longer than this (common in minified bundles) are sliced to a window
/// around the match column so a single hit never dumps megabytes into the
/// tool result. The agent can follow up with `read_file_tool` and
/// `column`/`length` for the full context.
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

            let base = ctx.dir.clone();
            let glob_filter = input["glob"].as_str().map(|s| s.to_string());
            let output_mode = input["output_mode"].as_str().unwrap_or("content");
            let case_insensitive = input["case_insensitive"].as_bool().unwrap_or(false);

            let needle = if case_insensitive {
                pattern.to_lowercase()
            } else {
                pattern.to_string()
            };

            let mut files = Vec::new();
            collect_files(&base, &base, &glob_filter, &mut files);

            let result = match output_mode {
                "files" => search_files(&files, &base, &needle, case_insensitive),
                _ => search_content(&files, &base, &needle, case_insensitive),
            };

            Ok(ToolResult::success(result))
        })
    }
}

fn search_content(files: &[PathBuf], base: &Path, needle: &str, case_insensitive: bool) -> String {
    let mut output = Vec::new();

    'outer: for file_path in files {
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let rel = relative_path(file_path, base);

        for (i, line) in content.lines().enumerate() {
            if let Some(col) = line_matches(line, needle, case_insensitive) {
                let snippet = truncate_around(line, col.saturating_sub(1), MAX_LINE_DISPLAY);
                output.push(format!("{}:{}:{}: {}", rel, i + 1, col, snippet));
                if output.len() >= MAX_RESULTS {
                    break 'outer;
                }
            }
        }
    }

    output.join("\n")
}

fn search_files(files: &[PathBuf], base: &Path, needle: &str, case_insensitive: bool) -> String {
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
            if matched.len() >= MAX_RESULTS {
                break;
            }
        }
    }

    matched.join("\n")
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

        // content mode is the default
        let result = tool
            .call(serde_json::json!({"pattern": "Hello"}), &ctx)
            .await
            .unwrap();
        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        // Match lines have format "file:line_no:col: content"
        for line in content.lines() {
            let parts: Vec<&str> = line.splitn(4, ':').collect();
            assert!(
                parts.len() == 4,
                "Expected 4-part format (path:line:col: content) but got: {line}"
            );
        }
    }

    #[tokio::test]
    async fn glob_with_directory_matches_relative_path() {
        let tmp = setup_test_dir();
        let tool = GrepTool;
        let ctx = test_ctx(tmp.path());

        let result = tool
            .call(
                serde_json::json!({"pattern": "Hello", "glob": "src/*.rs", "output_mode": "files"}),
                &ctx,
            )
            .await
            .unwrap();

        let (ToolResult::Success(content)
        | ToolResult::Error(content)
        | ToolResult::SchemaError(content)) = &result;
        assert!(content.contains("src/main.rs"), "got {content:?}");
        assert!(content.contains("src/lib.rs"), "got {content:?}");
        assert!(
            !content.contains("readme.md"),
            "readme.md lies outside src/, got {content:?}"
        );
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
