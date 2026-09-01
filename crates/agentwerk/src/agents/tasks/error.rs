//! What can go wrong changing a task.

use std::fmt;

use super::task::Status;

/// Errors raised by task-store mutations.
#[derive(Debug)]
#[non_exhaustive]
pub enum TaskError {
    /// No task exists with `id`.
    TaskNotFound {
        /// Requested task ID.
        id: String,
    },
    /// Status transition `from → to` is not allowed.
    TransitionRejected {
        /// Current status.
        from: Status,
        /// Requested status.
        to: Status,
    },
    /// The result failed the task's schema. The message lists the
    /// violations.
    ResultRejected {
        /// Schema violation detail.
        message: String,
    },
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TaskNotFound { id } => write!(f, "Task {id} not found"),
            Self::TransitionRejected { from, to } => {
                write!(f, "Illegal transition {from:?} -> {to:?}")
            }
            Self::ResultRejected { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for TaskError {}
