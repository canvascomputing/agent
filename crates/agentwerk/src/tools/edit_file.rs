//! In-place find-and-replace on a file, so a model can modify existing code without restating the whole file.

use super::tool::{Tool, ToolContext, ToolResult};
use crate::prompts::directives::{
    EDIT_FILE_OLD_STRING_NOT_FOUND, EDIT_FILE_OLD_STRING_NOT_UNIQUE, EDIT_FILE_READ_FAILED,
    EDIT_FILE_WRITE_FAILED,
};

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

#[derive(serde::Deserialize)]
pub struct EditFileArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

impl From<EditFileTool> for Tool {
    fn from(_: EditFileTool) -> Tool {
        Tool::new("edit_file")
            .description(include_str!("edit_file.tool.md"))
            .schema(include_str!("edit_file.schema.json"))
            .paths(["path"])
            .handler(run)
            .build()
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
            return ToolResult::error(ctx.directives.render(
                EDIT_FILE_READ_FAILED,
                &[("path", &path), ("error", &e.to_string())],
            ));
        }
    };

    let count = content.matches(old_string).count();

    if count == 0 {
        return ToolResult::error(
            ctx.directives
                .render(EDIT_FILE_OLD_STRING_NOT_FOUND, &[("path", &path)]),
        );
    }

    if count > 1 && !replace_all {
        return ToolResult::error(ctx.directives.render(
            EDIT_FILE_OLD_STRING_NOT_UNIQUE,
            &[("path", &path), ("count", &count.to_string())],
        ));
    }

    let new_content = if replace_all {
        content.replace(old_string, new_string)
    } else {
        content.replacen(old_string, new_string, 1)
    };

    match std::fs::write(&resolved, &new_content) {
        Ok(()) => ToolResult::success(format!("Edited {path}: replaced {count} occurrence(s)")),
        Err(e) => ToolResult::error(ctx.directives.render(
            EDIT_FILE_WRITE_FAILED,
            &[("path", &path), ("error", &e.to_string())],
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        let document = Tool::from(EditFileTool)
            .get_input_schema()
            .get_raw_schema()
            .clone();
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

        let result = Tool::from(tool)
            .call(
                serde_json::json!({
                    "path": "f.txt",
                    "old_string": "world",
                    "new_string": "rust"
                }),
                &ctx,
            )
            .await;

        let out = result.get_content();
        assert!(
            matches!(result, ToolResult::Success { .. }),
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

        let result = Tool::from(tool)
            .call(
                serde_json::json!({
                    "path": "f.txt",
                    "old_string": "aaa",
                    "new_string": "ccc"
                }),
                &ctx,
            )
            .await;

        let content = result.get_content();
        assert!(matches!(result, ToolResult::Error { .. }));
        assert!(content.contains("2"));
    }

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "aaa bbb aaa").unwrap();

        let tool = EditFileTool;
        let ctx = test_ctx(dir.path());

        let result = Tool::from(tool)
            .call(
                serde_json::json!({
                    "path": "f.txt",
                    "old_string": "aaa",
                    "new_string": "ccc",
                    "replace_all": true
                }),
                &ctx,
            )
            .await;

        let out = result.get_content();
        assert!(
            matches!(result, ToolResult::Success { .. }),
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

        let result = Tool::from(tool)
            .call(
                serde_json::json!({
                    "path": "f.txt",
                    "old_string": "missing",
                    "new_string": "replacement"
                }),
                &ctx,
            )
            .await;

        let content = result.get_content();
        assert!(matches!(result, ToolResult::Error { .. }));
        assert!(content.contains("No `old_string` match"));
    }
}
