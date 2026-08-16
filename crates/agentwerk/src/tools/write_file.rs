//! Lets an agent create or overwrite a file on disk. Pairs with `read_file` and `edit_file` to give a model full file-editing reach.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use crate::schemas::Schema;

use super::tool::{ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;

/// Create or overwrite a file. Destructive: existing content is replaced.
/// Not concurrent, so agentwerk runs it one call at a time.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::WriteFileTool;
///
/// Agent::new().tool(WriteFileTool);
/// ```
pub struct WriteFileTool;

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("write_file.tool.md")))
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

#[derive(serde::Deserialize)]
pub struct WriteFileArgs {
    path: String,
    content: String,
}

impl ToolLike for WriteFileTool {
    type Args = WriteFileArgs;

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
        args: WriteFileArgs,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let WriteFileArgs { path, content } = args;

            let resolved = ctx.dir.join(&path);

            if let Some(parent) = resolved.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolResult::error(format!("Failed to create parent directories: {e}"));
                }
            }

            match std::fs::write(&resolved, content) {
                Ok(()) => ToolResult::success(format!("File written: {path}")),
                Err(e) => ToolResult::error(format!("Failed to write file: {e}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        let document = tool_file().input_schema.get_raw_schema().clone();
        for example in document["examples"].as_array().expect("examples") {
            serde_json::from_value::<WriteFileArgs>(example.clone())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
    }
    use std::path::PathBuf;

    fn test_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(PathBuf::from(dir))
    }

    #[tokio::test]
    async fn create_new_file() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let tool = crate::tools::erase(WriteFileTool);
        let ctx = test_ctx(dir.path());

        let result = tool
            .call_with(
                serde_json::json!({ "path": "new.txt", "content": "hello world" }),
                &ctx,
            )
            .await;

        let content = result.content();
        assert!(content.contains("File written: new.txt"));

        let written = std::fs::read_to_string(dir.path().join("new.txt")).unwrap();
        assert_eq!(written, "hello world");
    }

    #[tokio::test]
    async fn overwrite_existing_file() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("existing.txt"), "old content").unwrap();

        let tool = crate::tools::erase(WriteFileTool);
        let ctx = test_ctx(dir.path());

        let result = tool
            .call_with(
                serde_json::json!({ "path": "existing.txt", "content": "new content" }),
                &ctx,
            )
            .await;

        assert!(matches!(result, ToolResult::Success(_)));
        let written = std::fs::read_to_string(dir.path().join("existing.txt")).unwrap();
        assert_eq!(written, "new content");
    }

    #[tokio::test]
    async fn creates_parent_dirs() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let tool = crate::tools::erase(WriteFileTool);
        let ctx = test_ctx(dir.path());

        let result = tool
            .call_with(
                serde_json::json!({ "path": "a/b/c/deep.txt", "content": "nested" }),
                &ctx,
            )
            .await;

        assert!(matches!(result, ToolResult::Success(_)));
        let written = std::fs::read_to_string(dir.path().join("a/b/c/deep.txt")).unwrap();
        assert_eq!(written, "nested");
    }
}
