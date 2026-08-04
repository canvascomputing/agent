//! The core entity of agentwerk: who an agent is, what it may call, and which
//! queue it works from.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use serde::Serialize;

use crate::prompts::{context_body, PromptBuilder};
use crate::providers::{Model, Provider, ProviderToolDefinition};
use crate::tools::{FinishTool, ManageKnowledgeTool, ToolLike, ToolRegistry};

use super::knowledge::Knowledge;
use super::policy::Policies;
use super::stats::Stats;
use super::tickets::{Ticket, TicketQueue};

static AGENT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn default_agent_name() -> String {
    let n = AGENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("agent-{n}")
}

// Builder

/// An `AgentBuilder` collects who the agent is, what it may call, and where it
/// works, then hands back the finished [`Agent`].
#[derive(Clone)]
pub struct AgentBuilder<P, M> {
    name: String,
    provider: P,
    model: M,
    role: String,
    labels: Vec<String>,
    interactive: bool,
    templates: Vec<(String, String)>,
    tools: ToolRegistry,
    dir: PathBuf,
    knowledge: Arc<Knowledge>,
}

impl AgentBuilder<(), ()> {
    pub fn new() -> Self {
        let knowledge = Knowledge::load(".agentwerk").expect("open knowledge store");
        let mut tools = ToolRegistry::default();
        tools.register(FinishTool);
        tools.register(ManageKnowledgeTool::new(Arc::clone(&knowledge)));
        Self {
            name: default_agent_name(),
            provider: (),
            model: (),
            role: String::new(),
            labels: Vec::new(),
            interactive: false,
            templates: Vec::new(),
            tools,
            dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            knowledge,
        }
    }

    /// Create an agent with no tools pre-registered.
    ///
    /// Register `FinishTool` yourself through [`Self::tool`]. Without it the
    /// agent cannot finish a ticket, and the same ticket is tried again.
    pub fn empty() -> Self {
        let knowledge = Knowledge::load(".agentwerk").expect("open knowledge store");
        let mut tools = ToolRegistry::default();
        tools.register(ManageKnowledgeTool::new(Arc::clone(&knowledge)));
        Self {
            name: default_agent_name(),
            provider: (),
            model: (),
            role: String::new(),
            labels: Vec::new(),
            interactive: false,
            templates: Vec::new(),
            tools,
            dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            knowledge,
        }
    }

    /// Read environment variables for configuration.
    ///
    /// The same as `provider_from_env().model_from_env()`. Panics when no LLM
    /// provider variable is set.
    pub fn from_env(self) -> AgentBuilder<Arc<dyn Provider>, Model> {
        self.provider_from_env().model_from_env()
    }
}

impl<M> AgentBuilder<(), M> {
    pub fn provider(self, p: Arc<dyn Provider>) -> AgentBuilder<Arc<dyn Provider>, M> {
        AgentBuilder {
            name: self.name,
            provider: p,
            model: self.model,
            role: self.role,
            labels: self.labels,
            interactive: self.interactive,
            templates: self.templates,
            tools: self.tools,
            dir: self.dir,
            knowledge: self.knowledge,
        }
    }

    /// Read only the LLM provider from environment variables. Panics when none is set.
    pub fn provider_from_env(self) -> AgentBuilder<Arc<dyn Provider>, M> {
        let p = crate::providers::provider_from_env().expect(
            "LLM provider required: set ANTHROPIC_API_KEY, OPENAI_API_KEY, MISTRAL_API_KEY, or LITELLM_API_KEY",
        );
        self.provider(p)
    }
}

impl<P> AgentBuilder<P, ()> {
    pub fn model(self, m: impl Into<Model>) -> AgentBuilder<P, Model> {
        AgentBuilder {
            name: self.name,
            provider: self.provider,
            model: m.into(),
            role: self.role,
            labels: self.labels,
            interactive: self.interactive,
            templates: self.templates,
            tools: self.tools,
            dir: self.dir,
            knowledge: self.knowledge,
        }
    }

    /// Read only the model from environment variables. Panics when none is set.
    pub fn model_from_env(self) -> AgentBuilder<P, Model> {
        let model = crate::providers::model_from_env().expect("model name required");
        self.model(model)
    }
}

impl<P, M> AgentBuilder<P, M> {
    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = n.into();
        self
    }

    /// Define who the agent is and how it should work.
    ///
    /// A `{context}` placeholder anywhere in the text expands to the facts of
    /// the moment: ticket key, date, working directory, platform, and one line
    /// per configured limit. Leave the placeholder out and nothing is added, so
    /// the role decides both whether those facts appear and where.
    pub fn role(mut self, r: impl Into<String>) -> Self {
        self.role = r.into();
        self
    }

    /// Restrict the agent to tickets carrying a matching label.
    pub fn label(mut self, l: impl Into<String>) -> Self {
        self.labels.push(l.into());
        self
    }

    /// Restrict the agent to tickets carrying any of these labels.
    pub fn labels<I, S>(mut self, iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.labels.extend(iter.into_iter().map(Into::into));
        self
    }

    /// Let the agent wait for new instructions to keep a ticket in-progress.
    ///
    /// The agent stops after a reply that calls no tool, and
    /// `TicketQueue::reply` drives the next turn.
    pub fn interactive(mut self) -> Self {
        self.interactive = true;
        self
    }

    /// Inject data into prompts with template strings.
    ///
    /// `{key}` is replaced in the agent's role and in any text task submitted
    /// through this agent. A placeholder with no value is left as it is.
    /// Binding `context` replaces the built-in block described on
    /// [`Self::role`].
    pub fn template(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.templates.push((key.into(), value.into()));
        self
    }

    /// Inject more than one entry into prompts.
    pub fn templates<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.templates
            .extend(vars.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Register a tool the agent may call.
    pub fn tool(mut self, tool: impl ToolLike + 'static) -> Self {
        self.tools.register(tool);
        self
    }

    /// Register several tools the agent may call.
    pub fn tools<I, T>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: ToolLike + 'static,
    {
        for t in tools {
            self.tools.register(t);
        }
        self
    }

    /// Set the directory the agent has access to, the current one by default.
    pub fn dir(mut self, p: impl Into<PathBuf>) -> Self {
        self.dir = p.into();
        self
    }

    /// Share a knowledge store, the durable memory the agent carries across
    /// tickets and shares with other agents.
    ///
    /// It replaces the store opened by default, both for what the prompt shows
    /// and for what `ManageKnowledgeTool` writes to. Hand the same store to
    /// several agents the way `ticket_queue(&shared)` shares a queue.
    pub fn knowledge(mut self, store: &Arc<Knowledge>) -> Self {
        self.tools
            .register(ManageKnowledgeTool::new(Arc::clone(store)));
        self.knowledge = Arc::clone(store);
        self
    }
}

// Inline-test inspectors. Production callers go through `Agent`, which
// carries its own copies of these methods; the builder-side ones exist
// so inline tests can exercise prompt assembly and tool registration
// without first calling `.build()`.
#[cfg(test)]
impl<P, M> AgentBuilder<P, M> {
    pub(super) fn get_name(&self) -> &str {
        &self.name
    }

    pub(super) fn is_interactive(&self) -> bool {
        self.interactive
    }

    pub(super) fn handles_labels(&self, ticket_labels: &[String]) -> bool {
        if ticket_labels.iter().any(|l| l == &self.name) {
            return true;
        }
        if self.labels.is_empty() {
            ticket_labels.is_empty()
        } else {
            self.labels
                .iter()
                .any(|l| ticket_labels.iter().any(|t| t == l))
        }
    }

    pub(super) fn tool_definitions(&self) -> Vec<ProviderToolDefinition> {
        self.tools.definitions()
    }

    pub(super) fn system_prompt(
        &self,
        knowledge: Option<&str>,
        policies: &Policies,
        stats: &Stats,
        ticket_key: &str,
    ) -> String {
        let mut b = PromptBuilder::default();
        if !self.role.is_empty() {
            let role = self.interpolate(&self.role);
            b = b.role(self.expand_context(role, policies, stats, ticket_key));
        }
        if let Some(snap) = knowledge.filter(|s| !s.is_empty()) {
            b = b.knowledge(snap.to_string());
        }
        b.build().system
    }

    /// Substitute the built-in `{context}` block. Runs after
    /// [`Self::interpolate`], so a caller-bound `context` has already
    /// consumed the placeholder and wins. Guarded on `contains` because
    /// building the block spawns `uname`.
    fn expand_context(
        &self,
        role: String,
        policies: &Policies,
        stats: &Stats,
        ticket_key: &str,
    ) -> String {
        if !role.contains("{context}") {
            return role;
        }
        role.replace(
            "{context}",
            &context_body(&self.dir, policies, stats, ticket_key),
        )
    }

    fn interpolate(&self, s: &str) -> String {
        if self.templates.is_empty() {
            return s.to_string();
        }
        let mut out = s.to_string();
        for (key, value) in &self.templates {
            out = out.replace(&format!("{{{key}}}"), value);
        }
        out
    }
}

impl AgentBuilder<Arc<dyn Provider>, Model> {
    /// Create the agent.
    ///
    /// It starts with a ticket queue of its own, so `.task(...).finish().await`
    /// works without one being set up. `TicketQueue::agent(...)` later moves
    /// those tickets into the shared queue.
    pub fn build(self) -> Agent {
        let mut agent = Agent {
            name: self.name,
            model: self.model,
            labels: self.labels,
            interactive: self.interactive,
            ticket_queue: TicketQueueRef::Shared(Weak::new()),
            provider: self.provider,
            role: self.role,
            templates: self.templates,
            tools: self.tools,
            dir: self.dir,
            knowledge: self.knowledge,
        };
        let private = TicketQueue::new();
        private.bind_agent(&mut agent);
        agent.ticket_queue = TicketQueueRef::Private(private);
        agent
    }
}

// Agent

/// How an `Agent` reaches its `TicketQueue`. A freshly built agent carries
/// `Private` until it is added to a queue; everything else carries `Shared`.
pub(crate) enum TicketQueueRef {
    Shared(Weak<TicketQueue>),
    Private(Arc<TicketQueue>),
}

impl TicketQueueRef {
    pub(crate) fn upgrade(&self) -> Option<Arc<TicketQueue>> {
        match self {
            Self::Shared(w) => w.upgrade(),
            Self::Private(a) => Some(Arc::clone(a)),
        }
    }
}

/// An `Agent` is the core entity of agentwerk. It has access to tools for
/// solving tasks in the form of tickets.
///
/// It claims the tickets its name or labels match, calls the LLM provider, runs
/// the tools the model asks for, and writes the result back.
///
/// ```no_run
/// use agentwerk::Agent;
/// use agentwerk::tools::ReadFileTool;
///
/// # async fn run() {
/// let agent = Agent::new()
///     .name("reader")
///     .from_env()
///     .role("Rust developer reading source files to answer questions.")
///     .tool(ReadFileTool)
///     .build();
/// # let _ = agent;
/// # }
/// ```
pub struct Agent {
    // pub(crate): read by loop, TicketQueue, or routing code
    pub(crate) name: String,
    pub(crate) model: Model,
    pub(crate) labels: Vec<String>,
    pub(crate) interactive: bool,
    pub(crate) ticket_queue: TicketQueueRef,
    // private: accessed through methods within agents::
    provider: Arc<dyn Provider>,
    role: String,
    templates: Vec<(String, String)>,
    tools: ToolRegistry,
    dir: PathBuf,
    knowledge: Arc<Knowledge>,
}

impl Clone for Agent {
    /// A clone points at the shared queue, so rebinding the original cannot
    /// leave the clone filing tickets into a queue nothing reads.
    fn clone(&self) -> Self {
        let ticket_queue = match &self.ticket_queue {
            TicketQueueRef::Shared(w) => TicketQueueRef::Shared(w.clone()),
            TicketQueueRef::Private(a) => TicketQueueRef::Shared(Arc::downgrade(a)),
        };
        Self {
            name: self.name.clone(),
            model: self.model.clone(),
            labels: self.labels.clone(),
            interactive: self.interactive,
            ticket_queue,
            provider: Arc::clone(&self.provider),
            role: self.role.clone(),
            templates: self.templates.clone(),
            tools: self.tools.clone(),
            dir: self.dir.clone(),
            knowledge: Arc::clone(&self.knowledge),
        }
    }
}

impl Agent {
    /// Start building an agent.
    pub fn new() -> AgentBuilder<(), ()> {
        AgentBuilder::new()
    }

    /// Start building an agent with no tools pre-registered.
    pub fn empty() -> AgentBuilder<(), ()> {
        AgentBuilder::empty()
    }

    pub(super) fn get_name(&self) -> &str {
        &self.name
    }

    pub(super) fn is_interactive(&self) -> bool {
        self.interactive
    }

    pub(super) fn handles_labels(&self, ticket_labels: &[String]) -> bool {
        if ticket_labels.iter().any(|l| l == &self.name) {
            return true;
        }
        if self.labels.is_empty() {
            ticket_labels.is_empty()
        } else {
            self.labels
                .iter()
                .any(|l| ticket_labels.iter().any(|t| t == l))
        }
    }

    pub(super) fn tool_definitions(&self) -> Vec<ProviderToolDefinition> {
        self.tools.definitions()
    }

    pub(super) fn tool_registry(&self) -> &ToolRegistry {
        &self.tools
    }

    pub(super) fn provider(&self) -> Arc<dyn Provider> {
        Arc::clone(&self.provider)
    }

    pub(super) fn knowledge(&self) -> Arc<Knowledge> {
        Arc::clone(&self.knowledge)
    }

    pub(super) fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    pub(super) fn system_prompt(
        &self,
        knowledge: Option<&str>,
        policies: &Policies,
        stats: &Stats,
        ticket_key: &str,
    ) -> String {
        let mut b = PromptBuilder::default();
        if !self.role.is_empty() {
            let role = self.interpolate(&self.role);
            b = b.role(self.expand_context(role, policies, stats, ticket_key));
        }
        if let Some(snap) = knowledge.filter(|s| !s.is_empty()) {
            b = b.knowledge(snap.to_string());
        }
        b.build().system
    }

    /// Substitute the built-in `{context}` block. Runs after
    /// [`Self::interpolate`], so a caller-bound `context` has already
    /// consumed the placeholder and wins. Guarded on `contains` because
    /// building the block spawns `uname`.
    fn expand_context(
        &self,
        role: String,
        policies: &Policies,
        stats: &Stats,
        ticket_key: &str,
    ) -> String {
        if !role.contains("{context}") {
            return role;
        }
        role.replace(
            "{context}",
            &context_body(&self.dir, policies, stats, ticket_key),
        )
    }

    fn interpolate(&self, s: &str) -> String {
        if self.templates.is_empty() {
            return s.to_string();
        }
        let mut out = s.to_string();
        for (key, value) in &self.templates {
            out = out.replace(&format!("{{{key}}}"), value);
        }
        out
    }

    /// Attach a built agent to a ticket queue.
    ///
    /// Any tickets the agent already queued move across, and the agent starts
    /// reading the shared queue.
    pub fn ticket_queue(mut self, queue: &Arc<TicketQueue>) -> Self {
        queue.bind_agent(&mut self);
        self
    }

    /// Submit a task and return its ticket key.
    ///
    /// Call it as often as you like: one agent can drive many tickets.
    pub fn task<T: Serialize>(&self, task: T) -> String {
        self.dispatch(Ticket::new(task))
    }

    /// Submit a `Ticket` with custom labels or schema, and return its key.
    pub fn ticket(&self, ticket: Ticket) -> String {
        self.dispatch(ticket)
    }

    fn dispatch(&self, mut ticket: Ticket) -> String {
        let queue = self
            .ticket_queue
            .upgrade()
            .expect("Agent::task requires a bound TicketQueue");
        if let serde_json::Value::String(s) = &ticket.task {
            ticket.task = serde_json::Value::String(self.interpolate(s));
        }
        queue.insert(ticket, self.name.clone())
    }

    /// Begin processing tickets, and hand back the ticket queue so results and
    /// cancellation stay one call away.
    pub fn start(&self) -> Arc<TicketQueue> {
        let queue = self
            .ticket_queue
            .upgrade()
            .expect("Agent::start requires a bound TicketQueue");
        queue.start();
        queue
    }

    /// Process every queued ticket, then hand back the ticket queue so results
    /// can be read from it.
    pub async fn finish(&self) -> Arc<TicketQueue> {
        let queue = self
            .ticket_queue
            .upgrade()
            .expect("Agent::finish requires a bound TicketQueue");
        let _ = queue.finish().await;
        queue
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::event::EventKind;
    use crate::providers::{Provider, TokenUsage};

    fn built(builder: AgentBuilder<(), ()>) -> Agent {
        use crate::agents::r#loop::test_util::MockProvider;
        builder
            .provider(MockProvider::with_results(vec![]) as Arc<dyn Provider>)
            .model("test")
            .build()
    }

    #[test]
    fn handles_labels_default_scope_only_picks_unlabeled_tickets() {
        let agent = Agent::new();
        assert!(agent.handles_labels(&[]));
        assert!(!agent.handles_labels(&["research".into()]));
    }

    #[test]
    fn handles_labels_with_labels_intersects_ticket_labels() {
        let agent = Agent::new().label("research").label("urgent");
        assert!(agent.handles_labels(&["research".into()]));
        assert!(agent.handles_labels(&["urgent".into(), "other".into()]));
        assert!(!agent.handles_labels(&["report".into()]));
        assert!(!agent.handles_labels(&[]));
    }

    #[test]
    fn handles_labels_matches_when_ticket_label_equals_agent_name() {
        let agent = Agent::new().name("alice");
        assert!(agent.handles_labels(&["alice".into()]));
        assert!(agent.handles_labels(&["alice".into(), "other".into()]));
        let agent = Agent::new().name("alice").label("math");
        assert!(agent.handles_labels(&["alice".into()]));
        assert!(agent.handles_labels(&["math".into()]));
        assert!(!agent.handles_labels(&["report".into()]));
    }

    #[test]
    fn get_name_returns_configured_name() {
        let agent = Agent::new().name("alice");
        assert_eq!(agent.get_name(), "alice");
    }

    #[test]
    fn interactive_defaults_to_false() {
        assert!(!Agent::new().is_interactive());
    }

    #[test]
    fn interactive_builder_sets_the_flag() {
        assert!(Agent::new().interactive().is_interactive());
    }

    #[test]
    fn default_name_is_unique_per_agent() {
        let a = Agent::new();
        let b = Agent::new();
        assert_ne!(a.get_name(), b.get_name());
        assert!(a.get_name().starts_with("agent-"));
        assert!(b.get_name().starts_with("agent-"));
    }

    /// The system prompt with no live state and a fixed ticket key.
    fn system_prompt<P, M>(agent: &AgentBuilder<P, M>, knowledge: Option<&str>) -> String {
        agent.system_prompt(knowledge, &Policies::default(), &Stats::new(), "T-1")
    }

    #[test]
    fn a_role_without_the_placeholder_gets_no_context_block() {
        let agent = Agent::new().role("ROLE");
        assert_eq!(system_prompt(&agent, None), "ROLE");
    }

    #[test]
    fn system_prompt_expands_the_context_placeholder() {
        let agent = Agent::new().role("ROLE\n\n{context}").dir("/tmp/check");
        let prompt = system_prompt(&agent, None);
        assert!(prompt.starts_with("ROLE\n\n"));
        assert!(prompt.contains("- Ticket: T-1"));
        assert!(prompt.contains("- Working directory: /tmp/check"));
        assert!(prompt.contains("- Platform: "));
        assert!(prompt.contains("- Date: "));
    }

    #[test]
    fn the_context_block_lists_the_remaining_budgets() {
        let agent = Agent::new().role("{context}").dir("/tmp/check");
        let policies = Policies {
            max_turns: Some(3),
            max_input_tokens: Some(1_000),
            ..Policies::default()
        };
        let stats = Stats::new();
        stats.record_event(&EventKind::TurnStarted, "", &[], "");
        stats.record_event(
            &EventKind::RequestFinished {
                model: "m".into(),
                usage: TokenUsage {
                    input_tokens: 250,
                    output_tokens: 0,
                },
            },
            "",
            &[],
            "",
        );

        // The exact rendering is pinned in `prompts`; what matters here is
        // that the role's placeholder sees the live policies and stats.
        let rendered = agent.system_prompt(None, &policies, &stats, "T-1");

        assert!(rendered.contains("- Turns remaining: 2"));
        assert!(rendered.contains("- Input tokens remaining: 750"));
    }

    #[test]
    fn a_bound_context_variable_shadows_the_built_in_block() {
        let agent = Agent::new()
            .role("{context}")
            .template("context", "- Note: mine");
        assert_eq!(system_prompt(&agent, None), "- Note: mine");
    }

    #[test]
    fn system_prompt_empty_when_role_unset() {
        let agent = Agent::new();
        assert!(system_prompt(&agent, None).is_empty());
    }

    #[test]
    fn new_agent_has_finish_registered() {
        let agent = Agent::new();
        let names: Vec<String> = agent
            .tool_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(names.iter().any(|n| n == "finish"));
    }

    #[test]
    fn system_prompt_interpolates_role_placeholders() {
        let agent = Agent::new()
            .role("You are {persona}.")
            .template("persona", "a senior reviewer");
        assert_eq!(system_prompt(&agent, None), "You are a senior reviewer.");
    }

    #[test]
    fn unresolved_placeholders_pass_through() {
        let agent = Agent::new().role("Hi {missing}.");
        assert_eq!(system_prompt(&agent, None), "Hi {missing}.");
    }

    #[test]
    fn multiple_variables_substitute_independently() {
        let agent = Agent::new()
            .role("{greeting}, {name}.")
            .templates([("greeting", "Hello"), ("name", "Alice")]);
        assert_eq!(system_prompt(&agent, None), "Hello, Alice.");
    }

    #[test]
    fn no_variables_renders_role_unchanged() {
        let agent = Agent::new().role("You are a senior reviewer.");
        assert_eq!(system_prompt(&agent, None), "You are a senior reviewer.");
    }

    #[tokio::test]
    async fn dispatch_interpolates_string_task_body() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = crate::agents::TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        let agent = built(Agent::new().template("topic", "rust")).ticket_queue(&queue);
        agent.task("Search {topic} forums.");
        let stored = queue
            .tickets()
            .into_iter()
            .next()
            .expect("ticket should have been enqueued");
        assert_eq!(
            stored.task,
            serde_json::Value::String("Search rust forums.".into()),
        );
    }

    #[tokio::test]
    async fn a_task_body_keeps_the_context_placeholder_verbatim() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = crate::agents::TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        // The block needs a ticket key and live budgets, neither of which
        // exists yet at dispatch. Only the role expands it.
        let agent = built(Agent::new()).ticket_queue(&queue);
        agent.task("Work on {context}.");
        let stored = queue
            .tickets()
            .into_iter()
            .next()
            .expect("ticket should have been enqueued");
        assert_eq!(
            stored.task,
            serde_json::Value::String("Work on {context}.".into()),
        );
    }

    #[tokio::test]
    async fn dispatch_leaves_object_task_unchanged() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = crate::agents::TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        let agent = built(Agent::new().template("topic", "rust")).ticket_queue(&queue);
        let value = serde_json::json!({"q": "Find {topic}"});
        agent.ticket(Ticket::new(value.clone()));
        let stored = queue
            .tickets()
            .into_iter()
            .next()
            .expect("ticket should have been enqueued");
        assert_eq!(stored.task, value);
    }

    #[test]
    fn knowledge_registers_manage_knowledge_on_the_agent() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let agent = Agent::new().knowledge(&store);
        let names: Vec<String> = agent
            .tool_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(
            names.iter().any(|n| n == "manage_knowledge"),
            "manage_knowledge should be registered: {names:?}"
        );
    }

    #[test]
    fn knowledge_binds_the_passed_store() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let agent = Agent::new().knowledge(&store);
        agent
            .knowledge
            .pages()
            .save(crate::agents::knowledge::Page {
                slug: "from-store".into(),
                kind: String::new(),
                description: "From store".into(),
                content: "# From Store".into(),
                tags: vec![],
            })
            .unwrap();
        assert!(dir
            .path()
            .join("knowledge")
            .join("pages")
            .join("from-store.md")
            .exists());
    }

    #[test]
    fn cloned_agent_observes_writes_through_original_handle() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let agent = Agent::new().knowledge(&store);
        let cloned = agent.clone();
        agent
            .knowledge
            .pages()
            .save(crate::agents::knowledge::Page {
                slug: "shared".into(),
                kind: String::new(),
                description: "Shared note".into(),
                content: "# Shared".into(),
                tags: vec![],
            })
            .unwrap();
        assert!(cloned.knowledge.index().contains("shared"));
    }

    #[test]
    fn two_agents_bound_to_one_store_see_each_others_writes() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let alice = Agent::new().knowledge(&store);
        let bob = Agent::new().knowledge(&store);
        alice
            .knowledge
            .pages()
            .save(crate::agents::knowledge::Page {
                slug: "from-alice".into(),
                kind: String::new(),
                description: "From Alice".into(),
                content: "# Alice".into(),
                tags: vec![],
            })
            .unwrap();
        assert!(bob.knowledge.index().contains("from-alice"));
    }

    #[test]
    fn system_prompt_renders_knowledge_section_when_body_present() {
        let agent = Agent::new().role("R");
        let prompt = system_prompt(&agent, Some("- **config**: Port 8080"));
        assert!(prompt.contains("R"));
        assert!(prompt.contains("## Knowledge\n\n- **config**: Port 8080"));
    }

    #[test]
    fn system_prompt_omits_knowledge_when_body_empty() {
        let agent = Agent::new().role("R");
        assert_eq!(system_prompt(&agent, Some("")), "R");
    }

    #[test]
    fn new_agent_has_manage_knowledge_registered() {
        let agent = Agent::new();
        let names: Vec<String> = agent
            .tool_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(
            names.iter().any(|n| n == "manage_knowledge"),
            "manage_knowledge must be registered on every new agent: {names:?}",
        );
    }

    #[tokio::test]
    async fn binding_agent_with_explicit_knowledge_keeps_explicit_store() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let queue = crate::agents::TicketQueue::new();
        let agent = built(Agent::new().knowledge(&store)).ticket_queue(&queue);
        assert!(Arc::ptr_eq(&store, &agent.knowledge));
    }
}
