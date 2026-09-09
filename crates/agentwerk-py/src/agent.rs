//! Exposes Rust agents through Python with the same task behavior.
//!
//! Rust configures through methods that consume and return the agent, which is
//! why the agent sits in an `Option` here: a setter takes it out and puts the
//! returned one back.

use std::collections::BTreeMap;
use std::sync::Arc;

use agentwerk::providers::{Model, Provider};
use agentwerk::{Agent, Knowledge};
use pyo3::prelude::*;

use crate::convert::{py_to_templates, runtime_error, value_to_py};
use crate::knowledge::PyKnowledge;
use crate::providers::{PyModel, PyProvider};
use crate::query::to_task_matcher;
use crate::task::to_task;
use crate::tools::extract_tool;
use crate::werk::PyWerk;

/// Use an LLM and registered tools to complete tasks claimed from a `Werk`.
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
    fn role(mut slf: PyRefMut<'_, Self>, role: String) -> PyRefMut<'_, Self> {
        slf.set(|agent| agent.role(role));
        slf
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

    /// Insert or replace a shared template through this agent's Werk.
    ///
    /// New tasks use the value before their first request; inserted values stay literal.
    fn template(mut slf: PyRefMut<'_, Self>, key: String, value: String) -> PyRefMut<'_, Self> {
        slf.set(|agent| agent.template(key, value));
        slf
    }

    /// Insert or replace shared templates together through the agent's Werk.
    fn templates<'py>(
        mut slf: PyRefMut<'py, Self>,
        variables: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let variables = py_to_templates(variables)?;
        slf.set(|agent| agent.templates(variables));
        Ok(slf)
    }

    /// Set the directory the agent has access to.
    fn dir(mut slf: PyRefMut<'_, Self>, dir: String) -> PyRefMut<'_, Self> {
        slf.set(|agent| agent.dir(dir));
        slf
    }

    /// Share a knowledge store and register its knowledge tool.
    fn knowledge<'py>(
        mut slf: PyRefMut<'py, Self>,
        store: PyRef<'_, PyKnowledge>,
    ) -> PyRefMut<'py, Self> {
        let store: Arc<Knowledge> = Arc::clone(&store.inner);
        slf.set(|agent| agent.knowledge(&store));
        slf
    }

    /// Override one model-facing directive or application-event acknowledgement.
    fn directive(mut slf: PyRefMut<'_, Self>, key: String, template: String) -> PyRefMut<'_, Self> {
        slf.set(|agent| agent.directive(key, template));
        slf
    }

    /// Override several model-facing directives.
    fn directives(
        mut slf: PyRefMut<'_, Self>,
        overrides: BTreeMap<String, String>,
    ) -> PyRefMut<'_, Self> {
        slf.set(|agent| agent.directives(overrides));
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
    /// A `str` is the task itself. A `Task` carries a custom label or schema
    /// with it. Call it as often as you like: one agent can work on many tasks.
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

    /// Wait for matching tasks in the bound Werk, starting automatically. Awaitable.
    fn finish_tasks<'py>(
        &self,
        py: Python<'py>,
        matches: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.ready()?.clone();
        let query = to_task_matcher(py, &matches)?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let results = inner.finish_tasks(query).await;
            Python::attach(|py| {
                results
                    .iter()
                    .map(|value| Ok(value_to_py(py, value)?.unbind()))
                    .collect::<PyResult<Vec<_>>>()
            })
        })
    }

    /// Wait for every task to be done, then give back every result in
    /// creation order. Awaitable.
    fn finish<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.ready()?.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let results = inner.finish().await;
            Python::attach(|py| {
                results
                    .iter()
                    .map(|value| Ok(value_to_py(py, value)?.unbind()))
                    .collect::<PyResult<Vec<_>>>()
            })
        })
    }

    /// Wait for the matching tasks to be done, then give back the first result
    /// in query order. `None` means no matching task finished with a result.
    /// Awaitable.
    fn finish_task<'py>(&self, py: Python<'py>, matches: Py<PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.ready()?.clone();
        let query = to_task_matcher(py, &matches)?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = inner.finish_task(query).await;
            Python::attach(|py| {
                result
                    .map(|value| Ok(value_to_py(py, &value)?.unbind()))
                    .transpose()
            })
        })
    }
}
