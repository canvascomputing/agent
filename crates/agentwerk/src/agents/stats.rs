//! What a run has spent, counted as it goes.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::event::{Event, EventKind, EventName};
use crate::providers::TokenUsage;

/// `Stats` counts a run as it happens, so the limit check that fires every
/// 50ms, the remaining turns and tokens a system prompt reports, and the
/// compaction estimate all read the current figures without touching the
/// filesystem. `TicketQueue::load` folds a session's log back into one, so a
/// resumed run keeps what it already spent.
///
/// Crate-internal on purpose. A host reads the three totals off
/// [`TicketQueue`](crate::TicketQueue) and folds anything finer out of the
/// events themselves.
pub(crate) struct Stats {
    /// Count per event kind. Every recorded event lands here, and
    /// [`Stats::event_count`] is a lookup into this map.
    event_counts: Mutex<HashMap<EventName, u64>>,
    /// Input tokens across the finished requests.
    input_tokens: AtomicU64,
    /// Output tokens across the finished requests.
    output_tokens: AtomicU64,
    /// When execution started, in milliseconds since the epoch. 0 until the
    /// first ticket starts, and the first writer wins.
    started_at: AtomicU64,
    /// When execution ended, in milliseconds since the epoch. 0 while it is
    /// still running.
    finished_at: AtomicU64,
    /// Token usage per ticket, oldest first. It feeds the estimate that decides
    /// when to compact, and is cleared once that happens.
    token_usage: Mutex<HashMap<String, Vec<TokenUsage>>>,
}

impl Stats {
    const FILE: &'static str = "events.jsonl";

    /// Visit every event in `dir`'s log, oldest first. A directory with no log
    /// visits nothing, and a line this build cannot parse is skipped rather
    /// than costing the caller every line after it.
    pub(crate) fn for_each_event(dir: &Path, mut visit: impl FnMut(&Event)) -> io::Result<()> {
        let path = dir.join(Self::FILE);
        if !path.exists() {
            return Ok(());
        }
        for line in std::fs::read_to_string(&path)?.lines() {
            if let Ok(event) = serde_json::from_str::<Event>(line) {
                visit(&event);
            }
        }
        Ok(())
    }

    /// Get how many events of one kind were recorded. A kind that never
    /// happened reads zero.
    pub(crate) fn event_count(&self, event: EventName) -> u64 {
        self.event_counts
            .lock()
            .unwrap()
            .get(&event)
            .copied()
            .unwrap_or(0)
    }

    /// Get the input tokens across the finished requests.
    pub(crate) fn input_tokens(&self) -> u64 {
        self.input_tokens.load(Ordering::Relaxed)
    }

    /// Get the output tokens across the finished requests.
    pub(crate) fn output_tokens(&self) -> u64 {
        self.output_tokens.load(Ordering::Relaxed)
    }

    /// Get the elapsed duration, which keeps growing while agents work and
    /// stops when execution ends. `None` until the first ticket starts.
    pub(crate) fn execution_duration(&self) -> Option<Duration> {
        let started = self.started_at.load(Ordering::Relaxed);
        if started == 0 {
            return None;
        }
        let finished = self.finished_at.load(Ordering::Relaxed);
        if finished != 0 && finished >= started {
            return Some(Duration::from_millis(finished - started));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis() as u64;
        Some(Duration::from_millis(now.saturating_sub(started)))
    }

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

    /// Add one event as a single line to `<dir>/events.jsonl`, without reading
    /// the file.
    pub(crate) fn append(dir: &Path, event: &Event) -> io::Result<()> {
        let line = serde_json::to_string(event).map_err(io::Error::other)?;
        crate::persistence::append_line(&dir.join(Self::FILE), &line)
    }

    /// Fold one event in. The single writer, so a live queue and a log read
    /// back off disk arrive at the same figures.
    ///
    /// Every kind is counted by its name, so a new variant needs no arm here.
    pub(crate) fn record(&self, event: &Event) {
        *self
            .event_counts
            .lock()
            .unwrap()
            .entry(event.kind.event_name())
            .or_default() += 1;
        match &event.kind {
            EventKind::RequestFinished { usage, .. } => {
                self.input_tokens
                    .fetch_add(usage.input_tokens, Ordering::Relaxed);
                self.output_tokens
                    .fetch_add(usage.output_tokens, Ordering::Relaxed);
                self.record_usage(&event.ticket_key, usage.clone());
            }
            // The first claim starts the clock; the run ending stops it.
            EventKind::TicketStarted => {
                let _ = self.started_at.compare_exchange(
                    0,
                    event.created_at,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
            }
            EventKind::RunFinished { .. } => {
                self.finished_at.store(event.created_at, Ordering::Relaxed)
            }
            _ => {}
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

    /// Drop a ticket's token usage, once its older messages are summarized
    /// and the earlier trend no longer predicts the next request.
    pub(crate) fn reset_usage(&self, ticket_key: &str) {
        self.token_usage.lock().unwrap().remove(ticket_key);
    }

    /// Start the clock over for a run resuming from a log that already holds
    /// one. Without this a session picked up tomorrow would measure `max_time`
    /// from yesterday and breach it on the first turn.
    pub(crate) fn restart_clock(&self) {
        self.started_at.store(0, Ordering::Relaxed);
        self.finished_at.store(0, Ordering::Relaxed);
    }

    /// Append `usage` to the per-ticket series.
    fn record_usage(&self, ticket_key: &str, usage: TokenUsage) {
        self.token_usage
            .lock()
            .unwrap()
            .entry(ticket_key.to_string())
            .or_default()
            .push(usage);
    }
}

#[cfg(test)]
impl Stats {
    /// Lets a test state the events it cares about without writing a log first.
    pub(crate) fn of(kinds: impl IntoIterator<Item = EventKind>) -> Self {
        let stats = Stats::new();
        for kind in kinds {
            stats.record(&Event::new("", "", None, kind));
        }
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::FinishReason;

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

    fn run_finished() -> EventKind {
        EventKind::RunFinished {
            reason: FinishReason::Drained,
        }
    }

    /// Every event in `dir`'s log folded back into fresh figures, the way
    /// `TicketQueue::load` folds them as it resumes a session.
    fn loaded(dir: &std::path::Path) -> Stats {
        let stats = Stats::new();
        Stats::for_each_event(dir, |event| stats.record(event)).unwrap();
        stats
    }

    /// Pins the stamp, which the clock reads and `Event::new` would set to now.
    fn at(created_at: u64, kind: EventKind) -> Event {
        Event {
            created_at,
            ..Event::new("", "TICKET-1", None, kind)
        }
    }

    #[test]
    fn fresh_stats_are_zero() {
        let stats = Stats::new();
        assert_eq!(stats.event_count(EventName::TurnStarted), 0);
        assert_eq!(stats.input_tokens(), 0);
        assert_eq!(stats.output_tokens(), 0);
        assert_eq!(stats.execution_duration(), None);
    }

    #[test]
    fn every_kind_is_counted_by_its_name() {
        let stats = Stats::of(crate::event::tests::all_variants());
        for kind in crate::event::tests::all_variants() {
            assert!(
                stats.event_count(kind.event_name()) > 0,
                "{} was never counted",
                kind.event_name(),
            );
        }
    }

    #[test]
    fn a_request_adds_its_usage_to_the_token_totals() {
        let stats = Stats::of([request(10, 5), request(2, 1)]);
        assert_eq!(stats.input_tokens(), 12);
        assert_eq!(stats.output_tokens(), 6);
        assert_eq!(stats.event_count(EventName::RequestFinished), 2);
    }

    #[test]
    fn a_request_appends_to_the_ticket_usage_series() {
        let stats = Stats::new();
        stats.record(&at(0, request(100, 10)));
        stats.record(&at(0, request(200, 20)));

        let series = stats.usage_for_ticket("TICKET-1");
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].input_tokens, 100);
        assert_eq!(series[1].input_tokens, 200);
    }

    #[test]
    fn reset_usage_clears_one_ticket_without_touching_others() {
        let stats = Stats::new();
        stats.record(&at(0, request(100, 10)));
        stats.record(&Event::new("", "TICKET-2", None, request(300, 30)));

        stats.reset_usage("TICKET-1");

        assert!(stats.usage_for_ticket("TICKET-1").is_empty());
        assert_eq!(stats.usage_for_ticket("TICKET-2").len(), 1);
    }

    #[test]
    fn the_first_ticket_to_start_starts_the_clock() {
        let stats = Stats::new();
        stats.record(&at(1_000, EventKind::TicketStarted));
        stats.record(&at(2_000, EventKind::TicketStarted));
        stats.record(&at(4_500, run_finished()));

        assert_eq!(
            stats.execution_duration(),
            Some(Duration::from_millis(3_500))
        );
    }

    #[test]
    fn execution_duration_freezes_when_the_run_ends() {
        let stats = Stats::new();
        stats.record(&at(1_000, EventKind::TicketStarted));
        stats.record(&at(2_500, run_finished()));

        let frozen = stats.execution_duration();
        stats.record(&at(9_000, turn()));
        assert_eq!(stats.execution_duration(), frozen);
    }

    #[test]
    fn execution_duration_is_none_until_a_ticket_starts() {
        let stats = Stats::of([EventKind::RunStarted, turn()]);
        assert_eq!(stats.execution_duration(), None);
    }

    #[test]
    fn restart_clock_keeps_the_counts_a_resumed_run_already_spent() {
        let stats = Stats::new();
        stats.record(&at(1_000, EventKind::TicketStarted));
        stats.record(&at(2_000, request(900, 120)));
        stats.record(&at(3_000, run_finished()));

        stats.restart_clock();

        assert_eq!(stats.execution_duration(), None);
        assert_eq!(stats.input_tokens(), 900);
        assert_eq!(stats.event_count(EventName::TicketStarted), 1);
    }

    #[test]
    fn load_reads_a_directory_without_a_log_as_no_events() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let stats = loaded(dir.path());
        assert_eq!(stats.event_count(EventName::TicketCreated), 0);
        assert_eq!(stats.input_tokens(), 0);
        assert_eq!(stats.execution_duration(), None);
    }

    #[test]
    fn load_folds_back_what_a_run_wrote() {
        let dir = crate::test_util::TempDir::new().unwrap();
        for event in [
            at(1_000, EventKind::TicketStarted),
            at(2_000, request(900, 120)),
            at(2_000, turn()),
            at(5_000, run_finished()),
        ] {
            Stats::append(dir.path(), &event).unwrap();
        }

        let stats = loaded(dir.path());
        assert_eq!(stats.input_tokens(), 900);
        assert_eq!(stats.output_tokens(), 120);
        assert_eq!(stats.event_count(EventName::TurnStarted), 1);
        assert_eq!(
            stats.execution_duration(),
            Some(Duration::from_millis(4_000))
        );
    }

    #[test]
    fn load_skips_a_line_it_cannot_read() {
        let dir = crate::test_util::TempDir::new().unwrap();
        Stats::append(dir.path(), &at(0, turn())).unwrap();
        crate::persistence::append_line(
            &dir.path().join("events.jsonl"),
            r#"{"event":"from_a_later_build"}"#,
        )
        .unwrap();
        Stats::append(dir.path(), &at(0, turn())).unwrap();

        let stats = loaded(dir.path());
        assert_eq!(stats.event_count(EventName::TurnStarted), 2);
    }

    #[test]
    fn every_variant_survives_the_log() {
        // The figures are folded from lines read back off disk, so a variant
        // that cannot round-trip is a kind the statistics never see.
        let dir = crate::test_util::TempDir::new().unwrap();
        for kind in crate::event::tests::all_variants() {
            Stats::append(dir.path(), &Event::new("agent", "TICKET-1", None, kind)).unwrap();
        }

        let stats = loaded(dir.path());
        for kind in crate::event::tests::all_variants() {
            assert!(
                stats.event_count(kind.event_name()) > 0,
                "{} missing from the log",
                kind.event_name(),
            );
        }
    }
}
