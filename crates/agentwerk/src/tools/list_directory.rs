//! Lets an agent enumerate the contents of a directory: the first turn of any exploratory task against an unknown layout.

use std::path::PathBuf;

use super::tool::{Tool, ToolContext, ToolResult};

/// List the entries of a directory with type and size. Concurrent. Pair with
/// [`GlobTool`](crate::tools::GlobTool) when you need pattern-based file discovery.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::ListDirectoryTool;
///
/// Agent::new().tool(ListDirectoryTool);
/// ```
pub struct ListDirectoryTool;

#[derive(serde::Deserialize)]
pub struct ListDirectoryArgs {
    #[serde(default = "here")]
    path: String,
    #[serde(default)]
    recursive: bool,
}

fn here() -> String {
    ".".to_string()
}

impl From<ListDirectoryTool> for Tool {
    fn from(_: ListDirectoryTool) -> Tool {
        Tool::from_tool_file(
            include_str!("list_directory.tool.md"),
            include_str!("list_directory.schema.json"),
            run,
        )
    }
}

async fn run(args: ListDirectoryArgs, ctx: ToolContext) -> ToolResult {
    let ListDirectoryArgs {
        path: path_str,
        recursive,
    } = args;
    let base = ctx.dir.join(&path_str);

    if base.exists() && !base.is_dir() {
        return ToolResult::error(format!("Path is not a directory: {path_str}"));
    }

    match list_entries(&base, &base, recursive) {
        Ok(mut entries) => {
            entries.sort_by(|a, b| a.display_name.cmp(&b.display_name));
            // Suffix the type onto the name (`ls -F` style) instead of a
            // separate column: a bare `dir`/`file` word reads as a second
            // entry and gets listed as a path that does not exist.
            let lines: Vec<String> = entries
                .iter()
                .map(|e| match e.kind {
                    "dir" => format!("{}/", e.display_name),
                    "symlink" => format!("{}@", e.display_name),
                    _ => format!("{}  {} bytes", e.display_name, e.size.unwrap_or(0)),
                })
                .collect();
            ToolResult::success(lines.join("\n"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ToolResult::error(format!(
            "Directory does not exist: {path_str}. {}",
            super::util::not_found_hint(&ctx.dir, &base)
        )),
        Err(e) => ToolResult::error(format!("Error listing directory: {e}")),
    }
}

struct EntryInfo {
    display_name: String,
    kind: &'static str,
    size: Option<u64>,
}

fn list_entries(dir: &PathBuf, base: &PathBuf, recursive: bool) -> std::io::Result<Vec<EntryInfo>> {
    let mut results = Vec::new();
    let read_dir = std::fs::read_dir(dir)?;

    for entry in read_dir {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let file_type = metadata.file_type();

        let display_name = if recursive {
            entry
                .path()
                .strip_prefix(base)
                .unwrap_or(&entry.path())
                .to_string_lossy()
                .to_string()
        } else {
            entry.file_name().to_string_lossy().to_string()
        };

        let (kind, size) = if file_type.is_symlink() {
            ("symlink", None)
        } else if file_type.is_dir() {
            ("dir", None)
        } else {
            ("file", Some(metadata.len()))
        };

        results.push(EntryInfo {
            display_name,
            kind,
            size,
        });

        if recursive && file_type.is_dir() {
            let sub = list_entries(&entry.path(), base, true)?;
            results.extend(sub);
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        let document = Tool::from(ListDirectoryTool)
            .input_schema()
            .get_raw_schema()
            .clone();
        for example in document["examples"].as_array().expect("examples") {
            serde_json::from_value::<ListDirectoryArgs>(example.clone())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
    }
    use std::fs;

    fn test_ctx(path: &std::path::Path) -> ToolContext {
        ToolContext::new(path.to_path_buf())
    }

    #[tokio::test]
    async fn flat_listing() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("alpha.txt"), "hello").unwrap();
        fs::write(tmp.path().join("beta.txt"), "world").unwrap();
        fs::create_dir(tmp.path().join("subdir")).unwrap();

        let tool = Tool::from(ListDirectoryTool);
        let ctx = test_ctx(tmp.path());
        let result = tool.call(serde_json::json!({}), &ctx).await;

        let content = result.content();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        // Sorted alphabetically
        assert!(lines[0].starts_with("alpha.txt"));
        assert!(lines[0].contains("5 bytes"));
        assert!(lines[1].starts_with("beta.txt"));
        assert_eq!(lines[2], "subdir/");
    }

    #[tokio::test]
    async fn recursive_listing() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("root.txt"), "r").unwrap();
        fs::create_dir(tmp.path().join("child")).unwrap();
        fs::write(tmp.path().join("child").join("nested.txt"), "n").unwrap();

        let tool = Tool::from(ListDirectoryTool);
        let ctx = test_ctx(tmp.path());
        let result = tool
            .call(serde_json::json!({"recursive": true}), &ctx)
            .await;

        let content = result.content();
        assert!(content.contains("child/nested.txt") || content.contains("child\\nested.txt"));
        assert!(content.contains("root.txt"));
        // Should have at least 3 entries: root.txt, child, child/nested.txt
        assert!(content.lines().count() >= 3);
    }

    #[tokio::test]
    async fn list_directory_on_a_file_reports_not_a_directory() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::write(tmp.path().join("app.py"), "x = 1\n").unwrap();

        let result = Tool::from(ListDirectoryTool)
            .call(
                serde_json::json!({ "path": "app.py" }),
                &test_ctx(tmp.path()),
            )
            .await;

        let ToolResult::Error(content) = &result else {
            panic!("listing a file should return an error result, got {result:?}");
        };
        assert!(
            content.contains("Path is not a directory"),
            "got {content:?}"
        );
    }

    #[tokio::test]
    async fn list_directory_not_found_lists_the_nearest_directory_in_tree() {
        let tmp = crate::test_util::TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("pkg")).unwrap();

        // Guess a non-existent directory directly under cwd.
        let result = Tool::from(ListDirectoryTool)
            .call(serde_json::json!({ "path": "nope" }), &test_ctx(tmp.path()))
            .await;

        let ToolResult::Error(content) = &result else {
            panic!("a missing directory should return an error result, got {result:?}");
        };
        assert!(
            content.contains("Directory does not exist"),
            "got {content:?}"
        );
        assert!(
            content.contains("contains:") && content.contains("pkg/"),
            "miss should list the nearest directory's entries, got {content:?}"
        );
    }

    #[tokio::test]
    async fn list_directory_not_found_echoes_working_directory_and_suggests_dropped_folder() {
        let root = crate::test_util::TempDir::new().unwrap();
        let cwd = root.path().join("data83");
        fs::create_dir(&cwd).unwrap();
        fs::create_dir(cwd.join("pkg")).unwrap();
        let dropped = root.path().join("pkg");

        let result = Tool::from(ListDirectoryTool)
            .call(
                serde_json::json!({ "path": dropped.to_str().unwrap() }),
                &test_ctx(&cwd),
            )
            .await;

        let ToolResult::Error(content) = &result else {
            panic!("a missing directory should return an error result, got {result:?}");
        };
        assert!(
            content.contains("Directory does not exist"),
            "got {content:?}"
        );
        assert!(
            content.contains(&cwd.display().to_string()),
            "error echoes the working directory, got {content:?}"
        );
        assert!(
            content.contains("Did you mean") && content.contains("data83/pkg"),
            "error suggests the dropped-folder candidate, got {content:?}"
        );
    }
}
