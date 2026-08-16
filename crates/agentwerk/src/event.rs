//! Insights into the lifecycle and activities of an agent's work.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::providers::{RequestErrorKind, TokenUsage, ToolDeclineKind};

/// Why the older messages were summarized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
/// Carried by [`EventKind::RunFinished`], and handed back by
/// `TicketQueue::finish` once the wait is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The queue emptied; nothing more to do.
    Drained,
    /// A limit was breached.
    PolicyViolated(PolicyKind),
    /// A `cancel` left nothing claimable.
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

/// How a tool call failed, carried by [`EventKind::ToolCallFailed`] and by
/// [`EventKind::FileOpenFailed`], since a path fails with the call that named
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ToolFailureKind {
    /// No tool of that name is registered.
    #[serde(rename = "not_found")]
    ToolNotFound,
    /// The tool ran and returned an error.
    #[serde(rename = "execution_failed")]
    ExecutionFailed,
    /// The tool rejected its input. Counted against `max_schema_retries`.
    #[serde(rename = "schema_failed")]
    SchemaValidationFailed,
}

impl ToolFailureKind {
    /// Every kind, in the order they are declared.
    pub const ALL: &'static [ToolFailureKind] = &[
        ToolFailureKind::ToolNotFound,
        ToolFailureKind::ExecutionFailed,
        ToolFailureKind::SchemaValidationFailed,
    ];

    /// The stable snake_case spelling, for a handler keying its own counts by
    /// reason.
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolFailureKind::ToolNotFound => "not_found",
            ToolFailureKind::ExecutionFailed => "execution_failed",
            ToolFailureKind::SchemaValidationFailed => "schema_failed",
        }
    }
}

impl fmt::Display for ToolFailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What was wrong with something the model sent, carried by
/// [`EventKind::ResponseRepaired`]. Each variant names the defect, since the fix
/// already happened and the defect is what a prompt change would address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RepairKind {
    /// The call arrived unusable: written as text in the reply, delivered
    /// without its arguments, or named with a spelling that had to be folded.
    #[serde(rename = "call_malformed")]
    CallMalformed,
    /// A value did not match the schema declared for it, in type or in shape:
    /// a scalar the model quoted, a structure it wrote as JSON text, or a
    /// result it sent under a `result` key the schema does not declare.
    #[serde(rename = "value_mistyped")]
    ValueMistyped,
}

impl RepairKind {
    /// Every kind, in the order they are declared.
    pub const ALL: &'static [RepairKind] = &[RepairKind::CallMalformed, RepairKind::ValueMistyped];

    /// The stable snake_case spelling, the one `Event.data["reason"]` carries.
    pub fn as_str(&self) -> &'static str {
        match self {
            RepairKind::CallMalformed => "call_malformed",
            RepairKind::ValueMistyped => "value_mistyped",
        }
    }
}

impl fmt::Display for RepairKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a knowledge operation did not go through, carried by
/// [`EventKind::KnowledgeFailed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeFailureKind {
    /// No page exists at that slug.
    PageMissing,
    /// The store refused it: the values were rejected, the write would push
    /// the index past its limit, or the file operation underneath failed.
    StoreRefused,
}

impl KnowledgeFailureKind {
    /// Every kind, in the order they are declared.
    pub const ALL: &'static [KnowledgeFailureKind] = &[
        KnowledgeFailureKind::PageMissing,
        KnowledgeFailureKind::StoreRefused,
    ];

    /// The stable snake_case spelling, for a handler keying its own counts by
    /// reason.
    pub fn as_str(&self) -> &'static str {
        match self {
            KnowledgeFailureKind::PageMissing => "page_missing",
            KnowledgeFailureKind::StoreRefused => "store_refused",
        }
    }
}

impl fmt::Display for KnowledgeFailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What was done to a knowledge page, carried by [`EventKind::KnowledgeUsed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeOp {
    Write,
    Read,
    Remove,
    List,
}

impl KnowledgeOp {
    /// The stable name this operation reports itself under.
    fn name(&self) -> &'static str {
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
///         eprintln!("[{}] done {}", event.agent_id, event.ticket_key);
///     }
/// });
/// tickets.finish_all().await;
/// # }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// When this event happened, in milliseconds since the epoch.
    pub created_at: u64,
    /// Name of the agent that produced this event.
    pub agent_id: String,
    /// Key of the ticket this event concerns. Empty on `RunStarted` and
    /// `RunFinished`, which no ticket owns.
    pub ticket_key: String,
    /// Label the ticket carries, so a handler counting per label reads it
    /// without looking the ticket up. `None` when the event names no ticket,
    /// and when the ticket carries no label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// What happened. Flattened into the event itself, so one logged line
    /// carries the kind's name and its payload beside the four fields here.
    #[serde(flatten)]
    pub kind: EventKind,
}

impl Event {
    pub(crate) fn new(
        agent_id: impl Into<String>,
        ticket_key: impl Into<String>,
        label: Option<String>,
        kind: EventKind,
    ) -> Self {
        Self {
            created_at: crate::agents::tickets::now_millis(),
            agent_id: agent_id.into(),
            ticket_key: ticket_key.into(),
            label,
            kind,
        }
    }
}

/// What an [`Event`] reports.
///
/// Most kinds name the agent they came from on the wrapping [`Event`].
/// `RunStarted` and `RunFinished` come from the `TicketQueue` itself and
/// arrive with an empty `agent_id`, as does `TicketFailed` when the host
/// fails a ticket through `TicketQueue::set_failed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    /// Execution began.
    RunStarted,
    /// Execution ended, carrying the reason.
    RunFinished { reason: FinishReason },
    /// A ticket was filed. The agent id is its reporter.
    TicketCreated,
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
    /// A malformed call or value was corrected here and then used, rather than
    /// asked for again. `message` reads `<name>: <what was corrected>`, so
    /// grouping by its prefix collects every repair one tool needed. Repeated
    /// repairs of one reason point at a prompt or tool description to fix.
    ResponseRepaired { reason: RepairKind, message: String },
    /// A framed tool call was found in the reply and left alone, with the
    /// reason it was not promoted.
    ToolCallDeclined {
        tool_name: String,
        reason: ToolDeclineKind,
    },
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
    /// A tool could not open a file. The reason is the enclosing call's, since
    /// a path fails with the call that named it.
    FileOpenFailed {
        path: String,
        reason: ToolFailureKind,
    },
    /// A page was written, read, removed, or listed.
    KnowledgeUsed { op: KnowledgeOp },
    /// A page operation did not go through.
    KnowledgeFailed {
        op: KnowledgeOp,
        reason: KnowledgeFailureKind,
    },
    /// A limit was breached and execution stopped.
    PolicyViolated { policy: PolicyKind, limit: u64 },
    /// A call or a result missed its schema and the agent was asked again.
    /// `attempt` counts from one.
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
    /// The stable snake_case spelling of this kind, as `events.jsonl` writes it.
    pub fn name(&self) -> &'static str {
        self.event_name().as_str()
    }

    /// Which count this event adds to.
    ///
    /// The match is exhaustive on purpose: a new variant must name itself here,
    /// and that one line is everything its count needs.
    pub fn event_name(&self) -> EventName {
        match self {
            EventKind::RunStarted => EventName::RunStarted,
            EventKind::RunFinished { .. } => EventName::RunFinished,
            EventKind::TicketCreated => EventName::TicketCreated,
            EventKind::TicketStarted => EventName::TicketStarted,
            EventKind::TicketFinished => EventName::TicketFinished,
            EventKind::TicketFailed => EventName::TicketFailed,
            EventKind::TurnStarted => EventName::TurnStarted,
            EventKind::RequestStarted { .. } => EventName::RequestStarted,
            EventKind::RequestFinished { .. } => EventName::RequestFinished,
            EventKind::RequestFailed { .. } => EventName::RequestFailed,
            EventKind::RequestRetried { .. } => EventName::RequestRetried,
            EventKind::TextChunkReceived { .. } => EventName::TextChunkReceived,
            EventKind::ResponseRepaired { .. } => EventName::ResponseRepaired,
            EventKind::ToolCallDeclined { .. } => EventName::ToolCallDeclined,
            EventKind::ToolCallStarted { .. } => EventName::ToolCallStarted,
            EventKind::ToolCallFinished { .. } => EventName::ToolCallFinished,
            EventKind::ToolCallFailed { .. } => EventName::ToolCallFailed,
            EventKind::FileOpenFinished { .. } => EventName::FileOpenFinished,
            EventKind::FileOpenFailed { .. } => EventName::FileOpenFailed,
            EventKind::KnowledgeUsed { .. } => EventName::KnowledgeUsed,
            EventKind::KnowledgeFailed { .. } => EventName::KnowledgeFailed,
            EventKind::PolicyViolated { .. } => EventName::PolicyViolated,
            EventKind::SchemaRetried { .. } => EventName::SchemaRetried,
            EventKind::CompactionStarted { .. } => EventName::CompactionStarted,
            EventKind::CompactionProgress { .. } => EventName::CompactionProgress,
            EventKind::CompactionFinished { .. } => EventName::CompactionFinished,
            EventKind::CompactionFailed { .. } => EventName::CompactionFailed,
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

/// Which [`EventKind`] a count belongs to, without the payload the kind
/// carries. Pass one to [`Stats::event_count`](crate::Stats::event_count); the
/// snake_case spelling is the one `events.jsonl` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventName {
    RunStarted,
    RunFinished,
    TicketCreated,
    TicketStarted,
    TicketFinished,
    TicketFailed,
    TurnStarted,
    RequestStarted,
    RequestFinished,
    RequestFailed,
    RequestRetried,
    TextChunkReceived,
    ResponseRepaired,
    ToolCallDeclined,
    ToolCallStarted,
    ToolCallFinished,
    ToolCallFailed,
    FileOpenFinished,
    FileOpenFailed,
    KnowledgeUsed,
    KnowledgeFailed,
    PolicyViolated,
    SchemaRetried,
    CompactionStarted,
    CompactionProgress,
    CompactionFinished,
    CompactionFailed,
}

impl EventName {
    /// Every name, in the order the kinds are declared. Lets a caller walk the
    /// counts without knowing which kinds exist.
    pub const ALL: &'static [EventName] = &[
        EventName::RunStarted,
        EventName::RunFinished,
        EventName::TicketCreated,
        EventName::TicketStarted,
        EventName::TicketFinished,
        EventName::TicketFailed,
        EventName::TurnStarted,
        EventName::RequestStarted,
        EventName::RequestFinished,
        EventName::RequestFailed,
        EventName::RequestRetried,
        EventName::TextChunkReceived,
        EventName::ResponseRepaired,
        EventName::ToolCallDeclined,
        EventName::ToolCallStarted,
        EventName::ToolCallFinished,
        EventName::ToolCallFailed,
        EventName::FileOpenFinished,
        EventName::FileOpenFailed,
        EventName::KnowledgeUsed,
        EventName::KnowledgeFailed,
        EventName::PolicyViolated,
        EventName::SchemaRetried,
        EventName::CompactionStarted,
        EventName::CompactionProgress,
        EventName::CompactionFinished,
        EventName::CompactionFailed,
    ];

    /// The stable snake_case spelling, the same one serde reads and writes.
    pub fn as_str(&self) -> &'static str {
        match self {
            EventName::RunStarted => "run_started",
            EventName::RunFinished => "run_finished",
            EventName::TicketCreated => "ticket_created",
            EventName::TicketStarted => "ticket_started",
            EventName::TicketFinished => "ticket_finished",
            EventName::TicketFailed => "ticket_failed",
            EventName::TurnStarted => "turn_started",
            EventName::RequestStarted => "request_started",
            EventName::RequestFinished => "request_finished",
            EventName::RequestFailed => "request_failed",
            EventName::RequestRetried => "request_retried",
            EventName::TextChunkReceived => "text_chunk_received",
            EventName::ResponseRepaired => "response_repaired",
            EventName::ToolCallDeclined => "tool_call_declined",
            EventName::ToolCallStarted => "tool_call_started",
            EventName::ToolCallFinished => "tool_call_finished",
            EventName::ToolCallFailed => "tool_call_failed",
            EventName::FileOpenFinished => "file_open_finished",
            EventName::FileOpenFailed => "file_open_failed",
            EventName::KnowledgeUsed => "knowledge_used",
            EventName::KnowledgeFailed => "knowledge_failed",
            EventName::PolicyViolated => "policy_violated",
            EventName::SchemaRetried => "schema_retried",
            EventName::CompactionStarted => "compaction_started",
            EventName::CompactionProgress => "compaction_progress",
            EventName::CompactionFinished => "compaction_finished",
            EventName::CompactionFailed => "compaction_failed",
        }
    }
}

impl fmt::Display for EventName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The handler that runs when you install none of your own.
///
/// It prints ticket lifecycle, tool activity, limit breaches, and failed
/// requests to stderr, and drops the rest.
pub fn default_logger() -> Arc<dyn Fn(&Event) + Send + Sync> {
    Arc::new(|event: &Event| {
        let agent = &event.agent_id;
        match &event.kind {
            EventKind::RunStarted => {
                eprintln!("run started");
            }
            EventKind::RunFinished { reason } => {
                eprintln!("run finished: {reason}");
            }
            EventKind::TicketCreated => {
                eprintln!("[{agent}] created {}", event.ticket_key);
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
pub(crate) mod tests {
    use super::*;
    use crate::providers::TokenUsage;
    use std::collections::BTreeSet;

    pub(crate) fn all_variants() -> Vec<EventKind> {
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
            EventKind::TicketCreated,
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
            EventKind::ResponseRepaired {
                reason: RepairKind::CallMalformed,
                message: "grep".into(),
            },
            EventKind::ToolCallDeclined {
                tool_name: "grep".into(),
                reason: ToolDeclineKind::OutputTruncated,
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
                tool_name: "tickets".into(),
                call_id: "c2".into(),
                reason: ToolFailureKind::SchemaValidationFailed,
                message: "Schema validation failed".into(),
            },
            EventKind::FileOpenFinished {
                path: "src/lib.rs".into(),
            },
            EventKind::FileOpenFailed {
                path: "src/missing.rs".into(),
                reason: ToolFailureKind::ExecutionFailed,
            },
            EventKind::KnowledgeUsed {
                op: KnowledgeOp::Write,
            },
            EventKind::KnowledgeFailed {
                op: KnowledgeOp::Read,
                reason: KnowledgeFailureKind::PageMissing,
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
            logger(&Event::new("agent", "T-1", None, kind));
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
    fn all_lists_the_name_of_every_event_kind() {
        // The bindings build Python's EventName from ALL, so a variant missing
        // here is a name Python never learns.
        let named: BTreeSet<EventName> = all_variants().iter().map(EventKind::event_name).collect();
        assert_eq!(named, EventName::ALL.iter().copied().collect());
    }

    #[test]
    fn every_event_name_matches_its_serde_spelling() {
        // `name()` is hand-written and `events.jsonl` is written by serde's
        // rename_all, so the two have to agree or a logged event reloads under
        // a name nothing asks for.
        for kind in all_variants() {
            let event = kind.event_name();
            let spelled = serde_json::to_value(event).unwrap();
            assert_eq!(spelled.as_str(), Some(event.as_str()));
        }
    }

    #[test]
    fn a_logged_kind_names_itself_the_way_event_name_spells_it() {
        for kind in all_variants() {
            let name = kind.event_name();
            let event = Event::new("agent", "TICKET-1", None, kind);
            let line = serde_json::to_value(&event).unwrap();
            assert_eq!(line["event"].as_str(), Some(name.as_str()));
        }
    }
}
