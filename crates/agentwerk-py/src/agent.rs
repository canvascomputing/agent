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

/// Which LLM provider the agent will use, decided at `build()`.
enum ProviderSpec {
    Unset,
    Env,
    Explicit(Arc<dyn Provider>),
}

/// Which model the agent will use, decided at `build()`.
enum ModelSpec {
    Unset,
    Env,
    Explicit(Model),
}

/// An `Agent` is the core entity of agentwerk. It has access to tools for
/// solving tasks in the form of tickets.
#[pyclass(name = "Agent")]
pub struct PyAgent {
    name: Option<String>,
    role: Option<String>,
    labels: Vec<String>,
    templates: Vec<(String, String)>,
    dir: Option<String>,
    interactive: bool,
    provider: ProviderSpec,
    model: ModelSpec,
    tools: Vec<Arc<dyn ToolLike>>,
    knowledge: Option<Arc<Knowledge>>,
    directive_editor: Option<Py<PyAny>>,
    /// True when the agent came from `Agent.empty()`, which registers no finish
    /// tool.
    empty: bool,
    /// Set by `build()`. Every method that reaches the queue needs it.
    agent: Option<Agent>,
}

impl PyAgent {
    fn create(empty: bool) -> Self {
        PyAgent {
            name: None,
            role: None,
            labels: Vec::new(),
            templates: Vec::new(),
            dir: None,
            interactive: false,
            provider: ProviderSpec::Unset,
            model: ModelSpec::Unset,
            tools: Vec::new(),
            knowledge: None,
            directive_editor: None,
            empty,
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
        let base = if self.empty {
            Agent::empty()
        } else {
            Agent::new()
        };

        let with_provider = match &self.provider {
            ProviderSpec::Env => {
                base.provider(agentwerk::providers::provider_from_env().map_err(runtime_error)?)
            }
            ProviderSpec::Explicit(provider) => base.provider(Arc::clone(provider)),
            ProviderSpec::Unset => {
                return Err(runtime_error(
                    "provider not set: call from_env(), provider_from_env(), or provider(...)",
                ))
            }
        };

        let mut builder = match &self.model {
            ModelSpec::Env => {
                with_provider.model(agentwerk::providers::model_from_env().map_err(runtime_error)?)
            }
            ModelSpec::Explicit(model) => with_provider.model(model.clone()),
            ModelSpec::Unset => {
                return Err(runtime_error(
                    "model not set: call model(...), model_from_env(), or from_env()",
                ))
            }
        };

        if let Some(name) = &self.name {
            builder = builder.name(name.clone());
        }
        if let Some(role) = &self.role {
            builder = builder.role(role.clone());
        }
        if !self.labels.is_empty() {
            builder = builder.labels(self.labels.clone());
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
        if let Some(editor) = &self.directive_editor {
            let editor = Python::attach(|py| editor.clone_ref(py));
            builder =
                builder.edit_directive_on_retry(move |detail: &str, directive: &mut String| {
                    Python::attach(|py| {
                        let Ok(result) = editor.bind(py).call1((detail, directive.as_str())) else {
                            return;
                        };
                        if let Ok(replacement) = result.extract::<String>() {
                            *directive = replacement;
                        }
                    })
                });
        }

        Ok(builder.build())
    }
}

#[pymethods]
impl PyAgent {
    #[new]
    fn new() -> Self {
        PyAgent::create(false)
    }

    /// Create an agent with no tools pre-registered.
    ///
    /// Register a finish tool yourself, or the agent cannot finish a ticket.
    #[staticmethod]
    fn empty() -> Self {
        PyAgent::create(true)
    }

    /// Read environment variables for configuration.
    fn from_env(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.provider = ProviderSpec::Env;
        slf.model = ModelSpec::Env;
        Ok(slf)
    }

    /// Read only the LLM provider from environment variables.
    fn provider_from_env(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.provider = ProviderSpec::Env;
        Ok(slf)
    }

    /// Define the LLM provider.
    fn provider<'py>(
        mut slf: PyRefMut<'py, Self>,
        provider: PyRef<'_, PyProvider>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.ensure_unbuilt()?;
        slf.provider = ProviderSpec::Explicit(Arc::clone(&provider.inner));
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
        slf.model = ModelSpec::Explicit(resolved);
        Ok(slf)
    }

    /// Read only the model from environment variables.
    fn model_from_env(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.model = ModelSpec::Env;
        Ok(slf)
    }

    fn name(mut slf: PyRefMut<'_, Self>, name: String) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.name = Some(name);
        Ok(slf)
    }

    fn role(mut slf: PyRefMut<'_, Self>, role: String) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.role = Some(role);
        Ok(slf)
    }

    fn label(mut slf: PyRefMut<'_, Self>, label: String) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.labels.push(label);
        Ok(slf)
    }

    fn labels(mut slf: PyRefMut<'_, Self>, labels: Vec<String>) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.labels.extend(labels);
        Ok(slf)
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
    /// replaces the built-in block the role expands.
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

    /// Override the prompt that corrects an agent asked to try again.
    ///
    /// Your function receives the reason and the built-in text, and returns the
    /// replacement, or `None` to keep it. It runs once per retry, so keep it
    /// cheap.
    fn edit_directive_on_retry(
        mut slf: PyRefMut<'_, Self>,
        editor: Py<PyAny>,
    ) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.directive_editor = Some(editor);
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

    /// Attach a built agent to a ticket queue, moving any tickets it queued on
    /// its own across first.
    fn ticket_queue<'py>(
        mut slf: PyRefMut<'py, Self>,
        queue: PyRef<'_, PyTicketQueue>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let rebound = slf.built()?.clone().ticket_queue(&queue.inner);
        slf.agent = Some(rebound);
        Ok(slf)
    }

    /// Begin processing tickets, and hand back the ticket queue.
    fn start(&self) -> PyResult<PyTicketQueue> {
        Ok(PyTicketQueue {
            inner: self.built()?.start(),
        })
    }

    /// Process every queued ticket, then hand back the ticket queue so results
    /// can be read from it. Awaitable.
    ///
    /// An agent that was never built raises when this is called, not when it is
    /// awaited.
    fn finish<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let agent = self.built()?.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let inner = agent.finish().await;
            Ok::<_, PyErr>(PyTicketQueue { inner })
        })
    }
}
