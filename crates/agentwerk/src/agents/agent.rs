//! Agent: identity + prompt parts + provider/model + a bound ticket system.
//! `AgentBuilder::build()` produces the final agent, which
//! `tickets.agent(agent)` binds to the system. agentwerk upgrades the
//! bound reference once at the start of `run_agent` and accesses
//! `tickets`, `policies`, `stats`, and `stop_signal` through the
//! resulting `Arc<TicketSystem>`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use serde::Serialize;

use crate::prompts::{default_context, PromptBuilder, Section};
use crate::providers::{Model, Provider, ProviderToolDefinition};
use crate::tools::{FinishTicketTool, ManageKnowledgeTool, ToolLike, ToolRegistry};

use super::knowledge::Knowledge;
use super::policy::Policies;
use super::stats::Stats;
use super::tickets::{Ticket, TicketSystem};

static AGENT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn default_agent_name() -> String {
    let n = AGENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("agent-{n}")
}

// --- builder ---

/// Builder for [`Agent`]. Configures the LLM provider, model, role,
/// tools, labels, and the working directory.
#[derive(Clone)]
pub struct AgentBuilder<P, M> {
    name: String,
    provider: P,
    model: M,
    role: String,
    context: String,
    labels: Vec<String>,
    interactive: bool,
    template_variables: Vec<(String, String)>,
    tools: ToolRegistry,
    dir: PathBuf,
    knowledge: Arc<Knowledge>,
}

impl AgentBuilder<(), ()> {
    pub fn new() -> Self {
        let knowledge = Knowledge::load(".agentwerk").expect("open knowledge store");
        let mut tools = ToolRegistry::default();
        tools.register(FinishTicketTool);
        tools.register(ManageKnowledgeTool::new(Arc::clone(&knowledge)));
        Self {
            name: default_agent_name(),
            provider: (),
            model: (),
            role: String::new(),
            context: String::new(),
            labels: Vec::new(),
            interactive: false,
            template_variables: Vec::new(),
            tools,
            dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            knowledge,
        }
    }

    /// Construct an `Agent` with no tools pre-registered. Use this
    /// when the agent must not have `FinishTicketTool` available: for
    /// example, a researcher in a chain that should only ever call
    /// `HandoverTicketTool`. The caller is responsible for registering
    /// at least one finish tool (`FinishTicketTool` or
    /// `HandoverTicketTool`) via [`Self::tool`].
    pub fn empty() -> Self {
        let knowledge = Knowledge::load(".agentwerk").expect("open knowledge store");
        let mut tools = ToolRegistry::default();
        tools.register(ManageKnowledgeTool::new(Arc::clone(&knowledge)));
        Self {
            name: default_agent_name(),
            provider: (),
            model: (),
            role: String::new(),
            context: String::new(),
            labels: Vec::new(),
            interactive: false,
            template_variables: Vec::new(),
            tools,
            dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            knowledge,
        }
    }

    /// Detect both the provider and the model from environment variables.
    /// Equivalent to `provider_from_env().model_from_env()`. Panics if no
    /// provider env var is set.
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
            context: self.context,
            labels: self.labels,
            interactive: self.interactive,
            template_variables: self.template_variables,
            tools: self.tools,
            dir: self.dir,
            knowledge: self.knowledge,
        }
    }

    /// Detect the provider from environment variables. Panics if no provider env var is set.
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
            context: self.context,
            labels: self.labels,
            interactive: self.interactive,
            template_variables: self.template_variables,
            tools: self.tools,
            dir: self.dir,
            knowledge: self.knowledge,
        }
    }

    /// Read the model name from environment variables. Panics if no model can be detected.
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

    pub fn role(mut self, r: impl Into<String>) -> Self {
        self.role = r.into();
        self
    }

    pub fn context(mut self, c: impl Into<String>) -> Self {
        self.context = c.into();
        self
    }

    /// Add a single label to the agent's scope. Use [`Self::labels`] to
    /// add several at once.
    pub fn label(mut self, l: impl Into<String>) -> Self {
        self.labels.push(l.into());
        self
    }

    /// Add many labels at once.
    pub fn labels<I, S>(mut self, iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.labels.extend(iter.into_iter().map(Into::into));
        self
    }

    /// Pause on assistant replies that carry no tool calls, for REPL
    /// hosts that drive subsequent turns via `TicketSystem::reply`.
    pub fn interactive(mut self) -> Self {
        self.interactive = true;
        self
    }

    /// Bind `{key}` to `value`. The placeholder is substituted in the
    /// agent's `role`, `context`, and any string-typed `Ticket::task`
    /// enqueued through this agent. Unresolved placeholders are left
    /// verbatim.
    pub fn template_variable(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.template_variables.push((key.into(), value.into()));
        self
    }

    /// Bind many `{key} → value` pairs at once.
    pub fn template_variables<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.template_variables
            .extend(vars.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Register a single tool the agent may call.
    pub fn tool(mut self, tool: impl ToolLike + 'static) -> Self {
        self.tools.register(tool);
        self
    }

    /// Register many tools at once.
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

    /// Directory tools resolve filesystem paths against. Defaults to the
    /// process's current directory at construction time.
    pub fn dir(mut self, p: impl Into<PathBuf>) -> Self {
        self.dir = p.into();
        self
    }

    /// Knowledge store the agent uses for its long-term memory. Replaces
    /// the default store opened at construction and re-registers
    /// `ManageKnowledgeTool` backed by `store`. Share one store across
    /// multiple agents the same way `ticket_system(&shared)` shares a queue.
    pub fn knowledge(mut self, store: &Arc<Knowledge>) -> Self {
        self.tools.deregister("manage_knowledge");
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

    pub(super) fn system_prompt(&self, knowledge: Option<&str>) -> String {
        let mut b = PromptBuilder::default();
        if !self.role.is_empty() {
            b = b.role(self.interpolate(&self.role));
        }
        if let Some(snap) = knowledge.filter(|s| !s.is_empty()) {
            b = b.knowledge(snap.to_string());
        }
        b.build().system
    }

    pub(super) fn context_message(
        &self,
        policies: &Policies,
        stats: &Stats,
        ticket_key: Option<&str>,
    ) -> Option<String> {
        let base = if !self.context.is_empty() {
            Section::context(self.interpolate(&self.context)).render()
        } else {
            default_context(&self.dir, policies, stats)
        };
        let Some(key) = ticket_key else {
            return Some(base);
        };
        const PREFIX: &str = "## Context\n\n";
        let body = base.strip_prefix(PREFIX).unwrap_or(&base);
        Some(format!("{PREFIX}- Ticket: {key}\n{body}"))
    }

    fn interpolate(&self, s: &str) -> String {
        if self.template_variables.is_empty() {
            return s.to_string();
        }
        let mut out = s.to_string();
        for (key, value) in &self.template_variables {
            out = out.replace(&format!("{{{key}}}"), value);
        }
        out
    }
}

impl AgentBuilder<Arc<dyn Provider>, Model> {
    /// Produce the final `Agent`. The agent is born bound to a private
    /// `TicketSystem` so `.task(...).finish().await` works without an
    /// external system; `TicketSystem::agent(...)` later drains the private
    /// queue into a shared system.
    pub fn build(self) -> Agent {
        let mut agent = Agent {
            name: self.name,
            model: self.model,
            labels: self.labels,
            interactive: self.interactive,
            ticket_system: TicketSystemRef::Shared(Weak::new()),
            provider: self.provider,
            role: self.role,
            context: self.context,
            template_variables: self.template_variables,
            tools: self.tools,
            dir: self.dir,
            knowledge: self.knowledge,
        };
        let private = TicketSystem::new();
        private.bind_agent(&mut agent);
        agent.ticket_system = TicketSystemRef::Private(private);
        agent
    }
}

// --- agent ---

/// Handle from an `Agent` to its `TicketSystem`. `Private` is what a
/// freshly-built agent carries until `bind_agent` adopts it; `Shared` is
/// what every other agent (rebound ones, clones, agents inside
/// `TicketSystem::agents`) carries.
pub(crate) enum TicketSystemRef {
    Shared(Weak<TicketSystem>),
    Private(Arc<TicketSystem>),
}

impl TicketSystemRef {
    pub(crate) fn upgrade(&self) -> Option<Arc<TicketSystem>> {
        match self {
            Self::Shared(w) => w.upgrade(),
            Self::Private(a) => Some(Arc::clone(a)),
        }
    }
}

/// Bound to a [`TicketSystem`]. Claims tickets assigned by name or
/// label, calls the LLM provider and runs the tools it requests, then
/// writes the result back through `FinishTicketTool` or
/// `HandoverTicketTool`.
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
    // pub(crate): read by loop, TicketSystem, or routing code
    pub(crate) name: String,
    pub(crate) model: Model,
    pub(crate) labels: Vec<String>,
    pub(crate) interactive: bool,
    pub(crate) ticket_system: TicketSystemRef,
    // private: accessed through methods within agents::
    provider: Arc<dyn Provider>,
    role: String,
    context: String,
    template_variables: Vec<(String, String)>,
    tools: ToolRegistry,
    dir: PathBuf,
    knowledge: Arc<Knowledge>,
}

impl Clone for Agent {
    /// Convert `Private` to `Shared(Weak)` on clone so rebinding the
    /// original cannot leave a stale clone enqueuing into an orphaned queue.
    fn clone(&self) -> Self {
        let ticket_system = match &self.ticket_system {
            TicketSystemRef::Shared(w) => TicketSystemRef::Shared(w.clone()),
            TicketSystemRef::Private(a) => TicketSystemRef::Shared(Arc::downgrade(a)),
        };
        Self {
            name: self.name.clone(),
            model: self.model.clone(),
            labels: self.labels.clone(),
            interactive: self.interactive,
            ticket_system,
            provider: Arc::clone(&self.provider),
            role: self.role.clone(),
            context: self.context.clone(),
            template_variables: self.template_variables.clone(),
            tools: self.tools.clone(),
            dir: self.dir.clone(),
            knowledge: Arc::clone(&self.knowledge),
        }
    }
}

impl Agent {
    /// Entry point for the fluent builder.
    pub fn new() -> AgentBuilder<(), ()> {
        AgentBuilder::new()
    }

    /// Entry point for a builder with no tools pre-registered.
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

    pub(super) fn system_prompt(&self, knowledge: Option<&str>) -> String {
        let mut b = PromptBuilder::default();
        if !self.role.is_empty() {
            b = b.role(self.interpolate(&self.role));
        }
        if let Some(snap) = knowledge.filter(|s| !s.is_empty()) {
            b = b.knowledge(snap.to_string());
        }
        b.build().system
    }

    pub(super) fn context_message(
        &self,
        policies: &Policies,
        stats: &Stats,
        ticket_key: Option<&str>,
    ) -> Option<String> {
        let base = if !self.context.is_empty() {
            Section::context(self.interpolate(&self.context)).render()
        } else {
            default_context(&self.dir, policies, stats)
        };
        let Some(key) = ticket_key else {
            return Some(base);
        };
        const PREFIX: &str = "## Context\n\n";
        let body = base.strip_prefix(PREFIX).unwrap_or(&base);
        Some(format!("{PREFIX}- Ticket: {key}\n{body}"))
    }

    fn interpolate(&self, s: &str) -> String {
        if self.template_variables.is_empty() {
            return s.to_string();
        }
        let mut out = s.to_string();
        for (key, value) in &self.template_variables {
            out = out.replace(&format!("{{{key}}}"), value);
        }
        out
    }

    /// Bind this agent to a shared `TicketSystem`. Drains any tickets
    /// the agent had already enqueued in its prior store into `sys`,
    /// binds `sys` to the agent so it reads the shared queue, and
    /// registers a clone of `self` into `sys`'s agents list so the
    /// loop will dispatch this agent at `run` / `finish` time.
    pub fn ticket_system(mut self, sys: &Arc<TicketSystem>) -> Self {
        sys.bind_agent(&mut self);
        self
    }

    /// Enqueue a ticket carrying `task` as its body. Returns `&self` so
    /// the call can chain into `.finish().await`. Callers who need the
    /// ticket's key go through [`TicketSystem::task`] instead.
    pub fn task<T: Serialize>(&self, task: T) -> &Self {
        let ticket = Ticket::new(task);
        self.dispatch(ticket);
        self
    }

    /// Enqueue a fully-built `Ticket`. Returns `&self` so the call can chain
    /// into `.finish().await`.
    pub fn ticket(&self, ticket: Ticket) -> &Self {
        self.dispatch(ticket);
        self
    }

    fn dispatch(&self, mut ticket: Ticket) {
        let sys = self
            .ticket_system
            .upgrade()
            .expect("Agent::task requires a bound TicketSystem");
        if let serde_json::Value::String(s) = &ticket.task {
            ticket.task = serde_json::Value::String(self.interpolate(s));
        }
        sys.insert(ticket, self.name.clone());
    }

    /// Start the agent loop on a background tokio task. Returns the bound
    /// system so callers can `cancel()` or read results on the same value.
    pub fn start(&self) -> Arc<TicketSystem> {
        let sys = self
            .ticket_system
            .upgrade()
            .expect("Agent::start requires a bound TicketSystem");
        sys.start();
        sys
    }

    /// Start a background run and wait for every queued ticket to finish.
    /// Returns the bound system so the caller can read results via
    /// [`TicketSystem::last_result`] etc.
    pub async fn finish(&self) -> Arc<TicketSystem> {
        let sys = self
            .ticket_system
            .upgrade()
            .expect("Agent::finish requires a bound TicketSystem");
        let _ = sys.finish().await;
        sys
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::agents::stats::LoopStats;
    use crate::providers::Provider;

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

    #[test]
    fn context_message_falls_back_to_default_when_unset() {
        let agent = Agent::new().role("R");
        let policies = Policies::default();
        let stats = Stats::new();
        let rendered = agent
            .context_message(&policies, &stats, None)
            .expect("default context");
        assert!(rendered.starts_with("## Context\n\n"));
        assert!(rendered.contains("- Working directory: "));
        assert!(rendered.contains("- Platform: "));
        assert!(rendered.contains("- Date: "));
    }

    #[test]
    fn context_message_renders_h2_heading_when_set() {
        let agent = Agent::new().context("- Note: /tmp");
        let policies = Policies::default();
        let stats = Stats::new();
        assert_eq!(
            agent.context_message(&policies, &stats, None).as_deref(),
            Some("## Context\n\n- Note: /tmp"),
        );
    }

    #[test]
    fn context_message_appends_runtime_lines_when_policy_budgets_are_set() {
        let agent = Agent::new().dir("/tmp/check");
        let policies = Policies {
            max_turns: Some(3),
            max_input_tokens: Some(1_000),
            ..Policies::default()
        };
        let stats = Stats::new();
        stats.record_turn();
        stats.record_request(250, 0);

        let rendered = agent
            .context_message(&policies, &stats, None)
            .expect("default context");

        let expected = format!(
            "{static_prefix}\n\
             - Turns remaining: 2\n\
             - Input tokens remaining: 750",
            static_prefix = default_context(
                &PathBuf::from("/tmp/check"),
                &Policies::default(),
                &Stats::new()
            ),
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn context_message_ignores_runtime_args_for_custom_context() {
        let agent = Agent::new().context("- Note: custom");
        let policies = Policies {
            max_turns: Some(3),
            ..Policies::default()
        };
        let stats = Stats::new();
        stats.record_turn();
        assert_eq!(
            agent.context_message(&policies, &stats, None).as_deref(),
            Some("## Context\n\n- Note: custom"),
        );
    }

    #[test]
    fn system_prompt_does_not_include_context() {
        let agent = Agent::new().role("ROLE").context("CTX");
        let prompt = agent.system_prompt(None);
        assert!(prompt.contains("ROLE"));
        assert!(!prompt.contains("CTX"));
        assert!(!prompt.contains("## Context"));
    }

    #[test]
    fn system_prompt_is_role_only() {
        let agent = Agent::new().role("ROLE");
        let prompt = agent.system_prompt(None);
        assert_eq!(prompt, "ROLE");
    }

    #[test]
    fn system_prompt_empty_when_role_unset() {
        let agent = Agent::new();
        assert!(agent.system_prompt(None).is_empty());
    }

    #[test]
    fn new_agent_has_finish_ticket_registered() {
        let agent = Agent::new();
        let names: Vec<String> = agent
            .tool_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(names.iter().any(|n| n == "finish_ticket"));
    }

    #[test]
    fn system_prompt_interpolates_role_placeholders() {
        let agent = Agent::new()
            .role("You are {persona}.")
            .template_variable("persona", "a senior reviewer");
        assert_eq!(agent.system_prompt(None), "You are a senior reviewer.");
    }

    #[test]
    fn context_message_interpolates_context_placeholders() {
        let agent = Agent::new()
            .context("- Topic: {topic}")
            .template_variable("topic", "Rust generics");
        let policies = Policies::default();
        let stats = Stats::new();
        assert_eq!(
            agent.context_message(&policies, &stats, None).as_deref(),
            Some("## Context\n\n- Topic: Rust generics"),
        );
    }

    #[test]
    fn unresolved_placeholders_pass_through() {
        let agent = Agent::new()
            .role("Hi {missing}.")
            .context("- Note: {also_missing}");
        let policies = Policies::default();
        let stats = Stats::new();
        assert_eq!(agent.system_prompt(None), "Hi {missing}.");
        assert_eq!(
            agent.context_message(&policies, &stats, None).as_deref(),
            Some("## Context\n\n- Note: {also_missing}"),
        );
    }

    #[test]
    fn multiple_variables_substitute_independently() {
        let agent = Agent::new()
            .role("{greeting}, {name}.")
            .template_variables([("greeting", "Hello"), ("name", "Alice")]);
        assert_eq!(agent.system_prompt(None), "Hello, Alice.");
    }

    #[test]
    fn no_variables_renders_role_unchanged() {
        let agent = Agent::new().role("You are a senior reviewer.");
        assert_eq!(agent.system_prompt(None), "You are a senior reviewer.");
    }

    #[tokio::test]
    async fn dispatch_interpolates_string_task_body() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let sys = crate::agents::TicketSystem::new();
        sys.dir(dir.path().to_path_buf());
        let agent = built(Agent::new().template_variable("topic", "rust")).ticket_system(&sys);
        agent.task("Search {topic} forums.");
        let stored = sys
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
    async fn dispatch_leaves_object_task_unchanged() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let sys = crate::agents::TicketSystem::new();
        sys.dir(dir.path().to_path_buf());
        let agent = built(Agent::new().template_variable("topic", "rust")).ticket_system(&sys);
        let value = serde_json::json!({"q": "Find {topic}"});
        agent.ticket(Ticket::new(value.clone()));
        let stored = sys
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
        assert!(dir.path().join("pages").join("from-store.md").exists());
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
        let prompt = agent.system_prompt(Some("- **config** — Port 8080"));
        assert!(prompt.contains("R"));
        assert!(prompt.contains("## Knowledge\n\n- **config** — Port 8080"));
    }

    #[test]
    fn system_prompt_omits_knowledge_when_body_empty() {
        let agent = Agent::new().role("R");
        assert_eq!(agent.system_prompt(Some("")), "R");
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
        let sys = crate::agents::TicketSystem::new();
        let agent = built(Agent::new().knowledge(&store)).ticket_system(&sys);
        assert!(Arc::ptr_eq(&store, &agent.knowledge));
    }
}
