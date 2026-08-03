//! Deep insights into how agents behave: working time, tickets, failure rates,
//! and bottlenecks.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

use crate::agents::tickets::Status;
use crate::event::{EventKind, KnowledgeOp, ToolFailureKind};
use crate::providers::types::TokenUsage;

/// `Stats` holds the metrics about tickets, tokens, and time. Reach it through
/// [`TicketQueue::stats()`](crate::TicketQueue::stats), during execution or
/// after it finishes.
///
/// ```no_run
/// use agentwerk::TicketQueue;
///
/// # async fn run() {
/// let tickets = TicketQueue::new();
/// tickets.finish().await;
///
/// let stats = tickets.stats();
/// println!(
///     "{} tickets, {} input tokens",
///     stats.tickets_finished(),
///     stats.input_tokens(),
/// );
/// # }
/// ```
pub struct Stats {
    /// Count per event kind, keyed by [`EventKind::name`]. Every emitted event
    /// lands here, and the named accessors are lookups into this map.
    event_counts: Mutex<HashMap<String, u64>>,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    tickets_created: AtomicU64,
    tickets_finished: AtomicU64,
    tickets_failed: AtomicU64,

    /// When execution started, in milliseconds since the epoch. 0 until then,
    /// and the first writer wins.
    started_at: AtomicU64,
    /// When execution ended, in milliseconds since the epoch. 0 while it is
    /// still running.
    finished_at: AtomicU64,
    /// Sum of finished tickets' time from creation to resolution, in seconds.
    total_ticket_duration: AtomicU64,
    /// Sum of finished tickets' time from claim to resolution, in seconds. With
    /// agents working in parallel this can exceed the elapsed duration.
    total_work_duration: AtomicU64,
    /// Nested slices keyed by ticket label, filled on demand. Only the run-wide
    /// `Stats` holds them; a slice's own map stays empty.
    label_stats: Mutex<HashMap<String, Arc<Stats>>>,
    /// Nested slices keyed by agent name, filled on demand. Kept apart from
    /// `label_stats` because claiming a ticket adds the agent's name to its
    /// labels, so one map would merge an agent with a label of the same name.
    agent_stats: Mutex<HashMap<String, Arc<Stats>>>,
    /// Token usage per ticket, oldest first. It feeds the estimate that decides
    /// when to compact, and is cleared once that happens. The one measure the
    /// run-wide `Stats` keeps to itself.
    token_usage: Mutex<HashMap<String, Vec<TokenUsage>>>,
    /// Call and failure tallies keyed by tool name.
    tool_stats: Mutex<HashMap<String, ToolCounters>>,
    /// Open and failure tallies keyed by the path a tool opened.
    file_stats: Mutex<HashMap<String, FileCounters>>,
    /// Attempt and failure tallies keyed by knowledge operation.
    knowledge_stats: Mutex<HashMap<String, KnowledgeCounters>>,
    /// Request, failure, and token tallies keyed by model name.
    model_stats: Mutex<HashMap<String, ModelCounters>>,
}

/// The stored per-tool tallies. `errors` is derived from the three failure
/// kinds, never stored.
#[derive(Default, Clone)]
struct ToolCounters {
    calls: u64,
    not_found: u64,
    execution_failed: u64,
    schema_failed: u64,
}

/// The stored per-path tallies.
#[derive(Default, Clone)]
struct FileCounters {
    opens: u64,
    failed: u64,
}

/// The stored per-operation tallies.
#[derive(Default, Clone)]
struct KnowledgeCounters {
    attempts: u64,
    failed: u64,
}

/// The stored per-model tallies. `failed` is a subset of `requests`.
#[derive(Default, Clone)]
struct ModelCounters {
    requests: u64,
    failed: u64,
    input_tokens: u64,
    output_tokens: u64,
}

/// A `ToolStat` counts one tool's calls and failures, split by how each failed.
///
/// Returned by [`Stats::tool_stats`]. A high error rate, or many `schema_failed`
/// or `not_found` calls, points at the tool's description or its input schema.
#[derive(Debug, Clone, Serialize)]
pub struct ToolStat {
    /// Every call, including calls that named an unknown tool.
    pub calls: u64,
    /// Calls naming a tool that is not registered.
    pub not_found: u64,
    /// Calls where the tool ran and returned an error.
    pub execution_failed: u64,
    /// Calls the tool rejected as malformed. This is the same population
    /// `max_schema_retries` governs, reported per tool.
    pub schema_failed: u64,
}

impl ToolStat {
    /// Get the total failures across the three kinds.
    pub fn errors(&self) -> u64 {
        self.not_found + self.execution_failed + self.schema_failed
    }

    /// Get `errors / calls`, or `None` when the tool was never called.
    pub fn error_rate(&self) -> Option<f64> {
        if self.calls == 0 {
            None
        } else {
            Some(self.errors() as f64 / self.calls as f64)
        }
    }
}

impl From<&ToolCounters> for ToolStat {
    fn from(c: &ToolCounters) -> Self {
        Self {
            calls: c.calls,
            not_found: c.not_found,
            execution_failed: c.execution_failed,
            schema_failed: c.schema_failed,
        }
    }
}

/// A `FileStat` counts how often a tool opened one path, and how often it could
/// not. Returned by [`Stats::file_stats`].
///
/// The path may name a directory rather than a file, since `grep` searches
/// under either.
#[derive(Debug, Clone, Serialize)]
pub struct FileStat {
    /// Every call that named this path, including the ones that failed.
    pub opens: u64,
    /// Calls that named this path and failed.
    pub failed: u64,
}

impl FileStat {
    /// Get the total failures.
    pub fn errors(&self) -> u64 {
        self.failed
    }

    /// Get `errors / opens`, or `None` when the path was never named.
    pub fn error_rate(&self) -> Option<f64> {
        if self.opens == 0 {
            None
        } else {
            Some(self.errors() as f64 / self.opens as f64)
        }
    }
}

impl From<&FileCounters> for FileStat {
    fn from(c: &FileCounters) -> Self {
        Self {
            opens: c.opens,
            failed: c.failed,
        }
    }
}

/// A `KnowledgeStat` counts one knowledge operation's attempts and failures.
/// Returned by [`Stats::knowledge_stats`], keyed by operation.
///
/// A high error rate points at a stale index, or at a prompt promising more
/// than the store holds.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeStat {
    /// Every attempt at this operation, including the ones the store refused
    /// and the ones that named a page it does not have.
    pub attempts: u64,
    /// Attempts that did not go through.
    pub failed: u64,
}

impl KnowledgeStat {
    /// Get the total failures.
    pub fn errors(&self) -> u64 {
        self.failed
    }

    /// Get `errors / attempts`, or `None` when the operation was never tried.
    pub fn error_rate(&self) -> Option<f64> {
        if self.attempts == 0 {
            None
        } else {
            Some(self.errors() as f64 / self.attempts as f64)
        }
    }
}

impl From<&KnowledgeCounters> for KnowledgeStat {
    fn from(c: &KnowledgeCounters) -> Self {
        Self {
            attempts: c.attempts,
            failed: c.failed,
        }
    }
}

/// A `ModelStat` counts one model's requests and token usage. Returned by
/// [`Stats::model_stats`], so agents running different models can be compared.
#[derive(Debug, Clone, Serialize)]
pub struct ModelStat {
    /// Every request to this model, including the ones that failed.
    pub requests: u64,
    /// Requests that came back as a failure and were not retried.
    pub failed: u64,
    /// Input tokens across this model's responses.
    pub input_tokens: u64,
    /// Output tokens across this model's responses.
    pub output_tokens: u64,
}

impl ModelStat {
    /// Get the total failures.
    pub fn errors(&self) -> u64 {
        self.failed
    }

    /// Get `errors / requests`, or `None` when the model was never asked.
    pub fn error_rate(&self) -> Option<f64> {
        if self.requests == 0 {
            None
        } else {
            Some(self.errors() as f64 / self.requests as f64)
        }
    }
}

impl From<&ModelCounters> for ModelStat {
    fn from(c: &ModelCounters) -> Self {
        Self {
            requests: c.requests,
            failed: c.failed,
            input_tokens: c.input_tokens,
            output_tokens: c.output_tokens,
        }
    }
}

impl Stats {
    pub(crate) fn new() -> Self {
        Self {
            event_counts: Mutex::new(HashMap::new()),
            input_tokens: AtomicU64::new(0),
            output_tokens: AtomicU64::new(0),
            tickets_created: AtomicU64::new(0),
            tickets_finished: AtomicU64::new(0),
            tickets_failed: AtomicU64::new(0),
            started_at: AtomicU64::new(0),
            finished_at: AtomicU64::new(0),
            total_ticket_duration: AtomicU64::new(0),
            total_work_duration: AtomicU64::new(0),
            label_stats: Mutex::new(HashMap::new()),
            agent_stats: Mutex::new(HashMap::new()),
            token_usage: Mutex::new(HashMap::new()),
            tool_stats: Mutex::new(HashMap::new()),
            file_stats: Mutex::new(HashMap::new()),
            knowledge_stats: Mutex::new(HashMap::new()),
            model_stats: Mutex::new(HashMap::new()),
        }
    }

    /// Get statistics scoped to one label. A label nothing was recorded
    /// against reads as zero.
    ///
    /// Every accessor answers the same question it answers run-wide, except
    /// `run_duration()`, which is `None` because timing stays global.
    pub fn stats_for_label(&self, label: &str) -> Arc<Stats> {
        self.label_stats
            .lock()
            .unwrap()
            .get(label)
            .cloned()
            .unwrap_or_default()
    }

    /// Get statistics scoped to one agent, by the name it was registered
    /// under. An agent nothing was recorded against reads as zero.
    ///
    /// `tickets_created()` counts the tickets that agent filed; the rest count
    /// the tickets it claimed.
    pub fn stats_for_agent(&self, agent_name: &str) -> Arc<Stats> {
        self.agent_stats
            .lock()
            .unwrap()
            .get(agent_name)
            .cloned()
            .unwrap_or_default()
    }

    /// The label's slice, created on first use. Reading through
    /// `stats_for_label` must not add one, or a misspelled lookup would
    /// leave a zeroed section in `stats.json` for good.
    pub(crate) fn slice_for_label(&self, label: &str) -> Arc<Stats> {
        self.label_stats
            .lock()
            .unwrap()
            .entry(label.to_string())
            .or_insert_with(|| Arc::new(Stats::new()))
            .clone()
    }

    /// The agent's slice, created on first use.
    pub(crate) fn slice_for_agent(&self, agent_name: &str) -> Arc<Stats> {
        self.agent_stats
            .lock()
            .unwrap()
            .entry(agent_name.to_string())
            .or_insert_with(|| Arc::new(Stats::new()))
            .clone()
    }

    /// A ticket's token usage, oldest first, for the compaction estimator.
    ///
    /// Crate-internal: it is cleared when a ticket is compacted, so a caller
    /// reading it would get a silently truncated series. A host that wants
    /// the figures reads `EventKind::RequestFinished`, which reports every
    /// one as it happens and is never cleared.
    pub(crate) fn usage_for_ticket(&self, ticket_key: &str) -> Vec<TokenUsage> {
        self.token_usage
            .lock()
            .unwrap()
            .get(ticket_key)
            .cloned()
            .unwrap_or_default()
    }

    /// Append `usage` to the per-ticket series.
    pub(crate) fn record_usage(&self, ticket_key: &str, usage: TokenUsage) {
        self.token_usage
            .lock()
            .unwrap()
            .entry(ticket_key.to_string())
            .or_default()
            .push(usage);
    }

    /// Drop a ticket's token usage, once its older messages are summarized
    /// and the earlier trend no longer predicts the next request.
    pub(crate) fn reset_usage(&self, ticket_key: &str) {
        self.token_usage.lock().unwrap().remove(ticket_key);
    }

    /// Get per-tool call and failure counts, sorted by tool name.
    pub fn tool_stats(&self) -> BTreeMap<String, ToolStat> {
        self.tool_stats
            .lock()
            .unwrap()
            .iter()
            .map(|(name, counters)| (name.clone(), counters.into()))
            .collect()
    }

    /// Count one call against `name`.
    pub(crate) fn record_tool_call(&self, name: &str) {
        self.tool_stats
            .lock()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .calls += 1;
    }

    /// Count one failure of `kind` against `name`.
    pub(crate) fn record_tool_error(&self, name: &str, kind: ToolFailureKind) {
        let mut map = self.tool_stats.lock().unwrap();
        let counters = map.entry(name.to_string()).or_default();
        match kind {
            ToolFailureKind::ToolNotFound => counters.not_found += 1,
            ToolFailureKind::ExecutionFailed => counters.execution_failed += 1,
            ToolFailureKind::SchemaValidationFailed => counters.schema_failed += 1,
        }
    }

    /// Get per-path open and failure counts, sorted by path.
    ///
    /// A path may name a directory rather than a file, since the search tools
    /// accept either.
    pub fn file_stats(&self) -> BTreeMap<String, FileStat> {
        self.file_stats
            .lock()
            .unwrap()
            .iter()
            .map(|(path, counters)| (path.clone(), counters.into()))
            .collect()
    }

    /// Count one successful open of `path`.
    pub(crate) fn record_file_open(&self, path: &str) {
        self.file_stats
            .lock()
            .unwrap()
            .entry(path.to_string())
            .or_default()
            .opens += 1;
    }

    /// Count one failed open naming `path`. It counts as an open as well, so
    /// `opens` is the attempt count and `error_rate` divides by it.
    pub(crate) fn record_file_open_failed(&self, path: &str) {
        let mut map = self.file_stats.lock().unwrap();
        let counters = map.entry(path.to_string()).or_default();
        counters.opens += 1;
        counters.failed += 1;
    }

    /// Get per-operation attempt and failure counts, sorted by operation.
    pub fn knowledge_stats(&self) -> BTreeMap<String, KnowledgeStat> {
        self.knowledge_stats
            .lock()
            .unwrap()
            .iter()
            .map(|(op, counters)| (op.clone(), counters.into()))
            .collect()
    }

    /// Count one knowledge operation, as `manage_knowledge` reports it.
    pub(crate) fn record_knowledge(&self, op: KnowledgeOp) {
        self.knowledge_stats
            .lock()
            .unwrap()
            .entry(op.to_string())
            .or_default()
            .attempts += 1;
    }

    /// Count one operation that did not go through. It counts as an attempt as
    /// well, so `attempts` is the attempt count and `error_rate` divides by it.
    pub(crate) fn record_knowledge_failed(&self, op: KnowledgeOp) {
        let mut map = self.knowledge_stats.lock().unwrap();
        let counters = map.entry(op.to_string()).or_default();
        counters.attempts += 1;
        counters.failed += 1;
    }

    /// Get per-model requests, failures, and token usage, sorted by model
    /// name.
    pub fn model_stats(&self) -> BTreeMap<String, ModelStat> {
        self.model_stats
            .lock()
            .unwrap()
            .iter()
            .map(|(name, counters)| (name.clone(), counters.into()))
            .collect()
    }

    /// Count one response against `model`.
    pub(crate) fn record_model_request(&self, model: &str, usage: &TokenUsage) {
        let mut map = self.model_stats.lock().unwrap();
        let counters = map.entry(model.to_string()).or_default();
        counters.requests += 1;
        counters.input_tokens += usage.input_tokens;
        counters.output_tokens += usage.output_tokens;
    }

    /// Count one failed request against `model`. It counts as a request as
    /// well, so `requests` is the attempt count.
    pub(crate) fn record_model_request_failed(&self, model: &str) {
        let mut map = self.model_stats.lock().unwrap();
        let counters = map.entry(model.to_string()).or_default();
        counters.requests += 1;
        counters.failed += 1;
    }

    /// Record an event.
    ///
    /// Every kind is counted by its name, so a new variant needs no arm here.
    /// Only the measures that read an event's payload do.
    pub(crate) fn record_event(&self, kind: &EventKind, key: &str, labels: &[String], agent: &str) {
        // The per-ticket series belongs to the compaction estimator rather
        // than to the figures, so it is the one measure that stays run-wide.
        if let EventKind::RequestFinished { usage, .. } = kind {
            self.record_usage(key, usage.clone());
        }
        // One walk of the labels for every measure, not one walk per measure.
        self.record_scoped(labels, agent, |s| {
            s.count_event(kind.name());
            match kind {
                EventKind::RequestFinished { model, usage } => {
                    s.record_tokens(usage.input_tokens, usage.output_tokens);
                    s.record_model_request(model, usage);
                }
                EventKind::RequestFailed { model, .. } => s.record_model_request_failed(model),
                EventKind::ToolCallStarted { tool_name, .. } => s.record_tool_call(tool_name),
                EventKind::ToolCallFailed {
                    tool_name, reason, ..
                } => s.record_tool_error(tool_name, *reason),
                EventKind::FileOpenFinished { path } => s.record_file_open(path),
                EventKind::FileOpenFailed { path } => s.record_file_open_failed(path),
                EventKind::KnowledgeUsed { op } => s.record_knowledge(*op),
                EventKind::KnowledgeFailed { op } => s.record_knowledge_failed(*op),
                _ => {}
            }
        });
    }

    /// Apply `f` to the run-wide statistics, to each label slice, and to the
    /// agent's slice. An event about execution itself carries no agent name.
    fn record_scoped(&self, labels: &[String], agent: &str, f: impl Fn(&Stats)) {
        f(self);
        for label in labels {
            f(&self.slice_for_label(label));
        }
        if !agent.is_empty() {
            f(&self.slice_for_agent(agent));
        }
    }

    /// Add one to this event kind's count.
    fn count_event(&self, name: &str) {
        *self
            .event_counts
            .lock()
            .unwrap()
            .entry(name.to_string())
            .or_default() += 1;
    }

    /// Add a response's token counts to the totals.
    fn record_tokens(&self, input_tokens: u64, output_tokens: u64) {
        self.input_tokens.fetch_add(input_tokens, Ordering::Relaxed);
        self.output_tokens
            .fetch_add(output_tokens, Ordering::Relaxed);
    }

    /// Record a ticket transition. Going from `Todo` to `InProgress` sets the
    /// start time, and reaching a terminal status adds to the duration totals.
    pub(crate) fn record_transition(
        &self,
        prev: Status,
        next: Status,
        now: u64,
        (ticket_duration, work_duration): (Duration, Duration),
    ) {
        if prev == next {
            return;
        }
        if prev == Status::Todo && next == Status::InProgress {
            self.record_started(now);
        }
        match next {
            Status::Finished => self.record_finished(ticket_duration, work_duration),
            Status::Failed => self.record_failed(ticket_duration, work_duration),
            _ => {}
        }
    }

    /// Mirror a terminal transition onto every label slice the ticket carries.
    ///
    /// The start time is deliberately not mirrored, so `run_duration()` reads
    /// `None` on a slice.
    pub(crate) fn record_transition_for(
        &self,
        labels: &[String],
        agent: &str,
        next: Status,
        (ticket_duration, work_duration): (Duration, Duration),
    ) {
        if !matches!(next, Status::Finished | Status::Failed) {
            return;
        }
        let slices = labels
            .iter()
            .map(|label| self.slice_for_label(label))
            .chain((!agent.is_empty()).then(|| self.slice_for_agent(agent)));
        for slice in slices {
            match next {
                Status::Finished => slice.record_finished(ticket_duration, work_duration),
                Status::Failed => slice.record_failed(ticket_duration, work_duration),
                _ => unreachable!(),
            }
        }
    }

    /// Get how many turns ran.
    pub fn turns(&self) -> u64 {
        self.event_count("turn_started")
    }

    /// Get how many responses arrived.
    pub fn requests(&self) -> u64 {
        self.event_count("request_finished")
    }

    /// Get the tool-call count.
    pub fn tool_calls(&self) -> u64 {
        self.event_count("tool_call_started")
    }

    /// Get the failed-request count.
    pub fn requests_failed(&self) -> u64 {
        self.event_count("request_failed")
    }

    /// Get one event kind's count, by its snake_case name.
    fn event_count(&self, name: &str) -> u64 {
        self.event_counts
            .lock()
            .unwrap()
            .get(name)
            .copied()
            .unwrap_or(0)
    }

    /// Get per-event counts, sorted by event name.
    pub fn event_counts(&self) -> BTreeMap<String, u64> {
        self.event_counts
            .lock()
            .unwrap()
            .iter()
            .map(|(name, count)| (name.clone(), *count))
            .collect()
    }

    /// Get the input tokens across requests.
    pub fn input_tokens(&self) -> u64 {
        self.input_tokens.load(Ordering::Relaxed)
    }

    /// Get the output tokens across requests.
    pub fn output_tokens(&self) -> u64 {
        self.output_tokens.load(Ordering::Relaxed)
    }

    /// Get how many tickets were created.
    pub fn tickets_created(&self) -> u64 {
        self.tickets_created.load(Ordering::Relaxed)
    }

    /// Get how many tickets finished successfully.
    pub fn tickets_finished(&self) -> u64 {
        self.tickets_finished.load(Ordering::Relaxed)
    }

    /// Get how many tickets failed.
    pub fn tickets_failed(&self) -> u64 {
        self.tickets_failed.load(Ordering::Relaxed)
    }

    /// Get the elapsed duration, which keeps growing while agents work and
    /// stops when execution ends. `None` until the first ticket starts.
    pub fn run_duration(&self) -> Option<Duration> {
        let s = self.started_at.load(Ordering::Relaxed);
        if s == 0 {
            return None;
        }
        let f = self.finished_at.load(Ordering::Relaxed);
        if f != 0 && f >= s {
            return Some(Duration::from_millis(f - s));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis() as u64;
        Some(Duration::from_millis(now.saturating_sub(s)))
    }

    /// Get `finished / (finished + failed)`, or `None` before any ticket is
    /// resolved.
    pub fn tickets_success_rate(&self) -> Option<f64> {
        let done = self.tickets_finished.load(Ordering::Relaxed);
        let failed = self.tickets_failed.load(Ordering::Relaxed);
        let total = done + failed;
        if total == 0 {
            None
        } else {
            Some(done as f64 / total as f64)
        }
    }

    /// Get the total time from creation to resolution.
    pub fn total_ticket_duration(&self) -> Duration {
        Duration::from_secs(self.total_ticket_duration.load(Ordering::Relaxed))
    }

    /// Get the average time from creation to resolution, or `None` before any
    /// ticket finishes.
    pub fn avg_ticket_duration(&self) -> Option<Duration> {
        let n = self.tickets_finished.load(Ordering::Relaxed)
            + self.tickets_failed.load(Ordering::Relaxed);
        if n == 0 {
            None
        } else {
            let secs = self.total_ticket_duration.load(Ordering::Relaxed);
            Some(Duration::from_secs(secs / n))
        }
    }

    /// Get the total time agents held tickets. With agents working in
    /// parallel this can exceed the elapsed duration.
    pub fn total_work_duration(&self) -> Duration {
        Duration::from_secs(self.total_work_duration.load(Ordering::Relaxed))
    }

    /// Get the average time an agent held a ticket, or `None` before any
    /// ticket finishes.
    pub fn avg_work_duration(&self) -> Option<Duration> {
        let n = self.tickets_finished.load(Ordering::Relaxed)
            + self.tickets_failed.load(Ordering::Relaxed);
        if n == 0 {
            None
        } else {
            let secs = self.total_work_duration.load(Ordering::Relaxed);
            Some(Duration::from_secs(secs / n))
        }
    }

    /// Record when execution ended. A later call overwrites the earlier one.
    pub(crate) fn record_run_finished(&self, when: u64) {
        self.finished_at.store(when, Ordering::Relaxed);
    }

    /// Rebuild the statistics from tickets already read off disk, for when
    /// `stats.json` is missing or unreadable.
    pub(crate) fn derive(tickets: &HashMap<String, crate::agents::tickets::Ticket>) -> Self {
        let stats = Stats::new();
        for t in tickets.values() {
            // No agent scope: a rebuilt ticket cannot say which agent
            // worked it, and an empty slice beats a half-filled one.
            stats.record_scoped(&t.labels, "", |s| s.record_created());
            let ticket_dur = ticket_duration(t).unwrap_or_default();
            let work_dur = work_duration(t).unwrap_or_default();
            match t.status {
                Status::Finished => {
                    stats.record_scoped(&t.labels, "", |s| s.record_finished(ticket_dur, work_dur));
                }
                Status::Failed => {
                    stats.record_scoped(&t.labels, "", |s| s.record_failed(ticket_dur, work_dur));
                }
                Status::Todo | Status::InProgress => {}
            }
        }
        stats
    }

    /// Save to `<dir>/stats.json`.
    pub(crate) fn load(dir: &std::path::Path) -> std::io::Result<Self> {
        <Self as crate::persistence::Persist>::load(dir, &())
    }
}

impl Stats {
    pub(crate) const FILE: &'static str = "stats.json";
}

impl crate::persistence::Persist for Stats {
    type Key = ();

    fn save(&self, dir: &std::path::Path) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        let body = serde_json::to_vec_pretty(&value).map_err(std::io::Error::other)?;
        crate::persistence::write_atomic(&dir.join(Self::FILE), &body)
    }

    fn load(dir: &std::path::Path, _: &Self::Key) -> std::io::Result<Self> {
        let bytes = std::fs::read(dir.join(Self::FILE))?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        let stats = Stats::new();
        stats.load_fields(&value);
        if let Some(labels) = value.get("labels").and_then(|v| v.as_object()) {
            for (name, body) in labels {
                stats.slice_for_label(name).load_fields(body);
            }
        }
        if let Some(agents) = value.get("agents").and_then(|v| v.as_object()) {
            for (name, body) in agents {
                stats.slice_for_agent(name).load_fields(body);
            }
        }
        Ok(stats)
    }
}

impl Serialize for Stats {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let events = self.event_counts();
        let labels = self.label_stats.lock().unwrap();
        let agents = self.agent_stats.lock().unwrap();
        let tools = self.tool_stats();
        let files = self.file_stats();
        let knowledge = self.knowledge_stats();
        let models = self.model_stats();
        let len = 15
            + usize::from(!events.is_empty())
            + usize::from(!labels.is_empty())
            + usize::from(!agents.is_empty())
            + usize::from(!tools.is_empty())
            + usize::from(!files.is_empty())
            + usize::from(!knowledge.is_empty())
            + usize::from(!models.is_empty());
        let mut st = serializer.serialize_struct("Stats", len)?;
        st.serialize_field("turns", &self.turns())?;
        st.serialize_field("requests", &self.requests())?;
        st.serialize_field("tool_calls", &self.tool_calls())?;
        st.serialize_field("requests_failed", &self.requests_failed())?;
        st.serialize_field("input_tokens", &self.input_tokens())?;
        st.serialize_field("output_tokens", &self.output_tokens())?;
        st.serialize_field("tickets_created", &self.tickets_created())?;
        st.serialize_field("tickets_finished", &self.tickets_finished())?;
        st.serialize_field("tickets_failed", &self.tickets_failed())?;
        st.serialize_field(
            "total_ticket_duration_secs",
            &self.total_ticket_duration.load(Ordering::Relaxed),
        )?;
        st.serialize_field(
            "total_work_duration_secs",
            &self.total_work_duration.load(Ordering::Relaxed),
        )?;
        st.serialize_field("tickets_success_rate", &self.tickets_success_rate())?;
        st.serialize_field(
            "run_duration_secs",
            &self.run_duration().map(|d| d.as_secs_f64()),
        )?;
        st.serialize_field(
            "avg_ticket_duration_secs",
            &self.avg_ticket_duration().map(|d| d.as_secs_f64()),
        )?;
        st.serialize_field(
            "avg_work_duration_secs",
            &self.avg_work_duration().map(|d| d.as_secs_f64()),
        )?;
        if !events.is_empty() {
            st.serialize_field("events", &events)?;
        }
        if !labels.is_empty() {
            let nested: BTreeMap<&String, &Stats> =
                labels.iter().map(|(k, v)| (k, v.as_ref())).collect();
            st.serialize_field("labels", &nested)?;
        }
        if !agents.is_empty() {
            let nested: BTreeMap<&String, &Stats> =
                agents.iter().map(|(k, v)| (k, v.as_ref())).collect();
            st.serialize_field("agents", &nested)?;
        }
        if !tools.is_empty() {
            // The four subject sections are hand-built the same way: raw
            // counters round-trip through load, while errors/error_rate are
            // derived, emitted for readers, and ignored on load.
            let nested: BTreeMap<&String, serde_json::Value> = tools
                .iter()
                .map(|(name, stat)| {
                    (
                        name,
                        serde_json::json!({
                            "calls": stat.calls,
                            "not_found": stat.not_found,
                            "execution_failed": stat.execution_failed,
                            "schema_failed": stat.schema_failed,
                            "errors": stat.errors(),
                            "error_rate": stat.error_rate(),
                        }),
                    )
                })
                .collect();
            st.serialize_field("tools", &nested)?;
        }
        if !files.is_empty() {
            let nested: BTreeMap<&String, serde_json::Value> = files
                .iter()
                .map(|(path, stat)| {
                    (
                        path,
                        serde_json::json!({
                            "opens": stat.opens,
                            "failed": stat.failed,
                            "errors": stat.errors(),
                            "error_rate": stat.error_rate(),
                        }),
                    )
                })
                .collect();
            st.serialize_field("files", &nested)?;
        }
        if !knowledge.is_empty() {
            let nested: BTreeMap<&String, serde_json::Value> = knowledge
                .iter()
                .map(|(op, stat)| {
                    (
                        op,
                        serde_json::json!({
                            "attempts": stat.attempts,
                            "failed": stat.failed,
                            "errors": stat.errors(),
                            "error_rate": stat.error_rate(),
                        }),
                    )
                })
                .collect();
            st.serialize_field("knowledge", &nested)?;
        }
        if !models.is_empty() {
            let nested: BTreeMap<&String, serde_json::Value> = models
                .iter()
                .map(|(name, stat)| {
                    (
                        name,
                        serde_json::json!({
                            "requests": stat.requests,
                            "failed": stat.failed,
                            "input_tokens": stat.input_tokens,
                            "output_tokens": stat.output_tokens,
                            "errors": stat.errors(),
                            "error_rate": stat.error_rate(),
                        }),
                    )
                })
                .collect();
            st.serialize_field("models", &nested)?;
        }
        st.end()
    }
}

impl Stats {
    fn load_fields(&self, value: &serde_json::Value) {
        let get = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        // The named loop counters ("turns", "requests", ...) are derived
        // views of this map; only the map round-trips.
        if let Some(events) = value.get("events").and_then(|v| v.as_object()) {
            let mut map = self.event_counts.lock().unwrap();
            for (name, count) in events {
                if let Some(count) = count.as_u64() {
                    map.insert(name.clone(), count);
                }
            }
        }
        self.input_tokens
            .store(get("input_tokens"), Ordering::Relaxed);
        self.output_tokens
            .store(get("output_tokens"), Ordering::Relaxed);
        self.tickets_created
            .store(get("tickets_created"), Ordering::Relaxed);
        self.tickets_finished
            .store(get("tickets_finished"), Ordering::Relaxed);
        self.tickets_failed
            .store(get("tickets_failed"), Ordering::Relaxed);
        self.total_ticket_duration
            .store(get("total_ticket_duration_secs"), Ordering::Relaxed);
        self.total_work_duration
            .store(get("total_work_duration_secs"), Ordering::Relaxed);

        if let Some(tools) = value.get("tools").and_then(|v| v.as_object()) {
            let mut map = self.tool_stats.lock().unwrap();
            for (name, body) in tools {
                let get = |key: &str| body.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                map.insert(
                    name.clone(),
                    ToolCounters {
                        calls: get("calls"),
                        not_found: get("not_found"),
                        execution_failed: get("execution_failed"),
                        schema_failed: get("schema_failed"),
                    },
                );
            }
        }
        if let Some(files) = value.get("files").and_then(|v| v.as_object()) {
            let mut map = self.file_stats.lock().unwrap();
            for (path, body) in files {
                let get = |key: &str| body.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                map.insert(
                    path.clone(),
                    FileCounters {
                        opens: get("opens"),
                        failed: get("failed"),
                    },
                );
            }
        }
        if let Some(knowledge) = value.get("knowledge").and_then(|v| v.as_object()) {
            let mut map = self.knowledge_stats.lock().unwrap();
            for (op, body) in knowledge {
                let get = |key: &str| body.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                map.insert(
                    op.clone(),
                    KnowledgeCounters {
                        attempts: get("attempts"),
                        failed: get("failed"),
                    },
                );
            }
        }
        if let Some(models) = value.get("models").and_then(|v| v.as_object()) {
            let mut map = self.model_stats.lock().unwrap();
            for (name, body) in models {
                let get = |key: &str| body.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                map.insert(
                    name.clone(),
                    ModelCounters {
                        requests: get("requests"),
                        failed: get("failed"),
                        input_tokens: get("input_tokens"),
                        output_tokens: get("output_tokens"),
                    },
                );
            }
        }
    }
}

fn ticket_duration(t: &crate::agents::tickets::Ticket) -> Option<Duration> {
    let end = t.finished_at.or(t.failed_at)?;
    Some(Duration::from_millis(end.saturating_sub(t.created_at)))
}

fn work_duration(t: &crate::agents::tickets::Ticket) -> Option<Duration> {
    let end = t.finished_at.or(t.failed_at)?;
    let start = t.started_at?;
    Some(Duration::from_millis(end.saturating_sub(start)))
}

impl Default for Stats {
    fn default() -> Self {
        Self::new()
    }
}

/// Ticket lifecycle. The store writes these directly rather than through
/// events, since a transition carries a duration no event reports.
impl Stats {
    pub(crate) fn record_created(&self) {
        self.tickets_created.fetch_add(1, Ordering::Relaxed);
    }

    /// The first call wins. A later claim leaves the original start time
    /// untouched.
    pub(crate) fn record_started(&self, when: u64) {
        let _ = self
            .started_at
            .compare_exchange(0, when, Ordering::Relaxed, Ordering::Relaxed);
    }

    pub(crate) fn record_finished(&self, ticket_duration: Duration, work_duration: Duration) {
        self.tickets_finished.fetch_add(1, Ordering::Relaxed);
        self.total_ticket_duration
            .fetch_add(ticket_duration.as_secs(), Ordering::Relaxed);
        self.total_work_duration
            .fetch_add(work_duration.as_secs(), Ordering::Relaxed);
    }

    pub(crate) fn record_failed(&self, ticket_duration: Duration, work_duration: Duration) {
        self.tickets_failed.fetch_add(1, Ordering::Relaxed);
        self.total_ticket_duration
            .fetch_add(ticket_duration.as_secs(), Ordering::Relaxed);
        self.total_work_duration
            .fetch_add(work_duration.as_secs(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn() -> EventKind {
        EventKind::TurnStarted
    }

    fn request(input_tokens: u64, output_tokens: u64) -> EventKind {
        EventKind::RequestFinished {
            model: "m".into(),
            usage: TokenUsage {
                input_tokens,
                output_tokens,
            },
        }
    }

    fn tool_call() -> EventKind {
        EventKind::ToolCallStarted {
            tool_name: "bash".into(),
            call_id: "c1".into(),
            input: serde_json::Value::Null,
        }
    }

    fn provider_error() -> EventKind {
        EventKind::RequestFailed {
            model: "m".into(),
            reason: crate::providers::RequestErrorKind::ConnectionFailed,
            message: "boom".into(),
        }
    }

    #[test]
    fn fresh_stats_are_zero() {
        let s = Stats::new();
        assert_eq!(s.turns(), 0);
        assert_eq!(s.requests(), 0);
        assert_eq!(s.tool_calls(), 0);
        assert_eq!(s.requests_failed(), 0);
        assert_eq!(s.input_tokens(), 0);
        assert_eq!(s.output_tokens(), 0);
        assert_eq!(s.tickets_created(), 0);
        assert_eq!(s.tickets_finished(), 0);
        assert_eq!(s.tickets_failed(), 0);
        assert_eq!(s.total_ticket_duration(), Duration::ZERO);
        assert_eq!(s.total_work_duration(), Duration::ZERO);
        assert!(s.run_duration().is_none());
        assert!(s.avg_ticket_duration().is_none());
        assert!(s.avg_work_duration().is_none());
        assert!(s.tickets_success_rate().is_none());
    }

    #[test]
    fn event_counts_show_up_in_reads() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &[], "");
        s.record_event(&turn(), "KEY", &[], "");
        s.record_event(&request(10, 5), "KEY", &[], "");
        s.record_event(&request(2, 1), "KEY", &[], "");
        s.record_event(&tool_call(), "KEY", &[], "");
        s.record_event(&provider_error(), "KEY", &[], "");

        assert_eq!(s.turns(), 2);
        assert_eq!(s.requests(), 2);
        assert_eq!(s.tool_calls(), 1);
        assert_eq!(s.requests_failed(), 1);
        assert_eq!(s.input_tokens(), 12);
        assert_eq!(s.output_tokens(), 6);
        assert_eq!(s.event_counts()["turn_started"], 2);
    }

    #[test]
    fn ticket_stats_writes_show_up_in_reads() {
        let s = Stats::new();
        s.record_created();
        s.record_created();
        s.record_finished(Duration::from_secs(3), Duration::from_secs(2));
        s.record_failed(Duration::from_secs(5), Duration::from_secs(4));

        assert_eq!(s.tickets_created(), 2);
        assert_eq!(s.tickets_finished(), 1);
        assert_eq!(s.tickets_failed(), 1);
        assert_eq!(s.total_ticket_duration(), Duration::from_secs(8));
        assert_eq!(s.total_work_duration(), Duration::from_secs(6));
    }

    #[test]
    fn record_started_first_call_wins() {
        let s = Stats::new();
        s.record_started(1_000);
        s.record_started(2_000);
        s.record_started(3_000);
        s.record_run_finished(4_500);
        assert_eq!(s.run_duration(), Some(Duration::from_millis(3500)));
    }

    #[test]
    fn run_duration_freezes_at_finish() {
        let s = Stats::new();
        assert!(s.run_duration().is_none());
        s.record_started(1_000);
        // Live before finish: anchored at started_at = 1_000ms epoch, so the
        // delta to "now" is enormous. We just check it's some duration.
        assert!(s.run_duration().is_some());
        s.record_run_finished(2_500);
        assert_eq!(s.run_duration(), Some(Duration::from_millis(1500)));
        // Stays frozen on a subsequent call.
        assert_eq!(s.run_duration(), Some(Duration::from_millis(1500)));
    }

    #[test]
    fn tickets_success_rate_done_failed_mix() {
        let s = Stats::new();
        s.record_finished(Duration::from_secs(1), Duration::from_secs(1));
        s.record_finished(Duration::from_secs(2), Duration::from_secs(2));
        s.record_failed(Duration::from_secs(3), Duration::from_secs(3));
        let rate = s.tickets_success_rate().unwrap();
        assert!((rate - 2.0 / 3.0).abs() < 1e-9, "rate = {rate}");
    }

    #[test]
    fn tickets_success_rate_none_when_nothing_finished() {
        let s = Stats::new();
        assert!(s.tickets_success_rate().is_none());
    }

    #[test]
    fn avg_ticket_duration_is_arithmetic_mean() {
        let s = Stats::new();
        s.record_finished(Duration::from_secs(2), Duration::from_secs(2));
        s.record_failed(Duration::from_secs(4), Duration::from_secs(4));
        assert_eq!(s.avg_ticket_duration(), Some(Duration::from_secs(3)));
    }

    #[test]
    fn avg_work_duration_is_arithmetic_mean() {
        let s = Stats::new();
        s.record_finished(Duration::from_secs(3), Duration::from_secs(2));
        s.record_failed(Duration::from_secs(5), Duration::from_secs(4));
        assert_eq!(s.avg_work_duration(), Some(Duration::from_secs(3)));
    }

    #[test]
    fn stats_for_label_returns_same_slice_on_repeat_access() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &["scan".into()], "");
        let a = s.stats_for_label("scan");
        let b = s.stats_for_label("scan");
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn reading_an_unrecorded_label_or_agent_creates_no_slice() {
        let s = Stats::new();
        assert_eq!(s.stats_for_label("typo").turns(), 0);
        assert_eq!(s.stats_for_agent("typo").turns(), 0);

        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("labels").is_none());
        assert!(value.get("agents").is_none());
    }

    #[test]
    fn record_event_mirrors_onto_label_slices() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &["scan".into()], "");
        s.record_event(&request(10, 5), "KEY", &["scan".into()], "");
        let slice = s.stats_for_label("scan");
        assert_eq!(slice.turns(), 1);
        assert_eq!(slice.input_tokens(), 10);
        assert_eq!(slice.output_tokens(), 5);
        assert_eq!(s.turns(), 1);
        assert_eq!(s.input_tokens(), 10);
        // A label the events never named stays empty.
        assert_eq!(s.stats_for_label("other").turns(), 0);
    }

    #[test]
    fn record_event_mirrors_onto_the_agent_slice() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &["scan".into()], "scout");
        s.record_event(&request(10, 5), "KEY", &["scan".into()], "scout");
        let slice = s.stats_for_agent("scout");
        assert_eq!(slice.turns(), 1);
        assert_eq!(slice.input_tokens(), 10);
        assert_eq!(s.stats_for_agent("writer").turns(), 0);
    }

    #[test]
    fn stats_for_agent_and_stats_for_label_do_not_share_a_slice() {
        // A claim stamps the agent's name onto the ticket's labels, so the
        // two namespaces collide unless the maps stay separate.
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &["scout".into()], "scout");
        assert_eq!(s.stats_for_label("scout").turns(), 1);
        assert_eq!(s.stats_for_agent("scout").turns(), 1);
        assert!(!Arc::ptr_eq(
            &s.stats_for_label("scout"),
            &s.stats_for_agent("scout"),
        ));
    }

    #[test]
    fn run_level_events_reach_no_agent_slice() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &[], "");
        assert_eq!(s.turns(), 1);
        assert_eq!(s.stats_for_agent("").turns(), 0);
    }

    #[test]
    fn stats_for_label_slice_run_duration_is_none() {
        let s = Stats::new();
        let slice = s.stats_for_label("scan");
        slice.record_finished(Duration::from_secs(2), Duration::from_secs(1));
        assert!(slice.run_duration().is_none());
        assert_eq!(slice.tickets_finished(), 1);
    }

    #[test]
    fn total_work_duration_can_exceed_run_duration_with_concurrency() {
        // Two tickets, each 5s of work, finished in a 6s window:
        // models 2 agents working in parallel.
        let s = Stats::new();
        s.record_started(1_000);
        s.record_finished(Duration::from_secs(5), Duration::from_secs(5));
        s.record_finished(Duration::from_secs(5), Duration::from_secs(5));
        s.record_run_finished(7_000);
        assert_eq!(s.run_duration(), Some(Duration::from_secs(6)));
        assert_eq!(s.total_work_duration(), Duration::from_secs(10));
    }

    #[test]
    fn stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_event(&turn(), "KEY", &[], "");
        s.record_event(&turn(), "KEY", &[], "");
        s.record_event(&request(100, 50), "KEY", &[], "");
        s.record_event(&tool_call(), "KEY", &[], "");
        s.record_event(&provider_error(), "KEY", &[], "");
        s.record_created();
        s.record_finished(Duration::from_secs(7), Duration::from_secs(5));
        s.record_failed(Duration::from_secs(3), Duration::from_secs(2));

        let slice = s.slice_for_label("scan");
        slice.record_event(&turn(), "KEY", &[], "");
        slice.record_event(&request(40, 20), "KEY", &[], "");
        slice.record_created();
        slice.record_finished(Duration::from_secs(4), Duration::from_secs(3));

        let agent_slice = s.slice_for_agent("seeker");
        agent_slice.record_event(&turn(), "KEY", &[], "");
        agent_slice.record_event(&request(30, 10), "KEY", &[], "");
        agent_slice.record_created();
        agent_slice.record_failed(Duration::from_secs(6), Duration::from_secs(2));

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();
        assert_eq!(restored.turns(), 2);
        assert_eq!(restored.requests(), 1);
        assert_eq!(restored.tool_calls(), 1);
        assert_eq!(restored.requests_failed(), 1);
        assert_eq!(restored.input_tokens(), 100);
        assert_eq!(restored.output_tokens(), 50);
        assert_eq!(restored.tickets_created(), 1);
        assert_eq!(restored.tickets_finished(), 1);
        assert_eq!(restored.tickets_failed(), 1);
        assert_eq!(restored.total_ticket_duration(), Duration::from_secs(10));
        assert_eq!(restored.total_work_duration(), Duration::from_secs(7));

        let restored_slice = restored.stats_for_label("scan");
        assert_eq!(restored_slice.turns(), 1);
        assert_eq!(restored_slice.input_tokens(), 40);
        assert_eq!(restored_slice.tickets_finished(), 1);
        assert_eq!(
            restored_slice.total_ticket_duration(),
            Duration::from_secs(4)
        );

        let restored_agent = restored.stats_for_agent("seeker");
        assert_eq!(restored_agent.turns(), 1);
        assert_eq!(restored_agent.input_tokens(), 30);
        assert_eq!(restored_agent.tickets_failed(), 1);
        assert_eq!(
            restored_agent.total_ticket_duration(),
            Duration::from_secs(6)
        );
    }

    #[test]
    fn stats_serializes_raw_counter_fields() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &[], "");
        s.record_event(&request(100, 50), "KEY", &[], "");
        s.record_event(&tool_call(), "KEY", &[], "");
        s.record_event(&provider_error(), "KEY", &[], "");
        s.record_created();
        s.record_finished(Duration::from_secs(7), Duration::from_secs(5));

        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["turns"], 1);
        assert_eq!(value["requests"], 1);
        assert_eq!(value["tool_calls"], 1);
        assert_eq!(value["requests_failed"], 1);
        assert_eq!(value["input_tokens"], 100);
        assert_eq!(value["output_tokens"], 50);
        assert_eq!(value["tickets_created"], 1);
        assert_eq!(value["tickets_finished"], 1);
        assert_eq!(value["total_ticket_duration_secs"], 7);
        assert_eq!(value["total_work_duration_secs"], 5);
        assert_eq!(value["events"]["turn_started"], 1);
        assert_eq!(value["events"]["request_finished"], 1);
    }

    #[test]
    fn stats_writes_each_duration_sum_once() {
        let s = Stats::new();
        s.record_created();
        s.record_finished(Duration::from_secs(7), Duration::from_secs(5));

        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("ticket_duration_secs").is_none());
        assert!(value.get("work_duration_secs").is_none());
    }

    #[test]
    fn stats_omits_events_when_empty() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("events").is_none());
    }

    #[test]
    fn stats_serializes_derived_run_duration_seconds() {
        let s = Stats::new();
        s.record_started(1_000);
        s.record_run_finished(3_500);
        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["run_duration_secs"].as_f64().unwrap(), 2.5);
    }

    #[test]
    fn stats_serializes_run_duration_secs_as_null_when_unstarted() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value["run_duration_secs"].is_null());
    }

    #[test]
    fn stats_serializes_tickets_success_rate_when_tickets_present() {
        let s = Stats::new();
        s.record_finished(Duration::from_secs(1), Duration::from_secs(1));
        s.record_finished(Duration::from_secs(1), Duration::from_secs(1));
        s.record_finished(Duration::from_secs(1), Duration::from_secs(1));
        s.record_failed(Duration::from_secs(1), Duration::from_secs(1));
        let value = serde_json::to_value(&s).unwrap();
        let rate = value["tickets_success_rate"].as_f64().unwrap();
        assert!((rate - 0.75).abs() < 1e-9, "got {rate}");
    }

    #[test]
    fn stats_serializes_labels_as_nested_object() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &["scan".into()], "");
        s.record_event(&request(40, 20), "KEY", &["scan".into()], "");
        let value = serde_json::to_value(&s).unwrap();
        let labels = value["labels"].as_object().unwrap();
        assert_eq!(labels["scan"]["turns"], 1);
        assert_eq!(labels["scan"]["input_tokens"], 40);
        // A slice carries its own subject sections, and `load` reads them
        // back through the same path the run-wide statistics use.
        assert_eq!(labels["scan"]["models"]["m"]["requests"], 1);
    }

    #[test]
    fn slice_subject_maps_round_trip_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_event(&tool_call(), "KEY", &["scan".into()], "scout");
        s.record_event(&provider_error(), "KEY", &["scan".into()], "scout");

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let label = restored.stats_for_label("scan");
        assert_eq!(label.tool_stats()["bash"].calls, 1);
        assert_eq!(label.model_stats()["m"].failed, 1);
        let agent = restored.stats_for_agent("scout");
        assert_eq!(agent.tool_stats()["bash"].calls, 1);
        assert_eq!(agent.model_stats()["m"].failed, 1);
    }

    #[test]
    fn stats_omits_labels_when_empty() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("labels").is_none());
    }

    #[test]
    fn reset_usage_clears_one_ticket_without_touching_others() {
        let s = Stats::new();
        s.record_usage(
            "TICKET-1",
            TokenUsage {
                input_tokens: 100,
                output_tokens: 10,
            },
        );
        s.record_usage(
            "TICKET-2",
            TokenUsage {
                input_tokens: 200,
                output_tokens: 20,
            },
        );

        s.reset_usage("TICKET-1");

        assert!(s.usage_for_ticket("TICKET-1").is_empty());
        let t2 = s.usage_for_ticket("TICKET-2");
        assert_eq!(t2.len(), 1);
        assert_eq!(t2[0].input_tokens, 200);
    }

    #[test]
    fn tool_stats_records_calls_and_errors_per_tool() {
        let s = Stats::new();
        s.record_tool_call("edit_file");
        s.record_tool_call("edit_file");
        s.record_tool_call("bash");
        s.record_tool_error("edit_file", ToolFailureKind::SchemaValidationFailed);
        s.record_tool_error("bash", ToolFailureKind::ExecutionFailed);
        s.record_tool_error("ghost", ToolFailureKind::ToolNotFound);

        let tools = s.tool_stats();
        let edit = &tools["edit_file"];
        assert_eq!(edit.calls, 2);
        assert_eq!(edit.schema_failed, 1);
        assert_eq!(edit.errors(), 1);
        assert_eq!(tools["bash"].execution_failed, 1);
        assert_eq!(tools["ghost"].not_found, 1);
    }

    #[test]
    fn tool_stat_error_rate_is_errors_over_calls() {
        let s = Stats::new();
        s.record_tool_call("edit_file");
        s.record_tool_call("edit_file");
        s.record_tool_call("edit_file");
        s.record_tool_call("edit_file");
        s.record_tool_error("edit_file", ToolFailureKind::SchemaValidationFailed);

        let tools = s.tool_stats();
        assert_eq!(tools["edit_file"].error_rate(), Some(0.25));
    }

    #[test]
    fn tool_stat_error_rate_is_none_without_calls() {
        // A failure can be recorded for a name that never logged a call only
        // in malformed cases; the rate is then undefined rather than infinite.
        let s = Stats::new();
        s.record_tool_error("ghost", ToolFailureKind::ToolNotFound);
        assert!(s.tool_stats()["ghost"].error_rate().is_none());
    }

    #[test]
    fn tool_stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_tool_call("edit_file");
        s.record_tool_call("edit_file");
        s.record_tool_error("edit_file", ToolFailureKind::SchemaValidationFailed);
        s.record_tool_call("bash");
        s.record_tool_error("bash", ToolFailureKind::ExecutionFailed);

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let tools = restored.tool_stats();
        assert_eq!(tools["edit_file"].calls, 2);
        assert_eq!(tools["edit_file"].schema_failed, 1);
        assert_eq!(tools["bash"].execution_failed, 1);
    }

    #[test]
    fn stats_serializes_tools_as_nested_object() {
        let s = Stats::new();
        s.record_tool_call("edit_file");
        s.record_tool_call("edit_file");
        s.record_tool_error("edit_file", ToolFailureKind::SchemaValidationFailed);

        let value = serde_json::to_value(&s).unwrap();
        let tools = value["tools"].as_object().unwrap();
        assert_eq!(tools["edit_file"]["calls"], 2);
        assert_eq!(tools["edit_file"]["schema_failed"], 1);
        assert_eq!(tools["edit_file"]["errors"], 1);
        assert_eq!(tools["edit_file"]["error_rate"].as_f64().unwrap(), 0.5);
    }

    #[test]
    fn stats_omits_tools_when_empty() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("tools").is_none());
    }

    #[test]
    fn tool_stats_reach_the_label_and_agent_slices() {
        let s = Stats::new();
        s.record_event(&tool_call(), "KEY", &["scan".into()], "scout");

        assert_eq!(s.stats_for_label("scan").tool_stats()["bash"].calls, 1);
        assert_eq!(s.stats_for_agent("scout").tool_stats()["bash"].calls, 1);
        assert!(s.stats_for_label("audit").tool_stats().is_empty());
    }

    #[test]
    fn file_stats_records_opens_and_failures_per_path() {
        let s = Stats::new();
        s.record_file_open("src/lib.rs");
        s.record_file_open("src/lib.rs");
        s.record_file_open("src/main.rs");
        s.record_file_open_failed("src/missing.rs");
        s.record_file_open_failed("src/missing.rs");

        let files = s.file_stats();
        assert_eq!(files["src/lib.rs"].opens, 2);
        assert_eq!(files["src/lib.rs"].failed, 0);
        assert_eq!(files["src/lib.rs"].error_rate(), Some(0.0));
        assert_eq!(files["src/main.rs"].opens, 1);
        // A failed open is an open too, so the rate divides by every attempt.
        assert_eq!(files["src/missing.rs"].opens, 2);
        assert_eq!(files["src/missing.rs"].failed, 2);
        assert_eq!(files["src/missing.rs"].errors(), 2);
        assert_eq!(files["src/missing.rs"].error_rate(), Some(1.0));
    }

    #[test]
    fn file_stats_reach_the_label_slice() {
        let s = Stats::new();
        let open = EventKind::FileOpenFinished {
            path: "src/lib.rs".into(),
        };
        s.record_event(&open, "KEY", &["scan".into()], "");

        assert_eq!(
            s.stats_for_label("scan").file_stats()["src/lib.rs"].opens,
            1
        );
        assert!(s.stats_for_label("audit").file_stats().is_empty());
    }

    #[test]
    fn file_stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_file_open("src/lib.rs");
        s.record_file_open("src/lib.rs");
        s.record_file_open_failed("src/missing.rs");

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let files = restored.file_stats();
        assert_eq!(files["src/lib.rs"].opens, 2);
        assert_eq!(files["src/missing.rs"].opens, 1);
        assert_eq!(files["src/missing.rs"].failed, 1);
    }

    #[test]
    fn stats_serializes_files_as_nested_object() {
        let s = Stats::new();
        s.record_file_open("src/lib.rs");
        s.record_file_open("src/lib.rs");
        s.record_file_open_failed("src/lib.rs");

        let value = serde_json::to_value(&s).unwrap();
        let files = value["files"].as_object().unwrap();
        assert_eq!(files["src/lib.rs"]["opens"], 3);
        assert_eq!(files["src/lib.rs"]["failed"], 1);
        assert_eq!(files["src/lib.rs"]["errors"], 1);
    }

    #[test]
    fn stats_omits_files_when_empty() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("files").is_none());
    }

    #[test]
    fn knowledge_records_attempts_and_failures_per_operation() {
        let s = Stats::new();
        s.record_knowledge(KnowledgeOp::Write);
        s.record_knowledge(KnowledgeOp::Read);
        s.record_knowledge(KnowledgeOp::Read);
        s.record_knowledge(KnowledgeOp::Remove);
        s.record_knowledge(KnowledgeOp::List);
        s.record_knowledge_failed(KnowledgeOp::Read);

        let k = s.knowledge_stats();
        assert_eq!(k["write"].attempts, 1);
        // The failed read counts as a read as well.
        assert_eq!(k["read"].attempts, 3);
        assert_eq!(k["remove"].attempts, 1);
        assert_eq!(k["list"].attempts, 1);
        assert_eq!(k["read"].failed, 1);
        assert_eq!(k["read"].errors(), 1);
    }

    #[test]
    fn knowledge_stats_attribute_a_failure_to_its_operation() {
        let s = Stats::new();
        s.record_knowledge(KnowledgeOp::Write);
        s.record_knowledge_failed(KnowledgeOp::Read);

        let k = s.knowledge_stats();
        assert_eq!(k["read"].failed, 1);
        assert_eq!(k["read"].error_rate(), Some(1.0));
        assert_eq!(k["write"].failed, 0);
        assert_eq!(k["write"].error_rate(), Some(0.0));
    }

    #[test]
    fn knowledge_reaches_the_label_slice() {
        let s = Stats::new();
        let used = EventKind::KnowledgeUsed {
            op: KnowledgeOp::Write,
        };
        s.record_event(&used, "KEY", &["scan".into()], "");

        assert_eq!(
            s.stats_for_label("scan").knowledge_stats()["write"].attempts,
            1
        );
        assert!(s.stats_for_label("audit").knowledge_stats().is_empty());
    }

    #[test]
    fn knowledge_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_knowledge(KnowledgeOp::Write);
        s.record_knowledge(KnowledgeOp::Read);
        s.record_knowledge_failed(KnowledgeOp::Read);

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let k = restored.knowledge_stats();
        assert_eq!(k["write"].attempts, 1);
        assert_eq!(k["read"].attempts, 2);
        assert_eq!(k["read"].failed, 1);
    }

    #[test]
    fn stats_serializes_knowledge_as_nested_object() {
        let s = Stats::new();
        s.record_knowledge(KnowledgeOp::Write);
        s.record_knowledge_failed(KnowledgeOp::Read);

        let value = serde_json::to_value(&s).unwrap();
        let knowledge = value["knowledge"].as_object().unwrap();
        assert_eq!(knowledge["write"]["attempts"], 1);
        assert_eq!(knowledge["read"]["attempts"], 1);
        assert_eq!(knowledge["read"]["failed"], 1);
        assert_eq!(knowledge["read"]["errors"], 1);
        assert_eq!(knowledge["read"]["error_rate"].as_f64().unwrap(), 1.0);
    }

    #[test]
    fn stats_omits_knowledge_when_unused() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("knowledge").is_none());
    }

    #[test]
    fn model_stats_records_requests_and_tokens_per_model() {
        let s = Stats::new();
        s.record_event(&request(10, 5), "KEY", &[], "");
        s.record_event(&request(2, 1), "KEY", &[], "");

        let models = s.model_stats();
        let m = &models["m"];
        assert_eq!(m.requests, 2);
        assert_eq!(m.input_tokens, 12);
        assert_eq!(m.output_tokens, 6);
        assert_eq!(m.failed, 0);
        assert_eq!(m.error_rate(), Some(0.0));
    }

    #[test]
    fn model_stats_records_failures_per_model() {
        let s = Stats::new();
        s.record_event(&request(10, 5), "KEY", &[], "");
        s.record_event(&provider_error(), "KEY", &[], "");

        let m = &s.model_stats()["m"];
        // A failed request is a request too, so the rate divides by both.
        assert_eq!(m.requests, 2);
        assert_eq!(m.failed, 1);
        assert_eq!(m.errors(), 1);
        assert_eq!(m.error_rate(), Some(0.5));
        assert_eq!(s.requests_failed(), 1);
    }

    #[test]
    fn model_stats_reach_the_label_slice() {
        let s = Stats::new();
        s.record_event(&request(10, 5), "KEY", &["scan".into()], "");

        assert_eq!(s.stats_for_label("scan").model_stats()["m"].requests, 1);
        assert!(s.stats_for_label("audit").model_stats().is_empty());
    }

    #[test]
    fn usage_for_ticket_stays_run_wide() {
        let s = Stats::new();
        s.record_event(&request(10, 5), "KEY", &["scan".into()], "");

        assert_eq!(s.usage_for_ticket("KEY").len(), 1);
        // The compaction estimator reads the run-wide series only, so
        // mirroring it onto a slice would buy nothing.
        assert!(s.stats_for_label("scan").usage_for_ticket("KEY").is_empty());
    }

    #[test]
    fn model_stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_event(&request(10, 5), "KEY", &[], "");
        s.record_event(&provider_error(), "KEY", &[], "");

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let models = restored.model_stats();
        assert_eq!(models["m"].requests, 2);
        assert_eq!(models["m"].failed, 1);
        assert_eq!(models["m"].input_tokens, 10);
        assert_eq!(models["m"].output_tokens, 5);
    }

    #[test]
    fn stats_serializes_models_as_nested_object() {
        let s = Stats::new();
        s.record_event(&request(10, 5), "KEY", &[], "");

        let value = serde_json::to_value(&s).unwrap();
        let models = value["models"].as_object().unwrap();
        assert_eq!(models["m"]["requests"], 1);
        assert_eq!(models["m"]["failed"], 0);
        assert_eq!(models["m"]["errors"], 0);
        assert_eq!(models["m"]["input_tokens"], 10);
        assert_eq!(models["m"]["output_tokens"], 5);
    }

    #[test]
    fn stats_omits_models_when_empty() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("models").is_none());
    }
}
