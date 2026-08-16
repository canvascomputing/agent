//! Lets an agent write, read, remove, and list the knowledge it shares across
//! tickets and with other agents.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use crate::agents::knowledge::{Knowledge, KnowledgeError};
use crate::event::{EventKind, KnowledgeFailureKind, KnowledgeOp};

use crate::schemas::Schema;

use super::tool::{ToolContext, ToolLike, ToolResult};
use super::tool_file::ToolFile;

/// The model's four-action handle on a `Knowledge` store:
/// `write`, `read`, `remove`, `list`. Registered automatically on every
/// agent; `AgentBuilder::knowledge` rebinds it to the passed store.
///
/// # Examples
///
/// ```no_run
/// use agentwerk::{Agent, Knowledge};
///
/// let store = Knowledge::load(".agentwerk").expect("knowledge dir");
/// Agent::new().knowledge(&store);
/// ```
pub struct KnowledgeTool {
    store: Arc<Knowledge>,
}

impl KnowledgeTool {
    /// Bind the tool to `store` without making it the agent's own knowledge.
    /// `AgentBuilder::knowledge` is the usual route: it does this and also
    /// renders the store's index into the system prompt. Reach for the
    /// constructor when an agent should write to a store it is not told about.
    pub fn new(store: Arc<Knowledge>) -> Self {
        Self { store }
    }
}

fn tool_file() -> &'static ToolFile {
    static FILE: OnceLock<ToolFile> = OnceLock::new();
    FILE.get_or_init(|| ToolFile::parse(include_str!("knowledge.tool.md")))
}

fn description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| tool_file().render_markdown())
}

/// Which failure the statistics count this under. Everything but a missing
/// page is the store refusing: rejected values or IO.
fn failure_kind(error: &KnowledgeError) -> KnowledgeFailureKind {
    match error {
        KnowledgeError::PageMissing { .. } => KnowledgeFailureKind::PageMissing,
        _ => KnowledgeFailureKind::StoreRefused,
    }
}

/// Progress line shown to the model after a mutation: how much of the
/// index budget is consumed.
fn usage_line(message: &str, store: &Knowledge) -> String {
    let (used, limit, pages) = store.index_usage();
    let pct = if limit > 0 { (used * 100) / limit } else { 0 };
    format!("{message} ({pages} pages, {pct}%, {used}/{limit} chars)")
}

/// What the model asks the store to do. The schema declares `action` as the
/// discriminator and states which fields each one requires; the variants are
/// the same statement in Rust, so a `write` without a `slug` can neither reach
/// this tool nor be forgotten inside it.
#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum KnowledgeArgs {
    Write {
        slug: String,
        description: String,
        content: String,
    },
    Read {
        slug: String,
    },
    Remove {
        slug: String,
    },
    List,
}

impl ToolLike for KnowledgeTool {
    type Args = KnowledgeArgs;

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

    fn call<'a>(
        &'a self,
        args: KnowledgeArgs,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            // The tool self-reports each outcome: only it can see a read/remove
            // miss, which returns Ok, so the shared tool-call loop cannot.
            let record = |kind: EventKind| ctx.emit(kind);

            match args {
                KnowledgeArgs::Write {
                    slug,
                    description,
                    content,
                } => {
                    // Kind and tags stay host-side concerns set through the
                    // Page API; the model only names, describes, and fills a page.
                    let page = crate::agents::knowledge::Page {
                        slug,
                        kind: String::new(),
                        description,
                        content,
                        tags: Vec::new(),
                    };
                    match self.store.pages().save(page) {
                        Ok(()) => {
                            record(EventKind::KnowledgeUsed {
                                op: KnowledgeOp::Write,
                            });
                            ToolResult::success(usage_line("page written", &self.store))
                        }
                        Err(why) => {
                            record(EventKind::KnowledgeFailed {
                                op: KnowledgeOp::Write,
                                reason: failure_kind(&why),
                            });
                            ToolResult::error(why.to_string())
                        }
                    }
                }

                KnowledgeArgs::Read { slug } => match self.store.pages().load(&slug) {
                    Ok(page) => {
                        record(EventKind::KnowledgeUsed {
                            op: KnowledgeOp::Read,
                        });
                        ToolResult::success(page.content)
                    }
                    Err(why) => {
                        record(EventKind::KnowledgeFailed {
                            op: KnowledgeOp::Read,
                            reason: failure_kind(&why),
                        });
                        ToolResult::success(format!(
                                "No page found for `{slug}`. The `list` action shows every page that exists: an unlisted slug cannot be read."
                            ))
                    }
                },

                KnowledgeArgs::Remove { slug } => match self.store.pages().remove(&slug) {
                    Ok(()) => {
                        record(EventKind::KnowledgeUsed {
                            op: KnowledgeOp::Remove,
                        });
                        ToolResult::success(usage_line("page removed", &self.store))
                    }
                    Err(why) => {
                        record(EventKind::KnowledgeFailed {
                            op: KnowledgeOp::Remove,
                            reason: failure_kind(&why),
                        });
                        ToolResult::error(why.to_string())
                    }
                },

                KnowledgeArgs::List => {
                    record(EventKind::KnowledgeUsed {
                        op: KnowledgeOp::List,
                    });
                    // Not the prompt's limited view: this is how the agent sees
                    // the pages the prompt had no room for.
                    let index = self.store.full_index();
                    let body = if index.is_empty() {
                        "(no pages)".to_string()
                    } else {
                        index
                    };
                    ToolResult::success(body)
                }
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

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        // The schema and this enum both describe the shape. The examples are
        // where they are held to the same one.
        let document = tool_file().input_schema.get_raw_schema().clone();
        for example in document["examples"].as_array().expect("examples") {
            serde_json::from_value::<KnowledgeArgs>(example.clone())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
    }

    #[tokio::test]
    async fn write_action_creates_page() {
        let (store, _dir) = fresh_store();
        let tool = KnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                KnowledgeArgs::Write {
                    slug: "test".into(),
                    description: "A test page".into(),
                    content: "# Test\n\nContent.".into(),
                },
                &ctx(),
            )
            .await;
        assert_success(&r, "page written");
        assert!(store.index().contains("test"));
    }

    #[tokio::test]
    async fn read_action_returns_page_body() {
        let (store, _dir) = fresh_store();
        save_page(&store, "test", "A test", "# Test\n\nHello.", &[]);
        let tool = KnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                KnowledgeArgs::Read {
                    slug: "test".into(),
                },
                &ctx(),
            )
            .await;
        assert_success(&r, "Hello.");
    }

    #[tokio::test]
    async fn read_action_missing_page_returns_soft_success() {
        let (store, _dir) = fresh_store();
        let tool = KnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                KnowledgeArgs::Read {
                    slug: "nonexistent".into(),
                },
                &ctx(),
            )
            .await;
        assert_success(&r, "No page found");
    }

    #[tokio::test]
    async fn read_action_strips_frontmatter() {
        let (store, _dir) = fresh_store();
        save_page(&store, "test", "A test", "# Test\n\nHello.", &["tag"]);
        let tool = KnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                KnowledgeArgs::Read {
                    slug: "test".into(),
                },
                &ctx(),
            )
            .await;
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
        let tool = KnowledgeTool::new(Arc::clone(&store));
        let r = tool
            .call(
                KnowledgeArgs::Remove {
                    slug: "temp".into(),
                },
                &ctx(),
            )
            .await;
        assert_success(&r, "page removed");
        assert!(store.index().is_empty());
    }

    #[tokio::test]
    async fn list_action_returns_index() {
        let (store, _dir) = fresh_store();
        save_page(&store, "config", "Config page", "# Config", &[]);
        let tool = KnowledgeTool::new(Arc::clone(&store));
        let r = tool.call(KnowledgeArgs::List, &ctx()).await;
        assert_success(&r, "config");
    }

    #[tokio::test]
    async fn list_action_returns_every_page_past_the_index_limit() {
        let (store, _dir) = fresh_store();
        store.index_char_limit(60);
        for i in 0..10 {
            save_page(&store, &format!("page-{i}"), "A note", "# Note", &[]);
        }
        let tool = KnowledgeTool::new(Arc::clone(&store));
        let r = tool.call(KnowledgeArgs::List, &ctx()).await;

        assert!(!store.index().contains("page-9"), "{}", store.index());
        assert_success(&r, "page-9");
    }

    #[tokio::test]
    async fn list_action_empty_store() {
        let (store, _dir) = fresh_store();
        let tool = KnowledgeTool::new(Arc::clone(&store));
        let r = tool.call(KnowledgeArgs::List, &ctx()).await;
        assert_success(&r, "(no pages)");
    }

    /// Run a call the way an agent does: through the registry, which checks the
    /// arguments against the schema before the tool sees them. The tests below
    /// read what the model would.
    async fn dispatch(store: &Arc<Knowledge>, input: serde_json::Value) -> String {
        let mut registry = crate::tools::ToolRegistry::default();
        registry.register(KnowledgeTool::new(Arc::clone(store)));
        let calls = vec![crate::tools::ToolCall {
            id: "c1".into(),
            name: "knowledge".into(),
            input,
        }];
        let results = registry.execute(&calls, &ctx()).await;
        results[0].content.clone()
    }

    #[tokio::test]
    async fn a_write_names_every_field_it_is_missing_at_once() {
        let (store, _dir) = fresh_store();
        let reported = dispatch(&store, serde_json::json!({"action": "write"})).await;
        assert!(reported.contains("`slug`"), "{reported}");
        assert!(reported.contains("`description`"), "{reported}");
        assert!(reported.contains("`content`"), "{reported}");
    }

    #[tokio::test]
    async fn a_read_without_a_slug_is_rejected() {
        let (store, _dir) = fresh_store();
        let reported = dispatch(&store, serde_json::json!({"action": "read"})).await;
        assert!(reported.contains("`slug`"), "{reported}");
    }

    #[tokio::test]
    async fn a_list_needs_no_other_field() {
        let (store, _dir) = fresh_store();
        let reported = dispatch(&store, serde_json::json!({"action": "list"})).await;
        assert_eq!(reported, "(no pages)");
    }

    #[tokio::test]
    async fn an_unknown_action_is_rejected() {
        let (store, _dir) = fresh_store();
        let reported = dispatch(&store, serde_json::json!({"action": "wat"})).await;
        assert!(reported.contains("`enum`"), "{reported}");
    }

    #[tokio::test]
    async fn a_missing_action_is_rejected() {
        let (store, _dir) = fresh_store();
        let reported = dispatch(&store, serde_json::json!({})).await;
        assert!(reported.contains("`action`"), "{reported}");
    }

    #[tokio::test]
    async fn self_reports_each_action_as_an_event() {
        use crate::agents::tickets::TicketQueue;
        use crate::event::EventKind;
        use std::sync::Mutex;

        let (store, _dir) = fresh_store();
        let tool = KnowledgeTool::new(Arc::clone(&store));
        let tickets = TicketQueue::new();
        let reported = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&reported);
        tickets.on_event(move |event| match &event.kind {
            EventKind::KnowledgeUsed { op } => seen.lock().unwrap().push(op.to_string()),
            EventKind::KnowledgeFailed { op, reason } => seen
                .lock()
                .unwrap()
                .push(format!("{op}:{}", reason.as_str())),
            _ => {}
        });
        let ctx =
            ToolContext::new(std::env::current_dir().unwrap()).ticket_queue(Arc::clone(&tickets));

        tool.call(
            KnowledgeArgs::Write {
                slug: "note".into(),
                description: "a note".into(),
                content: "body".into(),
            },
            &ctx,
        )
        .await;
        tool.call(KnowledgeArgs::List, &ctx).await;
        tool.call(
            KnowledgeArgs::Read {
                slug: "note".into(),
            },
            &ctx,
        )
        .await;
        tool.call(
            KnowledgeArgs::Read {
                slug: "ghost".into(),
            },
            &ctx,
        )
        .await;
        tool.call(
            KnowledgeArgs::Remove {
                slug: "note".into(),
            },
            &ctx,
        )
        .await;

        // Every action reports itself, and the read of an absent slug reports
        // the reason it did not go through.
        assert_eq!(
            *reported.lock().unwrap(),
            vec!["write", "list", "read", "read:page_missing", "remove"],
        );
    }
}
