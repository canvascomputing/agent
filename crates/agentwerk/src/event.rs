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
/// through `TicketQueue::finish_reason()`.
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

impl ToolFailureKind {
    /// The counter this failure adds to under `tools.<name>`.
    pub(crate) fn counter(&self) -> &'static str {
        match self {
            ToolFailureKind::ToolNotFound => "not_found",
            ToolFailureKind::ExecutionFailed => "execution_failed",
            ToolFailureKind::SchemaValidationFailed => "schema_failed",
        }
    }
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

impl KnowledgeOp {
    /// The stable name that keys this operation in the knowledge statistics.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            KnowledgeOp::Write => "write",
            KnowledgeOp::Read => "read",
            KnowledgeOp::Remove => "remove",
            KnowledgeOp::List => "list",
        }
    }
}

impl fmt::Display for KnowledgeOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a [`Measure`] is about. The variant picks the `stats.json` category;
/// the value inside is what that category is keyed on.
pub(crate) enum Subject<'a> {
    Tool(&'a str),
    File(&'a str),
    Model(&'a str),
    Knowledge(KnowledgeOp),
}

impl Subject<'_> {
    /// The `stats.json` category this lands in.
    pub(crate) fn category(&self) -> &'static str {
        match self {
            Subject::Tool(_) => "tools",
            Subject::File(_) => "files",
            Subject::Model(_) => "models",
            Subject::Knowledge(_) => "knowledge",
        }
    }

    /// What that category is keyed on.
    pub(crate) fn name(&self) -> &str {
        match self {
            Subject::Tool(name) | Subject::File(name) | Subject::Model(name) => name,
            Subject::Knowledge(op) => op.name(),
        }
    }
}

/// One counter an [`EventKind`] adds to, and by how much.
pub(crate) struct Measure<'a> {
    pub(crate) subject: Subject<'a>,
    pub(crate) counter: &'static str,
    pub(crate) amount: u64,
}

impl<'a> Measure<'a> {
    fn new(subject: Subject<'a>, counter: &'static str, amount: u64) -> Self {
        Self {
            subject,
            counter,
            amount,
        }
    }
}

/// An `Event` reports one thing that happened as agents work. It names the
/// agent that produced it, the ticket it concerns, and what happened.
///
/// ```no_run
/// use agentwerk::TicketQueue;
/// use agentwerk::event::EventKind;
///
/// # async fn run() {
/// let tickets = TicketQueue::new();
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
/// `RunStarted` and `RunFinished` come from the `TicketQueue` itself and
/// arrive with an empty `agent_name`, as does `TicketFailed` when the host
/// fails a ticket through `TicketQueue::set_failed`.
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
    /// A page operation did not go through: the page was not there, or the
    /// store refused the write.
    KnowledgeFailed { op: KnowledgeOp },
    /// A limit was breached and execution stopped.
    PolicyViolated { policy: PolicyKind, limit: u64 },
    /// A result missed its schema and the agent was asked again. `attempt`
    /// counts from one.
    SchemaRetried {
        attempt: u32,
        max_attempts: u32,
        message: String,
    },
    /// Compaction is about to rewrite the older messages. `total` is how many
    /// summaries the built-in summarizer intends to ask for, and `1` when an
    /// editor is installed, since only the summarizer works in chunks.
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
            EventKind::KnowledgeFailed { .. } => "knowledge_failed",
            EventKind::PolicyViolated { .. } => "policy_violated",
            EventKind::SchemaRetried { .. } => "schema_retried",
            EventKind::CompactionStarted { .. } => "compaction_started",
            EventKind::CompactionProgress { .. } => "compaction_progress",
            EventKind::CompactionFinished { .. } => "compaction_finished",
            EventKind::CompactionFailed { .. } => "compaction_failed",
        }
    }

    /// What this event adds to the statistics beyond its own count. A kind
    /// whose payload no measure reads adds nothing: `name()` already counted
    /// it.
    pub(crate) fn measures(&self) -> Vec<Measure<'_>> {
        match self {
            EventKind::RequestFinished { model, usage } => vec![
                Measure::new(Subject::Model(model), "requests", 1),
                Measure::new(Subject::Model(model), "input_tokens", usage.input_tokens),
                Measure::new(Subject::Model(model), "output_tokens", usage.output_tokens),
            ],
            // A failed request is a request too, so `requests` is the attempt
            // count and `error_rate` divides by it. The same holds for a
            // failed file open and a failed knowledge operation below.
            EventKind::RequestFailed { model, .. } => vec![
                Measure::new(Subject::Model(model), "requests", 1),
                Measure::new(Subject::Model(model), "failed", 1),
            ],
            EventKind::ToolCallStarted { tool_name, .. } => {
                vec![Measure::new(Subject::Tool(tool_name), "calls", 1)]
            }
            EventKind::ToolCallFailed {
                tool_name, reason, ..
            } => vec![Measure::new(Subject::Tool(tool_name), reason.counter(), 1)],
            EventKind::FileOpenFinished { path } => {
                vec![Measure::new(Subject::File(path), "opens", 1)]
            }
            EventKind::FileOpenFailed { path } => vec![
                Measure::new(Subject::File(path), "opens", 1),
                Measure::new(Subject::File(path), "failed", 1),
            ],
            EventKind::KnowledgeUsed { op } => {
                vec![Measure::new(Subject::Knowledge(*op), "attempts", 1)]
            }
            EventKind::KnowledgeFailed { op } => vec![
                Measure::new(Subject::Knowledge(*op), "attempts", 1),
                Measure::new(Subject::Knowledge(*op), "failed", 1),
            ],
            _ => Vec::new(),
        }
    }

    /// Whether this kind reports something that went wrong. Names the
    /// six kinds `TicketQueue::on_failure` fires on, so a handler on
    /// the plain event chain can ask the same question.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            EventKind::TicketFailed
                | EventKind::RequestFailed { .. }
                | EventKind::ToolCallFailed { .. }
                | EventKind::FileOpenFailed { .. }
                | EventKind::KnowledgeFailed { .. }
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
            EventKind::KnowledgeFailed {
                op: KnowledgeOp::Read,
            },
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
                "knowledge_failed",
                "compaction_failed",
            ]),
        );
    }

    #[test]
    fn every_measure_a_variant_declares_reaches_stats_json() {
        for kind in all_variants() {
            if kind.measures().is_empty() {
                continue;
            }
            let stats = crate::agents::stats::Stats::new();
            stats.record_event(&kind, "KEY", &[], "");
            let value = serde_json::to_value(&stats).unwrap();
            for measure in kind.measures() {
                let category = measure.subject.category();
                let name = measure.subject.name();
                assert!(
                    value[category][name].get(measure.counter).is_some(),
                    "{}: {category}.{name}.{} is missing",
                    kind.name(),
                    measure.counter,
                );
            }
        }
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
