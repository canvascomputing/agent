//! Defines agents that claim tasks and use LLMs and tools to complete them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::providers::{Model, Provider};
use crate::tools::{EventTool, FinishTool, KnowledgeTool, Tool};

use super::knowledge::Knowledge;
use super::query::Matcher;
use super::tasks::{Task, Werk};
use crate::prompts::directives::DirectiveStore;

/// One counter per label, behind the ids [`Agent::get_id`] hands out.
/// Numbering restarts at 1 for each label, so a host that creates the same
/// agents in the same order gets the same ids after a restart, which is what
/// [`Werk::load`] needs to resume an unfinished task.
static AGENT_IDS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

fn next_id(label: Option<&str>) -> String {
    let prefix = label.unwrap_or("agent");
    let mut counters = AGENT_IDS.lock().unwrap();
    let count = counters.entry(prefix.to_string()).or_insert(0);
    *count += 1;
    format!("{prefix}-{count}")
}

/// How every clone of an `Agent` reaches the same `Werk`.
pub(crate) struct WerkRef(Arc<Mutex<WerkTarget>>);

enum WerkTarget {
    Shared(Weak<Werk>),
    Private(Arc<Werk>),
}

impl WerkRef {
    fn private(werk: Arc<Werk>) -> Self {
        Self(Arc::new(Mutex::new(WerkTarget::Private(werk))))
    }

    pub(crate) fn upgrade(&self) -> Option<Arc<Werk>> {
        match &*self.0.lock().unwrap() {
            WerkTarget::Shared(werk) => werk.upgrade(),
            WerkTarget::Private(werk) => Some(Arc::clone(werk)),
        }
    }

    pub(crate) fn bind(&self, werk: Weak<Werk>) {
        *self.0.lock().unwrap() = WerkTarget::Shared(werk);
    }
}

impl Clone for WerkRef {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

/// Use an LLM and registered tools to complete tasks claimed from a `Werk`.
///
/// ```no_run
/// use agentwerk::Agent;
/// use agentwerk::tools::ReadFileTool;
///
/// # async fn run() {
/// let agent = Agent::from_env()
///     .label("reader")
///     .role("Rust developer reading source files to answer questions.")
///     .tool(ReadFileTool);
/// # let _ = agent;
/// # }
/// ```
pub struct Agent {
    pub(crate) label: Option<String>,
    pub(crate) interactive: bool,
    pub(crate) werk: WerkRef,
    /// Taken the first time anything needs it, since the label it is built from
    /// is set after construction.
    id: OnceLock<String>,
    provider: Option<Provider>,
    model: Option<Model>,
    role: String,
    tools: Vec<Tool>,
    dir: PathBuf,
    knowledge: Arc<Knowledge>,
    directives: Arc<DirectiveStore>,
}

impl Clone for Agent {
    /// A clone is the same agent, id and Werk binding included. Reading the id
    /// here fixes it for both copies, and the shared binding lets either copy
    /// add work after one is added to a Werk.
    fn clone(&self) -> Self {
        Self {
            id: OnceLock::from(self.get_id().to_string()),
            label: self.label.clone(),
            interactive: self.interactive,
            directives: Arc::clone(&self.directives),
            werk: self.werk.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            role: self.role.clone(),
            tools: self.tools.clone(),
            dir: self.dir.clone(),
            knowledge: Arc::clone(&self.knowledge),
        }
    }
}

impl Default for Agent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent {
    /// Create an agent, with a Werk of its own so
    /// `.add_task(...)`, `.start()`, and `.finish()` work without one being set up.
    /// `Werk::add_agent(...)` later moves those tasks into the shared Werk.
    ///
    /// Give it a provider and a model before it starts work.
    pub fn new() -> Self {
        let knowledge = Knowledge::load(".agentwerk/knowledge").expect("open knowledge store");
        let mut agent = Self {
            id: OnceLock::new(),
            provider: None,
            model: None,
            role: String::new(),
            label: None,
            interactive: false,
            werk: WerkRef::private(Werk::new()),
            tools: Vec::new(),
            dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            knowledge,
            directives: Arc::new(DirectiveStore::default()),
        };
        agent.register_tool(KnowledgeTool::new(Arc::clone(&agent.knowledge)));
        agent
    }

    /// Create an agent with the provider and model from the environment.
    /// Panics when no LLM provider variable is set.
    pub fn from_env() -> Self {
        let provider = Provider::from_env().expect(
            "LLM provider required: set ANTHROPIC_API_KEY, OPENAI_API_KEY, MISTRAL_API_KEY, or LITELLM_API_KEY",
        );
        Self::new()
            .provider(provider)
            .model(Model::from_env().expect("model name required"))
    }

    /// Define the LLM provider. Takes a vendor provider directly, or a
    /// [`Provider`] shared with other agents.
    pub fn provider(mut self, provider: impl Into<Provider>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Set the model the agent calls.
    pub fn model(mut self, model: impl Into<Model>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Define who the agent is and how it should work.
    ///
    /// A `{context}` placeholder anywhere in the text expands to the facts of
    /// the moment as a bullet list: task ID, date, working directory,
    /// platform, and one line per configured limit. Each of those values is
    /// also a placeholder of its own, so a role can place one without the list:
    /// `{task_id}`, `{date}`, `{dir}`, `{platform}`, `{os_version}`,
    /// `{turns_remaining}`, `{input_tokens_remaining}`,
    /// `{output_tokens_remaining}`, and `{time_remaining}`. A limit left
    /// unconfigured expands to nothing and shows no bullet. Leave the
    /// placeholders out and nothing is added, so the role decides both whether
    /// those facts appear and where.
    ///
    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.role = role.into().trim().to_string();
        self
    }

    /// Restrict the agent to tasks carrying this label, and name it after
    /// the label.
    ///
    /// An agent serves one label and a task carries one, so an agent claims
    /// a task when the two are equal, and every agent serving that label may
    /// claim it. Calling this twice replaces the label, and the id
    /// [`Agent::get_id`] hands back is built from whichever one is set when the id
    /// is first read.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Let the agent wait for new instructions to keep a task in-progress.
    ///
    /// The agent stops after a reply that calls no tool, and
    /// `Werk::add_reply` starts the next turn. It gets no `FinishTool`,
    /// since ending the task would end the conversation; the host closes it
    /// with `Werk::set_task_finished`. Register the tool by hand to give the
    /// agent one back.
    pub fn interactive(mut self) -> Self {
        self.interactive = true;
        self
    }

    /// Insert or replace a template value shared by this agent.
    ///
    /// Placeholders are filled just before each task's first request. All agents
    /// in the Werk share these values, which remain literal after insertion.
    pub fn template(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.werk
            .upgrade()
            .expect("agent has a Werk")
            .set_template(key, value);
        self
    }

    /// Insert or replace several template values shared by this agent.
    pub fn templates<I, K, V>(self, variables: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.werk
            .upgrade()
            .expect("agent has a Werk")
            .set_templates(variables);
        self
    }

    /// Register a tool the agent may call.
    pub fn tool(mut self, tool: impl Into<Tool>) -> Self {
        self.register_tool(tool);
        self
    }

    /// Register several tools the agent may call.
    pub fn tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Tool>,
    {
        for t in tools {
            self.register_tool(t);
        }
        self
    }

    /// Set the directory the agent has access to, the current one by default.
    pub fn dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = dir.into();
        self
    }

    /// Share a knowledge store, the durable memory the agent carries across
    /// tasks and shares with other agents.
    ///
    /// It replaces the store opened by default, both for what the prompt shows
    /// and for what `KnowledgeTool` writes to. Hand the same store to
    /// several agents to share it between them.
    pub fn knowledge(mut self, store: &Arc<Knowledge>) -> Self {
        self.register_tool(KnowledgeTool::new(Arc::clone(store)));
        self.knowledge = Arc::clone(store);
        self
    }

    /// Override one model-facing directive.
    ///
    /// The key is exact and may name a built-in directive or an application
    /// event published through `EventTool`. Runtime placeholders such as
    /// `{path}` remain available in the replacement.
    pub fn directive(mut self, key: impl Into<String>, template: impl Into<String>) -> Self {
        Arc::make_mut(&mut self.directives).insert(key, template);
        self
    }

    /// Override several model-facing directives.
    ///
    /// Later entries replace earlier entries carrying the same key.
    pub fn directives<I, K, V>(mut self, overrides: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let directives = Arc::make_mut(&mut self.directives);
        for (key, template) in overrides {
            directives.insert(key, template);
        }
        self
    }

    /// The unique identifier this agent works under, `<label>-<n>` for a
    /// labeled agent and `agent-<n>` for one without. It names the agent in
    /// [`Event::get_agent_id`] and in [`Task::get_assignee`].
    ///
    /// The number is taken the first time this is called, directly or through
    /// [`Self::add_task`], [`Self::start`], or `Werk::add_agent`. Label the
    /// agent before then.
    ///
    /// [`Event::get_agent_id`]: crate::Event::get_agent_id
    /// [`Task::get_assignee`]: crate::Task::get_assignee
    pub fn get_id(&self) -> &str {
        self.id.get_or_init(|| next_id(self.label.as_deref()))
    }

    pub(super) fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Equality in both directions: an agent with no label serves the default
    /// scope and nothing else. Both labels rather than a receiver, since the
    /// loop's claim filter holds the label and cannot hold the agent.
    pub(super) fn handles(agent_label: Option<&str>, task_label: Option<&str>) -> bool {
        agent_label == task_label
    }

    pub(super) fn get_tools(&self, task: &Task) -> Vec<Tool> {
        let mut tools = self.tools.clone();
        if tools.iter().any(|tool| tool.get_name() == FinishTool::NAME) {
            tools.retain(|tool| tool.get_name() != FinishTool::NAME);
            tools.push(FinishTool::from_schema(task.schema.clone()));
        }
        if tools.iter().any(|tool| tool.get_name() == EventTool::NAME) {
            tools.retain(|tool| tool.get_name() != EventTool::NAME);
            tools.push(EventTool::from_schema(task.schema.clone()));
        }
        tools
    }

    pub(crate) fn get_tool(&self, tools: &[Tool], tool_name: &str) -> Option<Tool> {
        Tool::find_tool(tools, tool_name).cloned()
    }

    fn register_tool(&mut self, tool: impl Into<Tool>) {
        let tool = tool.into();
        tool.require_description_and_handler();
        self.tools
            .retain(|registered| registered.get_name() != tool.get_name());
        self.tools.push(tool);
    }

    #[cfg(test)]
    pub(crate) fn tool_list(&self) -> &[Tool] {
        &self.tools
    }

    pub(super) fn get_provider(&self) -> Provider {
        self.provider
            .clone()
            .expect("agent joined a Werk without a provider")
    }

    pub(super) fn get_model(&self) -> &Model {
        self.model
            .as_ref()
            .expect("agent joined a Werk without a model")
    }

    pub(super) fn get_knowledge(&self) -> Arc<Knowledge> {
        Arc::clone(&self.knowledge)
    }

    pub(super) fn get_directives(&self) -> Arc<DirectiveStore> {
        Arc::clone(&self.directives)
    }

    pub(super) fn get_dir(&self) -> PathBuf {
        self.dir.clone()
    }

    pub(super) fn get_role(&self) -> &str {
        &self.role
    }

    /// Refuse an agent that cannot call an LLM, at the moment it joins a Werk
    /// rather than on its first request.
    pub(super) fn require_provider_and_model(&self) {
        assert!(
            self.provider.is_some(),
            "provider required: call Agent::provider(..), or Agent::from_env()",
        );
        assert!(
            self.model.is_some(),
            "model required: call Agent::model(..), or Agent::from_env()",
        );
    }

    /// Give the agent the tool that ends a task, unless it is interactive.
    ///
    /// Here rather than in [`Self::new`], because only when the agent joins a
    /// Werk is `interactive` final. Nothing is ever removed, so an
    /// interactive agent that registered `FinishTool` itself keeps it.
    pub(super) fn register_finish_tool(&mut self) {
        if !self.interactive {
            self.register_tool(FinishTool);
        }
    }

    /// Submit a task and return its task ID.
    ///
    /// A string is the task itself. A [`Task`] carries a custom label or schema
    /// with it. Call it as often as you like: one agent can work on many tasks.
    pub fn add_task(&self, task: impl Into<Task>) -> String {
        self.dispatch(task.into())
    }

    fn dispatch(&self, task: Task) -> String {
        let werk = self
            .werk
            .upgrade()
            .expect("Agent::add_task requires a bound Werk");
        werk.insert(task, self.get_id().to_string())
    }

    /// Begin processing tasks, and hand back the Werk so results,
    /// waiting, and cancellation stay one call away.
    ///
    /// Configure the role, provider, and tools before starting. Template
    /// setters update the shared Werk and remain effective after startup.
    ///
    /// ```no_run
    /// # use agentwerk::Agent;
    /// # async fn run(agent: Agent) {
    /// let werk = agent.start();
    /// werk.finish().await;
    /// # }
    /// ```
    pub fn start(&self) -> Arc<Werk> {
        let werk = self.register();
        werk.start();
        werk
    }

    /// Wait for matching tasks and get the first result in query order.
    ///
    /// Registers this agent if needed and lets [`Werk::finish_task`] start
    /// execution automatically. On a shared Werk, the query can select any
    /// task. `None` means no matching task finished with a result.
    /// Configure the agent before its first start or finish call.
    ///
    /// ```no_run
    /// # use agentwerk::Agent;
    /// # async fn run(agent: Agent) {
    /// let task = agent.add_task("Summarize the changelog.");
    /// let result = agent.finish_task(task).await;
    /// # }
    /// ```
    pub async fn finish_task(&self, matches: impl Matcher<Task>) -> Option<serde_json::Value> {
        self.register().finish_task(matches).await
    }

    /// Wait for matching tasks and get their results in query order.
    ///
    /// Registers this agent if needed and delegates to [`Werk::finish_tasks`],
    /// including automatic startup and selection across the shared Werk.
    /// Configure the agent before its first start or finish call.
    pub async fn finish_tasks(&self, matches: impl Matcher<Task>) -> Vec<serde_json::Value> {
        self.register().finish_tasks(matches).await
    }

    /// Wait for every task in the bound Werk and get results in creation order.
    ///
    /// Registers this agent if needed and delegates to [`Werk::finish`],
    /// including automatic startup. This waits for the entire shared Werk.
    /// Configure the agent before its first start or finish call.
    pub async fn finish(&self) -> Vec<serde_json::Value> {
        self.register().finish().await
    }

    fn register(&self) -> Arc<Werk> {
        let werk = self
            .werk
            .upgrade()
            .expect("Agent execution requires a bound Werk");
        if !werk.has_agent(self.get_id()) {
            werk.add_agent(self.clone());
        }
        werk
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn tools_accepts_a_mixed_list_of_tools() {
        use crate::event::Event;
        use crate::tools::{CommandTool, ReadFileTool, Tool};

        let agent = Agent::new().tools(vec![
            Tool::from(ReadFileTool),
            CommandTool::new("git").allow("git *").into(),
            Tool::new("greet")
                .description("Say hello.")
                .handler(|_: serde_json::Value| async { Event::success("hi") }),
        ]);
        let names: Vec<String> = agent
            .tool_list()
            .iter()
            .map(|tool| tool.get_name().to_string())
            .collect();
        for name in ["read_file", "git", "greet"] {
            assert!(names.contains(&name.to_string()), "{names:?}");
        }
    }

    use std::sync::Arc;

    use super::*;
    use crate::agents::policy::Policy;
    use crate::agents::stats::Stats;
    use crate::event::Event;
    use crate::providers::TokenUsage;

    /// An agent a Werk accepts: the provider and model joining one demands.
    fn callable(agent: Agent) -> Agent {
        use crate::agents::r#loop::test_util::MockProvider;
        agent
            .provider(MockProvider::with_results(vec![]))
            .model("test")
    }

    fn handles(agent: &Agent, task_label: Option<&str>) -> bool {
        Agent::handles(agent.label.as_deref(), task_label)
    }

    #[test]
    fn handles_default_scope_only_picks_unlabeled_tasks() {
        let agent = Agent::new();
        assert!(handles(&agent, None));
        assert!(!handles(&agent, Some("research")));
    }

    #[test]
    fn handles_only_the_task_carrying_its_own_label() {
        let agent = Agent::new().label("research");
        assert!(handles(&agent, Some("research")));
        assert!(!handles(&agent, Some("report")));
        assert!(!handles(&agent, None));
    }

    #[test]
    fn label_replaces_the_previous_one() {
        let agent = Agent::new().label("research").label("math");
        assert!(handles(&agent, Some("math")));
        assert!(!handles(&agent, Some("research")));
    }

    #[test]
    fn interactive_defaults_to_false() {
        assert!(!Agent::new().is_interactive());
    }

    #[test]
    fn interactive_sets_the_flag() {
        assert!(Agent::new().interactive().is_interactive());
    }

    #[test]
    fn ids_are_numbered_per_label() {
        let first = Agent::new().label("ids_per_label");
        let second = Agent::new().label("ids_per_label");
        assert_eq!(first.get_id(), "ids_per_label-1");
        assert_eq!(second.get_id(), "ids_per_label-2");
    }

    #[test]
    fn an_unlabeled_agent_is_numbered_under_agent() {
        let agent = Agent::new();
        assert!(
            agent.get_id().starts_with("agent-"),
            "unexpected id: {}",
            agent.get_id()
        );
    }

    #[test]
    fn a_clone_keeps_the_id_of_the_agent_it_came_from() {
        let agent = Agent::new().label("cloned_id");
        assert_eq!(agent.clone().get_id(), agent.get_id());
    }

    #[test]
    fn directive_and_directives_apply_overrides_in_order() {
        let agent = Agent::new()
            .directive("cache_miss", "one")
            .directives([("cache_miss", "two"), ("cache_hit", "three")]);
        let directives = agent.get_directives();

        assert_eq!(
            directives.render_override("cache_miss", &[]).as_deref(),
            Some("two"),
        );
        assert_eq!(
            directives.render_override("cache_hit", &[]).as_deref(),
            Some("three"),
        );
    }

    #[test]
    fn adding_an_override_to_a_clone_does_not_change_the_original() {
        let original = Agent::new().directive("cache_miss", "original");
        let changed = original.clone().directive("cache_miss", "changed");

        assert_eq!(
            original
                .get_directives()
                .render_override("cache_miss", &[])
                .as_deref(),
            Some("original"),
        );
        assert_eq!(
            changed
                .get_directives()
                .render_override("cache_miss", &[])
                .as_deref(),
            Some("changed"),
        );
    }

    #[test]
    fn agent_template_values_do_not_bind_directive_placeholders() {
        let agent = Agent::new()
            .template("path", "src/lib.rs")
            .directive("cache_miss", "Missing {path}");

        assert_eq!(
            agent
                .get_directives()
                .render_override("cache_miss", &[])
                .as_deref(),
            Some("Missing {path}"),
        );
    }

    fn create_system_prompt(
        agent: &Agent,
        knowledge: Option<&str>,
        policy: &Policy,
        stats: &Stats,
        task_id: &str,
    ) -> String {
        let werk = agent.werk.upgrade().unwrap();
        let context_values = crate::prompts::context_values(&agent.dir, policy, stats, task_id);
        let rendered_role = werk.render_prompt(&agent.role, &context_values).unwrap();
        let knowledge_body = knowledge.unwrap_or_default().trim_matches('\n');
        match (rendered_role.is_empty(), knowledge_body.is_empty()) {
            (_, true) => rendered_role,
            (true, false) => format!("## Knowledge\n\n{knowledge_body}"),
            (false, false) => {
                format!("{rendered_role}\n\n## Knowledge\n\n{knowledge_body}")
            }
        }
    }

    #[test]
    fn a_role_without_the_placeholder_gets_no_context_block() {
        let agent = Agent::new().role("ROLE");
        assert_eq!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1"),
            "ROLE"
        );
    }

    #[test]
    fn a_role_can_be_read_by_the_caller() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let file = dir.path().join("reviewer.md");
        std::fs::write(&file, "ROLE\n").unwrap();
        let role = std::fs::read_to_string(file).unwrap();
        let agent = Agent::new().role(role);
        assert_eq!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1"),
            "ROLE"
        );
    }

    #[test]
    fn a_role_read_by_the_caller_keeps_its_placeholders_expandable() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let file = dir.path().join("reviewer.md");
        std::fs::write(&file, "ROLE\n\n{context}\n").unwrap();
        let role = std::fs::read_to_string(file).unwrap();
        let agent = Agent::new().role(role).dir("/tmp/check");
        assert!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1")
                .contains("- Working directory: /tmp/check")
        );
    }

    #[test]
    fn system_prompt_expands_the_context_placeholder() {
        let agent = Agent::new().role("ROLE\n\n{context}").dir("/tmp/check");
        let prompt = create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1");
        assert!(prompt.starts_with("ROLE\n\n"));
        assert!(prompt.contains("- Task: T-1"));
        assert!(prompt.contains("- Working directory: /tmp/check"));
        assert!(prompt.contains("- Platform: "));
        assert!(prompt.contains("- Date: "));
    }

    #[test]
    fn the_context_block_lists_the_remaining_budgets() {
        let agent = Agent::new().role("{context}").dir("/tmp/check");
        let policy = Policy {
            max_turns: Some(3),
            max_input_tokens: Some(1_000),
            ..Policy::default()
        };
        let stats = Stats::of([
            Event::new(Event::TURN_STARTED),
            Event::new(Event::REQUEST_FINISHED).data(serde_json::json!({
                "model": "m",
                "usage": TokenUsage {
                    input_tokens: 250,
                    output_tokens: 0,
                },
            })),
        ]);

        // The exact rendering is pinned in `prompts`; what matters here is
        // that the role's placeholder sees the live policy and stats.
        let rendered = create_system_prompt(&agent, None, &policy, &stats, "T-1");

        assert!(rendered.contains("- Turns remaining: 2"));
        assert!(rendered.contains("- Input tokens remaining: 750"));
    }

    #[test]
    fn built_in_context_shadows_a_shared_template() {
        let agent = Agent::new()
            .role("{context}")
            .template("context", "- Note: mine");
        assert!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1")
                .starts_with("- Task: T-1")
        );
    }

    #[test]
    fn a_single_context_value_expands_without_the_block() {
        let agent = Agent::new()
            .role("Task {task_id} in {dir}, {turns_remaining} turns left.")
            .dir("/tmp/check");
        let policy = Policy {
            max_turns: Some(3),
            ..Policy::default()
        };
        let stats = Stats::of([Event::new(Event::TURN_STARTED)]);

        let rendered = create_system_prompt(&agent, None, &policy, &stats, "T-1");

        assert_eq!(rendered, "Task T-1 in /tmp/check, 2 turns left.");
    }

    #[test]
    fn task_is_not_an_alias_for_task_id() {
        let agent = Agent::new().role("{task}");

        assert_eq!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1"),
            "{task}"
        );
    }

    #[test]
    fn a_single_budget_value_expands_to_nothing_when_unconfigured() {
        let agent = Agent::new().role("Turns left: {turns_remaining}.");
        assert_eq!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1"),
            "Turns left: ."
        );
    }

    #[test]
    fn built_in_value_shadows_a_shared_template() {
        let agent = Agent::new().role("{task_id}").template("task_id", "mine");
        assert_eq!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1"),
            "T-1"
        );
    }

    #[test]
    fn system_prompt_empty_when_role_unset() {
        let agent = Agent::new();
        assert!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1").is_empty()
        );
    }

    fn tool_names(agent: &Agent) -> Vec<String> {
        agent
            .tool_list()
            .iter()
            .map(|tool| tool.get_name().to_string())
            .collect()
    }

    /// The tools the agent runs with, which `finish` only joins at the Werk.
    fn tool_names_in_a_werk(agent: Agent) -> Vec<String> {
        let mut agent = callable(agent);
        crate::agents::Werk::new().bind_agent(&mut agent);
        tool_names(&agent)
    }

    #[test]
    fn an_agent_that_joined_a_werk_has_finish_registered() {
        let names = tool_names_in_a_werk(Agent::new());
        assert!(names.iter().any(|n| n == "finish"), "{names:?}");
        assert!(
            !names.iter().any(|n| n == "event"),
            "event remains opt-in: {names:?}",
        );
    }

    #[test]
    fn an_agent_keeps_an_event_tool_it_registered_explicitly() {
        let names = tool_names_in_a_werk(Agent::new().tool(crate::tools::EventTool));
        assert!(names.iter().any(|n| n == "event"), "{names:?}");
        assert!(names.iter().any(|n| n == "finish"), "{names:?}");
    }

    #[test]
    fn an_interactive_agent_has_no_finish_tool() {
        let names = tool_names_in_a_werk(Agent::new().interactive());
        assert!(
            !names.iter().any(|n| n == "finish"),
            "an interactive agent ends its task through the host: {names:?}",
        );
    }

    #[test]
    fn an_interactive_agent_keeps_a_finish_tool_it_registered_itself() {
        let names = tool_names_in_a_werk(Agent::new().interactive().tool(FinishTool));
        assert!(names.iter().any(|n| n == "finish"), "{names:?}");
    }

    #[test]
    fn an_interactive_agent_keeps_an_event_tool_it_registered_itself() {
        let names = tool_names_in_a_werk(Agent::new().interactive().tool(crate::tools::EventTool));
        assert!(names.iter().any(|n| n == "event"), "{names:?}");
        assert!(!names.iter().any(|n| n == "finish"), "{names:?}");
    }

    #[test]
    fn system_prompt_interpolates_role_placeholders() {
        let agent = Agent::new()
            .role("You are {persona}.")
            .template("persona", "a senior reviewer");
        assert_eq!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1"),
            "You are a senior reviewer."
        );
    }

    #[test]
    fn unresolved_placeholders_pass_through() {
        let agent = Agent::new().role("Hi {missing}.");
        assert_eq!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1"),
            "Hi {missing}."
        );
    }

    #[test]
    fn multiple_variables_substitute_independently() {
        let agent = Agent::new()
            .role("{greeting}, {name}.")
            .templates([("greeting", "Hello"), ("name", "Alice")]);
        assert_eq!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1"),
            "Hello, Alice."
        );
    }

    #[test]
    fn no_variables_renders_role_unchanged() {
        let agent = Agent::new().role("You are a senior reviewer.");
        assert_eq!(
            create_system_prompt(&agent, None, &Policy::default(), &Stats::new(), "T-1"),
            "You are a senior reviewer."
        );
    }

    #[tokio::test]
    async fn add_task_keeps_source_for_initial_rendering() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = crate::agents::Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let mut agent = callable(Agent::new().template("topic", "rust"));
        werk.bind_agent(&mut agent);
        agent.add_task("Search {topic} forums.");
        let stored = werk
            .get_tasks()
            .into_iter()
            .next()
            .expect("task should have been enqueued");
        assert_eq!(
            stored.task,
            serde_json::Value::String("Search {topic} forums.".into()),
        );
    }

    #[tokio::test]
    async fn a_task_body_keeps_the_context_placeholder_verbatim() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = crate::agents::Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        // The block needs a task ID and live budgets, neither of which
        // exists yet at dispatch. Only the role expands it.
        let mut agent = callable(Agent::new());
        werk.bind_agent(&mut agent);
        agent.add_task("Work on {context}.");
        let stored = werk
            .get_tasks()
            .into_iter()
            .next()
            .expect("task should have been enqueued");
        assert_eq!(
            stored.task,
            serde_json::Value::String("Work on {context}.".into()),
        );
    }

    #[tokio::test]
    async fn add_task_leaves_object_task_unchanged() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = crate::agents::Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let mut agent = callable(Agent::new().template("topic", "rust"));
        werk.bind_agent(&mut agent);
        let value = serde_json::json!({"q": "Find {topic}"});
        agent.add_task(Task::new(value.clone()));
        let stored = werk
            .get_tasks()
            .into_iter()
            .next()
            .expect("task should have been enqueued");
        assert_eq!(stored.task, value);
    }

    #[test]
    fn add_task_returns_an_id_on_the_private_werk() {
        let agent = Agent::new();
        let id = agent.add_task(Task::new("inspect"));
        let private = agent.werk.upgrade().expect("private Werk exists");

        assert!(id.starts_with("t-"));
        assert_eq!(private.get_task(&id).unwrap().get_task(), "inspect");
    }

    #[test]
    fn add_task_uses_the_shared_werk_after_registration() {
        let werk = Werk::new();
        let agent = callable(Agent::new().template("target", "src"));
        werk.add_agent(agent.clone());

        let id = agent.add_task("Inspect {target}.");

        assert_eq!(werk.get_task(&id).unwrap().get_task(), "Inspect {target}.");
    }

    #[test]
    fn knowledge_registers_the_knowledge_tool_on_the_agent() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let agent = Agent::new().knowledge(&store);
        let registry = agent.tool_list();
        let names: Vec<String> = registry
            .iter()
            .map(|tool| tool.get_name().to_string())
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
            .get_pages()
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
            .get_pages()
            .save(crate::agents::knowledge::Page {
                slug: "shared".into(),
                kind: String::new(),
                description: "Shared note".into(),
                content: "# Shared".into(),
                tags: vec![],
            })
            .unwrap();
        assert!(cloned.knowledge.get_index().contains("shared"));
    }

    #[test]
    fn two_agents_bound_to_one_store_see_each_others_writes() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let alice = Agent::new().knowledge(&store);
        let bob = Agent::new().knowledge(&store);
        alice
            .knowledge
            .get_pages()
            .save(crate::agents::knowledge::Page {
                slug: "from-alice".into(),
                kind: String::new(),
                description: "From Alice".into(),
                content: "# Alice".into(),
                tags: vec![],
            })
            .unwrap();
        assert!(bob.knowledge.get_index().contains("from-alice"));
    }

    #[test]
    fn system_prompt_renders_knowledge_section_when_body_present() {
        let agent = Agent::new().role("R");
        let prompt = create_system_prompt(
            &agent,
            Some("- **config**: Port 8080"),
            &Policy::default(),
            &Stats::new(),
            "T-1",
        );
        assert_eq!(prompt, "R\n\n## Knowledge\n\n- **config**: Port 8080");
    }

    #[test]
    fn system_prompt_formats_knowledge_without_rendering_its_contents() {
        let agent = Agent::new().template("company", "Acme");
        assert_eq!(
            create_system_prompt(
                &agent,
                Some("\n{company}\n"),
                &Policy::default(),
                &Stats::new(),
                "T-1",
            ),
            "## Knowledge\n\n{company}"
        );
        let agent = agent.role("\nRole\n");
        assert_eq!(
            create_system_prompt(
                &agent,
                Some("\n{company}\n"),
                &Policy::default(),
                &Stats::new(),
                "T-1",
            ),
            "Role\n\n## Knowledge\n\n{company}"
        );
    }

    #[test]
    fn system_prompt_omits_knowledge_when_body_empty() {
        let agent = Agent::new().role("R");
        assert_eq!(
            create_system_prompt(&agent, Some(""), &Policy::default(), &Stats::new(), "T-1",),
            "R"
        );
    }

    #[test]
    fn new_agent_has_the_knowledge_tool_registered() {
        let agent = Agent::new();
        let registry = agent.tool_list();
        let names: Vec<String> = registry
            .iter()
            .map(|tool| tool.get_name().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "knowledge"),
            "knowledge must be registered on every new agent: {names:?}",
        );
    }

    #[test]
    #[should_panic(expected = "provider required")]
    fn joining_a_werk_without_a_provider_panics() {
        let mut agent = Agent::new().model("test");
        crate::agents::Werk::new().bind_agent(&mut agent);
    }

    #[test]
    fn the_label_set_before_the_id_is_read_names_it() {
        let agent = Agent::new().label("named_before_read");
        assert_eq!(agent.get_id(), "named_before_read-1");
    }

    #[test]
    fn a_label_set_after_the_id_was_read_leaves_it_alone() {
        let agent = Agent::new();
        let before = agent.get_id().to_string();
        let agent = agent.label("named_after_read");
        assert_eq!(agent.get_id(), before);
    }

    #[tokio::test]
    async fn starting_twice_registers_the_agent_once() {
        let agent = callable(Agent::new());
        let werk = agent.start();
        agent.start();
        assert_eq!(werk.clone_agents().len(), 1);
    }

    #[tokio::test]
    async fn binding_agent_with_explicit_knowledge_keeps_explicit_store() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let store = Knowledge::load(dir.path()).unwrap();
        let werk = crate::agents::Werk::new();
        let mut agent = callable(Agent::new().knowledge(&store));
        werk.bind_agent(&mut agent);
        assert!(Arc::ptr_eq(&store, &agent.knowledge));
    }

    #[tokio::test]
    async fn finish_task_starts_and_restarts_for_new_tasks_without_registering_twice() {
        use crate::agents::r#loop::test_util::{write_result_response, MockProvider};
        let dir = crate::test_util::TempDir::new().unwrap();
        let agent = Agent::new()
            .provider(MockProvider::with_results(vec![
                Ok(write_result_response("first")),
                Ok(write_result_response("second")),
            ]))
            .model("test");
        let werk = agent.werk.upgrade().unwrap();
        werk.set_dir(dir.path().to_path_buf());

        for answer in ["first", "second"] {
            let task = agent.add_task(answer);
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(5), agent.finish_task(task))
                    .await
                    .unwrap();
            assert_eq!(result, Some(serde_json::json!(answer)));
        }
        assert_eq!(werk.clone_agents().len(), 1);
        assert_eq!(werk.find_events("event.name = run_started").len(), 2);
    }

    #[tokio::test]
    async fn finish_tasks_starts_execution_and_returns_selected_results_in_query_order() {
        use crate::agents::r#loop::test_util::{write_result_response, MockProvider};
        let dir = crate::test_util::TempDir::new().unwrap();
        let agent = Agent::new()
            .provider(MockProvider::with_results(vec![
                Ok(write_result_response("first")),
                Ok(write_result_response("second")),
                Ok(write_result_response("third")),
            ]))
            .model("test");
        let werk = agent.werk.upgrade().unwrap();
        werk.set_dir(dir.path().to_path_buf());
        let first = agent.add_task("first");
        agent.add_task("second");
        agent.add_task("third");

        let results = agent
            .finish_tasks(format!("task.id != {first} ORDER BY task.id DESC"))
            .await;
        assert_eq!(
            results,
            vec![serde_json::json!("third"), serde_json::json!("second")]
        );
        assert_eq!(
            agent.finish_task("ORDER BY task.id DESC").await,
            Some(serde_json::json!("third"))
        );
    }

    #[tokio::test]
    async fn finish_starts_and_waits_for_every_agent_in_the_shared_werk() {
        use crate::agents::r#loop::test_util::{write_result_response, MockProvider};
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let agent = Agent::new()
            .label("scan")
            .provider(MockProvider::with_results(vec![Ok(write_result_response(
                "clean",
            ))]))
            .model("test");
        werk.add_agent(agent.clone()).add_agent(
            Agent::new()
                .label("report")
                .provider(MockProvider::with_results(vec![Ok(write_result_response(
                    "report",
                ))]))
                .model("test"),
        );
        agent.add_task(Task::labeled("scan", "scan"));
        let report = werk.add_task(Task::labeled("report", "report"));

        assert_eq!(
            agent.finish().await,
            vec![serde_json::json!("clean"), serde_json::json!("report")]
        );
        assert_eq!(
            agent.finish_task(report).await,
            Some(serde_json::json!("report"))
        );
        assert_eq!(
            agent.finish_tasks("report").await,
            vec![serde_json::json!("report")]
        );
        assert_eq!(werk.clone_agents().len(), 2);
    }

    #[tokio::test]
    async fn finish_methods_return_no_results_for_empty_or_failed_tasks() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let agent = callable(Agent::new());
        let werk = agent.werk.upgrade().unwrap();
        werk.set_dir(dir.path().to_path_buf());
        assert_eq!(agent.finish_task("missing").await, None);
        assert!(agent.finish_tasks("missing").await.is_empty());
        assert!(agent.finish().await.is_empty());

        let task = agent.add_task("the mock provider has no responses");
        assert_eq!(agent.finish_task(task.clone()).await, None);
        assert!(werk.get_task(&task).unwrap().is_failed());
        assert!(agent.finish_tasks(task).await.is_empty());
        assert!(agent.finish().await.is_empty());
    }

    #[tokio::test]
    async fn finish_methods_do_not_resume_cancelled_work() {
        use crate::agents::r#loop::test_util::MockProvider;
        let dir = crate::test_util::TempDir::new().unwrap();
        let provider = MockProvider::with_results(vec![]);
        let agent = Agent::new().provider(provider.clone()).model("test");
        let werk = agent.werk.upgrade().unwrap();
        werk.set_dir(dir.path().to_path_buf());
        let task = agent.add_task("cancel before the provider is called");
        agent.start().cancel_all_tasks();

        assert_eq!(agent.finish_task(task.clone()).await, None);
        assert!(agent.finish_tasks(task.clone()).await.is_empty());
        assert!(agent.finish().await.is_empty());
        assert!(werk.get_task(&task).unwrap().is_cancelled());
        assert_eq!(provider.requests(), 0);
        assert_eq!(werk.find_events("event.name = run_started").len(), 1);
    }

    #[tokio::test]
    async fn start_keeps_running_across_empty_queues_until_finish_is_awaited() {
        use crate::agents::r#loop::test_util::{write_result_response, MockProvider};
        use std::time::Duration;

        let dir = crate::test_util::TempDir::new().unwrap();
        let agent = Agent::new()
            .provider(MockProvider::with_results(vec![
                Ok(write_result_response("first")),
                Ok(write_result_response("second")),
            ]))
            .model("test");
        let werk = agent.werk.upgrade().unwrap();
        werk.set_dir(dir.path().to_path_buf());
        agent.start();

        for answer in ["first", "second"] {
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert!(werk.find_events("event.name = run_finished").is_empty());
            let task = agent.add_task(answer);
            tokio::time::timeout(Duration::from_secs(5), async {
                while !werk.get_task(&task).unwrap().is_finished() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the background run did not process the new task");
        }

        assert_eq!(
            agent.finish().await,
            vec![serde_json::json!("first"), serde_json::json!("second")]
        );
        assert_eq!(werk.find_events("event.name = run_started").len(), 1);
        assert_eq!(werk.find_events("event.name = run_finished").len(), 1);
    }
}
