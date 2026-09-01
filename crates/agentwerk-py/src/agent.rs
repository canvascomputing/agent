//! The agent as Python sees it. `Agent()` configures itself and drives its own
//! tasks, the way the Rust `Agent` does.
//!
//! Rust configures through methods that consume and return the agent, which is
//! why the agent sits in an `Option` here: a setter takes it out and puts the
//! returned one back.

use std::collections::BTreeMap;
use std::sync::Arc;

use agentwerk::providers::{Model, Provider};
use agentwerk::{Agent, Knowledge};
use pyo3::prelude::*;

use crate::convert::{py_to_text, runtime_error};
use crate::knowledge::PyKnowledge;
use crate::providers::{PyModel, PyProvider};
use crate::task::{to_task, PyTask};
use crate::tools::extract_tool;
use crate::werk::PyWerk;

/// An `Agent` is the core entity of agentwerk. It uses tools to solve tasks
/// assigned through a Werk.
#[pyclass(name = "Agent")]
pub struct PyAgent {
    /// Empty only while a setter has the agent.
    agent: Option<Agent>,
    /// Rust panics when an agent without these joins a Werk. Python answers
    /// with the error it has always raised, so these record what was set.
    has_provider: bool,
    has_model: bool,
}

impl PyAgent {
    fn get(&self) -> &Agent {
        self.agent.as_ref().expect("a setter kept the agent")
    }

    /// Apply a Rust setter, which consumes the agent and hands back the next one.
    fn set(&mut self, edit: impl FnOnce(Agent) -> Agent) {
        let agent = self.agent.take().expect("a setter kept the agent");
        self.agent = Some(edit(agent));
    }

    /// The agent, for what needs one that can call an LLM.
    pub(crate) fn ready(&self) -> PyResult<&Agent> {
        if !self.has_provider {
            return Err(runtime_error(
                "provider not set: use Agent.from_env(), or provider(Provider.from_env())",
            ));
        }
        if !self.has_model {
            return Err(runtime_error(
                "model not set: use Agent.from_env(), model(name), or model(Model.from_env())",
            ));
        }
        Ok(self.get())
    }
}

#[pymethods]
impl PyAgent {
    #[new]
    fn new() -> Self {
        PyAgent {
            agent: Some(Agent::new()),
            has_provider: false,
            has_model: false,
        }
    }

    /// Create an agent with the provider and model from the environment.
    #[staticmethod]
    fn from_env() -> PyResult<Self> {
        let provider = Provider::from_env().map_err(runtime_error)?;
        let model = Model::from_env().map_err(runtime_error)?;
        Ok(PyAgent {
            agent: Some(Agent::new().provider(provider).model(model)),
            has_provider: true,
            has_model: true,
        })
    }

    /// Define the LLM provider.
    fn provider<'py>(
        mut slf: PyRefMut<'py, Self>,
        provider: PyRef<'_, PyProvider>,
    ) -> PyRefMut<'py, Self> {
        let resolved = provider.inner.clone();
        slf.set(|agent| agent.provider(resolved));
        slf.has_provider = true;
        slf
    }

    /// Set the model, by name or as a `Model` carrying a context window size
    /// and a reasoning level.
    fn model<'py>(
        mut slf: PyRefMut<'py, Self>,
        model: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let resolved = if let Ok(name) = model.extract::<String>() {
            Model::new(name)
        } else {
            model.extract::<PyRef<PyModel>>()?.inner.clone()
        };
        slf.set(|agent| agent.model(resolved));
        slf.has_model = true;
        Ok(slf)
    }

    /// Define who the agent is and how it should work.
    ///
    /// A `str` is the role itself; an `os.PathLike` names the file holding it.
    fn role<'py>(
        mut slf: PyRefMut<'py, Self>,
        role: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let resolved = py_to_text(role)?;
        slf.set(|agent| agent.role(resolved));
        Ok(slf)
    }

    /// Restrict the agent to tasks carrying this label, and name it after
    /// the label. Calling it twice replaces the label.
    fn label(mut slf: PyRefMut<'_, Self>, label: String) -> PyRefMut<'_, Self> {
        slf.set(|agent| agent.label(label));
        slf
    }

    /// The id the agent works under, taken the first time it is read.
    fn get_id(&self) -> &str {
        self.get().get_id()
    }

    /// Let the agent wait for new instructions to keep a task in-progress.
    ///
    /// It gets no `FinishTool()`; the host closes the task with
    /// `set_task_finished(id, result)`.
    fn interactive(mut slf: PyRefMut<'_, Self>) -> PyRefMut<'_, Self> {
        slf.set(|agent| agent.interactive());
        slf
    }

    /// Inject data into prompts with template strings.
    ///
    /// `{key}` is replaced in the role and in any text task. Binding `context`
    /// replaces the built-in block the role expands, and binding one of its
    /// value names, such as `task` or `turns_remaining`, replaces that value.
    fn template(mut slf: PyRefMut<'_, Self>, key: String, value: String) -> PyRefMut<'_, Self> {
        slf.set(|agent| agent.template(key, value));
        slf
    }

    /// Inject more than one entry into prompts.
    fn templates(
        mut slf: PyRefMut<'_, Self>,
        variables: BTreeMap<String, String>,
    ) -> PyRefMut<'_, Self> {
        slf.set(|agent| agent.templates(variables));
        slf
    }

    /// Configure the one labeled task this agent creates when it finishes.
    /// Calling this again replaces the previous handover.
    fn handover<'py>(
        mut slf: PyRefMut<'py, Self>,
        task: PyRef<'_, PyTask>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let resolved = task.inner.clone();
        if resolved
            .get_label()
            .is_none_or(|label| label.trim().is_empty())
        {
            return Err(runtime_error("Agent.handover requires a labeled Task"));
        }
        slf.set(|agent| agent.handover(resolved));
        Ok(slf)
    }

    /// Set the directory the agent has access to.
    fn dir(mut slf: PyRefMut<'_, Self>, dir: String) -> PyRefMut<'_, Self> {
        slf.set(|agent| agent.dir(dir));
        slf
    }

    /// Share a knowledge store, the durable memory the agent carries across
    /// tasks and shares with other agents.
    fn knowledge<'py>(
        mut slf: PyRefMut<'py, Self>,
        store: PyRef<'_, PyKnowledge>,
    ) -> PyRefMut<'py, Self> {
        let store: Arc<Knowledge> = Arc::clone(&store.inner);
        slf.set(|agent| agent.knowledge(&store));
        slf
    }

    /// Decide what the agent tells the model when a call fails.
    ///
    /// `compute` sees every directive before it renders and returns the text to
    /// send, or `None` for the ones it leaves as they are.
    fn directives<'py>(mut slf: PyRefMut<'py, Self>, compute: Py<PyAny>) -> PyRefMut<'py, Self> {
        slf.set(|agent| agent.directives(crate::directives::compute(compute)));
        slf
    }

    /// Register a tool the agent may call, either a built-in such as
    /// `ReadFileTool()` or a `@tool`-decorated function.
    fn tool<'py>(
        mut slf: PyRefMut<'py, Self>,
        tool: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let resolved = extract_tool(tool)?;
        slf.set(|agent| agent.tool(resolved));
        Ok(slf)
    }

    /// Register several tools the agent may call.
    fn tools<'py>(
        mut slf: PyRefMut<'py, Self>,
        tools: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let mut resolved = Vec::new();
        for tool in tools.try_iter()? {
            resolved.push(extract_tool(&tool?)?);
        }
        slf.set(|agent| agent.tools(resolved));
        Ok(slf)
    }

    /// Submit a task and return its task ID.
    ///
    /// A `str` is the task itself, and an `os.PathLike` names the file holding
    /// it. A `Task` carries a custom label or schema with it. Call it as often
    /// as you like: one agent can drive many tasks.
    fn add_task(&self, task: &Bound<'_, PyAny>) -> PyResult<String> {
        Ok(self.get().add_task(to_task(task)?))
    }

    /// Begin processing tasks, and hand back the Werk so results,
    /// waiting, and cancellation stay one call away.
    ///
    /// An agent without a provider or a model raises here.
    fn start(&self) -> PyResult<PyWerk> {
        // The run spawns onto the ambient Tokio runtime, which a pymethod call
        // does not have entered on its own thread.
        let _guard = pyo3_async_runtimes::tokio::get_runtime().enter();
        Ok(PyWerk {
            inner: self.ready()?.start(),
        })
    }
}
