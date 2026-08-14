//! The built-in tools as Python sees them, and how a Python tool object becomes
//! one the agent builder can register.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use agentwerk::providers::ProviderResult;
use agentwerk::tools::{
    BashTool, EditFileTool, FetchUrlTool, FindToolsTool, FinishTool, GlobTool, GrepTool,
    KnowledgeTool, ListDirectoryTool, ReadFileTool, TicketsTool, ToolContext, ToolLike, ToolResult,
    WriteFileTool,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;

use crate::convert::{py_to_value, value_to_py};
use crate::knowledge::PyKnowledge;

/// Forwards `ToolLike` through an already-`Arc`'d trait object. The agent
/// builder's `.tool(...)` takes `impl ToolLike` by value and re-wraps it in an
/// `Arc`; a Python builder collects heterogeneous tools as `Arc<dyn ToolLike>`,
/// so this newtype lets each one pass back through that by-value gate.
pub struct BoxedTool(pub Arc<dyn ToolLike>);

impl ToolLike for BoxedTool {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn description(&self) -> &str {
        self.0.description()
    }

    fn input_schema(&self) -> Value {
        self.0.input_schema()
    }

    fn is_read_only(&self) -> bool {
        self.0.is_read_only()
    }

    fn should_defer(&self) -> bool {
        self.0.should_defer()
    }

    fn opened_paths(&self, input: &Value) -> Vec<String> {
        self.0.opened_paths(input)
    }

    fn call<'a>(
        &'a self,
        input: Value,
        ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>> {
        self.0.call(input, ctx)
    }
}

/// A tool handle Python passes to `Agent.tool(...)`. Every built-in returns
/// one.
#[pyclass(name = "Tool")]
pub struct PyTool {
    pub inner: Arc<dyn ToolLike>,
}

/// Turns a `@tool`-decorated Python function into a tool agentwerk can call.
///
/// The name, description, and schema are read once and kept. The function
/// itself runs on its own thread, so it never holds up the rest of the work.
struct PyToolAdapter {
    func: Py<PyAny>,
    name: String,
    description: String,
    schema: Value,
    read_only: bool,
    defer: bool,
    paths: Vec<String>,
}

impl ToolLike for PyToolAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.schema.clone()
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn should_defer(&self) -> bool {
        self.defer
    }

    fn opened_paths(&self, input: &Value) -> Vec<String> {
        self.paths
            .iter()
            .filter_map(|field| input.get(field).and_then(|v| v.as_str()))
            .map(str::to_string)
            .collect()
    }

    fn call<'a>(
        &'a self,
        input: Value,
        _ctx: &'a ToolContext,
    ) -> Pin<Box<dyn Future<Output = ProviderResult<ToolResult>> + Send + 'a>> {
        let func = Python::attach(|py| self.func.clone_ref(py));
        Box::pin(async move {
            // Read-only tool calls are spawned onto a multi-thread runtime, so
            // the GIL work must run on a blocking thread, not the async worker.
            let outcome: Result<ToolResult, String> = tokio::task::spawn_blocking(move || {
                Python::attach(|py| invoke_python(py, &func, &input).map_err(|e| e.to_string()))
            })
            .await
            .unwrap_or_else(|join| Err(format!("tool thread panicked: {join}")));
            // A Python exception is a recoverable failure shown back to the
            // model, not a hard `Err` that stops the run.
            Ok(match outcome {
                Ok(result) => result,
                Err(message) => ToolResult::error(message),
            })
        })
    }
}

/// Call the Python tool and turn what it returns into a `ToolResult`. The input
/// arrives as keyword arguments, and an async function is awaited.
fn invoke_python(py: Python<'_>, func: &Py<PyAny>, input: &Value) -> PyResult<ToolResult> {
    let arg = value_to_py(py, input)?;
    let bound = func.bind(py);
    let mut result = match arg.cast::<PyDict>() {
        Ok(kwargs) => bound.call((), Some(&kwargs))?,
        Err(_) => bound.call1((arg,))?,
    };

    let inspect = py.import("inspect")?;
    if inspect
        .call_method1("iscoroutine", (&result,))?
        .is_truthy()?
    {
        let asyncio = py.import("asyncio")?;
        result = asyncio.call_method1("run", (result,))?;
    }

    if result.is_none() {
        return Ok(ToolResult::success(String::new()));
    }
    if let Ok(explicit) = result.extract::<PyRef<PyToolResult>>() {
        return Ok(explicit.inner.clone());
    }
    if let Ok(text) = result.extract::<String>() {
        return Ok(ToolResult::success(text));
    }
    let value = py_to_value(&result)?;
    Ok(ToolResult::success(value.to_string()))
}

/// What a tool reports back when a bare return value is not enough. Returning a
/// plain string or dict is the same as `ToolResult.success(...)`.
#[pyclass(name = "ToolResult")]
pub struct PyToolResult {
    inner: ToolResult,
}

#[pymethods]
impl PyToolResult {
    /// The tool did its work, and `content` goes back to the model.
    #[staticmethod]
    fn success(content: String) -> Self {
        PyToolResult {
            inner: ToolResult::success(content),
        }
    }

    /// The tool failed, and `content` says why so the model can work around it.
    #[staticmethod]
    fn error(content: String) -> Self {
        PyToolResult {
            inner: ToolResult::error(content),
        }
    }

    /// The input was malformed. It counts against `max_schema_retries`.
    #[staticmethod]
    fn schema_error(content: String) -> Self {
        PyToolResult {
            inner: ToolResult::schema_error(content),
        }
    }
}

/// Read a usable tool out of whatever Python passed to `.tool(...)`: a built-in
/// handle, or a `@tool`-decorated function.
pub fn extract_tool(obj: &Bound<'_, PyAny>) -> PyResult<Arc<dyn ToolLike>> {
    if let Ok(handle) = obj.extract::<PyRef<PyTool>>() {
        return Ok(Arc::clone(&handle.inner));
    }
    if let Ok(bash) = obj.extract::<PyRef<PyBashTool>>() {
        return Ok(Arc::new(bash.inner.clone()));
    }
    if obj.hasattr("_agentwerk_tool")? {
        let name = obj.getattr("_agentwerk_name")?.extract()?;
        let description = obj.getattr("_agentwerk_description")?.extract()?;
        let read_only = obj.getattr("_agentwerk_read_only")?.extract()?;
        let defer = obj.getattr("_agentwerk_defer")?.extract()?;
        let paths = obj.getattr("_agentwerk_paths")?.extract()?;
        let schema = py_to_value(&obj.getattr("_agentwerk_schema")?)?;
        return Ok(Arc::new(PyToolAdapter {
            func: obj.clone().unbind(),
            name,
            description,
            schema,
            read_only,
            defer,
            paths,
        }));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(
        "expected a tool: a built-in tool handle (e.g. ReadFileTool()) or a @tool-decorated function",
    ))
}

fn handle(inner: Arc<dyn ToolLike>) -> PyTool {
    PyTool { inner }
}

// Built-in factories
// Each reads as a constructor at the Python call site (e.g. `ReadFileTool()`).

#[pyfunction]
#[pyo3(name = "ReadFileTool")]
fn read_file_tool() -> PyTool {
    handle(Arc::new(ReadFileTool))
}

#[pyfunction]
#[pyo3(name = "WriteFileTool")]
fn write_file_tool() -> PyTool {
    handle(Arc::new(WriteFileTool))
}

#[pyfunction]
#[pyo3(name = "EditFileTool")]
fn edit_file_tool() -> PyTool {
    handle(Arc::new(EditFileTool))
}

/// Search file contents by regular expression, or by code shape with
/// `syntax="code"`.
#[pyfunction]
#[pyo3(name = "GrepTool")]
fn grep_tool() -> PyTool {
    handle(Arc::new(GrepTool))
}

#[pyfunction]
#[pyo3(name = "GlobTool")]
fn glob_tool() -> PyTool {
    handle(Arc::new(GlobTool))
}

#[pyfunction]
#[pyo3(name = "ListDirectoryTool")]
fn list_directory_tool() -> PyTool {
    handle(Arc::new(ListDirectoryTool))
}

#[pyfunction]
#[pyo3(name = "FetchUrlTool")]
fn fetch_url_tool() -> PyTool {
    handle(Arc::new(FetchUrlTool))
}

/// Look up the tools held back until they are needed.
#[pyfunction]
#[pyo3(name = "FindToolsTool")]
fn find_tools_tool() -> PyTool {
    handle(Arc::new(FindToolsTool))
}

/// Point the knowledge tool at `store` without making it the agent's own.
///
/// `Agent.knowledge(store)` is the usual route, and also shows the store's index
/// in the prompt.
#[pyfunction]
#[pyo3(name = "KnowledgeTool")]
fn knowledge_tool(store: PyRef<'_, PyKnowledge>) -> PyTool {
    handle(Arc::new(KnowledgeTool::new(Arc::clone(&store.inner))))
}

/// Write the result for the current ticket and mark it finished, handing work
/// on to a child ticket when needed. Registered on every agent.
#[pyfunction]
#[pyo3(name = "FinishTool")]
fn finish_tool() -> PyTool {
    handle(Arc::new(FinishTool))
}

#[pyfunction]
#[pyo3(name = "TicketsTool")]
fn tickets_tool() -> PyTool {
    handle(Arc::new(TicketsTool))
}

/// Run a shell command the model calls by `name`, passed to `Agent.tool(...)`.
/// Until an `allow` pattern widens it, only the bare `name` runs.
#[pyclass(name = "BashTool")]
pub struct PyBashTool {
    inner: BashTool,
}

#[pymethods]
impl PyBashTool {
    #[new]
    fn new(name: &str) -> Self {
        PyBashTool {
            inner: BashTool::new(name),
        }
    }

    /// Permit commands matching `pattern`. The first call replaces the
    /// bare-name default, so what is listed is what runs.
    fn allow<'py>(mut slf: PyRefMut<'py, Self>, pattern: &str) -> PyRefMut<'py, Self> {
        slf.inner = slf.inner.clone().allow(pattern);
        slf
    }

    /// Refuse commands matching `pattern`, even when an allowed pattern
    /// matches them too.
    fn deny<'py>(mut slf: PyRefMut<'py, Self>, pattern: &str) -> PyRefMut<'py, Self> {
        slf.inner = slf.inner.clone().deny(pattern);
        slf
    }

    /// Override the auto-generated description.
    fn description<'py>(mut slf: PyRefMut<'py, Self>, description: &str) -> PyRefMut<'py, Self> {
        slf.inner = slf.inner.clone().description(description);
        slf
    }

    /// Set whether this tool is considered read-only.
    fn read_only<'py>(mut slf: PyRefMut<'py, Self>, read_only: bool) -> PyRefMut<'py, Self> {
        slf.inner = slf.inner.clone().read_only(read_only);
        slf
    }
}

/// Run any shell command. Only for input you trust.
#[pyfunction]
#[pyo3(name = "UnrestrictedBashTool")]
fn unrestricted_bash_tool() -> PyBashTool {
    PyBashTool {
        inner: BashTool::unrestricted(),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTool>()?;
    m.add_class::<PyBashTool>()?;
    m.add_class::<PyToolResult>()?;
    m.add_function(wrap_pyfunction!(read_file_tool, m)?)?;
    m.add_function(wrap_pyfunction!(write_file_tool, m)?)?;
    m.add_function(wrap_pyfunction!(edit_file_tool, m)?)?;
    m.add_function(wrap_pyfunction!(grep_tool, m)?)?;
    m.add_function(wrap_pyfunction!(glob_tool, m)?)?;
    m.add_function(wrap_pyfunction!(list_directory_tool, m)?)?;
    m.add_function(wrap_pyfunction!(fetch_url_tool, m)?)?;
    m.add_function(wrap_pyfunction!(find_tools_tool, m)?)?;
    m.add_function(wrap_pyfunction!(knowledge_tool, m)?)?;
    m.add_function(wrap_pyfunction!(finish_tool, m)?)?;
    m.add_function(wrap_pyfunction!(tickets_tool, m)?)?;
    m.add_function(wrap_pyfunction!(unrestricted_bash_tool, m)?)?;
    Ok(())
}
