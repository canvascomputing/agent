//! Replies as Python sees them. `Reply` carries its author, timestamp, and a
//! list of `ReplyContent` blocks; each block is flattened to a `kind` plus a
//! `data` dict, the same shape `Event` uses for a payload-carrying variant.

use agentwerk::agents::tasks::{Reply, ReplyContent};
use pyo3::prelude::*;

use serde_json::Value;

use crate::convert::{py_to_value, runtime_error, value_to_py};

/// One entry in a task's replies, which an editor hands back.
#[pyclass(name = "Reply", from_py_object)]
#[derive(Clone)]
pub struct PyReply {
    pub(crate) inner: Reply,
}

#[pymethods]
impl PyReply {
    /// A reply carrying one block of text.
    #[staticmethod]
    fn user_text(text: &str) -> Self {
        PyReply {
            inner: Reply::user_text(text),
        }
    }

    /// `"system"`, `"user"`, or `"assistant"`.
    fn get_author(&self) -> String {
        self.inner.get_author().get_name().into()
    }

    fn get_content(&self) -> Vec<PyReplyContent> {
        self.inner
            .get_content()
            .iter()
            .cloned()
            .map(|inner| PyReplyContent { inner })
            .collect()
    }

    /// Milliseconds since the epoch, zero on a reply built here until it is
    /// stored.
    fn get_created_at(&self) -> u64 {
        self.inner.get_created_at()
    }

    fn __repr__(&self) -> String {
        format!(
            "Reply(author={:?}, content={})",
            self.get_author(),
            self.inner.get_content().len()
        )
    }
}

/// One block inside a reply: text, a tool call, a tool result, or the model's
/// reasoning.
#[pyclass(name = "ReplyContent", skip_from_py_object)]
#[derive(Clone)]
pub struct PyReplyContent {
    pub(crate) inner: ReplyContent,
}

#[pymethods]
impl PyReplyContent {
    #[staticmethod]
    fn text(text: &str) -> Self {
        PyReplyContent {
            inner: ReplyContent::Text { text: text.into() },
        }
    }

    #[staticmethod]
    fn tool_use(id: &str, name: &str, input: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(PyReplyContent {
            inner: ReplyContent::ToolUse {
                id: id.into(),
                name: name.into(),
                input: py_to_value(input)?,
            },
        })
    }

    #[staticmethod]
    #[pyo3(signature = (tool_use_id, content, succeeded=true, path=None))]
    fn tool_result(tool_use_id: &str, content: &str, succeeded: bool, path: Option<&str>) -> Self {
        PyReplyContent {
            inner: ReplyContent::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
                succeeded,
                path: path.map(Into::into),
            },
        }
    }

    #[staticmethod]
    fn thinking(thinking: &str, signature: &str) -> Self {
        PyReplyContent {
            inner: ReplyContent::Thinking {
                thinking: thinking.into(),
                signature: signature.into(),
            },
        }
    }

    #[staticmethod]
    fn redacted_thinking(data: &str) -> Self {
        PyReplyContent {
            inner: ReplyContent::RedactedThinking { data: data.into() },
        }
    }

    /// The block's tag: `"text"`, `"tool_use"`, `"tool_result"`,
    /// `"thinking"`, or `"redacted_thinking"`.
    fn get_kind(&self) -> &'static str {
        self.inner.get_kind()
    }

    /// The block's fields as a dict, without `kind`.
    fn get_data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let mut value = serde_json::to_value(&self.inner).map_err(runtime_error)?;
        if let Value::Object(fields) = &mut value {
            fields.remove("type");
        }
        value_to_py(py, &value)
    }

    fn __repr__(&self) -> String {
        format!("ReplyContent(kind={:?})", self.get_kind())
    }
}

/// Hand the replies to Python for an editor to read.
pub(crate) fn replies_to_py(replies: &[Reply]) -> Vec<PyReply> {
    replies
        .iter()
        .cloned()
        .map(|inner| PyReply { inner })
        .collect()
}

/// Read an editor's returned list back into replies.
///
/// It takes the `Reply` objects the editor was handed, so a dict built by hand
/// is rejected by name rather than failing on a missing field.
pub(crate) fn py_to_replies(obj: &Bound<'_, PyAny>) -> PyResult<Vec<Reply>> {
    let replies: Vec<PyReply> = obj.extract().map_err(|_| {
        runtime_error(
            "a reply editor must return a list of Reply objects, or None to keep the current ones",
        )
    })?;
    Ok(replies.into_iter().map(|reply| reply.inner).collect())
}
