//! Structured events agentwerk emits so callers can observe a run
//! without wrapping the agent.

use std::fmt;
use std::sync::Arc;

use crate::providers::{RequestErrorKind, TokenUsage};

/// Why the context-window compaction seam fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactReason {
    /// The next-request token estimate crossed the model's compaction
    /// threshold before sending; the warning fired ahead of any failure.
    Proactive,
    /// The provider itself reported a context-window overflow, either as
    /// a `ProviderError::ContextWindowExceeded` or via
    /// `ResponseStatus::ContextWindowExceeded` on a successful reply.
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

/// Which configured policy a [`EventKind::PolicyViolated`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyKind {
    /// `max_turns` — the turn cap across all agents.
    Turns,
    /// `max_input_tokens` — cumulative request-side token cap.
    InputTokens,
    /// `max_output_tokens` — cumulative reply-side token cap.
    OutputTokens,
    /// `max_schema_retries` — consecutive schema-validation failures
    /// while processing one ticket. Resets after every successful
    /// schema-checked tool call.
    MaxSchemaRetries,
    /// `max_time`: total elapsed-duration limit. The `limit` field on
    /// the matching [`EventKind::PolicyViolated`] is reported in
    /// milliseconds.
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

/// Why a run ended. Carried by [`EventKind::RunFinished`] and readable
/// after `finish().await` via `TicketSystem::finish_reason()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// No tickets remained pending; nothing more to do.
    Drained,
    /// A `Policies` limit was exceeded.
    PolicyViolated(PolicyKind),
    /// An external party requested cancellation through `cancel()`,
    /// `cancel_on`, or `cancel_on_event`.
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

/// Categorical discriminant for [`EventKind::ToolCallFailed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFailureKind {
    /// The registry had no tool with that name.
    ToolNotFound,
    /// The tool was invoked but its execution raised an error.
    ExecutionFailed,
    /// A schema-checked tool rejected its input. Counted against
    /// `policies.max_schema_retries`.
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

/// One Knowledge-store operation, carried by [`EventKind::KnowledgeUsed`].
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

/// Observation emitted as agents work. Carries the name of the agent
/// that produced it, the key of the ticket it concerns, plus a typed
/// [`EventKind`].
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
    /// Key of the ticket this event concerns. Empty for run-lifecycle
    /// events (`RunStarted`, `RunFinished`), which no ticket owns.
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

/// Categorical discriminant of [`Event`].
///
/// Most variants are emitted by a per-agent loop and carry that agent's
/// name on the wrapping [`Event`]. Two run-lifecycle variants
/// (`RunStarted`, `RunFinished`) are emitted by the `TicketSystem`
/// itself and arrive with an empty `agent_name`, as does `TicketFailed`
/// when the host fails a ticket through `TicketSystem::set_failed`.
#[derive(Debug, Clone)]
pub enum EventKind {
    /// The `TicketSystem`'s background work loop has been spawned and
    /// the run is live. Emitted by `TicketSystem::start`.
    RunStarted,
    /// The `TicketSystem`'s run has stopped. Carries the reason
    /// `finish()` returned. Emitted by `TicketSystem::finish` after the
    /// worker tasks have joined.
    RunFinished { reason: FinishReason },
    /// Agent claimed a ticket and began working on it.
    TicketStarted,
    /// Ticket finished with `Status::Finished`.
    TicketFinished,
    /// Ticket failed with `Status::Failed`.
    TicketFailed,
    /// Agent loop started a new turn.
    TurnStarted,
    /// Provider request began.
    RequestStarted { model: String },
    /// Provider request finished successfully. Carries the model and the
    /// token counts the provider reported for the response.
    RequestFinished { model: String, usage: TokenUsage },
    /// Provider request failed. The run is about to stop for this ticket.
    RequestFailed {
        model: String,
        reason: RequestErrorKind,
        message: String,
    },
    /// Provider request failed transiently; agentwerk is about to sleep
    /// and retry. `attempt` is 1-based.
    RequestRetried {
        model: String,
        attempt: u32,
        max_attempts: u32,
        reason: RequestErrorKind,
        message: String,
    },
    /// A streamed text chunk arrived from the provider.
    TextChunkReceived { content: String },
    /// Tool invocation began.
    ToolCallStarted {
        tool_name: String,
        call_id: String,
        input: serde_json::Value,
    },
    /// Tool invocation succeeded.
    ToolCallFinished {
        tool_name: String,
        call_id: String,
        output: String,
    },
    /// Tool invocation failed. The error is sent back to the model as a
    /// tool-result message; the run continues.
    ToolCallFailed {
        tool_name: String,
        call_id: String,
        reason: ToolFailureKind,
        message: String,
    },
    /// A file-opening tool opened `path` successfully.
    FileOpenFinished { path: String },
    /// A file-opening tool failed on `path`.
    FileOpenFailed { path: String },
    /// The knowledge tool performed `op`. The tool self-reports, since
    /// only it sees which operation ran.
    KnowledgeUsed { op: KnowledgeOp },
    /// A knowledge `read` or `remove` named a slug the store does not
    /// have. Self-reported like `KnowledgeUsed`: the miss returns Ok, so
    /// the tool-call loop cannot see it.
    KnowledgeMissed,
    /// A configured policy was exceeded; the run is about to stop.
    PolicyViolated { policy: PolicyKind, limit: u64 },
    /// A `done`-side schema validation failed; agentwerk is about to
    /// re-prompt the model with a corrective directive. `attempt` is
    /// 1-based.
    SchemaRetried {
        attempt: u32,
        max_attempts: u32,
        message: String,
    },
    /// Compaction is about to run: agentwerk is about to call the
    /// summarizer to collapse the message tail. `total` is the number
    /// of summariser calls the algorithm intends to make.
    CompactionStarted { reason: CompactReason, total: u32 },
    /// One summariser call finished. Fires once per chunk processed by
    /// the algorithm; `completed` is the running count (1-based) and
    /// `total` is the same value as the matching `CompactionStarted`'s
    /// `total`.
    CompactionProgress {
        reason: CompactReason,
        completed: u32,
        total: u32,
    },
    /// Compaction finished successfully; the message tail has been
    /// replaced with the model's summary.
    CompactionFinished { reason: CompactReason },
    /// Compaction failed: the summarizer call returned a provider
    /// error. The ticket is about to fail via the usual
    /// `RequestFailed` path.
    CompactionFailed {
        reason: CompactReason,
        message: String,
    },
}

impl EventKind {
    /// Stable snake_case discriminant name; keys the per-event counts in
    /// `Stats`. Exhaustive on purpose: a new variant must name itself here,
    /// and that one line is its whole stats integration.
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
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Default observer. Prints ticket lifecycle, tool activity, policy
/// violations, and request failures to stderr. Quiet variants
/// (token counts, streaming chunks, request start/finish) are dropped.
pub fn default_logger() -> Arc<dyn Fn(Event) + Send + Sync> {
    Arc::new(|event: Event| {
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
            logger(Event::new("agent", "T-1", kind));
        }
    }

    #[test]
    fn stats_counts_every_variant() {
        let stats = crate::agents::stats::Stats::new();
        for kind in all_variants() {
            stats.record_event(&kind, "KEY", &[]);
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
