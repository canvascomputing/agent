//! The built-in tools as Python sees them, and how a Python tool object becomes
//! one the agent builder can register.

use std::sync::Arc;

use agentwerk::schemas::Schema;
use agentwerk::tools::{
    CommandTool, EditFileTool, FetchUrlTool, FinishTool, GlobTool, GrepTool, KnowledgeTool,
    ListDirectoryTool, ReadFileTool, TicketsTool, Tool, ToolContext, ToolResult, WriteFileTool,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;

use crate::convert::{py_to_value, value_to_py};
use crate::knowledge::PyKnowledge;

/// A tool handle Python passes to `Agent.tool(...)`. Every built-in returns
/// one.
#[pyclass(name = "Tool")]
pub struct PyTool {
    pub inner: Tool,
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
pub fn extract_tool(obj: &Bound<'_, PyAny>) -> PyResult<Tool> {
    if let Ok(handle) = obj.extract::<PyRef<PyTool>>() {
        return Ok(handle.inner.clone());
    }
    if let Ok(command) = obj.extract::<PyRef<PyCommandTool>>() {
        return Ok(command.inner.clone().into());
    }
    if obj.hasattr("_agentwerk_tool")? {
        let name: String = obj.getattr("_agentwerk_name")?.extract()?;
        let description: String = obj.getattr("_agentwerk_description")?.extract()?;
        let concurrent = obj.getattr("_agentwerk_concurrent")?.extract()?;
        let paths: Vec<String> = obj.getattr("_agentwerk_paths")?.extract()?;
        let document = py_to_value(&obj.getattr("_agentwerk_schema")?)?;
        // Reported here rather than absorbed: a schema that does not compile
        // would leave this tool checked against nothing, and the author is
        // right here to fix it. `Tool::schema` below cannot panic on a
        // document this compiled.
        Schema::new(document.clone()).map_err(|error| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "tool `{name}` declares a schema that does not compile: {error}"
            ))
        })?;
        let func = obj.clone().unbind();
        let tool = Tool::new(name, description, move |input: Value, _ctx: ToolContext| {
            let func = Python::attach(|py| func.clone_ref(py));
            async move {
                // Concurrent tool calls are spawned onto a multi-thread
                // runtime, so the GIL work must run on a blocking thread, not
                // the async worker.
                let outcome: Result<ToolResult, String> = tokio::task::spawn_blocking(move || {
                    Python::attach(|py| invoke_python(py, &func, &input).map_err(|e| e.to_string()))
                })
                .await
                .unwrap_or_else(|join| Err(format!("tool thread panicked: {join}")));
                // A Python exception is a recoverable failure shown back to
                // the model, not a hard error that stops the run.
                match outcome {
                    Ok(result) => result,
                    Err(message) => ToolResult::error(message),
                }
            }
        })
        .schema(document)
        .concurrent(concurrent)
        .paths(paths);
        return Ok(tool);
    }
    Err(pyo3::exceptions::PyTypeError::new_err(
        "expected a tool: a built-in tool handle (e.g. ReadFileTool()) or a @tool-decorated function",
    ))
}

fn handle(inner: impl Into<Tool>) -> PyTool {
    PyTool {
        inner: inner.into(),
    }
}

// Built-in factories
// Each reads as a constructor at the Python call site (e.g. `ReadFileTool()`).

#[pyfunction]
#[pyo3(name = "ReadFileTool")]
fn read_file_tool() -> PyTool {
    handle(ReadFileTool)
}

#[pyfunction]
#[pyo3(name = "WriteFileTool")]
fn write_file_tool() -> PyTool {
    handle(WriteFileTool)
}

#[pyfunction]
#[pyo3(name = "EditFileTool")]
fn edit_file_tool() -> PyTool {
    handle(EditFileTool)
}

/// Search file contents by regular expression, or by code shape with
/// `syntax="code"`.
#[pyfunction]
#[pyo3(name = "GrepTool")]
fn grep_tool() -> PyTool {
    handle(GrepTool)
}

#[pyfunction]
#[pyo3(name = "GlobTool")]
fn glob_tool() -> PyTool {
    handle(GlobTool)
}

#[pyfunction]
#[pyo3(name = "ListDirectoryTool")]
fn list_directory_tool() -> PyTool {
    handle(ListDirectoryTool)
}

#[pyfunction]
#[pyo3(name = "FetchUrlTool")]
fn fetch_url_tool() -> PyTool {
    handle(FetchUrlTool)
}

/// Point the knowledge tool at `store` without making it the agent's own.
///
/// `Agent.knowledge(store)` is the usual route, and also shows the store's index
/// in the prompt.
#[pyfunction]
#[pyo3(name = "KnowledgeTool")]
fn knowledge_tool(store: PyRef<'_, PyKnowledge>) -> PyTool {
    handle(KnowledgeTool::new(Arc::clone(&store.inner)))
}

/// Write the result for the current ticket and mark it finished, handing work
/// on to a child ticket when needed. Registered on every agent.
#[pyfunction]
#[pyo3(name = "FinishTool")]
fn finish_tool() -> PyTool {
    handle(FinishTool)
}

#[pyfunction]
#[pyo3(name = "TicketsTool")]
fn tickets_tool() -> PyTool {
    handle(TicketsTool)
}

/// Run a command the model calls by `name`, passed to `Agent.tool(...)`.
/// Until an `allow` pattern widens it, only the bare `name` runs.
#[pyclass(name = "CommandTool")]
pub struct PyCommandTool {
    inner: CommandTool,
}

#[pymethods]
impl PyCommandTool {
    #[new]
    fn new(name: &str) -> Self {
        PyCommandTool {
            inner: CommandTool::new(name),
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

    /// Refuse commands carrying `flag`, even when an allowed pattern matches
    /// them. `--force` also catches `--force=x`, and `-f` also catches `-rf`.
    fn deny_flag<'py>(mut slf: PyRefMut<'py, Self>, flag: &str) -> PyRefMut<'py, Self> {
        slf.inner = slf.inner.clone().deny_flag(flag);
        slf
    }

    /// Override the auto-generated description.
    fn description<'py>(mut slf: PyRefMut<'py, Self>, description: &str) -> PyRefMut<'py, Self> {
        slf.inner = slf.inner.clone().description(description);
        slf
    }

    /// Run this tool in parallel with the turn's other concurrent calls. Set it
    /// for a tool with no side effects.
    fn concurrent<'py>(mut slf: PyRefMut<'py, Self>, concurrent: bool) -> PyRefMut<'py, Self> {
        slf.inner = slf.inner.clone().concurrent(concurrent);
        slf
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTool>()?;
    m.add_class::<PyCommandTool>()?;
    m.add_class::<PyToolResult>()?;
    m.add_function(wrap_pyfunction!(read_file_tool, m)?)?;
    m.add_function(wrap_pyfunction!(write_file_tool, m)?)?;
    m.add_function(wrap_pyfunction!(edit_file_tool, m)?)?;
    m.add_function(wrap_pyfunction!(grep_tool, m)?)?;
    m.add_function(wrap_pyfunction!(glob_tool, m)?)?;
    m.add_function(wrap_pyfunction!(list_directory_tool, m)?)?;
    m.add_function(wrap_pyfunction!(fetch_url_tool, m)?)?;
    m.add_function(wrap_pyfunction!(knowledge_tool, m)?)?;
    m.add_function(wrap_pyfunction!(finish_tool, m)?)?;
    m.add_function(wrap_pyfunction!(tickets_tool, m)?)?;
    Ok(())
}
