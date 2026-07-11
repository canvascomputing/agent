//! Run-time stats. One [`Stats`] struct of atomic counters records every
//! observable event and exposes inherent read accessors. Callers reach
//! it through `TicketSystem::stats()`.
//!
//! Lock-free for counter increments; readers do one atomic load per
//! call.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

use crate::agents::tickets::Status;
use crate::event::ToolFailureKind;
use crate::providers::types::TokenUsage;

/// Recorder protocol for the agent loop. Each agent holds an
/// `Arc<dyn LoopStats + Send + Sync>` and reports loop events through
/// it. Write-only; reads happen on `Stats` directly.
pub(crate) trait LoopStats: Send + Sync {
    fn record_turn(&self);
    fn record_request(&self, input_tokens: u64, output_tokens: u64);
    fn record_tool_call(&self);
    fn record_error(&self);
}

/// Recorder protocol for the ticket system. The ticket system holds an
/// `Arc<Stats>` directly but only exercises these methods, so by
/// convention the ticket-domain surface is narrow.
pub(crate) trait TicketStats: Send + Sync {
    fn record_created(&self);
    /// First call wins (CAS into `started_at`); later calls are no-ops.
    fn record_started(&self, when: u64);
    /// Adds `ticket_duration.as_secs()` and `work_duration.as_secs()` to
    /// the corresponding atomic sums.
    fn record_finished(&self, ticket_duration: Duration, work_duration: Duration);
    fn record_failed(&self, ticket_duration: Duration, work_duration: Duration);
}

/// Run-time statistics for tickets, tokens, and activity. Reached
/// through [`TicketSystem::stats()`](crate::TicketSystem::stats);
/// available during the run and after it finishes.
///
/// ```no_run
/// use agentwerk::TicketSystem;
///
/// # async fn run() {
/// let tickets = TicketSystem::new();
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
    turns: AtomicU64,
    requests: AtomicU64,
    tool_calls: AtomicU64,
    errors: AtomicU64,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    tickets_created: AtomicU64,
    tickets_finished: AtomicU64,
    tickets_failed: AtomicU64,

    /// Run-start wall clock (millis since epoch). 0 = unset; first
    /// `record_started` wins via CAS.
    started_at: AtomicU64,
    /// Run-end wall clock (millis since epoch). 0 = still running;
    /// `mark_finished` stamps it when the watcher fires.
    finished_at: AtomicU64,
    /// Sum of finished tickets' creation→terminal durations, seconds.
    total_ticket_duration: AtomicU64,
    /// Sum of finished tickets' started→terminal durations, seconds.
    /// With concurrent agents this can exceed the run's wall-clock
    /// duration.
    total_work_duration: AtomicU64,
    /// Lazy-init map of nested counter slices keyed by ticket label.
    /// Always empty on a slice itself; populated only on the run-wide
    /// `Stats` owned by `TicketSystem`.
    label_stats: Mutex<HashMap<String, Arc<Stats>>>,
    /// Per-ticket append-only series of provider-reported token usages.
    /// Feeds the proactive-compaction predictor; cleared on compaction.
    /// Always empty on a label slice.
    usage_history: Mutex<HashMap<String, Vec<TokenUsage>>>,
    /// Per-tool call and failure tallies keyed by tool name. Populated
    /// only on the run-wide `Stats`; always empty on a label slice.
    tool_stats: Mutex<HashMap<String, ToolCounters>>,
    /// Per-path open and failure tallies keyed by the file a tool opened.
    /// Populated only on the run-wide `Stats`; always empty on a label slice.
    file_stats: Mutex<HashMap<String, FileCounters>>,
}

/// Raw per-tool tallies behind the `tool_stats` map. `errors` is derived
/// from the three kinds, never stored.
#[derive(Default, Clone)]
struct ToolCounters {
    calls: u64,
    not_found: u64,
    execution_failed: u64,
    schema_failed: u64,
}

/// Raw per-path tallies behind the `file_stats` map.
#[derive(Default, Clone)]
struct FileCounters {
    opens: u64,
    failed: u64,
}

/// Call and failure tallies for one tool, broken down by failure kind.
/// Returned by [`Stats::tool_stats`] to measure how often the model
/// misuses a given tool: a high error rate or `schema_failed`/`not_found`
/// count points at the tool's description or input schema.
#[derive(Debug, Clone, Serialize)]
pub struct ToolStat {
    /// Every invocation attempt, including calls that named an unknown tool.
    pub calls: u64,
    /// Model named a tool the registry does not have.
    pub not_found: u64,
    /// The tool ran but returned an error (bad arguments, non-zero exit, IO).
    pub execution_failed: u64,
    /// The tool rejected the input on schema validation. Overlaps the
    /// `SchemaRetried` event and the `max_schema_retries` policy: this is
    /// the same population the retry budget governs, surfaced per tool.
    pub schema_failed: u64,
}

impl ToolStat {
    /// Total failures across the three kinds.
    pub fn errors(&self) -> u64 {
        self.not_found + self.execution_failed + self.schema_failed
    }

    /// `errors / calls`. `None` when the tool was never called.
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

/// Open and failure tallies for one path, returned by
/// [`Stats::file_stats`]. Records how often a file-opening tool named this
/// path: `opens` counts successful opens, `failed` counts attempts that
/// errored. For the search tools (`grep`, `codegrep`) the path may be a
/// directory rather than a file, since their `path` argument searches under
/// a directory or file.
#[derive(Debug, Clone, Serialize)]
pub struct FileStat {
    /// Calls that opened this path successfully.
    pub opens: u64,
    /// Calls that named this path and errored.
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

impl Stats {
    pub(crate) fn new() -> Self {
        Self {
            turns: AtomicU64::new(0),
            requests: AtomicU64::new(0),
            tool_calls: AtomicU64::new(0),
            errors: AtomicU64::new(0),
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
            usage_history: Mutex::new(HashMap::new()),
            tool_stats: Mutex::new(HashMap::new()),
            file_stats: Mutex::new(HashMap::new()),
        }
    }

    /// Statistics scoped to one ticket label. Reads use the same
    /// accessors as the run-wide `Stats`; `run_duration()` is always
    /// `None` on a slice (run timing stays global).
    pub fn stats_for_label(&self, label: &str) -> Arc<Stats> {
        let mut map = self.label_stats.lock().unwrap();
        map.entry(label.to_string())
            .or_insert_with(|| Arc::new(Stats::new()))
            .clone()
    }

    /// Cloned per-ticket usage series, oldest first.
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

    /// Drop the per-ticket series. Called after a compaction commits,
    /// since the post-summary transcript breaks the pre-summary trend.
    pub(crate) fn reset_usage(&self, ticket_key: &str) {
        self.usage_history.lock().unwrap().remove(ticket_key);
    }

    /// Per-tool call and failure tallies, sorted by tool name. Empty on a
    /// label slice. The `schema_failed` field overlaps the `SchemaRetried`
    /// event and `max_schema_retries` policy: it counts the same population
    /// the retry budget governs, surfaced per tool.
    pub fn tool_stats(&self) -> BTreeMap<String, ToolStat> {
        self.tool_stats
            .lock()
            .unwrap()
            .iter()
            .map(|(name, counters)| (name.clone(), counters.into()))
            .collect()
    }

    /// Count one invocation attempt against `name`.
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

    /// Per-path open and failure tallies, sorted by path. Empty on a label
    /// slice. Entries are the paths file-opening tools named; for the search
    /// tools a path may be a directory rather than a file.
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

    pub(crate) fn record_turn_for(&self, labels: &[String]) {
        self.record_turn();
        for label in labels {
            self.stats_for_label(label).record_turn();
        }
    }

    pub(crate) fn record_request_for(
        &self,
        labels: &[String],
        input_tokens: u64,
        output_tokens: u64,
    ) {
        self.record_request(input_tokens, output_tokens);
        for label in labels {
            self.stats_for_label(label)
                .record_request(input_tokens, output_tokens);
        }
    }

    pub(crate) fn record_tool_call_for(&self, labels: &[String]) {
        self.record_tool_call();
        for label in labels {
            self.stats_for_label(label).record_tool_call();
        }
    }

    pub(crate) fn record_error_for(&self, labels: &[String]) {
        self.record_error();
        for label in labels {
            self.stats_for_label(label).record_error();
        }
    }

    /// Dispatch the run-wide recorders that match a ticket transition.
    /// `prev → next` of `Todo → InProgress` stamps `started_at`;
    /// terminal transitions add to the duration totals.
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

    /// Mirror a terminal transition onto every per-label slice the
    /// ticket carries. `record_started` is intentionally not mirrored:
    /// per-label `started_at` stays zero so `elapsed()` reads `None` on
    /// a slice.
    pub(crate) fn record_transition_for(
        &self,
        labels: &[String],
        next: Status,
        (ticket_duration, work_duration): (Duration, Duration),
    ) {
        if !matches!(next, Status::Finished | Status::Failed) {
            return;
        }
        for label in labels {
            let slice = self.stats_for_label(label);
            match next {
                Status::Finished => slice.record_finished(ticket_duration, work_duration),
                Status::Failed => slice.record_failed(ticket_duration, work_duration),
                _ => unreachable!(),
            }
        }
    }

    /// Count of times an agent picked up a ticket to process.
    pub fn turns(&self) -> u64 {
        self.turns.load(Ordering::Relaxed)
    }

    /// Total provider responses received.
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    /// Total tool calls.
    pub fn tool_calls(&self) -> u64 {
        self.tool_calls.load(Ordering::Relaxed)
    }

    /// Total provider errors.
    pub fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }

    /// Total input tokens across all provider responses.
    pub fn input_tokens(&self) -> u64 {
        self.input_tokens.load(Ordering::Relaxed)
    }

    /// Total output tokens across all provider responses.
    pub fn output_tokens(&self) -> u64 {
        self.output_tokens.load(Ordering::Relaxed)
    }

    /// Count of tickets created.
    pub fn tickets_created(&self) -> u64 {
        self.tickets_created.load(Ordering::Relaxed)
    }

    /// Count of tickets that finished successfully.
    pub fn tickets_finished(&self) -> u64 {
        self.tickets_finished.load(Ordering::Relaxed)
    }

    /// Count of tickets that failed.
    pub fn tickets_failed(&self) -> u64 {
        self.tickets_failed.load(Ordering::Relaxed)
    }

    /// Elapsed duration of the run, live while agents work and frozen
    /// once the run finishes. `None` until the first ticket starts.
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

    /// `tickets_finished / (tickets_finished + tickets_failed)`. `None` when
    /// no ticket has reached a terminal state.
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

    /// Sum of finished tickets' creation→terminal spans.
    pub fn ticket_duration(&self) -> Duration {
        Duration::from_secs(self.total_ticket_duration.load(Ordering::Relaxed))
    }

    /// Mean of finished tickets' creation→terminal spans. `None`
    /// while no ticket has finished.
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

    /// Sum of finished tickets' started→terminal spans. With
    /// concurrent agents this aggregates work across all of them, so
    /// it can exceed `run_duration`.
    pub fn work_duration(&self) -> Duration {
        Duration::from_secs(self.total_work_duration.load(Ordering::Relaxed))
    }

    /// Mean of finished tickets' started→terminal spans. `None` while
    /// no ticket has finished.
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

    /// Stamp the run's finish wall-clock. Idempotent in practice
    /// (the watcher fires once); successive calls overwrite, which
    /// is fine.
    pub(crate) fn mark_finished(&self, when: u64) {
        self.finished_at.store(when, Ordering::Relaxed);
    }

    /// Rebuild from already-loaded tickets. Used as the fallback when
    /// the stats file is absent or unreadable; otherwise `Stats::load`
    /// is preferred to keep observability continuous across restarts.
    pub(crate) fn derive(tickets: &HashMap<String, crate::agents::tickets::Ticket>) -> Self {
        let stats = Stats::new();
        for t in tickets.values() {
            TicketStats::record_created(&stats);
            for label in t.labels.iter() {
                TicketStats::record_created(&*stats.stats_for_label(label));
            }
            let ticket_dur = ticket_duration(t).unwrap_or_default();
            let work_dur = work_duration(t).unwrap_or_default();
            match t.status {
                Status::Finished => {
                    TicketStats::record_finished(&stats, ticket_dur, work_dur);
                    for label in t.labels.iter() {
                        TicketStats::record_finished(
                            &*stats.stats_for_label(label),
                            ticket_dur,
                            work_dur,
                        );
                    }
                }
                Status::Failed => {
                    TicketStats::record_failed(&stats, ticket_dur, work_dur);
                    for label in t.labels.iter() {
                        TicketStats::record_failed(
                            &*stats.stats_for_label(label),
                            ticket_dur,
                            work_dur,
                        );
                    }
                }
                Status::Todo | Status::InProgress => {}
            }
        }
        stats
    }

    /// Sugar over the `Persist` impl for the keyless case.
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
        Ok(stats)
    }
}

impl Serialize for Stats {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let labels = self.label_stats.lock().unwrap();
        let tools = self.tool_stats();
        let files = self.file_stats();
        let len = 17
            + usize::from(!labels.is_empty())
            + usize::from(!tools.is_empty())
            + usize::from(!files.is_empty());
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
        st.serialize_field("ticket_duration_secs", &self.ticket_duration().as_secs())?;
        st.serialize_field("work_duration_secs", &self.work_duration().as_secs())?;
        if !labels.is_empty() {
            let nested: BTreeMap<&String, &Stats> =
                labels.iter().map(|(k, v)| (k, v.as_ref())).collect();
            st.serialize_field("labels", &nested)?;
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
        st.end()
    }
}

impl Stats {
    fn load_fields(&self, value: &serde_json::Value) {
        let get = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        self.turns.store(get("turns"), Ordering::Relaxed);
        self.requests.store(get("requests"), Ordering::Relaxed);
        self.tool_calls.store(get("tool_calls"), Ordering::Relaxed);
        self.errors.store(get("errors"), Ordering::Relaxed);
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

impl LoopStats for Stats {
    fn record_turn(&self) {
        self.turns.fetch_add(1, Ordering::Relaxed);
    }

    fn record_request(&self, input_tokens: u64, output_tokens: u64) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.input_tokens.fetch_add(input_tokens, Ordering::Relaxed);
        self.output_tokens
            .fetch_add(output_tokens, Ordering::Relaxed);
    }

    fn record_tool_call(&self) {
        self.tool_calls.fetch_add(1, Ordering::Relaxed);
    }

    fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
}

impl TicketStats for Stats {
    fn record_created(&self) {
        self.tickets_created.fetch_add(1, Ordering::Relaxed);
    }

    fn record_started(&self, when: u64) {
        // First call wins. Subsequent claims (Path A reclaim, late
        // bind) leave the original run-start untouched.
        let _ = self
            .started_at
            .compare_exchange(0, when, Ordering::Relaxed, Ordering::Relaxed);
    }

    fn record_finished(&self, ticket_duration: Duration, work_duration: Duration) {
        self.tickets_finished.fetch_add(1, Ordering::Relaxed);
        self.total_ticket_duration
            .fetch_add(ticket_duration.as_secs(), Ordering::Relaxed);
        self.total_work_duration
            .fetch_add(work_duration.as_secs(), Ordering::Relaxed);
    }

    fn record_failed(&self, ticket_duration: Duration, work_duration: Duration) {
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
        assert_eq!(s.ticket_duration(), Duration::ZERO);
        assert_eq!(s.work_duration(), Duration::ZERO);
        assert!(s.run_duration().is_none());
        assert!(s.avg_ticket_duration().is_none());
        assert!(s.avg_work_duration().is_none());
        assert!(s.tickets_success_rate().is_none());
    }

    #[test]
    fn loop_stats_writes_show_up_in_reads() {
        let s = Stats::new();
        s.record_turn();
        s.record_turn();
        s.record_request(10, 5);
        s.record_request(2, 1);
        s.record_tool_call();
        s.record_error();

        assert_eq!(s.turns(), 2);
        assert_eq!(s.requests(), 2);
        assert_eq!(s.tool_calls(), 1);
        assert_eq!(s.errors(), 1);
        assert_eq!(s.input_tokens(), 12);
        assert_eq!(s.output_tokens(), 6);
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
        assert_eq!(s.ticket_duration(), Duration::from_secs(8));
        assert_eq!(s.work_duration(), Duration::from_secs(6));
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
    fn stats_for_label_slice_records_independently() {
        let s = Stats::new();
        let slice = s.stats_for_label("scan");
        slice.record_turn();
        slice.record_request(10, 5);
        assert_eq!(slice.turns(), 1);
        assert_eq!(slice.input_tokens(), 10);
        assert_eq!(slice.output_tokens(), 5);
        assert_eq!(s.turns(), 0);
        assert_eq!(s.input_tokens(), 0);
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
    fn work_duration_can_exceed_run_duration_with_concurrency() {
        // Two tickets, each 5s of work, finished in a 6s window —
        // models 2 agents working in parallel.
        let s = Stats::new();
        s.record_started(1_000);
        s.record_finished(Duration::from_secs(5), Duration::from_secs(5));
        s.record_finished(Duration::from_secs(5), Duration::from_secs(5));
        s.mark_finished(7_000);
        assert_eq!(s.run_duration(), Some(Duration::from_secs(6)));
        assert_eq!(s.work_duration(), Duration::from_secs(10));
    }

    #[test]
    fn stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_turn();
        s.record_turn();
        s.record_request(100, 50);
        s.record_tool_call();
        s.record_error();
        s.record_created();
        s.record_finished(Duration::from_secs(7), Duration::from_secs(5));
        s.record_failed(Duration::from_secs(3), Duration::from_secs(2));

        let slice = s.stats_for_label("scan");
        slice.record_turn();
        slice.record_request(40, 20);
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
        assert_eq!(restored.ticket_duration(), Duration::from_secs(10));
        assert_eq!(restored.work_duration(), Duration::from_secs(7));

        let restored_slice = restored.stats_for_label("scan");
        assert_eq!(restored_slice.turns(), 1);
        assert_eq!(restored_slice.input_tokens(), 40);
        assert_eq!(restored_slice.tickets_finished(), 1);
        assert_eq!(restored_slice.ticket_duration(), Duration::from_secs(4));
    }

    #[test]
    fn stats_serializes_raw_counter_fields() {
        let s = Stats::new();
        s.record_turn();
        s.record_request(100, 50);
        s.record_tool_call();
        s.record_error();
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
        let slice = s.stats_for_label("scan");
        slice.record_turn();
        slice.record_request(40, 20);
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
}
