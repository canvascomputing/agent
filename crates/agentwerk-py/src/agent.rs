//! The agent as Python sees it. One class in two phases: `Agent()` collects the
//! configuration, `build()` creates the agent, and the rest drives it.
//!
//! Rust splits those phases across two types, which Python cannot hold, so they
//! collapse into one class here. That is why `build()` runs once: the Python
//! object owns the agent's own ticket queue, and rebuilding would leave the
//! queue that its copies still point at with nothing reading it.

use std::collections::BTreeMap;
use std::sync::Arc;

use agentwerk::providers::{Model, Provider};
use agentwerk::tools::ToolLike;
use agentwerk::{Agent, Knowledge};
use pyo3::prelude::*;

use crate::convert::{py_to_value, runtime_error};
use crate::knowledge::PyKnowledge;
use crate::providers::{PyModel, PyProvider};
use crate::ticket::PyTicket;
use crate::ticket_queue::PyTicketQueue;
use crate::tools::{extract_tool, BoxedTool};

/// An `Agent` is the core entity of agentwerk. It has access to tools for
/// solving tasks in the form of tickets.
#[pyclass(name = "Agent")]
pub struct PyAgent {
    role: Option<String>,
    label: Option<String>,
    templates: Vec<(String, String)>,
    dir: Option<String>,
    interactive: bool,
    provider: Option<Provider>,
    model: Option<Model>,
    tools: Vec<Arc<dyn ToolLike>>,
    knowledge: Option<Arc<Knowledge>>,
    /// Set by `build()`. Every method that reaches the queue needs it.
    agent: Option<Agent>,
}

impl PyAgent {
    fn create() -> Self {
        PyAgent {
            role: None,
            label: None,
            templates: Vec::new(),
            dir: None,
            interactive: false,
            provider: None,
            model: None,
            tools: Vec::new(),
            knowledge: None,
            agent: None,
        }
    }

    /// The built agent, for the methods that drive it.
    pub(crate) fn built(&self) -> PyResult<&Agent> {
        self.agent
            .as_ref()
            .ok_or_else(|| runtime_error("agent not built: call build() first"))
    }

    /// Guards every configuration method. Rust prevents this at compile time;
    /// Python has to check.
    fn ensure_unbuilt(&self) -> PyResult<()> {
        match self.agent {
            Some(_) => Err(runtime_error(
                "agent already built: configure before calling build()",
            )),
            None => Ok(()),
        }
    }

    /// Turn the collected configuration into an `Agent`.
    fn assemble(&self) -> PyResult<Agent> {
        let provider = self.provider.as_ref().ok_or_else(|| {
            runtime_error(
                "provider not set: use Agent.from_env(), or provider(Provider.from_env())",
            )
        })?;
        let model = self.model.as_ref().ok_or_else(|| {
            runtime_error(
                "model not set: use Agent.from_env(), model(name), or model(Model.from_env())",
            )
        })?;

        let mut builder = Agent::new().provider(provider.clone()).model(model.clone());

        if let Some(role) = &self.role {
            builder = builder.role(role.clone());
        }
        if let Some(label) = &self.label {
            builder = builder.label(label.clone());
        }
        if self.interactive {
            builder = builder.interactive();
        }
        for (key, value) in &self.templates {
            builder = builder.template(key.clone(), value.clone());
        }
        if let Some(dir) = &self.dir {
            builder = builder.dir(std::path::PathBuf::from(dir));
        }
        if let Some(store) = &self.knowledge {
            builder = builder.knowledge(store);
        }
        for tool in &self.tools {
            builder = builder.tool(BoxedTool(Arc::clone(tool)));
        }
        Ok(builder.build())
    }
}

#[pymethods]
impl PyAgent {
    #[new]
    fn new() -> Self {
        PyAgent::create()
    }

    /// Create an agent with the provider and model from the environment.
    #[staticmethod]
    fn from_env() -> PyResult<Self> {
        let mut agent = PyAgent::create();
        agent.provider = Some(Provider::from_env().map_err(runtime_error)?);
        agent.model = Some(Model::from_env().map_err(runtime_error)?);
        Ok(agent)
    }

    /// Define the LLM provider.
    fn provider<'py>(
        mut slf: PyRefMut<'py, Self>,
        provider: PyRef<'_, PyProvider>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.ensure_unbuilt()?;
        slf.provider = Some(provider.inner.clone());
        Ok(slf)
    }

    /// Set the model, by name or as a `Model` carrying a context window size
    /// and a reasoning level.
    fn model<'py>(
        mut slf: PyRefMut<'py, Self>,
        model: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.ensure_unbuilt()?;
        let resolved = if let Ok(name) = model.extract::<String>() {
            Model::from_name(name)
        } else {
            model.extract::<PyRef<PyModel>>()?.inner.clone()
        };
        slf.model = Some(resolved);
        Ok(slf)
    }

    fn role(mut slf: PyRefMut<'_, Self>, role: String) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.role = Some(role);
        Ok(slf)
    }

    /// Restrict the agent to tickets carrying this label, and name it after
    /// the label. Calling it twice replaces the label.
    fn label(mut slf: PyRefMut<'_, Self>, label: String) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.label = Some(label);
        Ok(slf)
    }

    /// The id the agent works under, once it is built.
    #[getter]
    fn id(&self) -> PyResult<&str> {
        Ok(self.built()?.get_id())
    }

    /// Let the agent wait for new instructions to keep a ticket in-progress.
    fn interactive(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.interactive = true;
        Ok(slf)
    }

    /// Inject data into prompts with template strings.
    ///
    /// `{key}` is replaced in the role and in any text task. Binding `context`
    /// replaces the built-in block the role expands, and binding one of its
    /// value names, such as `ticket` or `turns_remaining`, replaces that value.
    fn template(
        mut slf: PyRefMut<'_, Self>,
        key: String,
        value: String,
    ) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.templates.push((key, value));
        Ok(slf)
    }

    /// Inject more than one entry into prompts.
    fn templates(
        mut slf: PyRefMut<'_, Self>,
        variables: BTreeMap<String, String>,
    ) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.templates.extend(variables);
        Ok(slf)
    }

    /// Set the directory the agent has access to.
    fn dir(mut slf: PyRefMut<'_, Self>, dir: String) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.dir = Some(dir);
        Ok(slf)
    }

    /// Share a knowledge store, the durable memory the agent carries across
    /// tickets and shares with other agents.
    fn knowledge<'py>(
        mut slf: PyRefMut<'py, Self>,
        store: PyRef<'_, PyKnowledge>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.ensure_unbuilt()?;
        slf.knowledge = Some(Arc::clone(&store.inner));
        Ok(slf)
    }

    /// Register a tool the agent may call, either a built-in such as
    /// `ReadFileTool()` or a `@tool`-decorated function.
    fn tool<'py>(
        mut slf: PyRefMut<'py, Self>,
        tool: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.ensure_unbuilt()?;
        let resolved = extract_tool(tool)?;
        slf.tools.push(resolved);
        Ok(slf)
    }

    /// Register several tools the agent may call.
    fn tools<'py>(
        mut slf: PyRefMut<'py, Self>,
        tools: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.ensure_unbuilt()?;
        for tool in tools.try_iter()? {
            let resolved = extract_tool(&tool?)?;
            slf.tools.push(resolved);
        }
        Ok(slf)
    }

    /// Create the agent.
    ///
    /// It runs once: afterwards the configuration methods and a second `build()`
    /// are rejected. Raises when the LLM provider or the model is unset, or when
    /// neither can be read from the environment.
    fn build(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        let agent = slf.assemble()?;
        slf.agent = Some(agent);
        Ok(slf)
    }

    /// Submit a task and return its ticket key.
    ///
    /// Call it as often as you like: one agent can drive many tickets.
    fn task(&self, task: &Bound<'_, PyAny>) -> PyResult<String> {
        let value = py_to_value(task)?;
        Ok(self.built()?.task(value))
    }

    /// Submit a `Ticket` with custom labels or schema, and return its key.
    fn ticket(&self, ticket: PyRef<'_, PyTicket>) -> PyResult<String> {
        Ok(self.built()?.ticket(ticket.to_ticket()))
    }

    /// Begin processing tickets, and hand back the ticket queue so results,
    /// waiting, and cancellation stay one call away.
    ///
    /// An agent that was never built raises here.
    fn start(&self) -> PyResult<PyTicketQueue> {
        // The run spawns onto the ambient Tokio runtime, which a pymethod call
        // does not have entered on its own thread.
        let _guard = pyo3_async_runtimes::tokio::get_runtime().enter();
        Ok(PyTicketQueue {
            inner: self.built()?.start(),
        })
    }
}
