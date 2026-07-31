//! Compaction as Python sees it: why it ran, the ticket it belongs to, and the
//! built-in summarizer an editor can call for part of the replies.

use std::sync::Arc;

use agentwerk::agents::tickets::Reply;
use agentwerk::Compaction;
use pyo3::prelude::*;

use crate::convert::runtime_error;
use crate::reply::{py_to_replies, replies_to_py, PyReply};
use crate::ticket::PyTicket;

/// One compaction round-trip, handed to the editor installed with
/// `TicketQueue.edit_replies_on_compaction`.
#[pyclass(name = "Compaction")]
pub struct PyCompaction {
    inner: Arc<Compaction>,
}

#[pymethods]
impl PyCompaction {
    /// `"proactive"` ahead of an overflow, `"reactive"` after one.
    fn reason(&self) -> String {
        self.inner.reason().to_string()
    }

    /// The ticket being compacted, as it stood when compaction started.
    fn ticket(&self) -> PyTicket {
        PyTicket::from_ticket(self.inner.ticket())
    }

    /// The model's context window in tokens, or `None` when it is unknown.
    fn window(&self) -> Option<u64> {
        self.inner.window()
    }

    /// Summarize `replies` with the built-in compaction directive. Awaitable.
    fn summarize<'py>(
        &self,
        py: Python<'py>,
        replies: Vec<PyReply>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        let replies = into_replies(replies);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            inner.summarize(&replies).await.map_err(runtime_error)
        })
    }
}

fn into_replies(replies: Vec<PyReply>) -> Vec<Reply> {
    replies.into_iter().map(|reply| reply.inner).collect()
}

/// Call a Python compaction editor with the round-trip and the replies, and
/// read back the list it returns. `None` keeps the replies as they are.
///
/// The editor is expected to be `async def`, since that is what lets it await
/// `Compaction.summarize`. `asyncio.run` drives the coroutine here, which is
/// why the caller runs this on a blocking thread rather than an async worker.
pub(crate) fn invoke_editor(
    py: Python<'_>,
    editor: &Py<PyAny>,
    compaction: Compaction,
    replies: &[Reply],
) -> PyResult<Option<Vec<Reply>>> {
    let view = Py::new(
        py,
        PyCompaction {
            inner: Arc::new(compaction),
        },
    )?;
    let mut returned = editor.bind(py).call1((view, replies_to_py(replies)))?;
    let inspect = py.import("inspect")?;
    if inspect
        .call_method1("iscoroutine", (&returned,))?
        .is_truthy()?
    {
        returned = py.import("asyncio")?.call_method1("run", (returned,))?;
    }
    if returned.is_none() {
        return Ok(None);
    }
    Ok(Some(py_to_replies(&returned)?))
}
