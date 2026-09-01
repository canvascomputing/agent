//! The core entity of agentwerk: who an agent is, what it may call, and which
//! Werk it works from.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::prompts::{context_values, render_context, PromptBuilder, Text};
use crate::providers::{Model, Provider};
use crate::tools::{EventTool, FinishTool, KnowledgeTool, Tool};

use super::knowledge::Knowledge;
use super::policy::Policy;
use super::stats::Stats;
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

/// How an `Agent` reaches its `Werk`. A new agent carries `Private`
/// until it is added to a Werk; everything else carries `Shared`.
pub(crate) enum WerkRef {
    Shared(Weak<Werk>),
    Private(Arc<Werk>),
}

impl WerkRef {
    pub(crate) fn upgrade(&self) -> Option<Arc<Werk>> {
        match self {
            Self::Shared(w) => w.upgrade(),
            Self::Private(a) => Some(Arc::clone(a)),
        }
    }
}

/// An `Agent` is the core entity of agentwerk. It has access to tools for
/// solving tasks in the form of tasks.
///
/// It claims the tasks its label matches, calls the LLM provider, runs
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
///     .tool(ReadFileTool);
/// # let _ = agent;
/// # }
/// ```
pub struct Agent {
    // pub(crate): read by loop, Werk, or assignment code
    pub(crate) label: Option<String>,
    pub(crate) interactive: bool,
    pub(crate) werk: WerkRef,
    // private: accessed through methods within agents::
    /// Taken the first time anything needs it, since the label it is built from
    /// is set after construction.
    id: OnceLock<String>,
    provider: Option<Provider>,
    model: Option<Model>,
    role: String,
    templates: Vec<(String, String)>,
    handover: Option<Task>,
    tools: Vec<Tool>,
    dir: PathBuf,
    knowledge: Arc<Knowledge>,
    directives: Arc<DirectiveStore>,
}

impl Clone for Agent {
    /// A clone is the same agent, id included: `bind_agent` keeps one, and the
    /// two would otherwise disagree about which tasks are theirs. Reading the
    /// id here is what fixes it for both. The clone points at the shared Werk,
    /// so rebinding the original cannot leave it filing tasks into a Werk
    /// nothing reads.
    fn clone(&self) -> Self {
        let werk = match &self.werk {
            WerkRef::Shared(w) => WerkRef::Shared(w.clone()),
            WerkRef::Private(a) => WerkRef::Shared(Arc::downgrade(a)),
        };
        Self {
            id: OnceLock::from(self.get_id().to_string()),
            label: self.label.clone(),
            interactive: self.interactive,
            directives: Arc::clone(&self.directives),
            werk,
            provider: self.provider.clone(),
            model: self.model.clone(),
            role: self.role.clone(),
            templates: self.templates.clone(),
            handover: self.handover.clone(),
            tools: self.tools.clone(),
            dir: self.dir.clone(),
            knowledge: Arc::clone(&self.knowledge),
        }
    }
}

impl Agent {
    /// Create an agent, with a Werk of its own so
    /// `.task(...)` and `.start()` work without one being set up.
    /// `Werk::add_agent(...)` later moves those tasks into the shared Werk.
    ///
    /// Give it a provider and a model before it starts work.
    pub fn new() -> Self {
        let knowledge = Knowledge::load(".agentwerk").expect("open knowledge store");
        let mut agent = Self {
            id: OnceLock::new(),
            provider: None,
            model: None,
            role: String::new(),
            label: None,
            interactive: false,
            werk: WerkRef::Private(Werk::new()),
            templates: Vec::new(),
            handover: None,
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
    /// A string is the role itself; a `&Path` or `PathBuf` names the file
    /// holding it, which panics when that file cannot be read.
    pub fn role(mut self, role: impl Into<Text>) -> Self {
        self.role = role.into().into_string();
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
    /// `Werk::add_reply` drives the next turn. It gets no `FinishTool`,
    /// since ending the task would end the conversation; the host closes it
    /// with `Werk::set_task_finished`. Register the tool by hand to give the
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

    /// Configure the one task this agent creates when it finishes its work.
    ///
    /// The task must carry a non-blank label. Its body and schema prefill the
    /// model-facing `handover` object, so the model may finish with only its
    /// result. Calling this again replaces the prior handover.
    pub fn handover(mut self, task: Task) -> Self {
        let label = task
            .label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .expect("Agent::handover requires a labeled task")
            .to_string();
        let mut handover = Task::new(task.task).label(label);
        if let Some(schema) = task.schema {
            handover = handover.schema(schema);
        }
        self.handover = Some(handover);
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

    /// The unique identifier this agent works under, `<label>-<n>` for a
    /// labeled agent and `agent-<n>` for one without. It names the agent in
    /// [`Event::get_agent_id`] and in [`Task::get_assignee`].
    ///
    /// The number is taken the first time this is called, directly or through
    /// [`Self::task`], [`Self::start`], or `Werk::add_agent`. Label the
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
            tools.push(FinishTool::from_schema(
                task.schema.clone(),
                self.handover.clone(),
            ));
        }
        if tools.iter().any(|tool| tool.get_name() == EventTool::NAME) {
            tools.retain(|tool| tool.get_name() != EventTool::NAME);
            tools.push(EventTool::from_schema(
                task.schema.clone(),
                self.handover.clone(),
            ));
        }
        tools
    }

    pub(crate) fn get_tool(&self, tools: &[Tool], tool_name: &str) -> Option<Tool> {
        let tool_name = tool_name.trim();
        if let Some(found) = tools.iter().find(|tool| tool.get_name() == tool_name) {
            return Some(found.clone());
        }

        let normalize = |tool_name: &str| {
            let name = tool_name.trim().to_lowercase().replace('-', "_");
            match name.strip_suffix("_tool") {
                Some(stem) if !stem.is_empty() => stem.to_string(),
                _ => name,
            }
        };
        let tool_name = normalize(tool_name);
        let mut folded = tools
            .iter()
            .filter(|tool| normalize(tool.get_name()) == tool_name);
        let found = folded.next()?;
        folded.next().is_none().then(|| found.clone())
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

    #[cfg(test)]
    fn get_handover(&self) -> Option<Task> {
        self.handover.clone()
    }

    pub(super) fn get_dir(&self) -> PathBuf {
        self.dir.clone()
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

    pub(super) fn system_prompt(
        &self,
        knowledge: Option<&str>,
        policy: &Policy,
        stats: &Stats,
        task_id: &str,
    ) -> String {
        let mut b = PromptBuilder::default();
        if !self.role.is_empty() {
            let role = self.interpolate(&self.role);
            b = b.role(self.expand_context(role, policy, stats, task_id));
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
        policy: &Policy,
        stats: &Stats,
        task_id: &str,
    ) -> String {
        if !role.contains('{') {
            return role;
        }
        let values = context_values(&self.dir, policy, stats, task_id);
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

    /// Submit a task and return its task ID.
    ///
    /// A string is the task itself, and a `&Path` or `PathBuf` names the file
    /// holding it. A [`Task`] carries a custom label or schema with it. Call
    /// it as often as you like: one agent can drive many tasks.
    pub fn task(&self, task: impl Into<Task>) -> String {
        self.dispatch(task.into())
    }

    fn dispatch(&self, mut task: Task) -> String {
        let werk = self
            .werk
            .upgrade()
            .expect("Agent::task requires a bound Werk");
        if let serde_json::Value::String(s) = &task.task {
            task.task = serde_json::Value::String(self.interpolate(s));
        }
        werk.insert(task, self.get_id().to_string())
    }

    /// Begin processing tasks, and hand back the Werk so results,
    /// waiting, and cancellation stay one call away.
    ///
    /// The Werk takes the agent as it stands, so configure it first: a
    /// setter called afterwards leaves the running copy untouched.
    ///
    /// ```no_run
    /// # use agentwerk::Agent;
    /// # async fn run(agent: Agent) {
    /// let work = agent.start();
    /// work.finish_all_tasks().await;
    /// # }
    /// ```
    pub fn start(&self) -> Arc<Werk> {
        let werk = self
            .werk
            .upgrade()
            .expect("Agent::start requires a bound Werk");
        if !werk.has_agent(self.get_id()) {
            werk.add_agent(self.clone());
        }
        werk.start();
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
    use crate::agents::tasks::Status;
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
    fn handover_replaces_the_previous_task() {
        let agent = Agent::new()
            .handover(Task::labeled("research", "find leads"))
            .handover(Task::labeled("report", serde_json::json!({"write": true})));

        let handover = agent.get_handover().unwrap();
        assert_eq!(handover.get_label(), Some("report"));
        assert_eq!(handover.get_task(), &serde_json::json!({"write": true}));
    }

    #[test]
    fn handover_keeps_only_label_task_and_schema() {
        let schema = crate::Schema::new(serde_json::json!({"type": "string"})).unwrap();
        let mut task = Task::labeled("report", "write")
            .schema(schema)
            .parent("old-parent");
        task.id = "old-id".into();
        task.reporter = "old-reporter".into();
        task.assignee = Some("old-assignee".into());
        task.status = Status::Finished;
        task.result = Some(serde_json::json!("old-result"));

        let handover = Agent::new().handover(task).get_handover().unwrap();
        assert_eq!(handover.get_label(), Some("report"));
        assert_eq!(handover.get_task(), "write");
        assert!(handover.get_schema().is_some());
        assert_eq!(handover.get_id(), "");
        assert_eq!(handover.get_status(), Status::Todo);
        assert_eq!(handover.get_reporter(), "");
        assert_eq!(handover.get_assignee(), None);
        assert_eq!(handover.get_parent(), None);
        assert_eq!(handover.get_result(), None);
    }

    #[test]
    #[should_panic(expected = "Agent::handover requires a labeled task")]
    fn handover_requires_a_label() {
        let _ = Agent::new().handover(Task::new("write"));
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

    /// The system prompt with no live state and a fixed task ID.
    fn system_prompt(agent: &Agent, knowledge: Option<&str>) -> String {
        agent.system_prompt(knowledge, &Policy::default(), &Stats::new(), "T-1")
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
        let rendered = agent.system_prompt(None, &policy, &stats, "T-1");

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
            .role("Task {task_id} in {dir}, {turns_remaining} turns left.")
            .dir("/tmp/check");
        let policy = Policy {
            max_turns: Some(3),
            ..Policy::default()
        };
        let stats = Stats::of([Event::new(Event::TURN_STARTED)]);

        let rendered = agent.system_prompt(None, &policy, &stats, "T-1");

        assert_eq!(rendered, "Task T-1 in /tmp/check, 2 turns left.");
    }

    #[test]
    fn task_is_not_an_alias_for_task_id() {
        let agent = Agent::new().role("{task}");

        assert_eq!(system_prompt(&agent, None), "{task}");
    }

    #[test]
    fn a_single_budget_value_expands_to_nothing_when_unconfigured() {
        let agent = Agent::new().role("Turns left: {turns_remaining}.");
        assert_eq!(system_prompt(&agent, None), "Turns left: .");
    }

    #[test]
    fn a_bound_single_value_shadows_the_built_in_one() {
        let agent = Agent::new().role("{task_id}").template("task_id", "mine");
        assert_eq!(system_prompt(&agent, None), "mine");
    }

    #[test]
    fn system_prompt_empty_when_role_unset() {
        let agent = Agent::new();
        assert!(system_prompt(&agent, None).is_empty());
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
        let werk = crate::agents::Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let mut agent = callable(Agent::new().template("topic", "rust"));
        werk.bind_agent(&mut agent);
        agent.task("Search {topic} forums.");
        let stored = werk
            .get_tasks()
            .into_iter()
            .next()
            .expect("task should have been enqueued");
        assert_eq!(
            stored.task,
            serde_json::Value::String("Search rust forums.".into()),
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
        agent.task("Work on {context}.");
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
    async fn dispatch_leaves_object_task_unchanged() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = crate::agents::Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let mut agent = callable(Agent::new().template("topic", "rust"));
        werk.bind_agent(&mut agent);
        let value = serde_json::json!({"q": "Find {topic}"});
        agent.task(Task::new(value.clone()));
        let stored = werk
            .get_tasks()
            .into_iter()
            .next()
            .expect("task should have been enqueued");
        assert_eq!(stored.task, value);
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
}
