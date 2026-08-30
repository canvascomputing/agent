//! Lets an agent create or overwrite a file on disk. Pairs with `read_file` and `edit_file` to give a model full file-editing reach.

use super::tool::{Event, Tool, ToolContext};
use crate::prompts::directives::{WRITE_FILE_FAILED, WRITE_FILE_PARENT_NOT_CREATED};

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

#[derive(serde::Deserialize)]
pub struct WriteFileArgs {
    path: String,
    content: String,
}

impl From<WriteFileTool> for Tool {
    fn from(_: WriteFileTool) -> Tool {
        Tool::new("write_file")
            .description(include_str!("write_file.tool.md"))
            .schema(include_str!("write_file.schema.json"))
            .paths(["path"])
            .handler(run)
    }
}

async fn run(args: WriteFileArgs, ctx: ToolContext) -> Event {
    let WriteFileArgs { path, content } = args;

    let resolved = ctx.dir.join(&path);

    if let Some(parent) = resolved.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Event::error(ctx.directives.render(
                WRITE_FILE_PARENT_NOT_CREATED,
                &[("path", &path), ("error", &e.to_string())],
            ))
            .directive(WRITE_FILE_PARENT_NOT_CREATED);
        }
    }

    match std::fs::write(&resolved, content) {
        Ok(()) => Event::success(format!("File written: {path}")),
        Err(e) => Event::error(ctx.directives.render(
            WRITE_FILE_FAILED,
            &[("path", &path), ("error", &e.to_string())],
        ))
        .directive(WRITE_FILE_FAILED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        let document = Tool::from(WriteFileTool)
            .get_input_schema()
            .get_raw_schema()
            .clone();
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
        let tool = Tool::from(WriteFileTool);
        let ctx = test_ctx(dir.path());

        let result = tool
            .call(
                serde_json::json!({ "path": "new.txt", "content": "hello world" }),
                &ctx,
            )
            .await;

        let content = result.get_content();
        assert!(content.contains("File written: new.txt"));

        let written = std::fs::read_to_string(dir.path().join("new.txt")).unwrap();
        assert_eq!(written, "hello world");
    }

    #[tokio::test]
    async fn overwrite_existing_file() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("existing.txt"), "old content").unwrap();

        let tool = Tool::from(WriteFileTool);
        let ctx = test_ctx(dir.path());

        let result = tool
            .call(
                serde_json::json!({ "path": "existing.txt", "content": "new content" }),
                &ctx,
            )
            .await;

        assert!(result.get_name() == Event::TOOL_CALL_FINISHED);
        let written = std::fs::read_to_string(dir.path().join("existing.txt")).unwrap();
        assert_eq!(written, "new content");
    }

    #[tokio::test]
    async fn creates_parent_dirs() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let tool = Tool::from(WriteFileTool);
        let ctx = test_ctx(dir.path());

        let result = tool
            .call(
                serde_json::json!({ "path": "a/b/c/deep.txt", "content": "nested" }),
                &ctx,
            )
            .await;

        assert!(result.get_name() == Event::TOOL_CALL_FINISHED);
        let written = std::fs::read_to_string(dir.path().join("a/b/c/deep.txt")).unwrap();
        assert_eq!(written, "nested");
    }
}
