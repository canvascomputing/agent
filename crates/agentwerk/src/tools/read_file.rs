//! The agent's eyes on the filesystem. Lets a model read a file it did not receive in the prompt.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use crate::providers::ProviderResult;

use crate::schemas::Schema;

use super::tool::{ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;

/// Read a file with optional line offset and limit. Returns line-numbered
/// text so the model can reference specific lines in subsequent edits.
/// Concurrent.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::ReadFileTool;
///
/// Agent::new().tool(ReadFileTool);
/// ```
pub struct ReadFileTool;

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("read_file.tool.md")))
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

/// `limit` declares no default: absent means the rest of the file from
/// `offset`, which only the file's length gives.
#[derive(serde::Deserialize)]
pub struct ReadFileArgs {
    path: String,
    #[serde(default = "first_line")]
    offset: u64,
    limit: Option<u64>,
    column: Option<u64>,
    length: Option<u64>,
}

fn first_line() -> u64 {
    1
}

impl ToolLike for ReadFileTool {
    type Args = ReadFileArgs;

    fn name(&self) -> &str {
        &tool_file().name
    }

    fn description(&self) -> &str {
        description()
    }

    fn input_schema(&self) -> Schema {
        tool_file().input_schema.clone()
    }

    fn is_concurrent(&self) -> bool {
        tool_file().concurrent
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
        args: ReadFileArgs,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>> {
        Box::pin(async move {
            let ReadFileArgs {
                path,
                offset,
                limit,
                column,
                length,
            } = args;

            let resolved = ctx.dir.join(&path);

            if resolved.is_dir() {
                let message = match super::util::directory_entries(&resolved) {
                    Some(entries) => format!(
                        "'{path}' is a directory, not a file. Read one of its entries by \
                         appending the name to the path:\n  {entries}"
                    ),
                    None => format!("'{path}' is a directory, not a file."),
                };
                return Ok(ToolResult::error(message));
            }

            let content = match std::fs::read(&resolved) {
                Ok(bytes) => {
                    // A NUL byte marks a true binary (image, archive, compiled
                    // object); text, even minified or lightly obfuscated, never
                    // contains one. Report it concisely instead of dumping decoded
                    // garbage that floods the transcript and breaks strict chat
                    // templates. Otherwise decode lossily so odd-encoded source
                    // stays inspectable, the point of a scan.
                    if bytes.contains(&0) {
                        return Ok(ToolResult::success(format!(
                            "{path} is a binary file ({} bytes), not text; it cannot be read as source. Judge from the information you already have.",
                            bytes.len()
                        )));
                    }
                    String::from_utf8_lossy(&bytes).into_owned()
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(ToolResult::error(format!(
                        "File does not exist: {path}. {}",
                        super::util::not_found_hint(&ctx.dir, &resolved)
                    )));
                }
                Err(e) => {
                    return Ok(ToolResult::error(format!("Failed to read file: {e}")));
                }
            };

            let lines: Vec<&str> = content.lines().collect();

            let offset = offset.max(1) as usize;
            let limit = limit
                .map(|l| l as usize)
                .unwrap_or(lines.len().saturating_sub(offset - 1));
            let column = column.map(|c| c.max(1) as usize);
            let length = length.map(|c| c as usize);

            let start = (offset - 1).min(lines.len());
            let end = (start + limit).min(lines.len());

            let mut result = String::new();
            for (i, line) in lines[start..end].iter().enumerate() {
                let line_num = start + i + 1;
                if !result.is_empty() {
                    result.push('\n');
                }
                match column {
                    Some(col) => {
                        let byte_start = snap_to_char_boundary(line, (col - 1).min(line.len()));
                        let byte_end = match length {
                            Some(len) => {
                                snap_to_char_boundary(line, (byte_start + len).min(line.len()))
                            }
                            None => line.len(),
                        };
                        let slice = &line[byte_start..byte_end];
                        let display_col = byte_start + 1;
                        result.push_str(&format!("{line_num}:{display_col}\t{slice}"));
                    }
                    None => {
                        result.push_str(&format!("{line_num}\t{line}"));
                    }
                }
            }

            Ok(ToolResult::success(result))
        })
    }
}

fn snap_to_char_boundary(s: &str, pos: usize) -> usize {
    let pos = pos.min(s.len());
    let mut p = pos;
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        let document = tool_file().input_schema.get_raw_schema().clone();
        for example in document["examples"].as_array().expect("examples") {
            serde_json::from_value::<ReadFileArgs>(example.clone())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
    }
    use std::path::PathBuf;

    fn test_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(PathBuf::from(dir))
    }

    #[test]
    fn opened_paths_reports_the_file_argument_for_file_tools() {
        use crate::tools::{EditFileTool, GlobTool, GrepTool, ListDirectoryTool, WriteFileTool};

        let input = serde_json::json!({"path": "src/lib.rs"});
        let openers: Vec<std::sync::Arc<dyn crate::tools::AnyTool>> = vec![
            crate::tools::erase(ReadFileTool),
            crate::tools::erase(WriteFileTool),
            crate::tools::erase(EditFileTool),
        ];
        for tool in &openers {
            assert_eq!(
                tool.opened_paths(&input),
                vec!["src/lib.rs".to_string()],
                "{} should report its path",
                tool.name(),
            );
        }

        // Directory, pattern, and content-search tools open no file.
        assert!(ListDirectoryTool.opened_paths(&input).is_empty());
        assert!(GlobTool.opened_paths(&input).is_empty());
        assert!(GrepTool.opened_paths(&input).is_empty());
    }

    #[tokio::test]
    async fn a_slice_the_model_quoted_reads_the_lines_it_asked_for() {
        // Dispatch retypes against the advertised schema, which is what keeps
        // these `as_u64` reads from silently defaulting to the whole file.
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "alpha\nbeta\ngamma\ndelta\n").unwrap();
        let mut registry = crate::tools::tool::ToolRegistry::default();
        registry.register(ReadFileTool);
        let calls = vec![crate::tools::ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "test.txt", "offset": "2", "limit": "2"}),
        }];

        let results = registry.execute(&calls, &test_ctx(dir.path())).await;

        assert_eq!(results[0].outcome.as_deref().unwrap(), "2\tbeta\n3\tgamma");
    }

    #[tokio::test]
    async fn read_file_cases() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "alpha\nbeta\ngamma\ndelta\n").unwrap();

        struct Case {
            name: &'static str,
            input: Value,
            expect_error: bool,
            expect_contains: &'static str,
        }

        let cases = vec![
            Case {
                name: "full file",
                input: serde_json::json!({ "path": "test.txt" }),
                expect_error: false,
                expect_contains: "1\talpha\n2\tbeta\n3\tgamma\n4\tdelta",
            },
            Case {
                name: "offset and limit",
                input: serde_json::json!({ "path": "test.txt", "offset": 2, "limit": 2 }),
                expect_error: false,
                expect_contains: "2\tbeta\n3\tgamma",
            },
            Case {
                name: "nonexistent file",
                input: serde_json::json!({ "path": "no_such_file.txt" }),
                expect_error: true,
                expect_contains: "File does not exist",
            },
            Case {
                name: "column slices from byte position",
                input: serde_json::json!({ "path": "test.txt", "offset": 1, "limit": 1, "column": 3 }),
                expect_error: false,
                expect_contains: "1:3\tpha",
            },
            Case {
                name: "column with length bounds the slice",
                input: serde_json::json!({ "path": "test.txt", "offset": 2, "limit": 1, "column": 2, "length": 3 }),
                expect_error: false,
                expect_contains: "2:2\teta",
            },
            Case {
                name: "column beyond line returns empty",
                input: serde_json::json!({ "path": "test.txt", "offset": 1, "limit": 1, "column": 100 }),
                expect_error: false,
                expect_contains: "1:6\t",
            },
            Case {
                name: "length past end of line clamps to EOL",
                input: serde_json::json!({ "path": "test.txt", "offset": 2, "limit": 1, "column": 2, "length": 100 }),
                expect_error: false,
                expect_contains: "2:2\teta",
            },
        ];

        let ctx = test_ctx(dir.path());

        for case in cases {
            let result = crate::tools::erase(ReadFileTool)
                .call_with(case.input, &ctx)
                .await
                .unwrap();
            let is_error = matches!(result, ToolResult::Error(_));
            let content = result.content();
            assert_eq!(
                is_error, case.expect_error,
                "case '{}': expected is_error={}, got is_error={}",
                case.name, case.expect_error, is_error
            );
            assert!(
                content.contains(case.expect_contains),
                "case '{}': expected content to contain {:?}, got {:?}",
                case.name,
                case.expect_contains,
                content
            );
        }
    }

    #[tokio::test]
    async fn read_file_on_directory_lists_entries() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("__init__.py"), "x = 1\n").unwrap();
        std::fs::write(dir.path().join("sessions.py"), "y = 2\n").unwrap();
        std::fs::create_dir(dir.path().join("subpkg")).unwrap();

        let result = crate::tools::erase(ReadFileTool)
            .call_with(serde_json::json!({ "path": "." }), &test_ctx(dir.path()))
            .await
            .unwrap();

        let ToolResult::Error(content) = &result else {
            panic!("reading a directory should return an error result, got {result:?}");
        };
        assert!(content.contains("is a directory"), "got {content:?}");
        assert!(content.contains("__init__.py"), "got {content:?}");
        assert!(content.contains("sessions.py"), "got {content:?}");
        assert!(
            content.contains("subpkg/"),
            "sub-directories carry a trailing slash, got {content:?}"
        );
    }

    #[tokio::test]
    async fn read_file_decodes_non_utf8_lossily_without_erroring() {
        let dir = crate::test_util::TempDir::new().unwrap();
        // Valid text with a stray non-UTF-8 byte, as in minified/obfuscated source.
        std::fs::write(dir.path().join("odd.py"), b"import os\xff\nx = 1\n").unwrap();

        let result = crate::tools::erase(ReadFileTool)
            .call_with(
                serde_json::json!({ "path": "odd.py" }),
                &test_ctx(dir.path()),
            )
            .await
            .unwrap();

        let ToolResult::Success(content) = &result else {
            panic!("a non-UTF-8 file should read lossily, not error, got {result:?}");
        };
        // The readable text survives; the bad byte becomes the replacement char.
        assert!(content.contains("import os"), "got {content:?}");
        assert!(content.contains("x = 1"), "got {content:?}");
        assert!(
            content.contains('\u{FFFD}'),
            "bad byte should be replaced, got {content:?}"
        );
    }

    #[tokio::test]
    async fn read_file_reports_binary_files_concisely_without_dumping_bytes() {
        let dir = crate::test_util::TempDir::new().unwrap();
        // A NUL byte marks a true binary; do not decode it to garbage.
        std::fs::write(dir.path().join("blob.bin"), [0x7f, 0x45, 0x00, 0x01, 0x02]).unwrap();

        let result = crate::tools::erase(ReadFileTool)
            .call_with(
                serde_json::json!({ "path": "blob.bin" }),
                &test_ctx(dir.path()),
            )
            .await
            .unwrap();

        let ToolResult::Success(content) = &result else {
            panic!("a binary file should report concisely as success, got {result:?}");
        };
        assert!(content.contains("binary file"), "got {content:?}");
        // No decoded garbage: the message is short, not the raw bytes.
        assert!(
            content.len() < 200,
            "should be a concise note, got {content:?}"
        );
    }

    #[tokio::test]
    async fn read_file_not_found_lists_the_directory_in_tree() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("helpers.py"), "x\n").unwrap();

        // Guess a file that does not exist; cwd is the dir holding helpers.py.
        let result = crate::tools::erase(ReadFileTool)
            .call_with(
                serde_json::json!({ "path": "missing.py" }),
                &test_ctx(dir.path()),
            )
            .await
            .unwrap();

        let ToolResult::Error(content) = &result else {
            panic!("a missing file should return an error result, got {result:?}");
        };
        assert!(content.contains("File does not exist"), "got {content:?}");
        assert!(
            content.contains("contains:") && content.contains("helpers.py"),
            "miss should list the directory's real entries, got {content:?}"
        );
    }

    #[tokio::test]
    async fn read_file_not_found_echoes_working_directory_and_suggests_dropped_folder() {
        // Working dir is a sub-folder holding the file; the model drops the
        // folder and reads <parent>/flask.py, which does not exist.
        let root = crate::test_util::TempDir::new().unwrap();
        let cwd = root.path().join("data83");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(cwd.join("flask.py"), "x = 1\n").unwrap();
        let dropped = root.path().join("flask.py");

        let result = crate::tools::erase(ReadFileTool)
            .call_with(
                serde_json::json!({ "path": dropped.to_str().unwrap() }),
                &test_ctx(&cwd),
            )
            .await
            .unwrap();

        let ToolResult::Error(content) = &result else {
            panic!("a missing file should return an error result, got {result:?}");
        };
        assert!(content.contains("File does not exist"), "got {content:?}");
        assert!(
            content.contains(&cwd.display().to_string()),
            "error echoes the working directory, got {content:?}"
        );
        assert!(
            content.contains("Did you mean") && content.contains("data83/flask.py"),
            "error suggests the dropped-folder candidate, got {content:?}"
        );
    }

    #[tokio::test]
    async fn read_past_eof_returns_empty() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "alpha\nbeta\n").unwrap();

        let result = crate::tools::erase(ReadFileTool)
            .call_with(
                serde_json::json!({ "path": "test.txt", "offset": 100 }),
                &test_ctx(dir.path()),
            )
            .await
            .unwrap();

        let ToolResult::Success(content) = &result else {
            panic!("offset past EOF should succeed with an empty slice, got {result:?}");
        };
        assert_eq!(content, "");
    }

    #[tokio::test]
    async fn column_snaps_to_char_boundary() {
        let dir = crate::test_util::TempDir::new().unwrap();
        // 'é' is two bytes; column 5 lands on its second byte.
        std::fs::write(dir.path().join("test.txt"), "caféx\n").unwrap();

        let result = crate::tools::erase(ReadFileTool)
            .call_with(
                serde_json::json!({ "path": "test.txt", "column": 5 }),
                &test_ctx(dir.path()),
            )
            .await
            .unwrap();

        let ToolResult::Success(content) = &result else {
            panic!("slicing mid-codepoint should succeed, got {result:?}");
        };
        // The slice starts at the next char boundary instead of splitting 'é'.
        assert_eq!(content, "1:6\tx");
    }
}
