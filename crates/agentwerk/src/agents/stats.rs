//! What a run counted: its events, the tokens they cost, and how long it took.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

use crate::agents::tickets::Status;
use crate::event::{EventKind, EventName};
use crate::providers::types::TokenUsage;

/// `Stats` holds the run-wide counts: how often each event happened, the tokens
/// the requests cost, and the elapsed duration. Reach it through
/// [`TicketQueue::stats()`](crate::TicketQueue::stats), during execution or
/// after it finishes.
///
/// Anything finer is yours to derive. An event names its tool, its model, its
/// agent, and its ticket's label, so a handler on
/// [`TicketQueue::on_event`](crate::TicketQueue::on_event) counts whichever of
/// those you care about.
///
/// ```no_run
/// use agentwerk::event::EventName;
/// use agentwerk::TicketQueue;
///
/// # async fn run() {
/// let tickets = TicketQueue::new();
/// tickets.finish_all().await;
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
    /// Input tokens across the finished requests.
    input_tokens: AtomicU64,
    /// Output tokens across the finished requests.
    output_tokens: AtomicU64,
    /// When execution started, in milliseconds since the epoch. 0 until then,
    /// and the first writer wins.
    started_at: AtomicU64,
    /// When execution ended, in milliseconds since the epoch. 0 while it is
    /// still running.
    finished_at: AtomicU64,
    /// Token usage per ticket, oldest first. It feeds the estimate that decides
    /// when to compact, and is cleared once that happens.
    token_usage: Mutex<HashMap<String, Vec<TokenUsage>>>,
}

impl Stats {
    pub(crate) fn new() -> Self {
        Self {
            event_counts: Mutex::new(HashMap::new()),
            input_tokens: AtomicU64::new(0),
            output_tokens: AtomicU64::new(0),
            started_at: AtomicU64::new(0),
            finished_at: AtomicU64::new(0),
            token_usage: Mutex::new(HashMap::new()),
        }
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

    /// Record an event.
    ///
    /// Every kind is counted by its name, so a new variant needs no arm here.
    /// Only a finished request carries anything beyond its own count.
    pub(crate) fn record_event(&self, kind: &EventKind, ticket_key: &str) {
        self.count_event(kind.event_name());
        if let EventKind::RequestFinished { usage, .. } = kind {
            self.input_tokens
                .fetch_add(usage.input_tokens, Ordering::Relaxed);
            self.output_tokens
                .fetch_add(usage.output_tokens, Ordering::Relaxed);
            self.record_usage(ticket_key, usage.clone());
        }
    }

    /// Add one to this event kind's count.
    fn count_event(&self, event: EventName) {
        *self.event_counts.lock().unwrap().entry(event).or_default() += 1;
    }

    /// Record a ticket transition. The first claim starts the execution clock,
    /// which no event reports.
    pub(crate) fn record_transition(&self, prev: Status, next: Status, now: u64) {
        if prev == Status::Todo && next == Status::InProgress {
            self.record_started(now);
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

    /// Get the input tokens across the finished requests.
    pub fn input_tokens(&self) -> u64 {
        self.input_tokens.load(Ordering::Relaxed)
    }

    /// Get the output tokens across the finished requests.
    pub fn output_tokens(&self) -> u64 {
        self.output_tokens.load(Ordering::Relaxed)
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

    /// Record when execution started. The first call wins, so a later claim
    /// leaves the original start time untouched.
    pub(crate) fn record_started(&self, when: u64) {
        let _ = self
            .started_at
            .compare_exchange(0, when, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// Record when execution ended. A later call overwrites the earlier one.
    pub(crate) fn record_execution_finished(&self, when: u64) {
        self.finished_at.store(when, Ordering::Relaxed);
    }

    /// Rebuild the statistics from tickets already read off disk, for when
    /// `stats.json` is missing or unreadable. A ticket file records its
    /// outcome and nothing else, so the tokens and the timings stay at zero.
    pub(crate) fn derive(tickets: &HashMap<String, crate::agents::tickets::Ticket>) -> Self {
        let stats = Stats::new();
        for t in tickets.values() {
            stats.count_event(EventName::TicketCreated);
            match t.status {
                Status::Finished => stats.count_event(EventName::TicketFinished),
                Status::Failed => stats.count_event(EventName::TicketFailed),
                Status::Todo | Status::InProgress => {}
            }
        }
        stats
    }

    /// Read back from `<dir>/stats.json`.
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
        if let Some(events) = value.get("events").and_then(|v| v.as_object()) {
            let mut map = stats.event_counts.lock().unwrap();
            for (name, count) in events {
                // One entry at a time, so a name this build does not know is
                // skipped rather than costing us the rest of the map.
                let event = serde_json::from_value(serde_json::Value::String(name.clone()));
                if let (Ok(event), Some(count)) = (event, count.as_u64()) {
                    map.insert(event, count);
                }
            }
        }
        let total = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        stats
            .input_tokens
            .store(total("input_tokens"), Ordering::Relaxed);
        stats
            .output_tokens
            .store(total("output_tokens"), Ordering::Relaxed);
        Ok(stats)
    }
}

impl Serialize for Stats {
    /// `execution_duration_secs` is derived for readers and skipped on load,
    /// since the two timestamps behind it are not written.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let events = self.event_counts();
        let mut st = serializer.serialize_struct("Stats", 3 + usize::from(!events.is_empty()))?;
        st.serialize_field("input_tokens", &self.input_tokens())?;
        st.serialize_field("output_tokens", &self.output_tokens())?;
        st.serialize_field(
            "execution_duration_secs",
            &self.execution_duration().map(|d| d.as_secs_f64()),
        )?;
        if !events.is_empty() {
            st.serialize_field("events", &events)?;
        }
        st.end()
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

    fn tool_call(tool_name: &str) -> EventKind {
        EventKind::ToolCallStarted {
            tool_name: tool_name.into(),
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

    /// File one ticket against `stats`, the way the store does.
    fn create(stats: &Stats) {
        stats.record_event(&EventKind::TicketCreated, "KEY");
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
        assert!(s.execution_duration().is_none());
    }

    #[test]
    fn event_counts_show_up_in_reads() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY");
        s.record_event(&turn(), "KEY");
        s.record_event(&request(10, 5), "KEY");
        s.record_event(&request(2, 1), "KEY");
        s.record_event(&tool_call("bash"), "KEY");
        s.record_event(&provider_error(), "KEY");

        assert_eq!(s.event_count(EventName::TurnStarted), 2);
        assert_eq!(s.event_count(EventName::RequestFinished), 2);
        assert_eq!(s.event_count(EventName::ToolCallStarted), 1);
        assert_eq!(s.event_count(EventName::RequestFailed), 1);
        assert_eq!(s.event_counts()[&EventName::TurnStarted], 2);
    }

    #[test]
    fn request_finished_adds_its_usage_to_the_token_totals() {
        let s = Stats::new();
        s.record_event(&request(10, 5), "KEY");
        s.record_event(&request(2, 1), "KEY");
        // A failed request reports no usage, so it moves neither total.
        s.record_event(&provider_error(), "KEY");

        assert_eq!(s.input_tokens(), 12);
        assert_eq!(s.output_tokens(), 6);
    }

    #[test]
    fn ticket_outcomes_show_up_in_reads() {
        let s = Stats::new();
        create(&s);
        create(&s);
        s.record_event(&EventKind::TicketFinished, "KEY");
        s.record_event(&EventKind::TicketFailed, "KEY");

        assert_eq!(s.event_count(EventName::TicketCreated), 2);
        assert_eq!(s.event_count(EventName::TicketFinished), 1);
        assert_eq!(s.event_count(EventName::TicketFailed), 1);
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
    fn claiming_a_ticket_starts_the_execution_clock() {
        let s = Stats::new();
        s.record_transition(Status::Todo, Status::InProgress, 1_000);
        s.record_execution_finished(3_000);
        assert_eq!(s.execution_duration(), Some(Duration::from_secs(2)));
    }

    #[test]
    fn finishing_a_ticket_does_not_start_the_execution_clock() {
        let s = Stats::new();
        s.record_transition(Status::InProgress, Status::Finished, 1_000);
        assert!(s.execution_duration().is_none());
    }

    #[test]
    fn stats_round_trips_through_save_load() {
        let dir = crate::test_util::TempDir::new().unwrap();

        let s = Stats::new();
        s.record_event(&turn(), "KEY");
        s.record_event(&turn(), "KEY");
        s.record_event(&request(100, 50), "KEY");
        s.record_event(&tool_call("bash"), "KEY");
        s.record_event(&provider_error(), "KEY");
        create(&s);
        s.record_event(&EventKind::TicketFinished, "KEY");
        s.record_event(&EventKind::TicketFailed, "KEY");

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
    }

    #[test]
    fn stats_serializes_raw_counter_fields() {
        let s = Stats::new();
        s.record_event(&turn(), "KEY");
        s.record_event(&request(100, 50), "KEY");
        s.record_event(&tool_call("bash"), "KEY");
        s.record_event(&provider_error(), "KEY");
        create(&s);
        s.record_event(&EventKind::TicketFinished, "KEY");

        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["input_tokens"], 100);
        assert_eq!(value["output_tokens"], 50);
        assert_eq!(value["events"]["ticket_created"], 1);
        assert_eq!(value["events"]["ticket_finished"], 1);
        assert_eq!(value["events"]["turn_started"], 1);
        assert_eq!(value["events"]["request_finished"], 1);
        assert_eq!(value["events"]["tool_call_started"], 1);
        assert_eq!(value["events"]["request_failed"], 1);
        // The counts live under `events` alone, never repeated at the top.
        assert!(value.get("turns").is_none());
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
    fn load_reads_a_file_written_before_the_sections_were_dropped() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let body = serde_json::json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "events": { "turn_started": 3 },
            "labels": { "scan": { "input_tokens": 40 } },
            "models": { "m": { "requests": 1, "input_tokens": 100 } },
        });
        std::fs::write(
            dir.path().join(Stats::FILE),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();

        let restored = Stats::load(dir.path()).unwrap();
        assert_eq!(restored.event_count(EventName::TurnStarted), 3);
        assert_eq!(restored.input_tokens(), 100);
        assert_eq!(restored.output_tokens(), 50);
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
    fn request_finished_appends_to_the_ticket_usage_series() {
        let s = Stats::new();
        s.record_event(&request(100, 10), "TICKET-1");
        s.record_event(&request(200, 20), "TICKET-1");

        let series = s.usage_for_ticket("TICKET-1");
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].input_tokens, 100);
        assert_eq!(series[1].input_tokens, 200);
    }
}
