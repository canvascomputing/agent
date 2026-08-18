//! The core entity of agentwerk: who an agent is, what it may call, and which
//! queue it works from.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use crate::prompts::{context_values, render_context, PromptBuilder, Text};
use crate::providers::{Model, Provider};
use crate::tools::{FinishTool, KnowledgeTool, Tool, ToolRegistry};

use super::knowledge::Knowledge;
use super::policy::Policies;
use super::stats::Stats;
use super::tickets::{Ticket, TicketQueue};
use crate::prompts::directives::DirectiveStore;

/// One counter per label, behind the ids [`AgentBuilder::build`] hands out.
/// Numbering restarts at 1 for each label, so a host that builds the same
/// agents in the same order gets the same ids after a restart, which is what
/// [`TicketQueue::load`] needs to resume an unfinished ticket.
static AGENT_IDS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

fn next_id(label: Option<&str>) -> String {
    let prefix = label.unwrap_or("agent");
    let mut counters = AGENT_IDS.lock().unwrap();
    let count = counters.entry(prefix.to_string()).or_insert(0);
    *count += 1;
    format!("{prefix}-{count}")
}

// Builder

/// An `AgentBuilder` collects who the agent is, what it may call, and where it
/// works, then hands back the finished [`Agent`].
#[derive(Clone)]
pub struct AgentBuilder<P, M> {
    provider: P,
    model: M,
    role: String,
    label: Option<String>,
    interactive: bool,
    templates: Vec<(String, String)>,
    tools: ToolRegistry,
    dir: PathBuf,
    knowledge: Arc<Knowledge>,
    directives: Arc<DirectiveStore>,
}

impl AgentBuilder<(), ()> {
    pub fn new() -> Self {
        let knowledge = Knowledge::load(".agentwerk").expect("open knowledge store");
        let mut tools = ToolRegistry::default();
        tools.register(KnowledgeTool::new(Arc::clone(&knowledge)));
        Self {
            provider: (),
            model: (),
            role: String::new(),
            label: None,
            interactive: false,
            templates: Vec::new(),
            tools,
            dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            knowledge,
            directives: Arc::new(DirectiveStore::default()),
        }
    }
}

impl<M> AgentBuilder<(), M> {
    /// Define the LLM provider. Takes a vendor provider directly, or a
    /// [`Provider`] shared with other agents.
    pub fn provider(self, provider: impl Into<Provider>) -> AgentBuilder<Provider, M> {
        AgentBuilder {
            provider: provider.into(),
            model: self.model,
            role: self.role,
            label: self.label,
            interactive: self.interactive,
            templates: self.templates,
            tools: self.tools,
            dir: self.dir,
            knowledge: self.knowledge,
            directives: self.directives,
        }
    }
}

impl<P> AgentBuilder<P, ()> {
    pub fn model(self, model: impl Into<Model>) -> AgentBuilder<P, Model> {
        AgentBuilder {
            provider: self.provider,
            model: model.into(),
            role: self.role,
            label: self.label,
            interactive: self.interactive,
            templates: self.templates,
            tools: self.tools,
            dir: self.dir,
            knowledge: self.knowledge,
            directives: self.directives,
        }
    }
}

impl<P, M> AgentBuilder<P, M> {
    /// Define who the agent is and how it should work.
    ///
    /// A `{context}` placeholder anywhere in the text expands to the facts of
    /// the moment as a bullet list: ticket key, date, working directory,
    /// platform, and one line per configured limit. Each of those values is
    /// also a placeholder of its own, so a role can place one without the list:
    /// `{ticket}`, `{date}`, `{dir}`, `{platform}`, `{os_version}`,
    /// `{turns_remaining}`, `{input_tokens_remaining}`,
    /// `{output_tokens_remaining}`, and `{time_remaining}`. A limit left
    /// unconfigured expands to nothing and shows no bullet. Leave the
    /// placeholders out and nothing is added, so the role decides both whether
    /// those facts appear and where.
    ///
    /// A string is the role itself; a `&Path` or `PathBuf` names the file
    /// holding it, which panics when that file cannot be read.
    pub fn role(mut self, role: impl Into<Text>) -> Self {
        self.role = role.into().into_string();
        self
    }

    /// Restrict the agent to tickets carrying this label, and name it after
    /// the label.
    ///
    /// An agent serves one label and a ticket carries one, so an agent claims
    /// a ticket when the two are equal, and every agent serving that label may
    /// claim it. Calling this twice replaces the label. The id
    /// [`Agent::id`] hands back is built from it.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Let the agent wait for new instructions to keep a ticket in-progress.
    ///
    /// The agent stops after a reply that calls no tool, and
    /// `TicketQueue::reply` drives the next turn. It gets no `FinishTool`,
    /// since ending the ticket would end the conversation; the host closes it
    /// with `TicketQueue::set_finished`. Register the tool by hand to give the
    /// agent one back.
    pub fn interactive(mut self) -> Self {
        self.interactive = true;
        self
    }

    /// Inject data into prompts with template strings.
    ///
    /// `{key}` is replaced in the agent's role and in any text task submitted
    /// through this agent. A placeholder with no value is left as it is.
    /// Binding `context`, or any of the single names described on
    /// [`Self::role`], replaces that built-in value.
    pub fn template(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.templates.push((key.into(), value.into()));
        self
    }

    /// Inject more than one entry into prompts.
    pub fn templates<I, K, V>(mut self, variables: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.templates
            .extend(variables.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Register a tool the agent may call.
    pub fn tool(mut self, tool: impl Into<Tool>) -> Self {
        self.tools.register(tool);
        self
    }

    /// Register several tools the agent may call.
    pub fn tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Tool>,
    {
        for t in tools {
            self.tools.register(t);
        }
        self
    }

    /// Set the directory the agent has access to, the current one by default.
    pub fn dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = dir.into();
        self
    }

    /// Share a knowledge store, the durable memory the agent carries across
    /// tickets and shares with other agents.
    ///
    /// It replaces the store opened by default, both for what the prompt shows
    /// and for what `KnowledgeTool` writes to. Hand the same store to
    /// several agents to share it between them.
    pub fn knowledge(mut self, store: &Arc<Knowledge>) -> Self {
        self.tools.register(KnowledgeTool::new(Arc::clone(store)));
        self.knowledge = Arc::clone(store);
        self
    }

    /// Decide what the agent tells the model when a call fails.
    ///
    /// `compute` sees the key of every directive before it renders. Match it
    /// against the constants [`crate::Directive`] carries, and answer `None` for the
    /// ones you leave as they are. What it returns is a template, bound
    /// afterwards, so a `{name}` it carries still resolves.
    ///
    /// ```no_run
    /// # use agentwerk::{Agent, Directive};
    /// Agent::from_env().directives(|key| match key {
    ///     Directive::GREP_FAILED => Some("The search did not run. Narrow `path`."),
    ///     _ => None,
    /// });
    /// ```
    pub fn directives<T: Into<String>>(
        mut self,
        compute: impl Fn(&str) -> Option<T> + Send + Sync + 'static,
    ) -> Self {
        self.directives = Arc::new(DirectiveStore::new(compute));
        self
    }
}

// Inline-test inspectors. Production callers go through `Agent`, which
// carries its own copies of these methods; the builder-side ones exist
// so inline tests can exercise prompt assembly and tool registration
// without first calling `.build()`.
#[cfg(test)]
impl<P, M> AgentBuilder<P, M> {
    pub(super) fn is_interactive(&self) -> bool {
        self.interactive
    }

    pub(super) fn handles(&self, ticket_label: Option<&str>) -> bool {
        self.label.as_deref() == ticket_label
    }

    pub(super) fn tool_registry(&self) -> &ToolRegistry {
        &self.tools
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

    /// Substitute the built-in `{context}` block and the single value behind
    /// each of its bullets. Runs after [`Self::interpolate`], so a caller-bound
    /// name has already consumed its placeholder and wins. Guarded on a brace
    /// because gathering the values spawns `uname`.
    fn expand_context(
        &self,
        role: String,
        policies: &Policies,
        stats: &Stats,
        ticket_key: &str,
    ) -> String {
        if !role.contains('{') {
            return role;
        }
        let values = context_values(&self.dir, policies, stats, ticket_key);
        let mut out = role.replace("{context}", &render_context(&values));
        for (name, value) in &values {
            out = out.replace(&format!("{{{name}}}"), value);
        }
        out
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

impl AgentBuilder<Provider, Model> {
    /// Create the agent, giving it the id it keeps for the rest of the run.
    ///
    /// It starts with a ticket queue of its own, so `.ticket(...).finish_all().await`
    /// works without one being set up. `TicketQueue::agent(...)` later moves
    /// those tickets into the shared queue.
    pub fn build(mut self) -> Agent {
        // Here rather than in `new`, because only now is `interactive` known.
        if !self.interactive {
            self.tools.register(FinishTool);
        }
        let mut agent = Agent {
            id: next_id(self.label.as_deref()),
            model: self.model,
            label: self.label,
            interactive: self.interactive,
            ticket_queue: TicketQueueRef::Shared(Weak::new()),
            provider: self.provider,
            role: self.role,
            templates: self.templates,
            tools: self.tools,
            dir: self.dir,
            knowledge: self.knowledge,
            directives: self.directives,
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
/// It claims the tickets its label matches, calls the LLM provider, runs
/// the tools the model asks for, and writes the result back.
///
/// ```no_run
/// use agentwerk::Agent;
/// use agentwerk::tools::ReadFileTool;
///
/// # async fn run() {
/// let agent = Agent::from_env()
///     .label("reader")
///     .role("Rust developer reading source files to answer questions.")
///     .tool(ReadFileTool)
///     .build();
/// # let _ = agent;
/// # }
/// ```
pub struct Agent {
    // pub(crate): read by loop, TicketQueue, or assignment code
    pub(crate) id: String,
    pub(crate) model: Model,
    pub(crate) label: Option<String>,
    pub(crate) interactive: bool,
    pub(crate) ticket_queue: TicketQueueRef,
    // private: accessed through methods within agents::
    provider: Provider,
    role: String,
    templates: Vec<(String, String)>,
    tools: ToolRegistry,
    dir: PathBuf,
    knowledge: Arc<Knowledge>,
    directives: Arc<DirectiveStore>,
}

impl Clone for Agent {
    /// A clone is the same agent, id included: `bind_agent` keeps one, and the
    /// two would otherwise disagree about which tickets are theirs. It points
    /// at the shared queue, so rebinding the original cannot leave the clone
    /// filing tickets into a queue nothing reads.
    fn clone(&self) -> Self {
        let ticket_queue = match &self.ticket_queue {
            TicketQueueRef::Shared(w) => TicketQueueRef::Shared(w.clone()),
            TicketQueueRef::Private(a) => TicketQueueRef::Shared(Arc::downgrade(a)),
        };
        Self {
            id: self.id.clone(),
            model: self.model.clone(),
            label: self.label.clone(),
            interactive: self.interactive,
            directives: Arc::clone(&self.directives),
            ticket_queue,
            provider: self.provider.clone(),
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

    /// Start building an agent with the provider and model from the environment.
    /// Panics when no LLM provider variable is set.
    pub fn from_env() -> AgentBuilder<Provider, Model> {
        let provider = Provider::from_env().expect(
            "LLM provider required: set ANTHROPIC_API_KEY, OPENAI_API_KEY, MISTRAL_API_KEY, or LITELLM_API_KEY",
        );
        AgentBuilder::new()
            .provider(provider)
            .model(Model::from_env().expect("model name required"))
    }

    /// The unique identifier this agent works under, `<label>-<n>` for a
    /// labeled agent and `agent-<n>` for one without. It names the agent in
    /// [`Event::agent_id`] and in [`Ticket::assignee`].
    ///
    /// [`Event::agent_id`]: crate::Event::agent_id
    /// [`Ticket::assignee`]: crate::Ticket::assignee
    pub fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn is_interactive(&self) -> bool {
        self.interactive
    }

    pub(super) fn handles(&self, ticket_label: Option<&str>) -> bool {
        self.label.as_deref() == ticket_label
    }

    pub(super) fn tool_registry(&self) -> &ToolRegistry {
        &self.tools
    }

    pub(super) fn provider(&self) -> Provider {
        self.provider.clone()
    }

    pub(super) fn knowledge(&self) -> Arc<Knowledge> {
        Arc::clone(&self.knowledge)
    }

    pub(super) fn directives(&self) -> Arc<DirectiveStore> {
        Arc::clone(&self.directives)
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

    /// Substitute the built-in `{context}` block and the single value behind
    /// each of its bullets. Runs after [`Self::interpolate`], so a caller-bound
    /// name has already consumed its placeholder and wins. Guarded on a brace
    /// because gathering the values spawns `uname`.
    fn expand_context(
        &self,
        role: String,
        policies: &Policies,
        stats: &Stats,
        ticket_key: &str,
    ) -> String {
        if !role.contains('{') {
            return role;
        }
        let values = context_values(&self.dir, policies, stats, ticket_key);
        let mut out = role.replace("{context}", &render_context(&values));
        for (name, value) in &values {
            out = out.replace(&format!("{{{name}}}"), value);
        }
        out
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

    /// Submit a task and return its ticket key.
    ///
    /// A string is the task itself, and a `&Path` or `PathBuf` names the file
    /// holding it. A [`Ticket`] carries a custom label or schema with it. Call
    /// it as often as you like: one agent can drive many tickets.
    pub fn ticket(&self, ticket: impl Into<Ticket>) -> String {
        self.dispatch(ticket.into())
    }

    fn dispatch(&self, mut ticket: Ticket) -> String {
        let queue = self
            .ticket_queue
            .upgrade()
            .expect("Agent::task requires a bound TicketQueue");
        if let serde_json::Value::String(s) = &ticket.task {
            ticket.task = serde_json::Value::String(self.interpolate(s));
        }
        queue.insert(ticket, self.id.clone())
    }

    /// Begin processing tickets, and hand back the ticket queue so results,
    /// waiting, and cancellation stay one call away.
    ///
    /// ```no_run
    /// # use agentwerk::Agent;
    /// # async fn run(agent: Agent) {
    /// let work = agent.start();
    /// work.finish_all().await;
    /// # }
    /// ```
    pub fn start(&self) -> Arc<TicketQueue> {
        let queue = self
            .ticket_queue
            .upgrade()
            .expect("Agent::start requires a bound TicketQueue");
        queue.start();
        queue
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn tools_accepts_a_mixed_list_of_tools() {
        use crate::tools::{CommandTool, ReadFileTool, Tool, ToolResult};

        let agent = Agent::new().tools(vec![
            Tool::from(ReadFileTool),
            CommandTool::new("git").allow("git *").into(),
            Tool::new("greet")
                .description("Say hello.")
                .handler(|_: serde_json::Value, _| async { ToolResult::success("hi") })
                .build(),
        ]);
        let names: Vec<String> = agent
            .tool_registry()
            .tools()
            .into_iter()
            .map(|tool| tool.name().to_string())
            .collect();
        for name in ["read_file", "git", "greet"] {
            assert!(names.contains(&name.to_string()), "{names:?}");
        }
    }

    use std::sync::Arc;

    use super::*;
    use crate::event::EventKind;
    use crate::providers::TokenUsage;

    fn built(builder: AgentBuilder<(), ()>) -> Agent {
        use crate::agents::r#loop::test_util::MockProvider;
        builder
            .provider(MockProvider::with_results(vec![]))
            .model("test")
            .build()
    }

    #[test]
    fn handles_default_scope_only_picks_unlabeled_tickets() {
        let agent = Agent::new();
        assert!(agent.handles(None));
        assert!(!agent.handles(Some("research")));
    }

    #[test]
    fn handles_only_the_ticket_carrying_its_own_label() {
        let agent = Agent::new().label("research");
        assert!(agent.handles(Some("research")));
        assert!(!agent.handles(Some("report")));
        assert!(!agent.handles(None));
    }

    #[test]
    fn label_replaces_the_previous_one() {
        let agent = Agent::new().label("research").label("math");
        assert!(agent.handles(Some("math")));
        assert!(!agent.handles(Some("research")));
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
    fn ids_are_numbered_per_label() {
        let first = built(Agent::new().label("ids_per_label"));
        let second = built(Agent::new().label("ids_per_label"));
        assert_eq!(first.id(), "ids_per_label-1");
        assert_eq!(second.id(), "ids_per_label-2");
    }

    #[test]
    fn an_unlabeled_agent_is_numbered_under_agent() {
        let agent = built(Agent::new());
        assert!(
            agent.id().starts_with("agent-"),
            "unexpected id: {}",
            agent.id()
        );
    }

    #[test]
    fn a_clone_keeps_the_id_of_the_agent_it_came_from() {
        let agent = built(Agent::new().label("cloned_id"));
        assert_eq!(agent.clone().id(), agent.id());
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
    fn a_role_read_from_a_file_becomes_the_system_prompt() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let file = dir.path().join("reviewer.md");
        std::fs::write(&file, "ROLE\n").unwrap();
        let agent = Agent::new().role(file.as_path());
        assert_eq!(system_prompt(&agent, None), "ROLE");
    }

    #[test]
    fn a_role_read_from_a_file_keeps_its_placeholders_expandable() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let file = dir.path().join("reviewer.md");
        std::fs::write(&file, "ROLE\n\n{context}\n").unwrap();
        let agent = Agent::new().role(file).dir("/tmp/check");
        assert!(system_prompt(&agent, None).contains("- Working directory: /tmp/check"));
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
        let stats = Stats::of([
            EventKind::TurnStarted,
            EventKind::RequestFinished {
                model: "m".into(),
                usage: TokenUsage {
                    input_tokens: 250,
                    output_tokens: 0,
                },
            },
        ]);

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
    fn a_single_context_value_expands_without_the_block() {
        let agent = Agent::new()
            .role("Ticket {ticket} in {dir}, {turns_remaining} turns left.")
            .dir("/tmp/check");
        let policies = Policies {
            max_turns: Some(3),
            ..Policies::default()
        };
        let stats = Stats::of([EventKind::TurnStarted]);

        let rendered = agent.system_prompt(None, &policies, &stats, "T-1");

        assert_eq!(rendered, "Ticket T-1 in /tmp/check, 2 turns left.");
    }

    #[test]
    fn a_single_budget_value_expands_to_nothing_when_unconfigured() {
        let agent = Agent::new().role("Turns left: {turns_remaining}.");
        assert_eq!(system_prompt(&agent, None), "Turns left: .");
    }

    #[test]
    fn a_bound_single_value_shadows_the_built_in_one() {
        let agent = Agent::new().role("{ticket}").template("ticket", "mine");
        assert_eq!(system_prompt(&agent, None), "mine");
    }

    #[test]
    fn system_prompt_empty_when_role_unset() {
        let agent = Agent::new();
        assert!(system_prompt(&agent, None).is_empty());
    }

    fn tool_names(agent: &Agent) -> Vec<String> {
        agent
            .tool_registry()
            .tools()
            .into_iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    #[test]
    fn a_built_agent_has_finish_registered() {
        let names = tool_names(&built(Agent::new()));
        assert!(names.iter().any(|n| n == "finish"), "{names:?}");
    }

    #[test]
    fn an_interactive_agent_has_no_finish_tool() {
        let names = tool_names(&built(Agent::new().interactive()));
        assert!(
            !names.iter().any(|n| n == "finish"),
            "an interactive agent ends its ticket through the host: {names:?}",
        );
    }

    #[test]
    fn an_interactive_agent_keeps_a_finish_tool_it_registered_itself() {
        let names = tool_names(&built(Agent::new().interactive().tool(FinishTool)));
        assert!(names.iter().any(|n| n == "finish"), "{names:?}");
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
        let mut agent = built(Agent::new().template("topic", "rust"));
        queue.bind_agent(&mut agent);
        agent.ticket("Search {topic} forums.");
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
        let mut agent = built(Agent::new());
        queue.bind_agent(&mut agent);
        agent.ticket("Work on {context}.");
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
        let mut agent = built(Agent::new().template("topic", "rust"));
        queue.bind_agent(&mut agent);
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
    fn knowledge_registers_the_knowledge_tool_on_the_agent() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let agent = Agent::new().knowledge(&store);
        let registry = agent.tool_registry();
        let names: Vec<String> = registry
            .tools()
            .into_iter()
            .map(|tool| tool.name().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "knowledge"),
            "knowledge should be registered: {names:?}"
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
    fn new_agent_has_the_knowledge_tool_registered() {
        let agent = Agent::new();
        let registry = agent.tool_registry();
        let names: Vec<String> = registry
            .tools()
            .into_iter()
            .map(|tool| tool.name().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "knowledge"),
            "knowledge must be registered on every new agent: {names:?}",
        );
    }

    #[tokio::test]
    async fn binding_agent_with_explicit_knowledge_keeps_explicit_store() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let queue = crate::agents::TicketQueue::new();
        let mut agent = built(Agent::new().knowledge(&store));
        queue.bind_agent(&mut agent);
        assert!(Arc::ptr_eq(&store, &agent.knowledge));
    }
}
