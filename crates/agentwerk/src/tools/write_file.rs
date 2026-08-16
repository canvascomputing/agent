//! Lets an agent create or overwrite a file on disk. Pairs with `read_file` and `edit_file` to give a model full file-editing reach.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use crate::schemas::Schema;

use super::tool::{ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;
use crate::providers::ProviderResult as Result;

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

impl ToolLike for WriteFileTool {
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
        input: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'a>> {
        Box::pin(async move {
            let path = input["path"].as_str().unwrap_or_default();
            let content = input["content"].as_str().unwrap_or_default();

            let resolved = ctx.dir.join(path);

            if let Some(parent) = resolved.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return Ok(ToolResult::error(format!(
                        "Failed to create parent directories: {e}"
                    )));
                }
            }

            match std::fs::write(&resolved, content) {
                Ok(()) => Ok(ToolResult::success(format!("File written: {path}"))),
                Err(e) => Ok(ToolResult::error(format!("Failed to write file: {e}"))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(PathBuf::from(dir))
    }

    #[tokio::test]
    async fn create_new_file() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let tool = WriteFileTool;
        let ctx = test_ctx(dir.path());

        let result = tool
            .call(
                serde_json::json!({ "path": "new.txt", "content": "hello world" }),
                &ctx,
            )
            .await
            .unwrap();

        let content = result.content();
        assert!(content.contains("File written: new.txt"));

        let written = std::fs::read_to_string(dir.path().join("new.txt")).unwrap();
        assert_eq!(written, "hello world");
    }

    #[tokio::test]
    async fn overwrite_existing_file() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("existing.txt"), "old content").unwrap();

        let tool = WriteFileTool;
        let ctx = test_ctx(dir.path());

        let result = tool
            .call(
                serde_json::json!({ "path": "existing.txt", "content": "new content" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(matches!(result, ToolResult::Success(_)));
        let written = std::fs::read_to_string(dir.path().join("existing.txt")).unwrap();
        assert_eq!(written, "new content");
    }

    #[tokio::test]
    async fn creates_parent_dirs() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let tool = WriteFileTool;
        let ctx = test_ctx(dir.path());

        let result = tool
            .call(
                serde_json::json!({ "path": "a/b/c/deep.txt", "content": "nested" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(matches!(result, ToolResult::Success(_)));
        let written = std::fs::read_to_string(dir.path().join("a/b/c/deep.txt")).unwrap();
        assert_eq!(written, "nested");
    }
}
