//! Built-in tools exposed to Python, plus the extraction seam that turns a
//! Python-visible tool object back into an `Arc<dyn ToolLike>` the agent
//! builder can register.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use agentwerk::providers::ProviderResult;
use agentwerk::tools::{
    BashTool, CodegrepTool, EditFileTool, FetchUrlTool, GlobTool, GrepTool, ListDirectoryTool,
    ManageTicketsTool, ReadFileTool, ReadTicketsTool, ToolContext, ToolLike, ToolResult,
    WriteFileTool,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;

use crate::convert::{py_to_value, value_to_py};

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

/// A tool handle Python holds and passes to `Agent.tool(...)`. Every built-in
/// factory returns one; the extraction seam reads its inner trait object.
#[pyclass(name = "Tool")]
pub struct PyTool {
    pub inner: Arc<dyn ToolLike>,
}

/// Wraps a `@tool`-decorated Python callable as a first-class `ToolLike`. The
/// five sync methods read cached metadata; `call` runs the callable on a
/// blocking thread under the GIL so it never stalls the async executor.
struct PyToolAdapter {
    func: Py<PyAny>,
    name: String,
    description: String,
    schema: Value,
    read_only: bool,
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

/// Call the Python tool and turn its return value into a `ToolResult`. The tool
/// input object is passed as keyword arguments; a coroutine result is driven to
/// completion so async tool bodies work too.
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
    if let Ok(text) = result.extract::<String>() {
        return Ok(ToolResult::success(text));
    }
    let value = py_to_value(&result)?;
    Ok(ToolResult::success(value.to_string()))
}

/// Pull an `Arc<dyn ToolLike>` out of whatever Python passed to `.tool(...)`:
/// a built-in `Tool` handle, or a `@tool`-decorated callable.
pub fn extract_tool(obj: &Bound<'_, PyAny>) -> PyResult<Arc<dyn ToolLike>> {
    if let Ok(handle) = obj.extract::<PyRef<PyTool>>() {
        return Ok(Arc::clone(&handle.inner));
    }
    if obj.hasattr("_agentwerk_tool")? {
        let name = obj.getattr("_agentwerk_name")?.extract()?;
        let description = obj.getattr("_agentwerk_description")?.extract()?;
        let read_only = obj.getattr("_agentwerk_read_only")?.extract()?;
        let schema = py_to_value(&obj.getattr("_agentwerk_schema")?)?;
        return Ok(Arc::new(PyToolAdapter {
            func: obj.clone().unbind(),
            name,
            description,
            schema,
            read_only,
        }));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(
        "expected a tool: a built-in tool handle (e.g. ReadFileTool()) or a @tool-decorated function",
    ))
}

fn handle(inner: Arc<dyn ToolLike>) -> PyTool {
    PyTool { inner }
}

// --- built-in factories ---
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
#[pyo3(name = "CodegrepTool")]
fn codegrep_tool() -> PyTool {
    handle(Arc::new(CodegrepTool))
}

#[pyfunction]
#[pyo3(name = "FetchUrlTool")]
fn fetch_url_tool() -> PyTool {
    handle(Arc::new(FetchUrlTool))
}

#[pyfunction]
#[pyo3(name = "ReadTicketsTool")]
fn read_tickets_tool() -> PyTool {
    handle(Arc::new(ReadTicketsTool))
}

#[pyfunction]
#[pyo3(name = "ManageTicketsTool")]
fn manage_tickets_tool() -> PyTool {
    handle(Arc::new(ManageTicketsTool))
}

/// Shell tool restricted to commands matching `pattern` (e.g. `"git *"`).
#[pyfunction]
#[pyo3(name = "BashTool", signature = (name, pattern, description=None, read_only=false))]
fn bash_tool(name: &str, pattern: &str, description: Option<&str>, read_only: bool) -> PyTool {
    let mut tool = BashTool::new(name, pattern);
    if let Some(description) = description {
        tool = tool.description(description);
    }
    tool = tool.read_only(read_only);
    handle(Arc::new(tool))
}

/// Shell tool with no command restriction. Only for trusted inputs.
#[pyfunction]
#[pyo3(name = "UnrestrictedBashTool")]
fn unrestricted_bash_tool() -> PyTool {
    handle(Arc::new(BashTool::unrestricted()))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTool>()?;
    m.add_function(wrap_pyfunction!(read_file_tool, m)?)?;
    m.add_function(wrap_pyfunction!(write_file_tool, m)?)?;
    m.add_function(wrap_pyfunction!(edit_file_tool, m)?)?;
    m.add_function(wrap_pyfunction!(grep_tool, m)?)?;
    m.add_function(wrap_pyfunction!(glob_tool, m)?)?;
    m.add_function(wrap_pyfunction!(list_directory_tool, m)?)?;
    m.add_function(wrap_pyfunction!(codegrep_tool, m)?)?;
    m.add_function(wrap_pyfunction!(fetch_url_tool, m)?)?;
    m.add_function(wrap_pyfunction!(read_tickets_tool, m)?)?;
    m.add_function(wrap_pyfunction!(manage_tickets_tool, m)?)?;
    m.add_function(wrap_pyfunction!(bash_tool, m)?)?;
    m.add_function(wrap_pyfunction!(unrestricted_bash_tool, m)?)?;
    Ok(())
}
