//! In-place find-and-replace on a file, so a model can modify existing code without restating the whole file.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use crate::schemas::Schema;

use super::tool::{Tool, ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;

/// In-place string replacement in an existing file. The model supplies the
/// old and new strings; the tool fails if the old string is absent or
/// matches more than once. Not concurrent.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::EditFileTool;
///
/// Agent::new().tool(EditFileTool);
/// ```
pub struct EditFileTool;

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| {
        ToolFile::parse(
            include_str!("edit_file.tool.md"),
            include_str!("edit_file.schema.json"),
        )
    })
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

#[derive(serde::Deserialize)]
pub struct EditFileArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

impl ToolLike for EditFileTool {
    type Args = EditFileArgs;

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
        args: EditFileArgs,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(run(args, ctx.clone()))
    }
}

impl From<EditFileTool> for Tool {
    fn from(_: EditFileTool) -> Tool {
        Tool::from_tool_file(
            include_str!("edit_file.tool.md"),
            include_str!("edit_file.schema.json"),
            run,
        )
        .paths(["path"])
    }
}

async fn run(args: EditFileArgs, ctx: ToolContext) -> ToolResult {
    let EditFileArgs {
        path,
        old_string,
        new_string,
        replace_all,
    } = args;
    let (old_string, new_string) = (old_string.as_str(), new_string.as_str());

    let resolved = ctx.dir.join(&path);

    let content = match std::fs::read_to_string(&resolved) {
        Ok(c) => c,
        Err(e) => {
            return ToolResult::error(format!("Failed to read file: {e}"));
        }
    };

    let count = content.matches(old_string).count();

    if count == 0 {
        return ToolResult::error(format!("old_string not found in {path}"));
    }

    if count > 1 && !replace_all {
        return ToolResult::error(format!(
            "Found {count} occurrences of old_string in {path}. Use replace_all to replace all."
        ));
    }

    let new_content = if replace_all {
        content.replace(old_string, new_string)
    } else {
        content.replacen(old_string, new_string, 1)
    };

    match std::fs::write(&resolved, &new_content) {
        Ok(()) => ToolResult::success(format!("Edited {path}: replaced {count} occurrence(s)")),
        Err(e) => ToolResult::error(format!("Failed to write file: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        let document = tool_file().input_schema.get_raw_schema().clone();
        for example in document["examples"].as_array().expect("examples") {
            serde_json::from_value::<EditFileArgs>(example.clone())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
    }
    use std::path::PathBuf;

    fn test_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(PathBuf::from(dir))
    }

    #[tokio::test]
    async fn unique_match_replaced() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello world").unwrap();

        let tool = EditFileTool;
        let ctx = test_ctx(dir.path());

        let result = crate::tools::erase(tool)
            .call_with(
                serde_json::json!({
                    "path": "f.txt",
                    "old_string": "world",
                    "new_string": "rust"
                }),
                &ctx,
            )
            .await;

        let (ToolResult::Success(out) | ToolResult::Error(out) | ToolResult::SchemaError(out)) =
            &result;
        assert!(
            matches!(result, ToolResult::Success(_)),
            "unexpected error: {out}"
        );
        let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "hello rust");
    }

    #[tokio::test]
    async fn non_unique_errors_without_replace_all() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "aaa bbb aaa").unwrap();

        let tool = EditFileTool;
        let ctx = test_ctx(dir.path());

        let result = crate::tools::erase(tool)
            .call_with(
                serde_json::json!({
                    "path": "f.txt",
                    "old_string": "aaa",
                    "new_string": "ccc"
                }),
                &ctx,
            )
            .await;

        let content = result.content();
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("2"));
    }

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "aaa bbb aaa").unwrap();

        let tool = EditFileTool;
        let ctx = test_ctx(dir.path());

        let result = crate::tools::erase(tool)
            .call_with(
                serde_json::json!({
                    "path": "f.txt",
                    "old_string": "aaa",
                    "new_string": "ccc",
                    "replace_all": true
                }),
                &ctx,
            )
            .await;

        let (ToolResult::Success(out) | ToolResult::Error(out) | ToolResult::SchemaError(out)) =
            &result;
        assert!(
            matches!(result, ToolResult::Success(_)),
            "unexpected error: {out}"
        );
        let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "ccc bbb ccc");
    }

    #[tokio::test]
    async fn not_found_errors() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello world").unwrap();

        let tool = EditFileTool;
        let ctx = test_ctx(dir.path());

        let result = crate::tools::erase(tool)
            .call_with(
                serde_json::json!({
                    "path": "f.txt",
                    "old_string": "missing",
                    "new_string": "replacement"
                }),
                &ctx,
            )
            .await;

        let content = result.content();
        assert!(matches!(result, ToolResult::Error(_)));
        assert!(content.contains("not found"));
    }
}
