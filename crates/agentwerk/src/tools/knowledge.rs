//! Lets an agent write, read, remove, and list the knowledge it shares across
//! tasks and with other agents.

use std::sync::Arc;

use crate::agents::knowledge::{Knowledge, KnowledgeError};
use crate::event::Event;
use crate::prompts::directives::{
    KNOWLEDGE_PAGE_NOT_FOUND, KNOWLEDGE_REMOVE_FAILED, KNOWLEDGE_WRITE_FAILED,
};

use super::tool::{Tool, ToolContext, ToolResult};

/// The model's four-action handle on a `Knowledge` store:
/// `write`, `read`, `remove`, `list`. Registered automatically on every
/// agent; `Agent::knowledge` rebinds it to the passed store.
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
    /// `Agent::knowledge` is the usual route: it does this and also
    /// renders the store's index into the system prompt. Reach for the
    /// constructor when an agent should write to a store it is not told about.
    pub fn new(store: Arc<Knowledge>) -> Self {
        Self { store }
    }
}

/// Which failure the statistics count this under. Everything but a missing
/// page is the store refusing: rejected values or IO.
fn failure_reason(error: &KnowledgeError) -> &'static str {
    match error {
        KnowledgeError::PageMissing { .. } => "page_missing",
        _ => "store_refused",
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

impl From<KnowledgeTool> for Tool {
    fn from(tool: KnowledgeTool) -> Tool {
        let store = tool.store;
        Tool::new("knowledge")
            .description(include_str!("knowledge.tool.md"))
            .schema(include_str!("knowledge.schema.json"))
            .handler(move |args: KnowledgeArgs, ctx: ToolContext| {
                let store = Arc::clone(&store);
                async move { run(&store, args, &ctx) }
            })
            .build()
    }
}

fn run(store: &Knowledge, args: KnowledgeArgs, ctx: &ToolContext) -> ToolResult {
    // The tool self-reports each outcome: only it can see a read/remove
    // miss, which returns Ok, so the shared tool-call loop cannot.
    let record = |event: Event| ctx.emit_event(event);

    match args {
        KnowledgeArgs::Write {
            slug,
            description,
            content,
        } => {
            // Kind and tags stay host-side concerns set through the
            // Page API; the model only names, describes, and fills a page.
            let written = slug.clone();
            let page = crate::agents::knowledge::Page {
                slug,
                kind: String::new(),
                description,
                content,
                tags: Vec::new(),
            };
            match store.get_pages().save(page) {
                Ok(()) => {
                    record(
                        Event::new(Event::KNOWLEDGE_WRITTEN)
                            .data(serde_json::json!({ "slug": written })),
                    );
                    ToolResult::success(usage_line("page written", &store))
                }
                Err(why) => {
                    record(Event::new(Event::KNOWLEDGE_FAILED).data(serde_json::json!({
                        "action": "write",
                        "reason": failure_reason(&why),
                    })));
                    ToolResult::error(
                        ctx.directives
                            .render(KNOWLEDGE_WRITE_FAILED, &[("error", &why.to_string())]),
                    )
                }
            }
        }

        KnowledgeArgs::Read { slug } => match store.get_pages().get_page(&slug) {
            Ok(page) => {
                record(Event::new(Event::KNOWLEDGE_READ).data(serde_json::json!({ "slug": slug })));
                ToolResult::success(page.content)
            }
            Err(why) => {
                record(Event::new(Event::KNOWLEDGE_FAILED).data(serde_json::json!({
                    "action": "read",
                    "reason": failure_reason(&why),
                })));
                ToolResult::success(
                    ctx.directives
                        .render(KNOWLEDGE_PAGE_NOT_FOUND, &[("slug", &slug)]),
                )
            }
        },

        KnowledgeArgs::Remove { slug } => match store.get_pages().remove(&slug) {
            Ok(()) => {
                record(
                    Event::new(Event::KNOWLEDGE_REMOVED).data(serde_json::json!({ "slug": slug })),
                );
                ToolResult::success(usage_line("page removed", &store))
            }
            Err(why) => {
                record(Event::new(Event::KNOWLEDGE_FAILED).data(serde_json::json!({
                    "action": "remove",
                    "reason": failure_reason(&why),
                })));
                ToolResult::error(
                    ctx.directives
                        .render(KNOWLEDGE_REMOVE_FAILED, &[("error", &why.to_string())]),
                )
            }
        },

        KnowledgeArgs::List => {
            record(Event::new(Event::KNOWLEDGE_LISTED));
            // Not the prompt's limited view: this is how the agent sees
            // the pages the prompt had no room for.
            let index = store.full_index();
            let body = if index.is_empty() {
                "(no pages)".to_string()
            } else {
                index
            };
            ToolResult::success(body)
        }
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
        store.get_pages().save(page).unwrap();
    }

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::current_dir().unwrap())
    }

    fn assert_success(result: &ToolResult, fragment: &str) {
        match result {
            ToolResult::Success { content: s, .. } => {
                assert!(s.contains(fragment), "expected `{fragment}` in `{s}`")
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        // The schema and this enum both describe the shape. The examples are
        // where they are held to the same one.
        let document =
            serde_json::from_str::<serde_json::Value>(include_str!("knowledge.schema.json"))
                .unwrap();
        for example in document["examples"].as_array().expect("examples") {
            serde_json::from_value::<KnowledgeArgs>(example.clone())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
    }

    #[tokio::test]
    async fn write_action_creates_page() {
        let (store, _dir) = fresh_store();
        let r = run(
            &store,
            KnowledgeArgs::Write {
                slug: "test".into(),
                description: "A test page".into(),
                content: "# Test\n\nContent.".into(),
            },
            &ctx(),
        );
        assert_success(&r, "page written");
        assert!(store.get_index().contains("test"));
    }

    #[tokio::test]
    async fn read_action_returns_page_body() {
        let (store, _dir) = fresh_store();
        save_page(&store, "test", "A test", "# Test\n\nHello.", &[]);
        let r = run(
            &store,
            KnowledgeArgs::Read {
                slug: "test".into(),
            },
            &ctx(),
        );
        assert_success(&r, "Hello.");
    }

    #[tokio::test]
    async fn read_action_missing_page_returns_soft_success() {
        let (store, _dir) = fresh_store();
        let r = run(
            &store,
            KnowledgeArgs::Read {
                slug: "nonexistent".into(),
            },
            &ctx(),
        );
        assert_success(&r, "No page found");
    }

    #[tokio::test]
    async fn read_action_strips_frontmatter() {
        let (store, _dir) = fresh_store();
        save_page(&store, "test", "A test", "# Test\n\nHello.", &["tag"]);
        let r = run(
            &store,
            KnowledgeArgs::Read {
                slug: "test".into(),
            },
            &ctx(),
        );
        match &r {
            ToolResult::Success { content: s, .. } => {
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
        let r = run(
            &store,
            KnowledgeArgs::Remove {
                slug: "temp".into(),
            },
            &ctx(),
        );
        assert_success(&r, "page removed");
        assert!(store.get_index().is_empty());
    }

    #[tokio::test]
    async fn list_action_returns_index() {
        let (store, _dir) = fresh_store();
        save_page(&store, "config", "Config page", "# Config", &[]);
        let r = run(&store, KnowledgeArgs::List, &ctx());
        assert_success(&r, "config");
    }

    #[tokio::test]
    async fn list_action_returns_every_page_past_the_index_limit() {
        let (store, _dir) = fresh_store();
        store.set_char_limit(60);
        for i in 0..10 {
            save_page(&store, &format!("page-{i}"), "A note", "# Note", &[]);
        }
        let r = run(&store, KnowledgeArgs::List, &ctx());

        assert!(
            !store.get_index().contains("page-9"),
            "{}",
            store.get_index()
        );
        assert_success(&r, "page-9");
    }

    #[tokio::test]
    async fn list_action_empty_store() {
        let (store, _dir) = fresh_store();
        let r = run(&store, KnowledgeArgs::List, &ctx());
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
        results[0].get_content().to_string()
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
        use crate::agents::tasks::Queue;
        use std::sync::Mutex;

        let (store, _dir) = fresh_store();
        let tasks = Queue::new();
        let reported = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&reported);
        tasks.on_event(move |_, event| {
            if event.get_name() == crate::event::Event::KNOWLEDGE_FAILED {
                let action = event.get_data()["action"].as_str().unwrap();
                let reason = event.get_data()["reason"].as_str().unwrap();
                seen.lock()
                    .unwrap()
                    .push(format!("{}:{action}:{reason}", event.get_name()));
            } else if event.get_name().starts_with("knowledge_") {
                seen.lock().unwrap().push(event.get_name().to_string());
            }
        });
        let ctx = ToolContext::new(std::env::current_dir().unwrap()).queue(Arc::clone(&tasks));

        run(
            &store,
            KnowledgeArgs::Write {
                slug: "note".into(),
                description: "a note".into(),
                content: "body".into(),
            },
            &ctx,
        );
        run(&store, KnowledgeArgs::List, &ctx);
        run(
            &store,
            KnowledgeArgs::Read {
                slug: "note".into(),
            },
            &ctx,
        );
        run(
            &store,
            KnowledgeArgs::Read {
                slug: "ghost".into(),
            },
            &ctx,
        );
        run(
            &store,
            KnowledgeArgs::Remove {
                slug: "note".into(),
            },
            &ctx,
        );

        // Every action reports itself under its own kind, and the read of an
        // absent slug reports the action and the reason it did not go through.
        assert_eq!(
            *reported.lock().unwrap(),
            vec![
                "knowledge_written",
                "knowledge_listed",
                "knowledge_read",
                "knowledge_failed:read:page_missing",
                "knowledge_removed",
            ],
        );
    }
}
