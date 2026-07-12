//! `ManageKnowledgeTool`: the model's interface to a `Knowledge` store.
//! The store lives in `agents::knowledge`; this file only wraps it
//! with a `ToolLike` impl driven by the declarative `manage_knowledge.tool.md`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::agents::knowledge::Knowledge;
use crate::agents::stats::KnowledgeOp;
use crate::providers::ProviderResult;

use super::tool::{ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;

/// The model's four-action handle on a `Knowledge` store:
/// `write`, `read`, `remove`, `list`.
///
/// # Examples
///
/// ```no_run
/// use agentwerk::{Agent, Knowledge};
/// use agentwerk::tools::ManageKnowledgeTool;
///
/// let store = Knowledge::load(".agentwerk").expect("knowledge dir");
/// Agent::new()
///     .knowledge(&store)
///     .tool(ManageKnowledgeTool::new(store));
/// ```
pub struct ManageKnowledgeTool {
    store: Arc<Knowledge>,
}

impl ManageKnowledgeTool {
    pub fn new(store: Arc<Knowledge>) -> Self {
        Self { store }
    }
}

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("manage_knowledge.tool.md")))
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

impl ToolLike for ManageKnowledgeTool {
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
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>> {
        Box::pin(async move {
            let action = input.get("action").and_then(Value::as_str).unwrap_or("");
            // The tool self-reports each outcome: only it can see a read/remove
            // miss, which returns Ok, so the shared tool-call loop cannot.
            let record = |op: KnowledgeOp| {
                if let Some(system) = ctx.ticket_system_handle() {
                    system.stats().record_knowledge(op);
                }
            };

            match action {
                "write" => {
                    let slug = match input.get("slug").and_then(Value::as_str) {
                        Some(s) => s,
                        None => return Ok(ToolResult::error("Missing required parameter: slug")),
                    };
                    let description = match input.get("description").and_then(Value::as_str) {
                        Some(s) => s,
                        None => {
                            return Ok(ToolResult::error("Missing required parameter: description"))
                        }
                    };
                    let content = match input.get("content").and_then(Value::as_str) {
                        Some(s) => s,
                        None => {
                            return Ok(ToolResult::error("Missing required parameter: content"))
                        }
                    };
                    // Kind and tags stay host-side concerns set through the
                    // Page API; the model only names, describes, and fills a page.
                    let page = crate::agents::knowledge::Page {
                        slug: slug.to_string(),
                        kind: String::new(),
                        description: description.to_string(),
                        content: content.to_string(),
                        tags: Vec::new(),
                    };
                    match self.store.pages().save(page) {
                        Ok(out) => {
                            record(KnowledgeOp::Write);
                            let pct = if out.index_char_limit > 0 {
                                (out.index_chars_used * 100) / out.index_char_limit
                            } else {
                                0
                            };
                            Ok(ToolResult::success(format!(
                                "{} ({} pages, {}% — {}/{} chars)",
                                out.message,
                                out.pages,
                                pct,
                                out.index_chars_used,
                                out.index_char_limit,
                            )))
                        }
                        Err(why) => Ok(ToolResult::error(why)),
                    }
                }

                "read" => {
                    let slug = match input.get("slug").and_then(Value::as_str) {
                        Some(s) => s,
                        None => return Ok(ToolResult::error("Missing required parameter: slug")),
                    };
                    match self.store.pages().load(slug) {
                        Ok(page) => {
                            record(KnowledgeOp::Read);
                            Ok(ToolResult::success(page.content))
                        }
                        Err(_) => {
                            record(KnowledgeOp::Miss);
                            Ok(ToolResult::success(format!(
                                "No page found for `{slug}`. Check the knowledge index before reading — only slugs listed there exist."
                            )))
                        }
                    }
                }

                "remove" => {
                    let slug = match input.get("slug").and_then(Value::as_str) {
                        Some(s) => s,
                        None => return Ok(ToolResult::error("Missing required parameter: slug")),
                    };
                    match self.store.pages().remove(slug) {
                        Ok(out) => {
                            record(KnowledgeOp::Remove);
                            let pct = if out.index_char_limit > 0 {
                                (out.index_chars_used * 100) / out.index_char_limit
                            } else {
                                0
                            };
                            Ok(ToolResult::success(format!(
                                "{} ({} pages, {}% — {}/{} chars)",
                                out.message,
                                out.pages,
                                pct,
                                out.index_chars_used,
                                out.index_char_limit,
                            )))
                        }
                        Err(why) => {
                            record(KnowledgeOp::Miss);
                            Ok(ToolResult::error(why))
                        }
                    }
                }

                "list" => {
                    record(KnowledgeOp::List);
                    let idx = self.store.index();
                    let body = if idx.is_empty() {
                        "(no pages)".to_string()
                    } else {
                        idx
                    };
                    Ok(ToolResult::success(body))
                }

                "" => Ok(ToolResult::error("Missing required parameter: action")),
                other => Ok(ToolResult::error(format!("Unknown action: {other}"))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::knowledge::{Knowledge, Page};

    fn fresh_store() -> (Arc<Knowledge>, crate::test_util::TempDir) {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        (store, dir)
    }

    fn save_page(store: &Knowledge, slug: &str, description: &str, content: &str, tags: &[&str]) {
        let page = Page {
            slug: slug.to_string(),
            kind: String::new(),
            description: description.to_string(),
            content: content.to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
        };
        store.pages().save(page).unwrap();
    }

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::current_dir().unwrap())
    }

    fn assert_success(result: &ToolResult, fragment: &str) {
        match result {
            ToolResult::Success(s) => {
                assert!(s.contains(fragment), "expected `{fragment}` in `{s}`")
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    fn assert_error(result: &ToolResult, fragment: &str) {
        match result {
            ToolResult::Error(s) => assert!(s.contains(fragment), "expected `{fragment}` in `{s}`"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_action_creates_page() {
        let (store, _dir) = fresh_store();
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                serde_json::json!({
                    "action": "write",
                    "slug": "test",
                    "description": "A test page",
                    "content": "# Test\n\nContent."
                }),
                &ctx(),
            )
            .await
            .unwrap();
        assert_success(&r, "page written");
        assert!(store.index().contains("test"));
    }

    #[tokio::test]
    async fn read_action_returns_page_body() {
        let (store, _dir) = fresh_store();
        save_page(&store, "test", "A test", "# Test\n\nHello.", &[]);
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                serde_json::json!({"action": "read", "slug": "test"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_success(&r, "Hello.");
    }

    #[tokio::test]
    async fn read_action_missing_page_returns_soft_success() {
        let (store, _dir) = fresh_store();
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                serde_json::json!({"action": "read", "slug": "nonexistent"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_success(&r, "No page found");
    }

    #[tokio::test]
    async fn read_action_strips_frontmatter() {
        let (store, _dir) = fresh_store();
        save_page(&store, "test", "A test", "# Test\n\nHello.", &["tag"]);
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                serde_json::json!({"action": "read", "slug": "test"}),
                &ctx(),
            )
            .await
            .unwrap();
        match &r {
            ToolResult::Success(s) => {
                assert!(!s.contains("---"));
                assert!(!s.contains("timestamp:"));
            }
            _ => panic!("expected Success"),
        }
    }

    #[tokio::test]
    async fn remove_action_deletes_page() {
        let (store, _dir) = fresh_store();
        save_page(&store, "temp", "Temporary", "# Temp", &[]);
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                serde_json::json!({"action": "remove", "slug": "temp"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_success(&r, "page removed");
        assert!(store.index().is_empty());
    }

    #[tokio::test]
    async fn list_action_returns_index() {
        let (store, _dir) = fresh_store();
        save_page(&store, "config", "Config page", "# Config", &[]);
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(serde_json::json!({"action": "list"}), &ctx())
            .await
            .unwrap();
        assert_success(&r, "config");
    }

    #[tokio::test]
    async fn list_action_empty_store() {
        let (store, _dir) = fresh_store();
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(serde_json::json!({"action": "list"}), &ctx())
            .await
            .unwrap();
        assert_success(&r, "(no pages)");
    }

    #[tokio::test]
    async fn write_without_slug_is_rejected() {
        let (store, _dir) = fresh_store();
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                serde_json::json!({"action": "write", "description": "s", "content": "c"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_error(&r, "slug");
    }

    #[tokio::test]
    async fn write_without_description_is_rejected() {
        let (store, _dir) = fresh_store();
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                serde_json::json!({"action": "write", "slug": "test", "content": "c"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_error(&r, "description");
    }

    #[tokio::test]
    async fn write_without_content_is_rejected() {
        let (store, _dir) = fresh_store();
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                serde_json::json!({"action": "write", "slug": "test", "description": "s"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_error(&r, "content");
    }

    #[tokio::test]
    async fn unknown_action_is_rejected() {
        let (store, _dir) = fresh_store();
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(serde_json::json!({"action": "wat"}), &ctx())
            .await
            .unwrap();
        assert_error(&r, "Unknown action");
    }

    #[tokio::test]
    async fn missing_action_is_rejected() {
        let (store, _dir) = fresh_store();
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let r = tool.call(serde_json::json!({}), &ctx()).await.unwrap();
        assert_error(&r, "action");
    }

    #[tokio::test]
    async fn self_reports_each_action_to_stats() {
        use crate::agents::tickets::TicketSystem;

        let (store, _dir) = fresh_store();
        let tool = ManageKnowledgeTool::new(Arc::clone(&store));
        let tickets = TicketSystem::new();
        let ctx =
            ToolContext::new(std::env::current_dir().unwrap()).ticket_system(Arc::clone(&tickets));

        tool.call(
            serde_json::json!({
                "action": "write", "slug": "note",
                "description": "a note", "content": "body",
            }),
            &ctx,
        )
        .await
        .unwrap();
        tool.call(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        tool.call(serde_json::json!({"action": "read", "slug": "note"}), &ctx)
            .await
            .unwrap();
        tool.call(serde_json::json!({"action": "read", "slug": "ghost"}), &ctx)
            .await
            .unwrap();
        tool.call(
            serde_json::json!({"action": "remove", "slug": "note"}),
            &ctx,
        )
        .await
        .unwrap();

        let k = tickets.stats().knowledge_stats();
        assert_eq!(k.writes, 1);
        assert_eq!(k.lists, 1);
        assert_eq!(k.reads, 1, "only the present slug counts as a read");
        assert_eq!(k.misses, 1, "the read of an absent slug is a miss");
        assert_eq!(k.removes, 1);
    }
}
