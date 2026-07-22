//! The agent builder and the built agent, as Python sees them.
//!
//! The Rust builder is type-state: `AgentBuilder<P, M>` changes type as the
//! provider and model slots fill, which Python cannot hold across calls. So the
//! Python builder collects plain config and assembles the real typed builder in
//! one shot at `build()`.

use std::sync::Arc;

use agentwerk::providers::Provider;
use agentwerk::tools::ToolLike;
use agentwerk::Agent;
use pyo3::prelude::*;

use crate::convert::{py_to_value, runtime_error};
use crate::providers::PyProvider;
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
    Explicit(String),
}

/// Collects agent configuration; `build()` turns it into a real `Agent`.
#[pyclass(name = "Agent")]
pub struct PyAgentBuilder {
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
}

#[pymethods]
impl PyAgentBuilder {
    #[new]
    fn new() -> Self {
        PyAgentBuilder {
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
        }
    }

    /// Detect both provider and model from environment variables.
    fn from_env(mut slf: PyRefMut<'_, Self>) -> PyRefMut<'_, Self> {
        slf.provider = ProviderSpec::Env;
        slf.model = ModelSpec::Env;
        slf
    }

    /// Detect the provider from environment variables.
    fn provider_from_env(mut slf: PyRefMut<'_, Self>) -> PyRefMut<'_, Self> {
        slf.provider = ProviderSpec::Env;
        slf
    }

    /// Use an explicit provider handle.
    fn provider<'py>(
        mut slf: PyRefMut<'py, Self>,
        provider: PyRef<'_, PyProvider>,
    ) -> PyRefMut<'py, Self> {
        slf.provider = ProviderSpec::Explicit(Arc::clone(&provider.inner));
        slf
    }

    /// Set the model by name (e.g. `"gpt-4o"`).
    fn model(mut slf: PyRefMut<'_, Self>, model: String) -> PyRefMut<'_, Self> {
        slf.model = ModelSpec::Explicit(model);
        slf
    }

    /// Detect the model name from environment variables.
    fn model_from_env(mut slf: PyRefMut<'_, Self>) -> PyRefMut<'_, Self> {
        slf.model = ModelSpec::Env;
        slf
    }

    fn name(mut slf: PyRefMut<'_, Self>, name: String) -> PyRefMut<'_, Self> {
        slf.name = Some(name);
        slf
    }

    fn role(mut slf: PyRefMut<'_, Self>, role: String) -> PyRefMut<'_, Self> {
        slf.role = Some(role);
        slf
    }

    fn context(mut slf: PyRefMut<'_, Self>, context: String) -> PyRefMut<'_, Self> {
        slf.context = Some(context);
        slf
    }

    fn label(mut slf: PyRefMut<'_, Self>, label: String) -> PyRefMut<'_, Self> {
        slf.labels.push(label);
        slf
    }

    fn labels(mut slf: PyRefMut<'_, Self>, labels: Vec<String>) -> PyRefMut<'_, Self> {
        slf.labels.extend(labels);
        slf
    }

    /// Pause on assistant replies that carry no tool calls, for REPL hosts.
    fn interactive(mut slf: PyRefMut<'_, Self>) -> PyRefMut<'_, Self> {
        slf.interactive = true;
        slf
    }

    /// Bind `{key}` to `value` in the role, context, and string tasks.
    fn template_variable(
        mut slf: PyRefMut<'_, Self>,
        key: String,
        value: String,
    ) -> PyRefMut<'_, Self> {
        slf.template_variables.push((key, value));
        slf
    }

    /// Directory tools resolve filesystem paths against.
    fn dir(mut slf: PyRefMut<'_, Self>, dir: String) -> PyRefMut<'_, Self> {
        slf.dir = Some(dir);
        slf
    }

    /// Register a tool the agent may call: a built-in handle (e.g.
    /// `ReadFileTool()`) or, from M2, a `@tool`-decorated function.
    fn tool<'py>(
        mut slf: PyRefMut<'py, Self>,
        tool: &Bound<'_, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let resolved = extract_tool(tool)?;
        slf.tools.push(resolved);
        Ok(slf)
    }

    /// Assemble the real `Agent`. Errors if provider or model is unset, or if
    /// environment detection fails.
    fn build(&self) -> PyResult<PyAgent> {
        let base = Agent::new();

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
        for tool in &self.tools {
            builder = builder.tool(BoxedTool(Arc::clone(tool)));
        }

        Ok(PyAgent {
            inner: builder.build(),
        })
    }
}

/// A built agent bound to its private ticket system: enqueue with `task(...)`,
/// then drive with `finish()`.
#[pyclass(name = "BuiltAgent")]
pub struct PyAgent {
    pub(crate) inner: Agent,
}

#[pymethods]
impl PyAgent {
    /// Enqueue a task (any JSON-serializable value) and return its ticket key.
    fn task(&self, task: &Bound<'_, PyAny>) -> PyResult<String> {
        let value = py_to_value(task)?;
        Ok(self.inner.task(value))
    }

    /// Start the loop on a background task and return the bound system.
    fn start(&self) -> PyTicketSystem {
        PyTicketSystem {
            inner: self.inner.start(),
        }
    }

    /// Run every queued ticket to completion. Awaitable; returns the bound
    /// `TicketSystem` so results are read off it.
    fn finish<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let agent = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let inner = agent.finish().await;
            Ok::<_, PyErr>(PyTicketSystem { inner })
        })
    }
}
