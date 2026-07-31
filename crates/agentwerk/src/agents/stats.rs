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
    /// when to compact, and is cleared once that happens.
    usage_history: Mutex<HashMap<String, Vec<TokenUsage>>>,
    /// Call and failure tallies keyed by tool name, run-wide only.
    tool_stats: Mutex<HashMap<String, ToolCounters>>,
    /// Open and failure tallies keyed by the path a tool opened, run-wide only.
    file_stats: Mutex<HashMap<String, FileCounters>>,
    /// Knowledge usage tallies, run-wide only.
    knowledge_stats: Mutex<KnowledgeCounters>,
    /// Request and token tallies keyed by model name, run-wide only.
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

/// The stored knowledge tallies.
#[derive(Default, Clone)]
struct KnowledgeCounters {
    writes: u64,
    reads: u64,
    removes: u64,
    lists: u64,
    misses: u64,
}

/// The stored per-model tallies.
#[derive(Default, Clone)]
struct ModelCounters {
    requests: u64,
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
    /// Calls that opened this path.
    pub opens: u64,
    /// Calls that named this path and failed.
    pub failed: u64,
}

impl From<&FileCounters> for FileStat {
    fn from(c: &FileCounters) -> Self {
        Self {
            opens: c.opens,
            failed: c.failed,
        }
    }
}

/// A `KnowledgeStat` counts what agents did to the knowledge pages. Returned by
/// [`Stats::knowledge_stats`].
///
/// A high `misses` count points at a stale index, or at a prompt promising more
/// than the store holds.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeStat {
    /// Pages written.
    pub writes: u64,
    /// Pages read.
    pub reads: u64,
    /// Pages removed.
    pub removes: u64,
    /// Times the pages were listed.
    pub lists: u64,
    /// Reads and removes naming a page the store does not have.
    pub misses: u64,
}

impl From<&KnowledgeCounters> for KnowledgeStat {
    fn from(c: &KnowledgeCounters) -> Self {
        Self {
            writes: c.writes,
            reads: c.reads,
            removes: c.removes,
            lists: c.lists,
            misses: c.misses,
        }
    }
}

/// A `ModelStat` counts one model's requests and token usage. Returned by
/// [`Stats::model_stats`], so agents running different models can be compared.
#[derive(Debug, Clone, Serialize)]
pub struct ModelStat {
    /// Responses received for this model.
    pub requests: u64,
    /// Input tokens across this model's responses.
    pub input_tokens: u64,
    /// Output tokens across this model's responses.
    pub output_tokens: u64,
}

impl From<&ModelCounters> for ModelStat {
    fn from(c: &ModelCounters) -> Self {
        Self {
            requests: c.requests,
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
            usage_history: Mutex::new(HashMap::new()),
            tool_stats: Mutex::new(HashMap::new()),
            file_stats: Mutex::new(HashMap::new()),
            knowledge_stats: Mutex::new(KnowledgeCounters::default()),
            model_stats: Mutex::new(HashMap::new()),
        }
    }

    /// Get statistics scoped to one label.
    ///
    /// Reads use the same accessors as the run-wide `Stats`. `run_duration()`
    /// is always `None` here, since timing stays global.
    pub fn stats_for_label(&self, label: &str) -> Arc<Stats> {
        let mut map = self.label_stats.lock().unwrap();
        map.entry(label.to_string())
            .or_insert_with(|| Arc::new(Stats::new()))
            .clone()
    }

    /// Get statistics scoped to one agent, by the name it was registered under.
    ///
    /// `tickets_created()` counts the tickets that agent filed; the rest count
    /// the tickets it claimed. Like the per-label slices, these are written to
    /// `stats.json` for readers and not restored by `TicketQueue::load`.
    pub fn stats_for_agent(&self, agent_name: &str) -> Arc<Stats> {
        let mut map = self.agent_stats.lock().unwrap();
        map.entry(agent_name.to_string())
            .or_insert_with(|| Arc::new(Stats::new()))
            .clone()
    }

    /// Get a ticket's token usage, oldest first.
    pub fn usage_history(&self, ticket_key: &str) -> Vec<TokenUsage> {
        self.usage_history
            .lock()
            .unwrap()
            .get(ticket_key)
            .cloned()
            .unwrap_or_default()
    }

    /// Append `usage` to the per-ticket series.
    pub(crate) fn record_usage(&self, ticket_key: &str, usage: TokenUsage) {
        self.usage_history
            .lock()
            .unwrap()
            .entry(ticket_key.to_string())
            .or_default()
            .push(usage);
    }

    /// Drop a ticket's usage history, once its older messages are summarized
    /// and the earlier trend no longer predicts the next request.
    pub(crate) fn reset_usage(&self, ticket_key: &str) {
        self.usage_history.lock().unwrap().remove(ticket_key);
    }

    /// Get per-tool call and failure counts, sorted by tool name.
    ///
    /// Empty on a label slice.
    pub fn tool_stats(&self) -> BTreeMap<String, ToolStat> {
        self.tool_stats
            .lock()
            .unwrap()
            .iter()
            .map(|(name, counters)| (name.clone(), counters.into()))
            .collect()
    }

    /// Count one call against `name`.
    pub(crate) fn record_tool_call_named(&self, name: &str) {
        self.tool_stats
            .lock()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .calls += 1;
    }

    /// Count one failure of `kind` against `name`.
    pub(crate) fn record_tool_error_named(&self, name: &str, kind: ToolFailureKind) {
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
    /// accept either. Empty on a label slice.
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

    /// Count one failed open naming `path`.
    pub(crate) fn record_file_open_error(&self, path: &str) {
        self.file_stats
            .lock()
            .unwrap()
            .entry(path.to_string())
            .or_default()
            .failed += 1;
    }

    /// Get knowledge usage: write, read, remove, list, and miss counts.
    ///
    /// Zero on a label slice.
    pub fn knowledge_stats(&self) -> KnowledgeStat {
        (&*self.knowledge_stats.lock().unwrap()).into()
    }

    /// Count one knowledge operation, as `manage_knowledge` reports it.
    pub(crate) fn record_knowledge(&self, op: KnowledgeOp) {
        let mut counters = self.knowledge_stats.lock().unwrap();
        match op {
            KnowledgeOp::Write => counters.writes += 1,
            KnowledgeOp::Read => counters.reads += 1,
            KnowledgeOp::Remove => counters.removes += 1,
            KnowledgeOp::List => counters.lists += 1,
        }
    }

    /// Count one read or remove naming a page the store does not have.
    pub(crate) fn record_knowledge_miss(&self) {
        self.knowledge_stats.lock().unwrap().misses += 1;
    }

    /// Get per-model requests and token usage, sorted by model name.
    ///
    /// Empty on a label slice.
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

    /// Record an event.
    ///
    /// Every kind is counted by its name, so a new variant needs no arm here.
    /// Only the measures that read an event's payload do.
    pub(crate) fn record_event(&self, kind: &EventKind, key: &str, labels: &[String], agent: &str) {
        self.record_scoped(labels, agent, |s| s.count_event(kind.name()));
        match kind {
            EventKind::RequestFinished { model, usage } => {
                self.record_scoped(labels, agent, |s| {
                    s.record_tokens(usage.input_tokens, usage.output_tokens)
                });
                self.record_usage(key, usage.clone());
                self.record_model_request(model, usage);
            }
            EventKind::ToolCallStarted { tool_name, .. } => self.record_tool_call_named(tool_name),
            EventKind::ToolCallFailed {
                tool_name, reason, ..
            } => self.record_tool_error_named(tool_name, *reason),
            EventKind::FileOpenFinished { path } => self.record_file_open(path),
            EventKind::FileOpenFailed { path } => self.record_file_open_error(path),
            EventKind::KnowledgeUsed { op } => self.record_knowledge(*op),
            EventKind::KnowledgeMissed => self.record_knowledge_miss(),
            _ => {}
        }
    }

    /// Apply `f` to the run-wide statistics, to each label slice, and to the
    /// agent's slice. An event about execution itself carries no agent name.
    fn record_scoped(&self, labels: &[String], agent: &str, f: impl Fn(&Stats)) {
        f(self);
        for label in labels {
            f(&self.stats_for_label(label));
        }
        if !agent.is_empty() {
            f(&self.stats_for_agent(agent));
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
            .map(|label| self.stats_for_label(label))
            .chain((!agent.is_empty()).then(|| self.stats_for_agent(agent)));
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
    pub fn errors(&self) -> u64 {
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
    pub(crate) fn mark_finished(&self, when: u64) {
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
                let slice = stats.stats_for_label(name);
                slice.load_fields(body);
            }
        }
        if let Some(tools) = value.get("tools").and_then(|v| v.as_object()) {
            let mut map = stats.tool_stats.lock().unwrap();
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
            let mut map = stats.file_stats.lock().unwrap();
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
        if let Some(knowledge) = value.get("knowledge") {
            let get = |key: &str| knowledge.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
            *stats.knowledge_stats.lock().unwrap() = KnowledgeCounters {
                writes: get("writes"),
                reads: get("reads"),
                removes: get("removes"),
                lists: get("lists"),
                misses: get("misses"),
            };
        }
        if let Some(models) = value.get("models").and_then(|v| v.as_object()) {
            let mut map = stats.model_stats.lock().unwrap();
            for (name, body) in models {
                let get = |key: &str| body.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                map.insert(
                    name.clone(),
                    ModelCounters {
                        requests: get("requests"),
                        input_tokens: get("input_tokens"),
                        output_tokens: get("output_tokens"),
                    },
                );
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
        let knowledge_used = knowledge.writes
            + knowledge.reads
            + knowledge.removes
            + knowledge.lists
            + knowledge.misses
            > 0;
        let len = 15
            + usize::from(!events.is_empty())
            + usize::from(!labels.is_empty())
            + usize::from(!agents.is_empty())
            + usize::from(!tools.is_empty())
            + usize::from(!files.is_empty())
            + usize::from(knowledge_used)
            + usize::from(!models.is_empty());
        let mut st = serializer.serialize_struct("Stats", len)?;
        st.serialize_field("turns", &self.turns())?;
        st.serialize_field("requests", &self.requests())?;
        st.serialize_field("tool_calls", &self.tool_calls())?;
        st.serialize_field("errors", &self.errors())?;
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
        st.serialize_field("success_rate", &self.tickets_success_rate())?;
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
            // Raw counters round-trip through load; errors/error_rate are
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
            st.serialize_field("files", &files)?;
        }
        if knowledge_used {
            st.serialize_field("knowledge", &knowledge)?;
        }
        if !models.is_empty() {
            st.serialize_field("models", &models)?;
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
        assert_eq!(s.errors(), 0);
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
        assert_eq!(s.errors(), 1);
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
        s.mark_finished(4_500);
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
        s.mark_finished(2_500);
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
        let a = s.stats_for_label("scan");
        let b = s.stats_for_label("scan");
        assert!(Arc::ptr_eq(&a, &b));
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
        s.mark_finished(7_000);
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

        let slice = s.stats_for_label("scan");
        slice.record_event(&turn(), "KEY", &[], "");
        slice.record_event(&request(40, 20), "KEY", &[], "");
        slice.record_created();
        slice.record_finished(Duration::from_secs(4), Duration::from_secs(3));

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();
        assert_eq!(restored.turns(), 2);
        assert_eq!(restored.requests(), 1);
        assert_eq!(restored.tool_calls(), 1);
        assert_eq!(restored.errors(), 1);
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
        assert_eq!(value["errors"], 1);
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
        s.mark_finished(3_500);
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
    fn stats_serializes_success_rate_when_tickets_present() {
        let s = Stats::new();
        s.record_finished(Duration::from_secs(1), Duration::from_secs(1));
        s.record_finished(Duration::from_secs(1), Duration::from_secs(1));
        s.record_finished(Duration::from_secs(1), Duration::from_secs(1));
        s.record_failed(Duration::from_secs(1), Duration::from_secs(1));
        let value = serde_json::to_value(&s).unwrap();
        let rate = value["success_rate"].as_f64().unwrap();
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

        assert!(s.usage_history("TICKET-1").is_empty());
        let t2 = s.usage_history("TICKET-2");
        assert_eq!(t2.len(), 1);
        assert_eq!(t2[0].input_tokens, 200);
    }

    #[test]
    fn tool_stats_records_calls_and_errors_per_tool() {
        let s = Stats::new();
        s.record_tool_call_named("edit_file");
        s.record_tool_call_named("edit_file");
        s.record_tool_call_named("bash");
        s.record_tool_error_named("edit_file", ToolFailureKind::SchemaValidationFailed);
        s.record_tool_error_named("bash", ToolFailureKind::ExecutionFailed);
        s.record_tool_error_named("ghost", ToolFailureKind::ToolNotFound);

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
        s.record_tool_call_named("edit_file");
        s.record_tool_call_named("edit_file");
        s.record_tool_call_named("edit_file");
        s.record_tool_call_named("edit_file");
        s.record_tool_error_named("edit_file", ToolFailureKind::SchemaValidationFailed);

        let tools = s.tool_stats();
        assert_eq!(tools["edit_file"].error_rate(), Some(0.25));
    }

    #[test]
    fn tool_stat_error_rate_is_none_without_calls() {
        // A failure can be recorded for a name that never logged a call only
        // in malformed cases; the rate is then undefined rather than infinite.
        let s = Stats::new();
        s.record_tool_error_named("ghost", ToolFailureKind::ToolNotFound);
        assert!(s.tool_stats()["ghost"].error_rate().is_none());
    }

    #[test]
    fn tool_stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_tool_call_named("edit_file");
        s.record_tool_call_named("edit_file");
        s.record_tool_error_named("edit_file", ToolFailureKind::SchemaValidationFailed);
        s.record_tool_call_named("bash");
        s.record_tool_error_named("bash", ToolFailureKind::ExecutionFailed);

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
        s.record_tool_call_named("edit_file");
        s.record_tool_call_named("edit_file");
        s.record_tool_error_named("edit_file", ToolFailureKind::SchemaValidationFailed);

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
    fn tool_stats_empty_on_label_slice() {
        let s = Stats::new();
        s.record_tool_call_named("edit_file");
        let slice = s.stats_for_label("scan");
        assert!(slice.tool_stats().is_empty());
    }

    #[test]
    fn file_stats_records_opens_and_failures_per_path() {
        let s = Stats::new();
        s.record_file_open("src/lib.rs");
        s.record_file_open("src/lib.rs");
        s.record_file_open("src/main.rs");
        s.record_file_open_error("src/missing.rs");
        s.record_file_open_error("src/missing.rs");

        let files = s.file_stats();
        assert_eq!(files["src/lib.rs"].opens, 2);
        assert_eq!(files["src/lib.rs"].failed, 0);
        assert_eq!(files["src/main.rs"].opens, 1);
        assert_eq!(files["src/missing.rs"].opens, 0);
        assert_eq!(files["src/missing.rs"].failed, 2);
    }

    #[test]
    fn file_stats_empty_on_label_slice() {
        let s = Stats::new();
        s.record_file_open("src/lib.rs");
        let slice = s.stats_for_label("scan");
        assert!(slice.file_stats().is_empty());
    }

    #[test]
    fn file_stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_file_open("src/lib.rs");
        s.record_file_open("src/lib.rs");
        s.record_file_open_error("src/missing.rs");

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let files = restored.file_stats();
        assert_eq!(files["src/lib.rs"].opens, 2);
        assert_eq!(files["src/missing.rs"].failed, 1);
    }

    #[test]
    fn stats_serializes_files_as_nested_object() {
        let s = Stats::new();
        s.record_file_open("src/lib.rs");
        s.record_file_open("src/lib.rs");
        s.record_file_open_error("src/lib.rs");

        let value = serde_json::to_value(&s).unwrap();
        let files = value["files"].as_object().unwrap();
        assert_eq!(files["src/lib.rs"]["opens"], 2);
        assert_eq!(files["src/lib.rs"]["failed"], 1);
    }

    #[test]
    fn stats_omits_files_when_empty() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("files").is_none());
    }

    #[test]
    fn knowledge_records_each_operation() {
        let s = Stats::new();
        s.record_knowledge(KnowledgeOp::Write);
        s.record_knowledge(KnowledgeOp::Read);
        s.record_knowledge(KnowledgeOp::Read);
        s.record_knowledge(KnowledgeOp::Remove);
        s.record_knowledge(KnowledgeOp::List);
        s.record_knowledge_miss();

        let k = s.knowledge_stats();
        assert_eq!(k.writes, 1);
        assert_eq!(k.reads, 2);
        assert_eq!(k.removes, 1);
        assert_eq!(k.lists, 1);
        assert_eq!(k.misses, 1);
    }

    #[test]
    fn knowledge_is_zero_on_label_slice() {
        let s = Stats::new();
        s.record_knowledge(KnowledgeOp::Write);
        let slice = s.stats_for_label("scan");
        let k = slice.knowledge_stats();
        assert_eq!(k.writes, 0);
        assert_eq!(k.reads, 0);
    }

    #[test]
    fn knowledge_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_knowledge(KnowledgeOp::Write);
        s.record_knowledge(KnowledgeOp::Read);
        s.record_knowledge_miss();

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let k = restored.knowledge_stats();
        assert_eq!(k.writes, 1);
        assert_eq!(k.reads, 1);
        assert_eq!(k.misses, 1);
    }

    #[test]
    fn stats_serializes_knowledge_as_nested_object() {
        let s = Stats::new();
        s.record_knowledge(KnowledgeOp::Write);
        s.record_knowledge_miss();

        let value = serde_json::to_value(&s).unwrap();
        let knowledge = value["knowledge"].as_object().unwrap();
        assert_eq!(knowledge["writes"], 1);
        assert_eq!(knowledge["misses"], 1);
        assert_eq!(knowledge["reads"], 0);
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
    }

    #[test]
    fn model_stats_empty_on_label_slice() {
        let s = Stats::new();
        s.record_event(&request(10, 5), "KEY", &["scan".into()], "");
        let slice = s.stats_for_label("scan");
        assert!(slice.model_stats().is_empty());
    }

    #[test]
    fn model_stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_event(&request(10, 5), "KEY", &[], "");

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let models = restored.model_stats();
        assert_eq!(models["m"].requests, 1);
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
