//! The agent as Python sees it. One class in two phases: `Agent()` collects
//! configuration, `build()` assembles the real agent, and the methods that drive
//! a run work from there on.
//!
//! Rust splits the phases across two types, because `AgentBuilder<P, M>` changes
//! type as the provider and model slots fill and `build(self)` consumes it.
//! Python can hold neither, so the two collapse into the built type's name and
//! the config is assembled in one shot at `build()`. That collapse is why
//! `build()` runs once: the Python object owns the private ticket system, so
//! rebuilding would orphan the queue that clones already handed out still point
//! at.

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
use crate::ticket_system::PyTicketSystem;
use crate::tools::{extract_tool, BoxedTool};

/// Which provider the built agent should use. Resolved at `build()`.
enum ProviderSpec {
    Unset,
    Env,
    Explicit(Arc<dyn Provider>),
}

/// Which model the built agent should use. Resolved at `build()`.
enum ModelSpec {
    Unset,
    Env,
    Explicit(Model),
}

/// An agent: configured with the fluent methods, armed by `build()`, then
/// driven with `task(...)` and `finish()`.
#[pyclass(name = "Agent")]
pub struct PyAgent {
    name: Option<String>,
    role: Option<String>,
    context: Option<String>,
    labels: Vec<String>,
    template_variables: Vec<(String, String)>,
    dir: Option<String>,
    interactive: bool,
    provider: ProviderSpec,
    model: ModelSpec,
    tools: Vec<Arc<dyn ToolLike>>,
    knowledge: Option<Arc<Knowledge>>,
    directive_editor: Option<Py<PyAny>>,
    /// True when the agent came from `Agent.empty()`, which leaves the finish
    /// tool unregistered.
    empty: bool,
    /// Filled by `build()`. Every method that reaches the queue needs it.
    agent: Option<Agent>,
}

impl PyAgent {
    fn create(empty: bool) -> Self {
        PyAgent {
            name: None,
            role: None,
            context: None,
            labels: Vec::new(),
            template_variables: Vec::new(),
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

    /// The built agent, for callers that drive a run.
    pub(crate) fn built(&self) -> PyResult<&Agent> {
        self.agent
            .as_ref()
            .ok_or_else(|| runtime_error("agent not built: call build() first"))
    }

    /// Guards every configuration method. Rust gets this from `build(self)`
    /// consuming the builder; Python has to check.
    fn ensure_unbuilt(&self) -> PyResult<()> {
        match self.agent {
            Some(_) => Err(runtime_error(
                "agent already built: configure before calling build()",
            )),
            None => Ok(()),
        }
    }

    /// Turn the collected configuration into a real `Agent`.
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
        if let Some(context) = &self.context {
            builder = builder.context(context.clone());
        }
        if !self.labels.is_empty() {
            builder = builder.labels(self.labels.clone());
        }
        if self.interactive {
            builder = builder.interactive();
        }
        for (key, value) in &self.template_variables {
            builder = builder.template_variable(key.clone(), value.clone());
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
                builder.edit_directive_on_failure(move |detail: &str, directive: &mut String| {
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

    /// An agent with no finish tool pre-registered, for callers that compose the
    /// whole tool set themselves.
    #[staticmethod]
    fn empty() -> Self {
        PyAgent::create(true)
    }

    /// Detect both provider and model from environment variables.
    fn from_env(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.provider = ProviderSpec::Env;
        slf.model = ModelSpec::Env;
        Ok(slf)
    }

    /// Detect the provider from environment variables.
    fn provider_from_env(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.provider = ProviderSpec::Env;
        Ok(slf)
    }

    /// Use an explicit provider handle.
    fn provider<'py>(
        mut slf: PyRefMut<'py, Self>,
        provider: PyRef<'_, PyProvider>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.ensure_unbuilt()?;
        slf.provider = ProviderSpec::Explicit(Arc::clone(&provider.inner));
        Ok(slf)
    }

    /// Set the model, either by name (e.g. `"gpt-4o"`) or with a `Model`
    /// carrying context-window and reasoning-effort overrides.
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

    /// Detect the model name from environment variables.
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

    fn context(mut slf: PyRefMut<'_, Self>, context: String) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.context = Some(context);
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

    /// Pause on assistant replies that carry no tool calls, for REPL hosts.
    fn interactive(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.interactive = true;
        Ok(slf)
    }

    /// Bind `{key}` to `value` in the role, context, and string tasks.
    fn template_variable(
        mut slf: PyRefMut<'_, Self>,
        key: String,
        value: String,
    ) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.template_variables.push((key, value));
        Ok(slf)
    }

    /// Bind every `{key}` in the mapping at once.
    fn template_variables(
        mut slf: PyRefMut<'_, Self>,
        variables: BTreeMap<String, String>,
    ) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.template_variables.extend(variables);
        Ok(slf)
    }

    /// Directory tools resolve filesystem paths against.
    fn dir(mut slf: PyRefMut<'_, Self>, dir: String) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.dir = Some(dir);
        Ok(slf)
    }

    /// Share a knowledge store for durable, cross-ticket memory. Bind the same
    /// store to several agents to share their memory.
    fn knowledge<'py>(
        mut slf: PyRefMut<'py, Self>,
        store: PyRef<'_, PyKnowledge>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.ensure_unbuilt()?;
        slf.knowledge = Some(Arc::clone(&store.inner));
        Ok(slf)
    }

    /// Register a tool the agent may call: a built-in handle (e.g.
    /// `ReadFileTool()`) or a `@tool`-decorated function.
    fn tool<'py>(
        mut slf: PyRefMut<'py, Self>,
        tool: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.ensure_unbuilt()?;
        let resolved = extract_tool(tool)?;
        slf.tools.push(resolved);
        Ok(slf)
    }

    /// Register several tools at once, each of the shapes `tool(...)` accepts.
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

    /// Rewrite the corrective directive injected when a turn ends without an
    /// accepted result. The editor receives the bare reason and the default
    /// directive, and returns the replacement, or `None` to keep the default.
    /// Called inline per failure, so keep it cheap.
    fn edit_directive_on_failure(
        mut slf: PyRefMut<'_, Self>,
        editor: Py<PyAny>,
    ) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        slf.directive_editor = Some(editor);
        Ok(slf)
    }

    /// Arm the agent from the configuration collected so far. Runs once: after
    /// it, the configuration methods and a second `build()` are rejected. Errors
    /// if provider or model is unset, or if environment detection fails.
    fn build(mut slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        slf.ensure_unbuilt()?;
        let agent = slf.assemble()?;
        slf.agent = Some(agent);
        Ok(slf)
    }

    /// Enqueue a task (any JSON-serializable value) and return its ticket key.
    fn task(&self, task: &Bound<'_, PyAny>) -> PyResult<String> {
        let value = py_to_value(task)?;
        Ok(self.built()?.task(value))
    }

    /// Enqueue a fully-built ticket on the bound system and return its key.
    fn ticket(&self, ticket: PyRef<'_, PyTicket>) -> PyResult<String> {
        Ok(self.built()?.ticket(ticket.to_ticket()))
    }

    /// Bind the agent to a shared ticket system, draining any work it had
    /// queued privately into that system.
    fn ticket_system<'py>(
        mut slf: PyRefMut<'py, Self>,
        system: PyRef<'_, PyTicketSystem>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let rebound = slf.built()?.clone().ticket_system(&system.inner);
        slf.agent = Some(rebound);
        Ok(slf)
    }

    /// Start the loop on a background task and return the bound system.
    fn start(&self) -> PyResult<PyTicketSystem> {
        Ok(PyTicketSystem {
            inner: self.built()?.start(),
        })
    }

    /// Run every queued ticket to completion. Awaitable; returns the bound
    /// `TicketSystem` so results are read off it. Raises at call time, not on
    /// await, when the agent is not built.
    fn finish<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let agent = self.built()?.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let inner = agent.finish().await;
            Ok::<_, PyErr>(PyTicketSystem { inner })
        })
    }
}
