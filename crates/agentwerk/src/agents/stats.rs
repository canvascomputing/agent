//! Deep insights into how agents behave: working time, tickets, failure rates,
//! and bottlenecks.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

use crate::agents::tickets::Status;
use crate::event::{EventKind, EventName, KnowledgeFailureKind, Measure, ToolFailureKind};
use crate::providers::types::TokenUsage;
use crate::providers::RequestErrorKind;

/// `Stats` holds the metrics about tickets, tokens, and time. Reach it through
/// [`TicketQueue::stats()`](crate::TicketQueue::stats), during execution or
/// after it finishes.
///
/// ```no_run
/// use agentwerk::event::EventName;
/// use agentwerk::TicketQueue;
///
/// # async fn run() {
/// let tickets = TicketQueue::new();
/// tickets.finish(|_| true).await;
///
/// let stats = tickets.stats();
/// println!(
///     "{} tickets, {} input tokens",
///     stats.event_count(EventName::TicketFinished),
///     stats.input_tokens(),
/// );
/// # }
/// ```
pub struct Stats {
    /// Count per event kind. Every emitted event lands here, and
    /// [`Stats::event_count`] is a lookup into this map.
    event_counts: Mutex<HashMap<EventName, u64>>,

    /// When execution started, in milliseconds since the epoch. 0 until then,
    /// and the first writer wins.
    started_at: AtomicU64,
    /// When execution ended, in milliseconds since the epoch. 0 while it is
    /// still running.
    finished_at: AtomicU64,
    /// Sum of resolved tickets' time from creation to resolution, in seconds.
    ticket_duration: AtomicU64,
    /// Sum of resolved tickets' time from claim to resolution, in seconds. With
    /// agents working in parallel this can exceed the elapsed duration.
    work_duration: AtomicU64,
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
    /// Counters per subject: category -> name -> counter -> count. Every
    /// measure an event declares lands here, and the four `*_stats()`
    /// accessors are views onto one category.
    counters: Mutex<HashMap<&'static str, HashMap<String, Counters>>>,
}

/// The stored counters of one subject, such as `bash` under `tools`.
type Counters = HashMap<&'static str, u64>;

/// Every subject category, in the order `stats.json` writes them: the counters
/// it stores, then which of those count attempts and which count failures.
/// `errors` and `error_rate` are derived from the last two for readers, and
/// never stored, so `load` skips them by not listing them.
struct Category {
    name: &'static str,
    counters: &'static [&'static str],
    attempts: &'static [&'static str],
    failures: &'static [&'static str],
}

/// The counters a category's failures split into, one per reason its events
/// carry. `counters` below is the attempt counter plus these.
const TOOL_FAILURES: &[&str] = &["not_found", "execution_failed", "schema_failed"];
const REQUEST_FAILURES: &[&str] = &[
    "authentication_failed",
    "permission_denied",
    "model_not_found",
    "context_window_exceeded",
    "safety_filter_triggered",
    "rate_limited",
    "status_unclassified",
    "connection_failed",
    "stream_interrupted",
    "response_malformed",
    "provider_unrecognized",
];
const KNOWLEDGE_FAILURES: &[&str] = &["page_missing", "store_refused"];

const CATEGORIES: &[Category] = &[
    Category {
        name: "tools",
        counters: &["calls", "not_found", "execution_failed", "schema_failed"],
        attempts: &["calls"],
        failures: TOOL_FAILURES,
    },
    // A path fails with the call that named it, so it splits by the same
    // reasons a tool call does.
    Category {
        name: "files",
        counters: &["opens", "not_found", "execution_failed", "schema_failed"],
        attempts: &["opens"],
        failures: TOOL_FAILURES,
    },
    Category {
        name: "models",
        counters: &[
            "requests",
            "input_tokens",
            "output_tokens",
            "authentication_failed",
            "permission_denied",
            "model_not_found",
            "context_window_exceeded",
            "safety_filter_triggered",
            "rate_limited",
            "status_unclassified",
            "connection_failed",
            "stream_interrupted",
            "response_malformed",
            "provider_unrecognized",
        ],
        attempts: &["requests"],
        failures: REQUEST_FAILURES,
    },
    Category {
        name: "knowledge",
        counters: &["attempts", "page_missing", "store_refused"],
        attempts: &["attempts"],
        failures: KNOWLEDGE_FAILURES,
    },
];

/// A `ToolStat` counts one tool's calls and the failures they ended in.
///
/// Returned by [`Stats::tool_stats`]. A high error rate, or many failures
/// under `SchemaValidationFailed` or `ToolNotFound`, points at the tool's
/// description or its input schema.
#[derive(Debug, Clone, Serialize)]
pub struct ToolStat {
    /// Every call, including calls that named an unknown tool.
    pub calls: u64,
    /// Those calls that failed, by the reason the event carried.
    pub failures: BTreeMap<ToolFailureKind, u64>,
}

impl ToolStat {
    /// Get the total failures across the kinds.
    pub fn errors(&self) -> u64 {
        self.failures.values().sum()
    }

    /// Get `errors / calls`, or `None` when the tool was never called.
    pub fn error_rate(&self) -> Option<f64> {
        rate(self.errors(), self.calls)
    }
}

impl From<&Counters> for ToolStat {
    fn from(counters: &Counters) -> Self {
        Self {
            calls: count(counters, "calls"),
            failures: failures(counters, ToolFailureKind::ALL, ToolFailureKind::as_str),
        }
    }
}

/// A `FileStat` counts how often a tool opened one path, and the failures it
/// ended in. Returned by [`Stats::file_stats`].
///
/// The path may name a directory rather than a file, since `grep` searches
/// under either. A path fails with the call that named it, so the reasons are
/// a tool call's.
#[derive(Debug, Clone, Serialize)]
pub struct FileStat {
    /// Every call that named this path, including the ones that failed.
    pub opens: u64,
    /// Those calls that failed, by the reason the event carried.
    pub failures: BTreeMap<ToolFailureKind, u64>,
}

impl FileStat {
    /// Get the total failures across the kinds.
    pub fn errors(&self) -> u64 {
        self.failures.values().sum()
    }

    /// Get `errors / opens`, or `None` when the path was never named.
    pub fn error_rate(&self) -> Option<f64> {
        rate(self.errors(), self.opens)
    }
}

impl From<&Counters> for FileStat {
    fn from(counters: &Counters) -> Self {
        Self {
            opens: count(counters, "opens"),
            failures: failures(counters, ToolFailureKind::ALL, ToolFailureKind::as_str),
        }
    }
}

/// A `KnowledgeStat` counts one knowledge operation's attempts and the
/// failures they ended in. Returned by [`Stats::knowledge_stats`], keyed by
/// operation.
///
/// A high error rate points at a stale index, or at a prompt promising more
/// than the store holds.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeStat {
    /// Every attempt at this operation, including the ones that did not go
    /// through.
    pub attempts: u64,
    /// Those attempts that failed, by the reason the event carried.
    pub failures: BTreeMap<KnowledgeFailureKind, u64>,
}

impl KnowledgeStat {
    /// Get the total failures across the kinds.
    pub fn errors(&self) -> u64 {
        self.failures.values().sum()
    }

    /// Get `errors / attempts`, or `None` when the operation was never tried.
    pub fn error_rate(&self) -> Option<f64> {
        rate(self.errors(), self.attempts)
    }
}

impl From<&Counters> for KnowledgeStat {
    fn from(counters: &Counters) -> Self {
        Self {
            attempts: count(counters, "attempts"),
            failures: failures(
                counters,
                KnowledgeFailureKind::ALL,
                KnowledgeFailureKind::as_str,
            ),
        }
    }
}

/// A `ModelStat` counts one model's requests, token usage, and the failures
/// its requests ended in. Returned by [`Stats::model_stats`], so agents
/// running different models can be compared.
#[derive(Debug, Clone, Serialize)]
pub struct ModelStat {
    /// Every request to this model, including the ones that failed.
    pub requests: u64,
    /// Input tokens across this model's responses.
    pub input_tokens: u64,
    /// Output tokens across this model's responses.
    pub output_tokens: u64,
    /// Those requests that failed, by the reason the event carried.
    pub failures: BTreeMap<RequestErrorKind, u64>,
}

impl ModelStat {
    /// Get the total failures across the kinds.
    pub fn errors(&self) -> u64 {
        self.failures.values().sum()
    }

    /// Get `errors / requests`, or `None` when the model was never asked.
    pub fn error_rate(&self) -> Option<f64> {
        rate(self.errors(), self.requests)
    }
}

impl From<&Counters> for ModelStat {
    fn from(counters: &Counters) -> Self {
        Self {
            requests: count(counters, "requests"),
            input_tokens: count(counters, "input_tokens"),
            output_tokens: count(counters, "output_tokens"),
            failures: failures(counters, RequestErrorKind::ALL, RequestErrorKind::as_str),
        }
    }
}

/// The failures of one subject, keyed by the reason its events carry. A kind
/// nothing failed under is left out, so an empty map reads as "nothing failed".
fn failures<K: Copy + Ord>(
    counters: &Counters,
    all: &[K],
    as_str: fn(&K) -> &'static str,
) -> BTreeMap<K, u64> {
    all.iter()
        .filter_map(|kind| match count(counters, as_str(kind)) {
            0 => None,
            n => Some((*kind, n)),
        })
        .collect()
}

/// Get `errors / attempts`, or `None` over an empty population.
fn rate(errors: u64, attempts: u64) -> Option<f64> {
    (attempts > 0).then(|| errors as f64 / attempts as f64)
}

/// Read one counter, absent meaning zero.
fn count(counters: &Counters, counter: &str) -> u64 {
    counters.get(counter).copied().unwrap_or(0)
}

/// A `TimeStat` is one duration summed over the tickets that reached it, and
/// its mean. Returned by [`Stats::ticket_duration`] and
/// [`Stats::work_duration`].
///
/// `average` has no answer until the first ticket resolves, while `total` is
/// zero, the honest sum over nothing.
#[derive(Debug, Clone, Serialize)]
pub struct TimeStat {
    pub total: Duration,
    pub average: Option<Duration>,
}

impl Stats {
    pub(crate) fn new() -> Self {
        Self {
            event_counts: Mutex::new(HashMap::new()),
            started_at: AtomicU64::new(0),
            finished_at: AtomicU64::new(0),
            ticket_duration: AtomicU64::new(0),
            work_duration: AtomicU64::new(0),
            label_stats: Mutex::new(HashMap::new()),
            agent_stats: Mutex::new(HashMap::new()),
            token_usage: Mutex::new(HashMap::new()),
            counters: Mutex::new(HashMap::new()),
        }
    }

    /// Get statistics scoped to one label. A label nothing was recorded
    /// against reads as zero.
    ///
    /// Every accessor answers the same question it answers run-wide, except
    /// `execution_duration()`, which is `None` because timing stays global.
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
    /// `event_count(EventName::TicketCreated)` counts the tickets that agent
    /// filed; the rest count the tickets it claimed.
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
        self.category_stats("tools")
    }

    /// Get per-path open and failure counts, sorted by path.
    ///
    /// A path may name a directory rather than a file, since the search tools
    /// accept either.
    pub fn file_stats(&self) -> BTreeMap<String, FileStat> {
        self.category_stats("files")
    }

    /// Get per-operation attempt and failure counts, sorted by operation.
    pub fn knowledge_stats(&self) -> BTreeMap<String, KnowledgeStat> {
        self.category_stats("knowledge")
    }

    /// Get per-model requests, failures, and token usage, sorted by model
    /// name.
    pub fn model_stats(&self) -> BTreeMap<String, ModelStat> {
        self.category_stats("models")
    }

    /// One category as the `*Stat` view its accessor returns, sorted by name.
    fn category_stats<T: for<'a> From<&'a Counters>>(&self, category: &str) -> BTreeMap<String, T> {
        self.category_counters(category)
            .iter()
            .map(|(name, counters)| (name.clone(), counters.into()))
            .collect()
    }

    /// One category's stored counters, sorted by name.
    fn category_counters(&self, category: &str) -> BTreeMap<String, Counters> {
        self.counters
            .lock()
            .unwrap()
            .get(category)
            .map(|entries| {
                entries
                    .iter()
                    .map(|(name, counters)| (name.clone(), counters.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Sum one counter across a whole category.
    fn total(&self, category: &str, counter: &str) -> u64 {
        self.counters
            .lock()
            .unwrap()
            .get(category)
            .map(|entries| entries.values().map(|c| count(c, counter)).sum())
            .unwrap_or(0)
    }

    /// Add one measure an event declared.
    fn add(&self, measure: &Measure) {
        self.add_count(
            measure.subject.category(),
            measure.subject.name(),
            measure.counter,
            measure.amount,
        );
    }

    fn add_count(&self, category: &'static str, name: &str, counter: &'static str, amount: u64) {
        *self
            .counters
            .lock()
            .unwrap()
            .entry(category)
            .or_default()
            .entry(name.to_string())
            .or_default()
            .entry(counter)
            .or_default() += amount;
    }

    /// Record an event.
    ///
    /// Every kind is counted by its name and adds the measures it declares, so
    /// a new variant needs no arm here.
    pub(crate) fn record_event(&self, kind: &EventKind, key: &str, labels: &[String], agent: &str) {
        // The per-ticket series belongs to the compaction estimator rather
        // than to the figures, so it is the one measure that stays run-wide.
        if let EventKind::RequestFinished { usage, .. } = kind {
            self.record_usage(key, usage.clone());
        }
        let measures = kind.measures();
        // One walk of the labels for every measure, not one walk per measure.
        self.record_scoped(labels, agent, |s| {
            s.count_event(kind.event_name());
            for measure in &measures {
                s.add(measure);
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
    fn count_event(&self, event: EventName) {
        *self.event_counts.lock().unwrap().entry(event).or_default() += 1;
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
        if matches!(next, Status::Finished | Status::Failed) {
            self.record_durations(ticket_duration, work_duration);
        }
    }

    /// Mirror a terminal transition onto every label slice the ticket carries.
    ///
    /// The start time is deliberately not mirrored, so `execution_duration()` reads
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
            slice.record_durations(ticket_duration, work_duration);
        }
    }

    /// Get how many events of one kind were recorded. A kind that never
    /// happened reads zero.
    pub fn event_count(&self, event: EventName) -> u64 {
        self.event_counts
            .lock()
            .unwrap()
            .get(&event)
            .copied()
            .unwrap_or(0)
    }

    /// Get per-event counts, in the order the kinds are declared.
    pub fn event_counts(&self) -> BTreeMap<EventName, u64> {
        self.event_counts
            .lock()
            .unwrap()
            .iter()
            .map(|(event, count)| (*event, *count))
            .collect()
    }

    /// Get the input tokens across requests, summed over the models. Every
    /// response names the model it came from, so the sum is the total.
    pub fn input_tokens(&self) -> u64 {
        self.total("models", "input_tokens")
    }

    /// Get the output tokens across requests, summed over the models.
    pub fn output_tokens(&self) -> u64 {
        self.total("models", "output_tokens")
    }

    /// Get the elapsed duration, which keeps growing while agents work and
    /// stops when execution ends. `None` until the first ticket starts.
    pub fn execution_duration(&self) -> Option<Duration> {
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

    /// How many tickets reached a terminal status, the population both
    /// duration averages divide by.
    fn tickets_resolved(&self) -> u64 {
        self.event_count(EventName::TicketFinished) + self.event_count(EventName::TicketFailed)
    }

    /// Get the time from creation to resolution, over every resolved ticket.
    pub fn ticket_duration(&self) -> TimeStat {
        self.time_stat(&self.ticket_duration)
    }

    /// Get the time agents spent working, over the same tickets. With agents
    /// working in parallel the total can exceed the elapsed duration.
    pub fn work_duration(&self) -> TimeStat {
        self.time_stat(&self.work_duration)
    }

    /// One stored sum of whole seconds, over the tickets that reached it.
    fn time_stat(&self, secs: &AtomicU64) -> TimeStat {
        let total = secs.load(Ordering::Relaxed);
        let resolved = self.tickets_resolved();
        TimeStat {
            total: Duration::from_secs(total),
            average: (resolved > 0).then(|| Duration::from_secs(total / resolved)),
        }
    }

    /// Record when execution ended. A later call overwrites the earlier one.
    pub(crate) fn record_execution_finished(&self, when: u64) {
        self.finished_at.store(when, Ordering::Relaxed);
    }

    /// Rebuild the statistics from tickets already read off disk, for when
    /// `stats.json` is missing or unreadable.
    pub(crate) fn derive(tickets: &HashMap<String, crate::agents::tickets::Ticket>) -> Self {
        let stats = Stats::new();
        for t in tickets.values() {
            // No agent scope: a rebuilt ticket cannot say which agent
            // worked it, and an empty slice beats a half-filled one.
            stats.record_scoped(&t.labels, "", |s| s.count_event(EventName::TicketCreated));
            let ticket_dur = ticket_duration(t).unwrap_or_default();
            let work_dur = work_duration(t).unwrap_or_default();
            let resolved = match t.status {
                Status::Finished => EventName::TicketFinished,
                Status::Failed => EventName::TicketFailed,
                Status::Todo | Status::InProgress => continue,
            };
            stats.record_scoped(&t.labels, "", |s| {
                s.count_event(resolved);
                s.record_durations(ticket_dur, work_dur);
            });
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
        let categories: Vec<(&Category, BTreeMap<String, Counters>)> = CATEGORIES
            .iter()
            .map(|category| (category, self.category_counters(category.name)))
            .filter(|(_, entries)| !entries.is_empty())
            .collect();
        let len = 5
            + usize::from(!events.is_empty())
            + usize::from(!labels.is_empty())
            + usize::from(!agents.is_empty())
            + categories.len();
        let mut st = serializer.serialize_struct("Stats", len)?;
        st.serialize_field("input_tokens", &self.input_tokens())?;
        st.serialize_field("output_tokens", &self.output_tokens())?;
        st.serialize_field(
            "execution_duration_secs",
            &self.execution_duration().map(|d| d.as_secs_f64()),
        )?;
        // `average_secs` is derived for readers and skipped on load, the same
        // contract `errors` and `error_rate` have inside a category.
        st.serialize_field("ticket_duration", &seconds(&self.ticket_duration()))?;
        st.serialize_field("work_duration", &seconds(&self.work_duration()))?;
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
        // Every category is written the same way: its stored counters
        // round-trip through load, while errors and error_rate are derived,
        // emitted for readers, and skipped on load.
        for (category, entries) in &categories {
            let nested: BTreeMap<&String, serde_json::Value> = entries
                .iter()
                .map(|(name, counters)| (name, category.body(counters)))
                .collect();
            st.serialize_field(category.name, &nested)?;
        }
        st.end()
    }
}

impl Stats {
    fn load_fields(&self, value: &serde_json::Value) {
        if let Some(events) = value.get("events").and_then(|v| v.as_object()) {
            let mut map = self.event_counts.lock().unwrap();
            for (name, count) in events {
                // One entry at a time, so a name this build does not know is
                // skipped rather than costing us the rest of the map.
                let event = serde_json::from_value(serde_json::Value::String(name.clone()));
                if let (Ok(event), Some(count)) = (event, count.as_u64()) {
                    map.insert(event, count);
                }
            }
        }
        let total_secs = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.get("total_secs"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        };
        self.ticket_duration
            .store(total_secs("ticket_duration"), Ordering::Relaxed);
        self.work_duration
            .store(total_secs("work_duration"), Ordering::Relaxed);

        for category in CATEGORIES {
            let Some(entries) = value.get(category.name).and_then(|v| v.as_object()) else {
                continue;
            };
            for (name, body) in entries {
                for counter in category.counters {
                    if let Some(count) = body.get(*counter).and_then(|v| v.as_u64()) {
                        self.add_count(category.name, name, counter, count);
                    }
                }
            }
        }
    }
}

impl Category {
    /// One subject's `stats.json` body: the stored counters plus the derived
    /// pair every category reports.
    fn body(&self, counters: &Counters) -> serde_json::Value {
        let mut body = serde_json::Map::new();
        for counter in self.counters {
            body.insert((*counter).to_string(), count(counters, counter).into());
        }
        let errors: u64 = self.failures.iter().map(|c| count(counters, c)).sum();
        let attempts: u64 = self.attempts.iter().map(|c| count(counters, c)).sum();
        body.insert("errors".to_string(), errors.into());
        body.insert(
            "error_rate".to_string(),
            serde_json::json!((attempts > 0).then(|| errors as f64 / attempts as f64)),
        );
        serde_json::Value::Object(body)
    }
}

/// One `TimeStat` as `stats.json` writes it, both figures in seconds.
fn seconds(stat: &TimeStat) -> serde_json::Value {
    serde_json::json!({
        "total_secs": stat.total.as_secs(),
        "average_secs": stat.average.map(|d| d.as_secs_f64()),
    })
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

/// Ticket timings. The store writes these directly rather than through events,
/// since a transition carries a duration no event reports. The counts that used
/// to live here are `EventName::Ticket*`.
impl Stats {
    /// The first call wins. A later claim leaves the original start time
    /// untouched.
    pub(crate) fn record_started(&self, when: u64) {
        let _ = self
            .started_at
            .compare_exchange(0, when, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// Add one resolved ticket's two spans to the sums. Finished and failed
    /// tickets add alike; which outcome it was, the event count answers.
    pub(crate) fn record_durations(&self, ticket: Duration, work: Duration) {
        self.ticket_duration
            .fetch_add(ticket.as_secs(), Ordering::Relaxed);
        self.work_duration
            .fetch_add(work.as_secs(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{KnowledgeOp, ToolFailureKind};

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

    fn tool_call(tool_name: &str) -> EventKind {
        EventKind::ToolCallStarted {
            tool_name: tool_name.into(),
            call_id: "c1".into(),
            input: serde_json::Value::Null,
        }
    }

    fn tool_failure(tool_name: &str, reason: ToolFailureKind) -> EventKind {
        EventKind::ToolCallFailed {
            tool_name: tool_name.into(),
            call_id: "c1".into(),
            reason,
            message: "boom".into(),
        }
    }

    fn file_open(path: &str) -> EventKind {
        EventKind::FileOpenFinished { path: path.into() }
    }

    fn file_open_failed(path: &str) -> EventKind {
        EventKind::FileOpenFailed {
            path: path.into(),
            reason: ToolFailureKind::ExecutionFailed,
        }
    }

    fn knowledge(op: KnowledgeOp) -> EventKind {
        EventKind::KnowledgeUsed { op }
    }

    fn knowledge_failed(op: KnowledgeOp) -> EventKind {
        EventKind::KnowledgeFailed {
            op,
            reason: KnowledgeFailureKind::PageMissing,
        }
    }

    /// One failure count out of a `*Stat`, absent meaning zero.
    fn failed<K: Ord>(failures: &BTreeMap<K, u64>, kind: K) -> u64 {
        failures.get(&kind).copied().unwrap_or(0)
    }

    /// File one ticket against `stats`, the way the store does.
    fn create(stats: &Stats) {
        stats.record_event(&EventKind::TicketCreated, "KEY", &[], "");
    }

    /// Resolve one ticket: the event that counts the outcome, then the two
    /// spans the store records alongside it.
    fn resolve(stats: &Stats, outcome: EventKind, ticket: Duration, agent: Duration) {
        stats.record_event(&outcome, "KEY", &[], "");
        stats.record_durations(ticket, agent);
    }

    fn finish(stats: &Stats, ticket_secs: u64, agent_secs: u64) {
        resolve(
            stats,
            EventKind::TicketFinished,
            Duration::from_secs(ticket_secs),
            Duration::from_secs(agent_secs),
        );
    }

    fn fail(stats: &Stats, ticket_secs: u64, agent_secs: u64) {
        resolve(
            stats,
            EventKind::TicketFailed,
            Duration::from_secs(ticket_secs),
            Duration::from_secs(agent_secs),
        );
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
        assert_eq!(s.event_count(EventName::TurnStarted), 0);
        assert_eq!(s.event_count(EventName::RequestFinished), 0);
        assert_eq!(s.event_count(EventName::ToolCallStarted), 0);
        assert_eq!(s.event_count(EventName::RequestFailed), 0);
        assert_eq!(s.input_tokens(), 0);
        assert_eq!(s.output_tokens(), 0);
        assert_eq!(s.event_count(EventName::TicketCreated), 0);
        assert_eq!(s.event_count(EventName::TicketFinished), 0);
        assert_eq!(s.event_count(EventName::TicketFailed), 0);
        assert_eq!(s.ticket_duration().total, Duration::ZERO);
        assert_eq!(s.work_duration().total, Duration::ZERO);
        assert!(s.execution_duration().is_none());
        assert!(s.ticket_duration().average.is_none());
        assert!(s.work_duration().average.is_none());
    }

    #[test]
    fn event_counts_show_up_in_reads() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &[], "");
        s.record_event(&turn(), "KEY", &[], "");
        s.record_event(&request(10, 5), "KEY", &[], "");
        s.record_event(&request(2, 1), "KEY", &[], "");
        s.record_event(&tool_call("bash"), "KEY", &[], "");
        s.record_event(&provider_error(), "KEY", &[], "");

        assert_eq!(s.event_count(EventName::TurnStarted), 2);
        assert_eq!(s.event_count(EventName::RequestFinished), 2);
        assert_eq!(s.event_count(EventName::ToolCallStarted), 1);
        assert_eq!(s.event_count(EventName::RequestFailed), 1);
        assert_eq!(s.input_tokens(), 12);
        assert_eq!(s.output_tokens(), 6);
        assert_eq!(s.event_counts()[&EventName::TurnStarted], 2);
    }

    #[test]
    fn ticket_stats_writes_show_up_in_reads() {
        let s = Stats::new();
        create(&s);
        create(&s);
        finish(&s, 3, 2);
        fail(&s, 5, 4);

        assert_eq!(s.event_count(EventName::TicketCreated), 2);
        assert_eq!(s.event_count(EventName::TicketFinished), 1);
        assert_eq!(s.event_count(EventName::TicketFailed), 1);
        assert_eq!(s.ticket_duration().total, Duration::from_secs(8));
        assert_eq!(s.work_duration().total, Duration::from_secs(6));
    }

    #[test]
    fn record_started_first_call_wins() {
        let s = Stats::new();
        s.record_started(1_000);
        s.record_started(2_000);
        s.record_started(3_000);
        s.record_execution_finished(4_500);
        assert_eq!(s.execution_duration(), Some(Duration::from_millis(3500)));
    }

    #[test]
    fn execution_duration_freezes_at_finish() {
        let s = Stats::new();
        assert!(s.execution_duration().is_none());
        s.record_started(1_000);
        // Live before finish: anchored at started_at = 1_000ms epoch, so the
        // delta to "now" is enormous. We just check it's some duration.
        assert!(s.execution_duration().is_some());
        s.record_execution_finished(2_500);
        assert_eq!(s.execution_duration(), Some(Duration::from_millis(1500)));
        // Stays frozen on a subsequent call.
        assert_eq!(s.execution_duration(), Some(Duration::from_millis(1500)));
    }

    #[test]
    fn avg_ticket_duration_is_arithmetic_mean() {
        let s = Stats::new();
        finish(&s, 2, 2);
        fail(&s, 4, 4);
        assert_eq!(s.ticket_duration().average, Some(Duration::from_secs(3)));
    }

    #[test]
    fn avg_work_duration_is_arithmetic_mean() {
        let s = Stats::new();
        finish(&s, 3, 2);
        fail(&s, 5, 4);
        assert_eq!(s.work_duration().average, Some(Duration::from_secs(3)));
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
        assert_eq!(
            s.stats_for_label("typo")
                .event_count(EventName::TurnStarted),
            0
        );
        assert_eq!(
            s.stats_for_agent("typo")
                .event_count(EventName::TurnStarted),
            0
        );

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
        assert_eq!(slice.event_count(EventName::TurnStarted), 1);
        assert_eq!(slice.input_tokens(), 10);
        assert_eq!(slice.output_tokens(), 5);
        assert_eq!(s.event_count(EventName::TurnStarted), 1);
        assert_eq!(s.input_tokens(), 10);
        // A label the events never named stays empty.
        assert_eq!(
            s.stats_for_label("other")
                .event_count(EventName::TurnStarted),
            0
        );
    }

    #[test]
    fn record_event_mirrors_onto_the_agent_slice() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &["scan".into()], "scout");
        s.record_event(&request(10, 5), "KEY", &["scan".into()], "scout");
        let slice = s.stats_for_agent("scout");
        assert_eq!(slice.event_count(EventName::TurnStarted), 1);
        assert_eq!(slice.input_tokens(), 10);
        assert_eq!(
            s.stats_for_agent("writer")
                .event_count(EventName::TurnStarted),
            0
        );
    }

    #[test]
    fn stats_for_agent_and_stats_for_label_do_not_share_a_slice() {
        // A claim stamps the agent's name onto the ticket's labels, so the
        // two namespaces collide unless the maps stay separate.
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &["scout".into()], "scout");
        assert_eq!(
            s.stats_for_label("scout")
                .event_count(EventName::TurnStarted),
            1
        );
        assert_eq!(
            s.stats_for_agent("scout")
                .event_count(EventName::TurnStarted),
            1
        );
        assert!(!Arc::ptr_eq(
            &s.stats_for_label("scout"),
            &s.stats_for_agent("scout"),
        ));
    }

    #[test]
    fn run_level_events_reach_no_agent_slice() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &[], "");
        assert_eq!(s.event_count(EventName::TurnStarted), 1);
        assert_eq!(s.stats_for_agent("").event_count(EventName::TurnStarted), 0);
    }

    #[test]
    fn stats_for_label_slice_execution_duration_is_none() {
        let s = Stats::new();
        let slice = s.stats_for_label("scan");
        finish(&slice, 2, 1);
        assert!(slice.execution_duration().is_none());
        assert_eq!(slice.event_count(EventName::TicketFinished), 1);
    }

    #[test]
    fn work_duration_can_exceed_execution_duration_with_concurrency() {
        // Two tickets, each 5s of work, finished in a 6s window:
        // models 2 agents working in parallel.
        let s = Stats::new();
        s.record_started(1_000);
        finish(&s, 5, 5);
        finish(&s, 5, 5);
        s.record_execution_finished(7_000);
        assert_eq!(s.execution_duration(), Some(Duration::from_secs(6)));
        assert_eq!(s.work_duration().total, Duration::from_secs(10));
    }

    #[test]
    fn stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_event(&turn(), "KEY", &[], "");
        s.record_event(&turn(), "KEY", &[], "");
        s.record_event(&request(100, 50), "KEY", &[], "");
        s.record_event(&tool_call("bash"), "KEY", &[], "");
        s.record_event(&provider_error(), "KEY", &[], "");
        create(&s);
        finish(&s, 7, 5);
        fail(&s, 3, 2);

        let slice = s.slice_for_label("scan");
        slice.record_event(&turn(), "KEY", &[], "");
        slice.record_event(&request(40, 20), "KEY", &[], "");
        create(&slice);
        finish(&slice, 4, 3);

        let agent_slice = s.slice_for_agent("seeker");
        agent_slice.record_event(&turn(), "KEY", &[], "");
        agent_slice.record_event(&request(30, 10), "KEY", &[], "");
        create(&agent_slice);
        fail(&agent_slice, 6, 2);

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();
        assert_eq!(restored.event_count(EventName::TurnStarted), 2);
        assert_eq!(restored.event_count(EventName::RequestFinished), 1);
        assert_eq!(restored.event_count(EventName::ToolCallStarted), 1);
        assert_eq!(restored.event_count(EventName::RequestFailed), 1);
        assert_eq!(restored.input_tokens(), 100);
        assert_eq!(restored.output_tokens(), 50);
        assert_eq!(restored.event_count(EventName::TicketCreated), 1);
        assert_eq!(restored.event_count(EventName::TicketFinished), 1);
        assert_eq!(restored.event_count(EventName::TicketFailed), 1);
        assert_eq!(restored.ticket_duration().total, Duration::from_secs(10));
        assert_eq!(restored.work_duration().total, Duration::from_secs(7));

        let restored_slice = restored.stats_for_label("scan");
        assert_eq!(restored_slice.event_count(EventName::TurnStarted), 1);
        assert_eq!(restored_slice.input_tokens(), 40);
        assert_eq!(restored_slice.event_count(EventName::TicketFinished), 1);
        assert_eq!(
            restored_slice.ticket_duration().total,
            Duration::from_secs(4)
        );

        let restored_agent = restored.stats_for_agent("seeker");
        assert_eq!(restored_agent.event_count(EventName::TurnStarted), 1);
        assert_eq!(restored_agent.input_tokens(), 30);
        assert_eq!(restored_agent.event_count(EventName::TicketFailed), 1);
        assert_eq!(
            restored_agent.ticket_duration().total,
            Duration::from_secs(6)
        );
    }

    #[test]
    fn stats_serializes_raw_counter_fields() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &[], "");
        s.record_event(&request(100, 50), "KEY", &[], "");
        s.record_event(&tool_call("bash"), "KEY", &[], "");
        s.record_event(&provider_error(), "KEY", &[], "");
        create(&s);
        finish(&s, 7, 5);

        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["input_tokens"], 100);
        assert_eq!(value["output_tokens"], 50);
        assert_eq!(value["events"]["ticket_created"], 1);
        assert_eq!(value["events"]["ticket_finished"], 1);
        assert_eq!(value["ticket_duration"]["total_secs"], 7);
        assert_eq!(value["work_duration"]["total_secs"], 5);
        assert_eq!(value["events"]["turn_started"], 1);
        assert_eq!(value["events"]["request_finished"], 1);
        assert_eq!(value["events"]["tool_call_started"], 1);
        assert_eq!(value["events"]["request_failed"], 1);
        // The counts live under `events` alone, never repeated at the top.
        assert!(value.get("turns").is_none());
    }

    #[test]
    fn stats_writes_each_duration_sum_once() {
        let s = Stats::new();
        create(&s);
        finish(&s, 7, 5);

        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("ticket_duration_secs").is_none());
        assert!(value.get("work_duration_secs").is_none());
    }

    #[test]
    fn duration_average_is_none_until_a_ticket_resolves() {
        let s = Stats::new();
        create(&s);

        assert_eq!(s.ticket_duration().total, Duration::ZERO);
        assert!(s.ticket_duration().average.is_none());

        finish(&s, 4, 3);

        assert_eq!(s.ticket_duration().total, Duration::from_secs(4));
        assert_eq!(s.ticket_duration().average, Some(Duration::from_secs(4)));
        assert_eq!(s.work_duration().average, Some(Duration::from_secs(3)));
    }

    #[test]
    fn stats_serializes_each_duration_as_a_total_and_an_average() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        finish(&s, 6, 4);
        finish(&s, 2, 2);

        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["ticket_duration"]["total_secs"], 8);
        assert_eq!(value["ticket_duration"]["average_secs"], 4.0);
        assert_eq!(value["work_duration"]["total_secs"], 6);
        assert_eq!(value["work_duration"]["average_secs"], 3.0);

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();
        // The average is derived on both sides, so only the sum round-trips.
        assert_eq!(restored.ticket_duration().total, Duration::from_secs(8));
        assert_eq!(restored.work_duration().total, Duration::from_secs(6));
    }

    #[test]
    fn load_skips_an_event_name_the_build_does_not_know() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let body = serde_json::json!({
            "events": { "turn_started": 3, "hyperdrive_engaged": 9 }
        });
        std::fs::write(
            dir.path().join(Stats::FILE),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();

        let restored = Stats::load(dir.path()).unwrap();
        assert_eq!(restored.event_count(EventName::TurnStarted), 3);
        assert_eq!(restored.event_counts().len(), 1);
    }

    #[test]
    fn stats_omits_events_when_empty() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("events").is_none());
    }

    #[test]
    fn stats_serializes_derived_execution_duration_seconds() {
        let s = Stats::new();
        s.record_started(1_000);
        s.record_execution_finished(3_500);
        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["execution_duration_secs"].as_f64().unwrap(), 2.5);
    }

    #[test]
    fn stats_serializes_execution_duration_secs_as_null_when_unstarted() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value["execution_duration_secs"].is_null());
    }

    #[test]
    fn stats_serializes_labels_as_nested_object() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY", &["scan".into()], "");
        s.record_event(&request(40, 20), "KEY", &["scan".into()], "");
        let value = serde_json::to_value(&s).unwrap();
        let labels = value["labels"].as_object().unwrap();
        assert_eq!(labels["scan"]["events"]["turn_started"], 1);
        assert_eq!(labels["scan"]["input_tokens"], 40);
        // A slice carries its own subject sections, and `load` reads them
        // back through the same path the run-wide statistics use.
        assert_eq!(labels["scan"]["models"]["m"]["requests"], 1);
    }

    #[test]
    fn slice_subject_maps_round_trip_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_event(&tool_call("bash"), "KEY", &["scan".into()], "scout");
        s.record_event(&provider_error(), "KEY", &["scan".into()], "scout");

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let label = restored.stats_for_label("scan");
        assert_eq!(label.tool_stats()["bash"].calls, 1);
        assert_eq!(
            failed(
                &label.model_stats()["m"].failures,
                RequestErrorKind::ConnectionFailed
            ),
            1
        );
        let agent = restored.stats_for_agent("scout");
        assert_eq!(agent.tool_stats()["bash"].calls, 1);
        assert_eq!(
            failed(
                &agent.model_stats()["m"].failures,
                RequestErrorKind::ConnectionFailed
            ),
            1
        );
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
        for kind in [
            tool_call("edit_file"),
            tool_call("edit_file"),
            tool_call("bash"),
            tool_failure("edit_file", ToolFailureKind::SchemaValidationFailed),
            tool_failure("bash", ToolFailureKind::ExecutionFailed),
            tool_failure("ghost", ToolFailureKind::ToolNotFound),
        ] {
            s.record_event(&kind, "KEY", &[], "");
        }

        let tools = s.tool_stats();
        let edit = &tools["edit_file"];
        assert_eq!(edit.calls, 2);
        assert_eq!(
            failed(&edit.failures, ToolFailureKind::SchemaValidationFailed),
            1
        );
        assert_eq!(edit.errors(), 1);
        assert_eq!(
            failed(&tools["bash"].failures, ToolFailureKind::ExecutionFailed),
            1
        );
        assert_eq!(
            failed(&tools["ghost"].failures, ToolFailureKind::ToolNotFound),
            1
        );
    }

    #[test]
    fn tool_stat_error_rate_is_errors_over_calls() {
        let s = Stats::new();
        for _ in 0..4 {
            s.record_event(&tool_call("edit_file"), "KEY", &[], "");
        }
        s.record_event(
            &tool_failure("edit_file", ToolFailureKind::SchemaValidationFailed),
            "KEY",
            &[],
            "",
        );

        let tools = s.tool_stats();
        assert_eq!(tools["edit_file"].error_rate(), Some(0.25));
    }

    #[test]
    fn tool_stat_error_rate_is_none_without_calls() {
        // A failure can be recorded for a name that never logged a call only
        // in malformed cases; the rate is then undefined rather than infinite.
        let s = Stats::new();
        s.record_event(
            &tool_failure("ghost", ToolFailureKind::ToolNotFound),
            "KEY",
            &[],
            "",
        );
        assert!(s.tool_stats()["ghost"].error_rate().is_none());
    }

    #[test]
    fn tool_stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        for kind in [
            tool_call("edit_file"),
            tool_call("edit_file"),
            tool_failure("edit_file", ToolFailureKind::SchemaValidationFailed),
            tool_call("bash"),
            tool_failure("bash", ToolFailureKind::ExecutionFailed),
        ] {
            s.record_event(&kind, "KEY", &[], "");
        }

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let tools = restored.tool_stats();
        assert_eq!(tools["edit_file"].calls, 2);
        assert_eq!(
            failed(
                &tools["edit_file"].failures,
                ToolFailureKind::SchemaValidationFailed
            ),
            1
        );
        assert_eq!(
            failed(&tools["bash"].failures, ToolFailureKind::ExecutionFailed),
            1
        );
    }

    #[test]
    fn stats_serializes_tools_as_nested_object() {
        let s = Stats::new();
        for kind in [
            tool_call("edit_file"),
            tool_call("edit_file"),
            tool_failure("edit_file", ToolFailureKind::SchemaValidationFailed),
        ] {
            s.record_event(&kind, "KEY", &[], "");
        }

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
        s.record_event(&tool_call("bash"), "KEY", &["scan".into()], "scout");

        assert_eq!(s.stats_for_label("scan").tool_stats()["bash"].calls, 1);
        assert_eq!(s.stats_for_agent("scout").tool_stats()["bash"].calls, 1);
        assert!(s.stats_for_label("audit").tool_stats().is_empty());
    }

    #[test]
    fn file_stats_records_opens_and_failures_per_path() {
        let s = Stats::new();
        for kind in [
            file_open("src/lib.rs"),
            file_open("src/lib.rs"),
            file_open("src/main.rs"),
            file_open_failed("src/missing.rs"),
            file_open_failed("src/missing.rs"),
        ] {
            s.record_event(&kind, "KEY", &[], "");
        }

        let files = s.file_stats();
        assert_eq!(files["src/lib.rs"].opens, 2);
        assert_eq!(
            failed(
                &files["src/lib.rs"].failures,
                ToolFailureKind::ExecutionFailed
            ),
            0
        );
        assert_eq!(files["src/lib.rs"].error_rate(), Some(0.0));
        assert_eq!(files["src/main.rs"].opens, 1);
        // A failed open is an open too, so the rate divides by every attempt.
        assert_eq!(files["src/missing.rs"].opens, 2);
        assert_eq!(
            failed(
                &files["src/missing.rs"].failures,
                ToolFailureKind::ExecutionFailed
            ),
            2
        );
        assert_eq!(files["src/missing.rs"].errors(), 2);
        assert_eq!(files["src/missing.rs"].error_rate(), Some(1.0));
    }

    #[test]
    fn file_stats_reach_the_label_slice() {
        let s = Stats::new();
        s.record_event(&file_open("src/lib.rs"), "KEY", &["scan".into()], "");

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
        for kind in [
            file_open("src/lib.rs"),
            file_open("src/lib.rs"),
            file_open_failed("src/missing.rs"),
        ] {
            s.record_event(&kind, "KEY", &[], "");
        }

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let files = restored.file_stats();
        assert_eq!(files["src/lib.rs"].opens, 2);
        assert_eq!(files["src/missing.rs"].opens, 1);
        assert_eq!(
            failed(
                &files["src/missing.rs"].failures,
                ToolFailureKind::ExecutionFailed
            ),
            1
        );
    }

    #[test]
    fn stats_serializes_files_as_nested_object() {
        let s = Stats::new();
        for kind in [
            file_open("src/lib.rs"),
            file_open("src/lib.rs"),
            file_open_failed("src/lib.rs"),
        ] {
            s.record_event(&kind, "KEY", &[], "");
        }

        let value = serde_json::to_value(&s).unwrap();
        let files = value["files"].as_object().unwrap();
        assert_eq!(files["src/lib.rs"]["opens"], 3);
        assert_eq!(files["src/lib.rs"]["execution_failed"], 1);
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
        for kind in [
            knowledge(KnowledgeOp::Write),
            knowledge(KnowledgeOp::Read),
            knowledge(KnowledgeOp::Read),
            knowledge(KnowledgeOp::Remove),
            knowledge(KnowledgeOp::List),
            knowledge_failed(KnowledgeOp::Read),
        ] {
            s.record_event(&kind, "KEY", &[], "");
        }

        let k = s.knowledge_stats();
        assert_eq!(k["write"].attempts, 1);
        // The failed read counts as a read as well.
        assert_eq!(k["read"].attempts, 3);
        assert_eq!(k["remove"].attempts, 1);
        assert_eq!(k["list"].attempts, 1);
        assert_eq!(
            failed(&k["read"].failures, KnowledgeFailureKind::PageMissing),
            1
        );
        assert_eq!(k["read"].errors(), 1);
    }

    #[test]
    fn knowledge_stats_attribute_a_failure_to_its_operation() {
        let s = Stats::new();
        s.record_event(&knowledge(KnowledgeOp::Write), "KEY", &[], "");
        s.record_event(&knowledge_failed(KnowledgeOp::Read), "KEY", &[], "");

        let k = s.knowledge_stats();
        assert_eq!(
            failed(&k["read"].failures, KnowledgeFailureKind::PageMissing),
            1
        );
        assert_eq!(k["read"].error_rate(), Some(1.0));
        assert_eq!(
            failed(&k["write"].failures, KnowledgeFailureKind::PageMissing),
            0
        );
        assert_eq!(k["write"].error_rate(), Some(0.0));
    }

    #[test]
    fn knowledge_reaches_the_label_slice() {
        let s = Stats::new();
        s.record_event(&knowledge(KnowledgeOp::Write), "KEY", &["scan".into()], "");

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
        for kind in [
            knowledge(KnowledgeOp::Write),
            knowledge(KnowledgeOp::Read),
            knowledge_failed(KnowledgeOp::Read),
        ] {
            s.record_event(&kind, "KEY", &[], "");
        }

        use crate::persistence::Persist;
        s.save(dir.path()).unwrap();
        let restored = Stats::load(dir.path()).unwrap();

        let k = restored.knowledge_stats();
        assert_eq!(k["write"].attempts, 1);
        assert_eq!(k["read"].attempts, 2);
        assert_eq!(
            failed(&k["read"].failures, KnowledgeFailureKind::PageMissing),
            1
        );
    }

    #[test]
    fn stats_serializes_knowledge_as_nested_object() {
        let s = Stats::new();
        s.record_event(&knowledge(KnowledgeOp::Write), "KEY", &[], "");
        s.record_event(&knowledge_failed(KnowledgeOp::Read), "KEY", &[], "");

        let value = serde_json::to_value(&s).unwrap();
        let pages = value["knowledge"].as_object().unwrap();
        assert_eq!(pages["write"]["attempts"], 1);
        assert_eq!(pages["read"]["attempts"], 1);
        assert_eq!(pages["read"]["page_missing"], 1);
        assert_eq!(pages["read"]["errors"], 1);
        assert_eq!(pages["read"]["error_rate"].as_f64().unwrap(), 1.0);
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
        assert_eq!(failed(&m.failures, RequestErrorKind::ConnectionFailed), 0);
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
        assert_eq!(failed(&m.failures, RequestErrorKind::ConnectionFailed), 1);
        assert_eq!(m.errors(), 1);
        assert_eq!(m.error_rate(), Some(0.5));
        assert_eq!(s.event_count(EventName::RequestFailed), 1);
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
        assert_eq!(
            failed(&models["m"].failures, RequestErrorKind::ConnectionFailed),
            1
        );
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
        assert_eq!(models["m"]["connection_failed"], 0);
        assert_eq!(models["m"]["errors"], 0);
        assert_eq!(models["m"]["input_tokens"], 10);
        assert_eq!(models["m"]["output_tokens"], 5);
    }

    #[test]
    fn category_attempts_and_failures_are_stored_counters() {
        // `load` reads only what `counters` lists, so a derived pair built
        // from a counter outside it would not survive a save and load.
        for category in CATEGORIES {
            for counter in category.attempts.iter().chain(category.failures) {
                assert!(
                    category.counters.contains(counter),
                    "{}: {counter} is not a stored counter",
                    category.name,
                );
            }
        }
    }

    #[test]
    fn every_category_splits_its_failures_by_reason() {
        let s = Stats::new();
        s.record_event(
            &tool_failure("bash", ToolFailureKind::ExecutionFailed),
            "KEY",
            &[],
            "",
        );
        s.record_event(&file_open_failed("src/lib.rs"), "KEY", &[], "");
        s.record_event(&provider_error(), "KEY", &[], "");
        s.record_event(&knowledge_failed(KnowledgeOp::Read), "KEY", &[], "");

        // Each failure lands under its own reason, and no category keeps a
        // bare `failed` column beside them.
        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["tools"]["bash"]["execution_failed"], 1);
        assert_eq!(value["files"]["src/lib.rs"]["execution_failed"], 1);
        assert_eq!(value["models"]["m"]["connection_failed"], 1);
        assert_eq!(value["knowledge"]["read"]["page_missing"], 1);
        for category in ["tools", "files", "models", "knowledge"] {
            assert!(
                !value[category].to_string().contains("\"failed\""),
                "{category} still carries a bare failed",
            );
        }
    }

    #[test]
    fn a_reason_nothing_failed_under_is_left_out() {
        let s = Stats::new();
        s.record_event(&tool_call("bash"), "KEY", &[], "");
        s.record_event(&tool_call("bash"), "KEY", &[], "");
        s.record_event(
            &tool_failure("bash", ToolFailureKind::ExecutionFailed),
            "KEY",
            &[],
            "",
        );

        let bash = &s.tool_stats()["bash"];
        assert_eq!(bash.failures.len(), 1);
        assert_eq!(bash.failures[&ToolFailureKind::ExecutionFailed], 1);
        assert_eq!(bash.errors(), 1);
        assert_eq!(bash.error_rate(), Some(0.5));
    }

    #[test]
    fn stats_omits_models_when_empty() {
        let s = Stats::new();
        let value = serde_json::to_value(&s).unwrap();
        assert!(value.get("models").is_none());
    }
}
