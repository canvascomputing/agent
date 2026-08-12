//! Every change a [`TicketQueue`] makes to its tickets, and the events each
//! change emits.

use std::path::{Path, PathBuf};

use crate::event::EventKind;
use crate::persistence::{Append, Persist, Results, TicketEvents};
use crate::schemas::SchemaViolations;

use super::error::TicketError;
use super::reply::Reply;
use super::ticket::{Status, Ticket};
use super::ticket_queue::TicketQueue;
use super::{now_millis, numeric_id, Replies};

/// Highest `TICKET-<N>` already on disk under `<dir>/tickets/`, or 0 if
/// none. Only needed for a queue built via `new()`, which never reads
/// the directory itself; `load()` derives this from what it already read.
fn max_existing_ticket_id(dir: &Path) -> u64 {
    std::fs::read_dir(dir.join("tickets"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            name.to_str().map(numeric_id).map(u64::from)
        })
        .filter(|&n| n != u64::from(u32::MAX))
        .max()
        .unwrap_or(0)
}

impl TicketQueue {
    /// Insert `ticket`, filling in the fields agentwerk owns. The ticket is always born
    /// `Todo`; to pin it to a specific agent, label it with the agent's
    /// name. Returns the inserted ticket's key.
    pub(crate) fn insert(&self, mut ticket: Ticket, reporter: String) -> String {
        let id = {
            let mut next = self.next_ticket_id.lock().unwrap();
            let base = next.get_or_insert_with(|| max_existing_ticket_id(&self.get_dir()));
            *base += 1;
            *base
        };
        let mut store = self.tickets.lock().unwrap();
        ticket.key = format!("TICKET-{id}");
        ticket.created_at = now_millis();
        ticket.reporter = reporter;
        ticket.result = None;
        ticket.status = Status::Todo;
        let key = ticket.key.clone();
        let label = ticket.label.clone();
        let reporter = ticket.reporter.clone();
        let task = ticket.task.clone();
        let created_at = ticket.created_at;
        let parent = ticket.parent.clone();
        store.insert(key.clone(), ticket);
        drop(store);
        self.save_ticket(&key);
        self.emit(&key, &reporter, EventKind::TicketCreated);
        let mut event = serde_json::json!({
            "event": "created",
            "ts": created_at,
            "key": key,
            "reporter": reporter,
            "label": label,
            "task": task,
        });
        if let Some(p) = &parent {
            event["parent"] = serde_json::Value::String(p.clone());
        }
        self.append_ticket_event(event);
        key
    }

    /// Write the ticket at `key` to disk. No-op when the ticket is missing.
    fn save_ticket(&self, key: &str) {
        if let Some(t) = self.get_ticket(key) {
            let _ = t.save(&self.get_dir());
        }
    }

    /// Append one JSON line to `<dir>/tickets.jsonl` and refresh
    /// `<dir>/stats.json` from the current statistics. Both writes happen
    /// under the same lock so a concurrent reader sees consistent
    /// observational state. Errors are swallowed: persistence is
    /// best-effort, not load-bearing for run correctness.
    pub(crate) fn append_ticket_event(&self, event: serde_json::Value) {
        let dir = self.get_dir();
        let _guard = self.tickets_log_lock.lock().unwrap();
        let _ = TicketEvents::append(&dir, &event);
        let _ = self.stats.save(&dir);
    }

    /// Write a tool's full output to `<dir>/tickets/<key>/outputs/<tool_use_id>.txt`.
    /// Returns the path relative to the configured `dir` on success,
    /// `None` when the write fails. The relative form keeps the
    /// replies portable across moves of the tickets dir; join with
    /// [`Self::get_dir`] to recover the on-disk path. Best-effort,
    /// matching the surrounding observational-persistence contract.
    pub(crate) fn write_tool_output(
        &self,
        key: &str,
        tool_use_id: &str,
        content: &str,
    ) -> Option<PathBuf> {
        let rel = crate::persistence::output_path(key, tool_use_id);
        let absolute = self.get_dir().join(&rel);
        crate::persistence::write_atomic(&absolute, content.as_bytes())
            .ok()
            .map(|_| rel)
    }

    /// Atomically find a `Todo` ticket matching `predicate`, assign it to
    /// `agent_id`, and transition to `InProgress`.
    pub(crate) fn claim<F>(&self, predicate: F, agent_id: &str) -> Option<String>
    where
        F: Fn(&Ticket) -> bool,
    {
        let now = now_millis();
        let schemas = self.schemas.lock().unwrap().clone();
        let (key, prev, durations, label) = {
            let mut store = self.tickets.lock().unwrap();
            let mut candidates: Vec<&String> = store
                .iter()
                .filter(|(_, t)| predicate(t))
                .map(|(k, _)| k)
                .collect();
            candidates.sort_by_key(|k| {
                let t = &store[k.as_str()];
                (t.created_at, numeric_id(k))
            });
            let key = candidates.into_iter().next()?.clone();
            let ticket = store.get_mut(&key)?;
            if ticket.status != Status::Todo {
                return None;
            }
            ticket.assignee = Some(agent_id.to_string());
            if ticket.schema.is_none() {
                let bound = schemas
                    .as_ref()
                    .zip(ticket.label.as_deref())
                    .and_then(|(s, label)| s.get(label));
                ticket.schema = bound;
            }
            let prev = ticket.status;
            ticket.stamp_transition(Status::InProgress, now);
            ticket.status = Status::InProgress;
            let durations = ticket.terminal_durations();
            let label = ticket.label.clone();
            (key, prev, durations, label)
        };
        self.record_transition(
            &key,
            prev,
            Status::InProgress,
            now,
            durations,
            label.as_deref(),
            agent_id,
        );
        self.save_ticket(&key);
        Some(key)
    }

    /// Append `reply` to the ticket's replies. No-op when the
    /// ticket is missing: the loop drops out shortly afterwards on the
    /// same condition. The ticket record is not rewritten; the replies
    /// live only in `replies.jsonl`.
    pub(crate) fn add_reply(&self, key: &str, reply: Reply) {
        {
            let mut store = self.tickets.lock().unwrap();
            let Some(t) = store.get_mut(key) else { return };
            t.replies.push(reply.clone());
        }
        let _ = Replies::append(&self.get_dir(), key, &reply);
    }

    /// Transition a ticket to `Finished`, emitting `TicketFinished`
    /// under `agent`'s name.
    pub(crate) fn set_finished_by(&self, key: &str, agent: &str) -> Result<(), TicketError> {
        self.set_final_status(key, Status::Finished, agent)
    }

    /// Attach `result` to the ticket and transition it to `Finished`,
    /// resolving it from outside the run. Validates against the ticket's
    /// schema first, so a host finish and an agent finish record the same
    /// contract. The emitted `TicketFinished` carries an empty agent id,
    /// like the run-level events no single agent causes.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// # fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let tickets = TicketQueue::new();
    /// let key = tickets.task("Look up the cached answer.");
    /// tickets.set_finished(&key, "42")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_finished(
        &self,
        key: &str,
        result: impl serde::Serialize,
    ) -> Result<(), TicketError> {
        let value = serde_json::to_value(result).expect("result is serializable");
        self.set_result(key, value)
            .map_err(|violations| TicketError::ResultRejected {
                message: violations.to_string(),
            })?;
        self.set_final_status(key, Status::Finished, "")
    }

    /// Transition a ticket to `Failed`. No result argument, unlike
    /// [`Self::set_finished`]: a failed ticket has none. The emitted
    /// `TicketFailed` carries an empty agent id, like the run-level
    /// events no single agent causes.
    pub fn set_failed(&self, key: &str) -> Result<(), TicketError> {
        self.set_final_status(key, Status::Failed, "")
    }

    /// Transition a ticket to `Failed`, emitting `TicketFailed` under
    /// `agent`'s name. The loop's failure paths route through this.
    pub(crate) fn set_failed_by(&self, key: &str, agent: &str) -> Result<(), TicketError> {
        self.set_final_status(key, Status::Failed, agent)
    }

    fn set_final_status(&self, key: &str, status: Status, agent: &str) -> Result<(), TicketError> {
        // Increment BEFORE the status flip and decrement only after the
        // terminal event has been emitted: the drain check in `finish()`
        // must never observe (empty queue, zero counter) mid-transition,
        // or it drains before an event handler can enqueue follow-up work.
        struct InFlight<'a>(&'a std::sync::atomic::AtomicUsize);
        impl Drop for InFlight<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        self.terminal_transitions_in_flight
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _in_flight = InFlight(&self.terminal_transitions_in_flight);

        let now = now_millis();
        let (prev, durations, label) = {
            let mut store = self.tickets.lock().unwrap();
            let ticket = store
                .get_mut(key)
                .ok_or_else(|| TicketError::TicketMissing {
                    key: key.to_string(),
                })?;
            let prev = ticket.status;
            ticket.stamp_transition(status, now);
            ticket.status = status;
            let durations = ticket.terminal_durations();
            let label = ticket.label.clone();
            (prev, durations, label)
        };
        // Emitted before the transition is recorded: the outcome is an event
        // count now, and `record_transition` ends by writing `stats.json`.
        if prev != status && !matches!(prev, Status::Finished | Status::Failed) {
            let kind = match status {
                Status::Finished => EventKind::TicketFinished,
                _ => EventKind::TicketFailed,
            };
            self.emit(key, agent, kind);
        }
        self.record_transition(key, prev, status, now, durations, label.as_deref(), agent);
        self.save_ticket(key);
        // The ticket will never request again, so drop any events buffered
        // for its message editors instead of leaking them for the run.
        self.reply_editing.lock().unwrap().pending.remove(key);
        Ok(())
    }

    /// Fire stats recorders and ticket log after a transition.
    fn record_transition(
        &self,
        key: &str,
        prev: Status,
        next: Status,
        now: u64,
        durations: (std::time::Duration, std::time::Duration),
        label: Option<&str>,
        agent: &str,
    ) {
        self.stats.record_transition(prev, next, now, durations);
        self.stats
            .record_transition_for(label, agent, next, durations);
        self.log_transition(key, prev, next, now, durations, label);
    }

    /// Append a `started` / `finished` / `failed` line to `tickets.jsonl` if
    /// `prev → next` is observable. No-op when prev == next or when the
    /// transition is not one we surface.
    fn log_transition(
        &self,
        key: &str,
        prev: Status,
        next: Status,
        ts: u64,
        (ticket_duration, work_duration): (std::time::Duration, std::time::Duration),
        label: Option<&str>,
    ) {
        if prev == next {
            return;
        }
        if prev == Status::Todo && next == Status::InProgress {
            self.append_ticket_event(serde_json::json!({
                "event": "started",
                "ts": ts,
                "key": key,
                "label": label,
            }));
        }
        match next {
            Status::Finished | Status::Failed => {
                let event = if next == Status::Finished {
                    "finished"
                } else {
                    "failed"
                };
                self.append_ticket_event(serde_json::json!({
                    "event": event,
                    "ts": ts,
                    "key": key,
                    "duration_ms": ticket_duration.as_millis() as u64,
                    "work_ms": work_duration.as_millis() as u64,
                }));
            }
            _ => {}
        }
    }

    /// Validate `result` against the ticket's schema, append it to
    /// `results.jsonl`, and store the validated result on the ticket,
    /// which it returns. A result an agent double-encoded as a JSON
    /// string is decoded so the stored value is the object. Does not
    /// finish the ticket: the caller does.
    pub(crate) fn set_result(
        &self,
        key: &str,
        result: serde_json::Value,
    ) -> Result<serde_json::Value, SchemaViolations> {
        let schema = self
            .tickets
            .lock()
            .unwrap()
            .get(key)
            .and_then(|t| t.schema.clone());
        let result = match schema.as_ref() {
            Some(schema) => schema.validate(result)?,
            None => result,
        };
        let ticket_copy = {
            let mut store = self.tickets.lock().unwrap();
            store.get_mut(key).map(|ticket| {
                ticket.result = Some(result.clone());
                ticket.clone()
            })
        };
        // A missing ticket records nothing: no phantom results line, no save.
        if let Some(ticket_copy) = ticket_copy {
            let log_line = serde_json::json!({ "ticket": key, "result": result });
            let _guard = self.results_log_lock.lock().unwrap();
            // Both writes are best-effort: the result is already attached in
            // memory, so a failed log or save is observational, not load-bearing.
            let _ = Results::append(&self.get_dir(), &log_line);
            let _ = ticket_copy.save(&self.get_dir());
        }
        Ok(result)
    }

    /// Apply `edit` to ticket `key`'s replies now, then rewrite them
    /// in place so the change survives resumption. The on-demand
    /// sibling of [`Self::edit_replies_on_event`]; triggers no request.
    /// No-op when the ticket is missing. The edit must keep the replies
    /// well-formed (matched tool_use/tool_result pairs); they are sent
    /// as-is. Leaves the task and the token accounting untouched, which
    /// is why compaction resets the usage history itself.
    pub fn edit_replies(&self, key: &str, edit: impl FnOnce(&mut Vec<Reply>)) -> &Self {
        let ticket_copy = {
            let mut store = self.tickets.lock().unwrap();
            let Some(ticket) = store.get_mut(key) else {
                return self;
            };
            let before = ticket.replies.clone();
            edit(&mut ticket.replies);
            // An editor that inspected without changing anything must not
            // trigger a rewrite: leave the on-disk files alone.
            if ticket.replies == before {
                return self;
            }
            ticket.clone()
        };
        let replies = Replies {
            key: key.to_string(),
            entries: ticket_copy.replies,
        };
        let _ = replies.save(&self.get_dir());
        self
    }

    /// Edit caller-settable fields. Each `Some` overwrites; `None`
    /// leaves the field untouched. A label can be replaced but not removed.
    pub(crate) fn edit(
        &self,
        key: &str,
        task: Option<serde_json::Value>,
        label: Option<String>,
    ) -> Result<(), TicketError> {
        let mut store = self.tickets.lock().unwrap();
        let ticket = store
            .get_mut(key)
            .ok_or_else(|| TicketError::TicketMissing {
                key: key.to_string(),
            })?;
        if let Some(t) = task {
            ticket.task = t;
        }
        if let Some(l) = label {
            ticket.label = Some(l);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;
    use crate::event::EventName;

    #[test]
    fn task_creates_ticket_with_user_reporter() {
        let (queue, _tmp) = test_queue();
        queue.task("hello");
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.task, serde_json::Value::String("hello".into()));
        assert_eq!(t.reporter, "user");
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn labeled_ticket_attaches_label_and_leaves_status_todo() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("hello").label("research"));
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert!(t.has_label("research"));
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn create_with_named_label_is_born_todo_and_carries_label() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("specific work for alice").label("alice"));
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert!(t.has_label("alice"));
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn create_with_label_and_schema_is_stored_verbatim() {
        let (queue, _tmp) = test_queue();
        let schema = crate::schemas::Schema::new(serde_json::json!({"type": "string"})).unwrap();
        queue.ticket(Ticket::new("x").label("urgent").schema(schema));
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert!(t.has_label("urgent"));
        assert!(t.schema.is_some());
    }

    #[test]
    fn set_result_updates_ticket() {
        let (queue, _tmp) = test_queue();
        queue.task("hi");
        queue
            .set_result("TICKET-1", serde_json::Value::String("answer".into()))
            .unwrap();
        let stored = queue.get_ticket("TICKET-1").unwrap();
        assert_eq!(
            stored.result.as_ref(),
            Some(&serde_json::Value::String("answer".into()))
        );
        assert_eq!(
            stored.result.as_ref().and_then(|v| v.as_str()),
            Some("answer")
        );
    }

    #[test]
    fn done_and_failed_filter_by_status() {
        let (queue, _tmp) = test_queue();
        queue.task("ok");
        queue.task("oops");
        queue.task("pending");
        queue.claim(|t| t.key == "TICKET-1", "agent");
        queue.set_finished_by("TICKET-1", "agent").unwrap();
        queue.set_failed("TICKET-2").unwrap();
        let done = queue.find_tickets(|t| t.status == Status::Finished);
        let failed = queue.find_tickets(|t| t.status == Status::Failed);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].key, "TICKET-1");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].key, "TICKET-2");
    }

    #[test]
    fn ticket_status_transitions_record_stats() {
        let (queue, _tmp) = test_queue();
        queue.task("a");
        queue.task("b");
        queue.task("c");
        assert_eq!(queue.stats().event_count(EventName::TicketCreated), 3);
        queue.claim(|t| t.key == "TICKET-1", "agent");
        queue.set_finished_by("TICKET-1", "agent").unwrap();
        queue.claim(|t| t.key == "TICKET-2", "agent");
        queue.set_failed("TICKET-2").unwrap();
        assert_eq!(queue.stats().event_count(EventName::TicketFinished), 1);
        assert_eq!(queue.stats().event_count(EventName::TicketFailed), 1);
    }

    #[test]
    fn stats_for_label_counts_creation_per_label() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("scan"));
        queue.ticket(Ticket::new("b").label("scan"));
        queue.ticket(Ticket::new("c"));
        let stats = queue.stats();
        assert_eq!(stats.event_count(EventName::TicketCreated), 3);
        assert_eq!(
            stats
                .stats_for_label("scan")
                .event_count(EventName::TicketCreated),
            2
        );
        assert_eq!(
            stats
                .stats_for_label("never-used")
                .event_count(EventName::TicketCreated),
            0
        );
    }

    #[test]
    fn stats_for_label_counts_terminal_transitions_per_label() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("scan"));
        queue.ticket(Ticket::new("b").label("scan"));
        queue.ticket(Ticket::new("c").label("high"));
        queue.claim(|t| t.key == "TICKET-1", "agent");
        queue.set_finished_by("TICKET-1", "agent").unwrap();
        queue.claim(|t| t.key == "TICKET-2", "agent");
        queue.set_failed("TICKET-2").unwrap();
        let stats = queue.stats();
        let scan = stats.stats_for_label("scan");
        let high = stats.stats_for_label("high");
        assert_eq!(scan.event_count(EventName::TicketFinished), 1);
        assert_eq!(scan.event_count(EventName::TicketFailed), 1);
        assert_eq!(high.event_count(EventName::TicketFinished), 0);
        assert_eq!(high.event_count(EventName::TicketFailed), 0);
    }

    #[test]
    fn stats_for_label_set_failed_path_records_per_label() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("scan"));
        queue.set_failed("TICKET-1").unwrap();
        assert_eq!(
            queue
                .stats()
                .stats_for_label("scan")
                .event_count(EventName::TicketFailed),
            1
        );
    }

    #[test]
    fn stats_for_label_unaffected_by_no_label_ticket() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a"));
        queue.claim(|t| t.key == "TICKET-1", "agent");
        queue.set_finished_by("TICKET-1", "agent").unwrap();
        assert_eq!(queue.stats().event_count(EventName::TicketFinished), 1);
        assert_eq!(
            queue
                .stats()
                .stats_for_label("scan")
                .event_count(EventName::TicketFinished),
            0
        );
        assert_eq!(
            queue
                .stats()
                .stats_for_label("scan")
                .event_count(EventName::TicketCreated),
            0
        );
    }

    #[test]
    fn workspace_emits_created_started_done_in_order() {
        let (queue, dir) = test_queue();
        queue.task("hello");
        queue.claim(|t| t.key == "TICKET-1", "agent");
        queue.set_finished_by("TICKET-1", "agent").unwrap();
        let lines = read_tickets_log(dir.path());
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["event"], "created");
        assert_eq!(lines[0]["key"], "TICKET-1");
        assert_eq!(lines[0]["reporter"], "user");
        assert_eq!(lines[0]["task"], "hello");
        assert_eq!(lines[1]["event"], "started");
        assert_eq!(lines[1]["key"], "TICKET-1");
        assert_eq!(lines[2]["event"], "finished");
        assert_eq!(lines[2]["key"], "TICKET-1");
        assert!(lines[2]["duration_ms"].is_u64());
        assert!(lines[2]["work_ms"].is_u64());
    }

    #[test]
    fn workspace_emits_failed_event_on_set_failed() {
        let (queue, dir) = test_queue();
        queue.task("hello");
        queue.set_failed("TICKET-1").unwrap();
        let lines = read_tickets_log(dir.path());
        // created + failed (no started since Todo→Failed via set_failed)
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "created");
        assert_eq!(lines[1]["event"], "failed");
        assert_eq!(lines[1]["key"], "TICKET-1");
    }

    #[test]
    fn workspace_created_event_carries_the_label_when_pinned() {
        let (queue, dir) = test_queue();
        queue.ticket(Ticket::new("specific").label("alice"));
        let lines = read_tickets_log(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["event"], "created");
        assert_eq!(lines[0]["label"], "alice");
    }

    #[test]
    fn workspace_logs_one_line_per_lifecycle_turn_for_multiple_tickets() {
        let (queue, dir) = test_queue();
        queue.task("a");
        queue.task("b");
        queue.claim(|t| t.key == "TICKET-1", "agent");
        queue.set_finished_by("TICKET-1", "agent").unwrap();
        queue.claim(|t| t.key == "TICKET-2", "agent");
        queue.set_failed("TICKET-2").unwrap();
        let lines = read_tickets_log(dir.path());
        // 2 created + 2 started + 1 done + 1 failed
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn claim_transitions_todo_to_in_progress_and_sets_the_assignee() {
        let (queue, _tmp) = test_queue();
        queue.task("hello");
        let key = queue.claim(|t| t.status == Status::Todo, "alice").unwrap();
        assert_eq!(key, "TICKET-1");
        let t = queue.get_ticket(&key).unwrap();
        assert_eq!(t.status, Status::InProgress);
        assert_eq!(t.assignee.as_deref(), Some("alice"));
        assert!(t.started_at.is_some());
    }

    #[test]
    fn claim_leaves_the_label_the_ticket_was_filed_with() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("hello").label("analysis"));
        let key = queue.claim(|t| t.has_label("analysis"), "alice").unwrap();
        assert!(queue.get_ticket(&key).unwrap().has_label("analysis"));
    }

    #[test]
    fn claim_returns_none_when_no_ticket_matches() {
        let (queue, _tmp) = test_queue();
        queue.task("hello");
        assert!(queue
            .claim(|t| t.has_label("nonexistent"), "alice")
            .is_none());
    }

    #[test]
    fn second_claim_of_same_ticket_returns_none() {
        let (queue, _tmp) = test_queue();
        queue.task("hello");
        let first = queue.claim(|t| t.key == "TICKET-1", "alice");
        assert!(first.is_some());
        // Second claim: ticket is now InProgress, not Todo.
        let second = queue.claim(|t| t.key == "TICKET-1", "bob");
        assert!(second.is_none());
    }

    #[test]
    fn claim_picks_earliest_eligible_ticket() {
        let (queue, _tmp) = test_queue();
        queue.task("a");
        queue.task("b");
        queue.task("c");
        let key = queue.claim(|t| t.status == Status::Todo, "alice").unwrap();
        assert_eq!(key, "TICKET-1");
    }

    fn queue_with_analysis_schema() -> (std::sync::Arc<TicketQueue>, crate::test_util::TempDir) {
        let (queue, dir) = test_queue();
        let schemas = crate::schemas::SchemaStore::new();
        schemas.label("analysis", document("verdict")).unwrap();
        queue.schemas(&schemas);
        (queue, dir)
    }

    /// A schema document the assertions can tell apart by its `title`.
    fn document(title: &str) -> serde_json::Value {
        serde_json::json!({ "type": "object", "title": title })
    }

    /// The `title` of the schema bound to a ticket, naming which one it took.
    fn bound_title(queue: &TicketQueue, key: &str) -> Option<String> {
        let schema = queue.get_ticket(key)?.schema?;
        let document = serde_json::to_value(schema).ok()?;
        Some(document["title"].as_str().unwrap_or_default().to_string())
    }

    #[test]
    fn claim_takes_the_schema_bound_to_a_label_of_the_ticket() {
        let (queue, _tmp) = queue_with_analysis_schema();
        queue.ticket(Ticket::new("audit").label("analysis"));
        assert_eq!(bound_title(&queue, "TICKET-1"), None);

        queue.claim(|t| t.has_label("analysis"), "alice");
        assert_eq!(bound_title(&queue, "TICKET-1").as_deref(), Some("verdict"));
    }

    #[test]
    fn claim_leaves_a_schema_the_ticket_already_carries() {
        let (queue, _tmp) = queue_with_analysis_schema();
        let own = crate::schemas::Schema::new(document("its own")).unwrap();
        queue.ticket(Ticket::new("audit").label("analysis").schema(own));
        assert_eq!(bound_title(&queue, "TICKET-1").as_deref(), Some("its own"));

        queue.claim(|t| t.has_label("analysis"), "alice");
        assert_eq!(bound_title(&queue, "TICKET-1").as_deref(), Some("its own"));
    }

    #[test]
    fn claim_prefers_the_label_the_ticket_was_filed_under_to_the_agent_id() {
        let (queue, _tmp) = test_queue();
        let schemas = crate::schemas::SchemaStore::new();
        schemas.label("analysis", document("by scope")).unwrap();
        schemas.label("alice", document("by agent")).unwrap();
        queue.schemas(&schemas);
        queue.ticket(Ticket::new("audit").label("analysis"));
        assert_eq!(bound_title(&queue, "TICKET-1"), None);

        queue.claim(|t| t.has_label("analysis"), "alice");
        assert_eq!(bound_title(&queue, "TICKET-1").as_deref(), Some("by scope"));
    }

    #[test]
    fn claim_binds_no_schema_when_no_label_of_the_ticket_has_one() {
        let (queue, _tmp) = queue_with_analysis_schema();
        queue.ticket(Ticket::new("search").label("discovery"));
        assert_eq!(bound_title(&queue, "TICKET-1"), None);

        queue.claim(|t| t.has_label("discovery"), "alice");
        assert_eq!(bound_title(&queue, "TICKET-1"), None);
    }

    #[test]
    fn a_ticket_already_in_progress_keeps_the_schema_its_claim_bound() {
        let (queue, _tmp) = test_queue();
        let schemas = crate::schemas::SchemaStore::new();
        schemas.label("analysis", document("first")).unwrap();
        queue.schemas(&schemas);
        queue.ticket(Ticket::new("audit").label("analysis"));
        queue.claim(|t| t.has_label("analysis"), "alice");
        assert_eq!(bound_title(&queue, "TICKET-1").as_deref(), Some("first"));

        schemas.label("analysis", document("second")).unwrap();

        // The loop resumes an `InProgress` ticket through `find_ticket`, never
        // through a second `claim`, so a later binding cannot reach it.
        assert!(queue.claim(|t| t.has_label("analysis"), "alice").is_none());
        assert_eq!(bound_title(&queue, "TICKET-1").as_deref(), Some("first"));
    }

    #[test]
    fn a_schema_bound_at_claim_survives_load() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        let schemas = crate::schemas::SchemaStore::new();
        schemas.label("analysis", document("verdict")).unwrap();
        original.schemas(&schemas);
        original.ticket(Ticket::new("audit").label("analysis"));
        original.claim(|t| t.has_label("analysis"), "alice");
        drop(original);

        let resumed = TicketQueue::load(dir.path()).unwrap();
        assert_eq!(
            bound_title(&resumed, "TICKET-1").as_deref(),
            Some("verdict")
        );
    }

    #[test]
    fn claim_emits_started_event_in_workspace_log() {
        let (queue, dir) = test_queue();
        queue.task("hello");
        queue.claim(|t| t.status == Status::Todo, "alice");
        let lines = read_tickets_log(dir.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "created");
        assert_eq!(lines[1]["event"], "started");
        assert_eq!(lines[1]["key"], "TICKET-1");
    }

    #[test]
    fn set_finished_transitions_to_finished() {
        let (queue, _tmp) = test_queue();
        queue.task("hello");
        queue.claim(|t| t.status == Status::Todo, "alice");
        queue.set_finished_by("TICKET-1", "agent").unwrap();
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.status, Status::Finished);
        assert!(t.finished_at.is_some());
    }

    #[test]
    fn set_failed_transitions_to_failed() {
        let (queue, _tmp) = test_queue();
        queue.task("hello");
        queue.claim(|t| t.status == Status::Todo, "alice");
        queue.set_failed("TICKET-1").unwrap();
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.status, Status::Failed);
        assert!(t.failed_at.is_some());
    }

    #[test]
    fn set_finished_stores_the_result_and_resolves_the_ticket() {
        let (queue, _tmp) = test_queue();
        queue.task("hello");
        queue
            .set_finished("TICKET-1", serde_json::json!({"answer": 42}))
            .unwrap();
        let t = queue.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result, Some(serde_json::json!({"answer": 42})));
        assert_eq!(
            queue.results().pop(),
            Some(serde_json::json!({"answer": 42}))
        );
    }

    #[test]
    fn set_finished_rejects_a_result_that_misses_the_ticket_schema() {
        let (queue, _tmp) = test_queue();
        let schema = crate::schemas::Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
        }))
        .unwrap();
        queue.ticket(Ticket::new("write a report").schema(schema));

        let err = queue
            .set_finished("TICKET-1", serde_json::json!({"body": "no title"}))
            .unwrap_err();
        assert!(matches!(err, TicketError::ResultRejected { .. }));
        assert_eq!(queue.get_ticket("TICKET-1").unwrap().status, Status::Todo);
    }

    #[test]
    fn set_finished_errors_on_an_unknown_key() {
        let (queue, _tmp) = test_queue();
        let err = queue.set_finished("TICKET-9", "done").unwrap_err();
        assert!(matches!(err, TicketError::TicketMissing { .. }));
    }

    #[test]
    fn ticket_parent_builder_round_trips() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("child body").parent("TICKET-1"));
        let stored = queue.get_ticket("TICKET-1").unwrap();
        assert_eq!(stored.parent.as_deref(), Some("TICKET-1"));
    }

    #[test]
    fn write_tool_output_returns_relative_path_and_writes_absolute() {
        let (queue, dir) = test_queue();
        queue.task("seed");
        let rel = queue
            .write_tool_output("TICKET-1", "call-1", "the full content")
            .expect("write succeeds when dir exists");
        let expected_rel: PathBuf = ["tickets", "TICKET-1", "outputs", "call-1.txt"]
            .iter()
            .collect();
        assert_eq!(rel, expected_rel);
        let body = std::fs::read_to_string(dir.path().join(&rel)).unwrap();
        assert_eq!(body, "the full content");
    }

    #[test]
    fn write_tool_output_creates_outputs_subdir_lazily() {
        let (queue, dir) = test_queue();
        queue.task("seed");
        let outputs = dir.path().join("tickets").join("TICKET-1").join("outputs");
        assert!(!outputs.exists());
        queue
            .write_tool_output("TICKET-1", "call-1", "payload")
            .unwrap();
        assert!(outputs.is_dir());
    }

    #[test]
    fn parent_field_renders_in_created_event() {
        let (queue, dir) = test_queue();
        queue.task("first");
        queue.ticket(Ticket::new("child").parent("TICKET-1"));
        let lines = read_tickets_log(dir.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "created");
        assert!(lines[0].get("parent").is_none());
        assert_eq!(lines[1]["event"], "created");
        assert_eq!(lines[1]["parent"], "TICKET-1");
    }

    // Resumption: TicketQueue::load

    #[test]
    fn load_creates_tickets_dir_when_missing() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::load(dir.path()).unwrap();
        assert!(queue.tickets().is_empty());
        assert!(dir.path().join("tickets").is_dir());
    }

    /// A replies file written by an older version no longer parses. The
    /// ticket must not be skipped: its status and result would vanish with
    /// it, and the caller would receive a short store reporting success.
    #[test]
    fn load_reports_a_ticket_whose_replies_cannot_be_read() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        original.task("seed work");
        original.add_reply("TICKET-1", Reply::user_text("hello"));
        original.set_finished_by("TICKET-1", "agent").unwrap();
        drop(original);

        let replies = dir.path().join("tickets/TICKET-1/replies.jsonl");
        std::fs::write(
            &replies,
            "{\"author\":\"user\",\"content\":[{\"Text\":\"hello\"}]}\n",
        )
        .unwrap();

        let Err(error) = TicketQueue::load(dir.path()) else {
            panic!("load must fail when a ticket's replies cannot be read");
        };
        assert!(
            error.to_string().contains("TICKET-1"),
            "the failure must name the ticket: {error}",
        );
    }

    #[test]
    fn load_restores_done_ticket_with_result_and_replies() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        original.task("seed work");
        original
            .set_result("TICKET-1", serde_json::json!({"ok": true}))
            .unwrap();
        original.set_finished_by("TICKET-1", "agent").unwrap();
        drop(original);

        let resumed = TicketQueue::load(dir.path()).unwrap();
        let t = resumed.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref(), Some(&serde_json::json!({"ok": true})));
        assert_eq!(t.task, serde_json::Value::String("seed work".into()));
    }

    #[test]
    fn insert_after_load_never_reuses_an_existing_key() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        original.task("seed work");
        drop(original);

        let resumed = TicketQueue::load(dir.path()).unwrap();
        assert_eq!(resumed.task("more work"), "TICKET-2");
    }

    #[test]
    fn insert_after_new_plus_dir_never_reuses_an_existing_key() {
        // The pattern a fresh process actually uses against a directory a
        // prior run already wrote into: `new()` (not `load()`) plus `.dir(..)`.
        let dir = crate::test_util::TempDir::new().unwrap();
        let first = TicketQueue::new();
        first.dir(dir.path().to_path_buf());
        first.task("seed work");
        drop(first);

        let second = TicketQueue::new();
        second.dir(dir.path().to_path_buf());
        assert_eq!(second.task("more work"), "TICKET-2");
    }

    #[test]
    fn load_seeds_next_ticket_id_without_rescanning_tickets_dir() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        original.task("seed work");
        drop(original);

        let resumed = TicketQueue::load(dir.path()).unwrap();
        // `load()` already knows the highest key from what it just read
        // into memory; removing the directory here proves `insert()` does
        // not rescan it, since a rescan would find nothing and wrongly
        // restart numbering at 1.
        std::fs::remove_dir_all(dir.path().join("tickets")).unwrap();
        assert_eq!(resumed.task("more work"), "TICKET-2");
    }

    #[test]
    fn load_restores_in_progress_replies() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        original.task("mid flight");
        original
            .claim(|t| t.status == Status::Todo, "alice")
            .unwrap();
        drop(original);

        let resumed = TicketQueue::load(dir.path()).unwrap();
        let t = resumed.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.status, Status::InProgress);
        assert_eq!(t.assignee.as_deref(), Some("alice"));
    }

    #[test]
    fn load_derives_stats_from_ticket_files_when_stats_file_missing() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        original.ticket(Ticket::new("a").label("scan"));
        original.ticket(Ticket::new("b").label("scan"));
        original.ticket(Ticket::new("c").label("scan"));
        original
            .set_result("TICKET-1", serde_json::Value::Null)
            .unwrap();
        original.set_finished_by("TICKET-1", "agent").unwrap();
        original.set_failed("TICKET-2").unwrap();
        drop(original);

        std::fs::remove_file(dir.path().join("stats.json")).unwrap();

        let resumed = TicketQueue::load(dir.path()).unwrap();
        let s = resumed.stats();
        assert_eq!(s.event_count(EventName::TicketCreated), 3);
        assert_eq!(s.event_count(EventName::TicketFinished), 1);
        assert_eq!(s.event_count(EventName::TicketFailed), 1);
        let scan = s.stats_for_label("scan");
        assert_eq!(scan.event_count(EventName::TicketCreated), 3);
        assert_eq!(scan.event_count(EventName::TicketFinished), 1);
        assert_eq!(scan.event_count(EventName::TicketFailed), 1);
    }

    #[test]
    fn load_skips_dir_without_ticket_json() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        original.task("valid");
        drop(original);

        // A leftover directory with no `ticket.json` is ignored by the
        // loader: there is no migration from the pre-split layout.
        let stray_dir = dir.path().join("tickets").join("TICKET-99");
        std::fs::create_dir_all(&stray_dir).unwrap();
        std::fs::write(stray_dir.join("anything.json"), "not json").unwrap();

        let resumed = TicketQueue::load(dir.path()).unwrap();
        assert!(resumed.get_ticket("TICKET-1").is_some());
        assert!(resumed.get_ticket("TICKET-99").is_none());
    }

    #[test]
    fn load_reports_a_malformed_ticket_json() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("tickets").join("TICKET-7")).unwrap();
        std::fs::write(
            dir.path()
                .join("tickets")
                .join("TICKET-7")
                .join("ticket.json"),
            "not json",
        )
        .unwrap();
        let Err(error) = TicketQueue::load(dir.path()) else {
            panic!("load must fail on a malformed ticket.json");
        };
        assert!(
            error.to_string().contains("TICKET-7"),
            "the failure must name the ticket: {error}",
        );
    }

    #[test]
    fn ticket_json_does_not_carry_replies_field() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        queue.task("hello");
        let stored = std::fs::read_to_string(
            dir.path()
                .join("tickets")
                .join("TICKET-1")
                .join("ticket.json"),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert!(
            v.as_object().unwrap().get("replies").is_none(),
            "ticket.json must not carry a `replies` field; got: {stored}",
        );
    }

    #[test]
    fn add_reply_appends_one_line_to_replies_jsonl() {
        let (queue, dir) = test_queue();
        queue.task("hello");
        queue.reply("TICKET-1", "first");
        queue.reply("TICKET-1", "second");
        let body = std::fs::read_to_string(
            dir.path()
                .join("tickets")
                .join("TICKET-1")
                .join("replies.jsonl"),
        )
        .unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        // Each line is a single, parseable JSON object.
        let _: Reply = serde_json::from_str(lines[0]).unwrap();
        let _: Reply = serde_json::from_str(lines[1]).unwrap();
    }

    #[test]
    fn load_replays_replies_jsonl_into_in_memory_ticket() {
        use super::super::reply::ReplyContent;
        let dir = crate::test_util::TempDir::new().unwrap();
        {
            let queue = TicketQueue::new();
            queue.dir(dir.path().to_path_buf());
            queue.task("hello");
            queue.reply("TICKET-1", "first");
            queue.reply("TICKET-1", "second");
        }
        let resumed = TicketQueue::load(dir.path()).unwrap();
        let t = resumed.get_ticket("TICKET-1").unwrap();
        let texts: Vec<_> = t
            .replies
            .iter()
            .filter_map(|r| match r.content.first()? {
                ReplyContent::Text { text: s } => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["first", "second"]);
    }

    #[test]
    fn ticket_lifecycle_event_writes_stats_file() {
        let (queue, dir) = test_queue();
        queue.task("seed");
        queue.claim(|t| t.status == Status::Todo, "alice").unwrap();
        queue
            .set_result("TICKET-1", serde_json::Value::Null)
            .unwrap();
        queue.set_finished_by("TICKET-1", "agent").unwrap();

        let bytes = std::fs::read(dir.path().join("stats.json")).expect("stats file written");
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["events"]["ticket_created"], 1);
        assert_eq!(body["events"]["ticket_finished"], 1);
    }

    #[test]
    fn creating_a_ticket_counts_it_against_the_reporter() {
        let (queue, _tmp) = test_queue();
        queue.task("seed");

        let stats = queue.stats();
        assert_eq!(stats.event_count(EventName::TicketCreated), 1);
        assert_eq!(
            stats
                .stats_for_agent("user")
                .event_count(EventName::TicketCreated),
            1
        );
    }

    #[test]
    fn a_finished_ticket_failed_afterwards_is_not_counted_twice() {
        let (queue, _tmp) = test_queue();
        queue.task("seed");
        queue.set_finished_by("TICKET-1", "alice").unwrap();
        // Refused by the emit guard, so the count never sees it either.
        queue.set_failed("TICKET-1").unwrap();

        let stats = queue.stats();
        assert_eq!(stats.event_count(EventName::TicketFinished), 1);
        assert_eq!(stats.event_count(EventName::TicketFailed), 0);
    }

    #[test]
    fn load_prefers_stats_file_over_derivation() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("tickets")).unwrap();
        let body = serde_json::json!({
            "events": { "turn_started": 42, "request_finished": 7 }
        });
        std::fs::write(
            dir.path().join("stats.json"),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();

        let queue = TicketQueue::load(dir.path()).unwrap();
        assert_eq!(queue.stats().event_count(EventName::TurnStarted), 42);
        assert_eq!(queue.stats().event_count(EventName::RequestFinished), 7);
    }

    #[test]
    fn load_falls_back_to_derivation_when_stats_file_malformed() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        original.task("seed");
        original
            .set_result("TICKET-1", serde_json::Value::Null)
            .unwrap();
        original.set_finished_by("TICKET-1", "agent").unwrap();
        drop(original);

        std::fs::write(dir.path().join("stats.json"), "not json").unwrap();

        let queue = TicketQueue::load(dir.path()).unwrap();
        assert_eq!(queue.stats().event_count(EventName::TicketCreated), 1);
        assert_eq!(queue.stats().event_count(EventName::TicketFinished), 1);
    }

    #[test]
    fn ticket_with_json_schema_round_trips_through_load() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        let schema_doc = serde_json::json!({
            "type": "object",
            "properties": { "n": { "type": "integer" } },
            "required": ["n"],
        });
        let schema = crate::schemas::Schema::new(schema_doc.clone()).unwrap();
        original.ticket(Ticket::new("counted").schema(schema));
        drop(original);

        let resumed = TicketQueue::load(dir.path()).unwrap();
        let t = resumed.get_ticket("TICKET-1").unwrap();
        let restored = t.schema.expect("JSON schema must restore");
        assert!(restored.validate(serde_json::json!({"n": 3})).is_ok());
        assert!(restored.validate(serde_json::json!({})).is_err());
    }

    #[test]
    fn edit_replies_rewrites_replies_without_touching_task() {
        use crate::agents::tickets::ReplyContent;
        let (queue, _tmp) = test_queue();
        let key = queue.task("original task");
        queue.add_reply(&key, Reply::user_text("keep me"));
        queue.add_reply(&key, Reply::user_text("drop me"));

        queue.edit_replies(&key, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
            });
        });

        // Unlike summarize, the task is left as it was.
        let ticket = queue.get_ticket(&key).unwrap();
        assert_eq!(
            ticket.task,
            serde_json::Value::String("original task".into())
        );

        // The drop is committed and reloads from disk.
        let reloaded = TicketQueue::load(queue.get_dir()).unwrap();
        let replies = reloaded.get_ticket(&key).unwrap().replies;
        assert!(replies.iter().any(
            |r| matches!(r.content.first(), Some(ReplyContent::Text { text: t }) if t == "keep me")
        ));
        assert!(replies.iter().all(
            |r| !matches!(r.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
        ));
    }

    #[test]
    fn edit_replies_that_changes_nothing_writes_nothing() {
        let (queue, _tmp) = test_queue();
        let key = queue.task("go");
        queue.add_reply(&key, Reply::user_text("keep me"));
        let ticket_dir = queue.get_dir().join("tickets").join(&key);

        queue.edit_replies(&key, |_replies| {}); // inspect, change nothing

        let rewrites = std::fs::read_dir(&ticket_dir)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .strip_prefix("replies.")
                    .and_then(|rest| rest.strip_suffix(".jsonl"))
                    .is_some()
            })
            .count();
        assert_eq!(rewrites, 0, "a no-op edit must not rewrite replies.jsonl");
    }

    #[test]
    fn edit_rewrites_replies_in_place_without_a_snapshot_file() {
        use crate::agents::tickets::ReplyContent;
        let (queue, _tmp) = test_queue();
        let key = queue.task("go");
        queue.add_reply(&key, Reply::user_text("keep me"));
        queue.add_reply(&key, Reply::user_text("drop me"));
        let ticket_dir = queue.get_dir().join("tickets").join(&key);

        queue.edit_replies(&key, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
            });
        });

        // The edit landed in the base file, and minted no replies.<ts>.jsonl.
        let names: Vec<String> = std::fs::read_dir(&ticket_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"replies.jsonl".to_string()));
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("replies.") && n != "replies.jsonl"),
            "no snapshot file expected, found: {names:?}",
        );
        let body = std::fs::read_to_string(ticket_dir.join("replies.jsonl")).unwrap();
        assert!(body.contains("keep me"));
        assert!(!body.contains("drop me"));
    }

    #[test]
    fn compaction_then_edit_leaves_one_replies_file_and_no_leak() {
        use crate::agents::tickets::ReplyContent;
        let (queue, _tmp) = test_queue();
        let key = queue.task("go");
        queue.add_reply(&key, Reply::user_text("SECRET"));
        // What compaction applies: the replies wholesale, rewritten in place.
        queue.edit_replies(&key, |replies| *replies = vec![Reply::user_text("summary")]);
        queue.add_reply(&key, Reply::user_text("after"));
        queue.edit_replies(&key, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "after")
            });
        });

        // One replies file, no snapshots, and neither dropped string survives.
        let ticket_dir = queue.get_dir().join("tickets").join(&key);
        let replies_files: Vec<String> = std::fs::read_dir(&ticket_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("replies."))
            .collect();
        assert_eq!(replies_files, vec!["replies.jsonl".to_string()]);
        let body = std::fs::read_to_string(ticket_dir.join("replies.jsonl")).unwrap();
        assert!(
            !body.contains("SECRET") && !body.contains("after"),
            "leaked: {body}"
        );
    }
}
