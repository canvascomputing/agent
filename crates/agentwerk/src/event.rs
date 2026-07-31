//! Insights into the lifecycle and activities of an agent's work.

use std::fmt;
use std::sync::Arc;

use crate::providers::{RequestErrorKind, TokenUsage};

/// Why the older messages were summarized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactReason {
    /// The next request was estimated to be too long for the model, ahead of
    /// any failure.
    Proactive,
    /// The LLM provider reported the context window exceeded.
    Reactive,
}

impl fmt::Display for CompactReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CompactReason::Proactive => "proactive",
            CompactReason::Reactive => "reactive",
        })
    }
}

/// Which limit a [`EventKind::PolicyViolated`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyKind {
    /// `max_turns`: the turn limit across all agents.
    Turns,
    /// `max_input_tokens`: the total input-token limit.
    InputTokens,
    /// `max_output_tokens`: the total output-token limit.
    OutputTokens,
    /// `max_schema_retries`: consecutive schema failures on one ticket. The
    /// count resets after every result that validates.
    MaxSchemaRetries,
    /// `max_time`: the elapsed-duration limit. The matching event reports its
    /// `limit` in milliseconds.
    Time,
}

impl fmt::Display for PolicyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PolicyKind::Turns => "turns",
            PolicyKind::InputTokens => "input_tokens",
            PolicyKind::OutputTokens => "output_tokens",
            PolicyKind::MaxSchemaRetries => "max_schema_retries",
            PolicyKind::Time => "time",
        })
    }
}

/// Why execution ended.
///
/// Carried by [`EventKind::RunFinished`] and readable after `finish().await`
/// through `TicketSystem::finish_reason()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// The queue emptied; nothing more to do.
    Drained,
    /// A limit was breached.
    PolicyViolated(PolicyKind),
    /// Cancellation was requested through `cancel()`, `cancel_on`, or
    /// `cancel_on_event`.
    Cancelled,
}

impl fmt::Display for FinishReason {
    /// The violated limit is named inside the parentheses, as in
    /// `policy_violated(turns)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FinishReason::Drained => f.write_str("drained"),
            FinishReason::PolicyViolated(kind) => write!(f, "policy_violated({kind})"),
            FinishReason::Cancelled => f.write_str("cancelled"),
        }
    }
}

/// How a tool call failed, carried by [`EventKind::ToolCallFailed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFailureKind {
    /// No tool of that name is registered.
    ToolNotFound,
    /// The tool ran and returned an error.
    ExecutionFailed,
    /// The tool rejected its input. Counted against `max_schema_retries`.
    SchemaValidationFailed,
}

impl fmt::Display for ToolFailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ToolFailureKind::ToolNotFound => "tool_not_found",
            ToolFailureKind::ExecutionFailed => "execution_failed",
            ToolFailureKind::SchemaValidationFailed => "schema_validation_failed",
        })
    }
}

/// What was done to a knowledge page, carried by [`EventKind::KnowledgeUsed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeOp {
    Write,
    Read,
    Remove,
    List,
}

impl fmt::Display for KnowledgeOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KnowledgeOp::Write => "write",
            KnowledgeOp::Read => "read",
            KnowledgeOp::Remove => "remove",
            KnowledgeOp::List => "list",
        })
    }
}

/// An `Event` reports one thing that happened as agents work. It names the
/// agent that produced it, the ticket it concerns, and what happened.
///
/// ```no_run
/// use agentwerk::TicketSystem;
/// use agentwerk::event::EventKind;
///
/// # async fn run() {
/// let tickets = TicketSystem::new();
/// tickets.on_event(|event| {
///     if let EventKind::TicketFinished = &event.kind {
///         eprintln!("[{}] done {}", event.agent_name, event.ticket_key);
///     }
/// });
/// tickets.finish().await;
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Event {
    /// Name of the agent that produced this event.
    pub agent_name: String,
    /// Key of the ticket this event concerns. Empty on `RunStarted` and
    /// `RunFinished`, which no ticket owns.
    pub ticket_key: String,
    /// What happened.
    pub kind: EventKind,
}

impl Event {
    pub(crate) fn new(
        agent_name: impl Into<String>,
        ticket_key: impl Into<String>,
        kind: EventKind,
    ) -> Self {
        Self {
            agent_name: agent_name.into(),
            ticket_key: ticket_key.into(),
            kind,
        }
    }
}

/// What an [`Event`] reports.
///
/// Most kinds name the agent they came from on the wrapping [`Event`].
/// `RunStarted` and `RunFinished` come from the `TicketSystem` itself and
/// arrive with an empty `agent_name`, as does `TicketFailed` when the host
/// fails a ticket through `TicketSystem::set_failed`.
#[derive(Debug, Clone)]
pub enum EventKind {
    /// Execution began.
    RunStarted,
    /// Execution ended, carrying the reason.
    RunFinished { reason: FinishReason },
    /// An agent claimed a ticket.
    TicketStarted,
    /// A ticket finished successfully.
    TicketFinished,
    /// A ticket failed.
    TicketFailed,
    /// The agent began another turn on its ticket.
    TurnStarted,
    /// A request went out to the model.
    RequestStarted { model: String },
    /// A request finished and reported its token usage.
    RequestFinished { model: String, usage: TokenUsage },
    /// A request failed and was not retried. The ticket is about to fail.
    RequestFailed {
        model: String,
        reason: RequestErrorKind,
        message: String,
    },
    /// A transient provider error triggered a retry. `attempt` counts from one.
    RequestRetried {
        model: String,
        attempt: u32,
        max_attempts: u32,
        reason: RequestErrorKind,
        message: String,
    },
    /// A piece of the reply arrived.
    TextChunkReceived { content: String },
    /// A tool invocation began.
    ToolCallStarted {
        tool_name: String,
        call_id: String,
        input: serde_json::Value,
    },
    /// A tool invocation finished.
    ToolCallFinished {
        tool_name: String,
        call_id: String,
        output: String,
    },
    /// A tool invocation failed but the ticket continues. The message goes back
    /// to the model as a tool result.
    ToolCallFailed {
        tool_name: String,
        call_id: String,
        reason: ToolFailureKind,
        message: String,
    },
    /// A tool opened a file.
    FileOpenFinished { path: String },
    /// A tool could not open a file.
    FileOpenFailed { path: String },
    /// A page was written, read, removed, or listed.
    KnowledgeUsed { op: KnowledgeOp },
    /// A page the agent asked for was not there.
    KnowledgeMissed,
    /// A limit was breached and execution stopped.
    PolicyViolated { policy: PolicyKind, limit: u64 },
    /// A result missed its schema and the agent was asked again. `attempt`
    /// counts from one.
    SchemaRetried {
        attempt: u32,
        max_attempts: u32,
        message: String,
    },
    /// Compaction is about to summarize the older messages. `total` is how many
    /// summaries it intends to ask for.
    CompactionStarted { reason: CompactReason, total: u32 },
    /// Compaction finished part of the work. `completed` counts from one and
    /// `total` repeats the matching `CompactionStarted`.
    CompactionProgress {
        reason: CompactReason,
        completed: u32,
        total: u32,
    },
    /// Compaction replaced the older messages with a summary.
    CompactionFinished { reason: CompactReason },
    /// Compaction could not finish. The ticket is about to fail the same way a
    /// failed request ends it.
    CompactionFailed {
        reason: CompactReason,
        message: String,
    },
}

impl EventKind {
    /// The stable snake_case name that keys the per-event counts in `Stats`.
    ///
    /// The match is exhaustive on purpose: a new variant must name itself here,
    /// and that one line is everything its statistics need.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            EventKind::RunStarted => "run_started",
            EventKind::RunFinished { .. } => "run_finished",
            EventKind::TicketStarted => "ticket_started",
            EventKind::TicketFinished => "ticket_finished",
            EventKind::TicketFailed => "ticket_failed",
            EventKind::TurnStarted => "turn_started",
            EventKind::RequestStarted { .. } => "request_started",
            EventKind::RequestFinished { .. } => "request_finished",
            EventKind::RequestFailed { .. } => "request_failed",
            EventKind::RequestRetried { .. } => "request_retried",
            EventKind::TextChunkReceived { .. } => "text_chunk_received",
            EventKind::ToolCallStarted { .. } => "tool_call_started",
            EventKind::ToolCallFinished { .. } => "tool_call_finished",
            EventKind::ToolCallFailed { .. } => "tool_call_failed",
            EventKind::FileOpenFinished { .. } => "file_open_finished",
            EventKind::FileOpenFailed { .. } => "file_open_failed",
            EventKind::KnowledgeUsed { .. } => "knowledge_used",
            EventKind::KnowledgeMissed => "knowledge_missed",
            EventKind::PolicyViolated { .. } => "policy_violated",
            EventKind::SchemaRetried { .. } => "schema_retried",
            EventKind::CompactionStarted { .. } => "compaction_started",
            EventKind::CompactionProgress { .. } => "compaction_progress",
            EventKind::CompactionFinished { .. } => "compaction_finished",
            EventKind::CompactionFailed { .. } => "compaction_failed",
        }
    }

    /// Whether this kind reports something that went wrong. Names the
    /// five kinds `TicketSystem::on_failure` fires on, so a handler on
    /// the plain event chain can ask the same question.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            EventKind::TicketFailed
                | EventKind::RequestFailed { .. }
                | EventKind::ToolCallFailed { .. }
                | EventKind::FileOpenFailed { .. }
                | EventKind::CompactionFailed { .. }
        )
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The handler that runs when you install none of your own.
///
/// It prints ticket lifecycle, tool activity, limit breaches, and failed
/// requests to stderr, and drops the rest.
pub fn default_logger() -> Arc<dyn Fn(&Event) + Send + Sync> {
    Arc::new(|event: &Event| {
        let agent = &event.agent_name;
        match &event.kind {
            EventKind::RunStarted => {
                eprintln!("run started");
            }
            EventKind::RunFinished { reason } => {
                eprintln!("run finished: {reason}");
            }
            EventKind::TicketStarted => {
                eprintln!("[{agent}] started {}", event.ticket_key);
            }
            EventKind::TicketFinished => {
                eprintln!("[{agent}] finished {}", event.ticket_key);
            }
            EventKind::TicketFailed => {
                eprintln!("[{agent}] failed {}", event.ticket_key);
            }
            EventKind::ToolCallStarted {
                tool_name, input, ..
            } => {
                eprintln!("[{agent}] {tool_name}({})", compact_input(input));
            }
            EventKind::ToolCallFailed {
                tool_name,
                message,
                reason,
                ..
            } => {
                eprintln!("[{agent}] {tool_name} failed ({reason}): {message}");
            }
            EventKind::RequestFailed { message, .. } => {
                eprintln!("[{agent}] request failed: {message}");
            }
            EventKind::RequestRetried {
                attempt,
                max_attempts,
                message,
                ..
            } => {
                eprintln!("[{agent}] retry {attempt}/{max_attempts}: {message}");
            }
            EventKind::SchemaRetried {
                attempt,
                max_attempts,
                message,
            } => {
                eprintln!("[{agent}] schema retry {attempt}/{max_attempts}: {message}");
            }
            EventKind::PolicyViolated { policy, limit } => {
                eprintln!("[{agent}] policy violated: {policy} limit={limit}");
            }
            EventKind::CompactionStarted { reason, total } => {
                eprintln!("[{agent}] compacting context ({reason}): {total} chunks");
            }
            EventKind::CompactionProgress {
                reason,
                completed,
                total,
            } => {
                eprintln!("[{agent}] compaction progress ({reason}): {completed}/{total}");
            }
            EventKind::CompactionFinished { reason } => {
                eprintln!("[{agent}] context compacted ({reason})");
            }
            EventKind::CompactionFailed { reason, message } => {
                eprintln!("[{agent}] compaction failed ({reason}): {message}");
            }
            _ => {}
        }
    })
}

fn compact_input(input: &serde_json::Value) -> String {
    let one_line = input.to_string().replace('\n', " ");
    const MAX: usize = 80;
    if one_line.chars().count() <= MAX {
        one_line
    } else {
        let cut: String = one_line.chars().take(MAX).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::TokenUsage;
    use std::collections::BTreeSet;

    fn all_variants() -> Vec<EventKind> {
        vec![
            EventKind::RunStarted,
            EventKind::RunFinished {
                reason: FinishReason::Drained,
            },
            EventKind::RunFinished {
                reason: FinishReason::PolicyViolated(PolicyKind::Time),
            },
            EventKind::RunFinished {
                reason: FinishReason::Cancelled,
            },
            EventKind::TicketStarted,
            EventKind::TicketFinished,
            EventKind::TicketFailed,
            EventKind::TurnStarted,
            EventKind::RequestStarted { model: "m".into() },
            EventKind::RequestFinished {
                model: "m".into(),
                usage: TokenUsage::default(),
            },
            EventKind::RequestFailed {
                model: "m".into(),
                reason: RequestErrorKind::ConnectionFailed,
                message: "timeout".into(),
            },
            EventKind::RequestRetried {
                model: "m".into(),
                attempt: 1,
                max_attempts: 10,
                reason: RequestErrorKind::ConnectionFailed,
                message: "transient".into(),
            },
            EventKind::SchemaRetried {
                attempt: 1,
                max_attempts: 5,
                message: "missing required field 'idx'".into(),
            },
            EventKind::TextChunkReceived {
                content: "hello".into(),
            },
            EventKind::ToolCallStarted {
                tool_name: "bash".into(),
                call_id: "c1".into(),
                input: serde_json::json!({"cmd": "ls"}),
            },
            EventKind::ToolCallFinished {
                tool_name: "bash".into(),
                call_id: "c1".into(),
                output: "file.txt".into(),
            },
            EventKind::ToolCallFailed {
                tool_name: "bash".into(),
                call_id: "c1".into(),
                reason: ToolFailureKind::ToolNotFound,
                message: "not found".into(),
            },
            EventKind::ToolCallFailed {
                tool_name: "manage_tickets".into(),
                call_id: "c2".into(),
                reason: ToolFailureKind::SchemaValidationFailed,
                message: "Schema validation failed".into(),
            },
            EventKind::FileOpenFinished {
                path: "src/lib.rs".into(),
            },
            EventKind::FileOpenFailed {
                path: "src/missing.rs".into(),
            },
            EventKind::KnowledgeUsed {
                op: KnowledgeOp::Write,
            },
            EventKind::KnowledgeMissed,
            EventKind::PolicyViolated {
                policy: PolicyKind::Turns,
                limit: 10,
            },
            EventKind::PolicyViolated {
                policy: PolicyKind::MaxSchemaRetries,
                limit: 10,
            },
            EventKind::PolicyViolated {
                policy: PolicyKind::Time,
                limit: 60_000,
            },
            EventKind::CompactionStarted {
                reason: CompactReason::Proactive,
                total: 3,
            },
            EventKind::CompactionProgress {
                reason: CompactReason::Proactive,
                completed: 1,
                total: 3,
            },
            EventKind::CompactionFinished {
                reason: CompactReason::Proactive,
            },
            EventKind::CompactionFailed {
                reason: CompactReason::Reactive,
                message: "summarize call failed".into(),
            },
        ]
    }

    #[test]
    fn default_logger_handles_every_variant() {
        let logger = default_logger();
        for kind in all_variants() {
            logger(&Event::new("agent", "T-1", kind));
        }
    }

    #[test]
    fn is_failure_covers_every_failed_kind() {
        let failures: BTreeSet<&str> = all_variants()
            .iter()
            .filter(|kind| kind.is_failure())
            .map(|kind| kind.name())
            .collect();
        assert_eq!(
            failures,
            BTreeSet::from([
                "ticket_failed",
                "request_failed",
                "tool_call_failed",
                "file_open_failed",
                "compaction_failed",
            ]),
        );
    }

    #[test]
    fn stats_counts_every_variant() {
        let stats = crate::agents::stats::Stats::new();
        for kind in all_variants() {
            stats.record_event(&kind, "KEY", &[], "");
        }
        let counts = stats.event_counts();
        for kind in all_variants() {
            assert!(
                counts.get(kind.name()).copied().unwrap_or(0) > 0,
                "{} missing from event counts",
                kind.name(),
            );
        }
    }
}
