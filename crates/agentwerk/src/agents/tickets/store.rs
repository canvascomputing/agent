//! Store mutations for [`TicketSystem`]: insertion, claiming,
//! status transitions, transcript appends, result attachment,
//! compaction-pair writes, and the matching observational events.

use std::path::{Path, PathBuf};

use crate::event::EventKind;
use crate::persistence::{Append, Persist, Results, TicketEvents};
use crate::schemas::SchemaViolations;

use super::error::TicketError;
use super::reply::Reply;
use super::ticket::{Status, Ticket};
use super::ticket_system::TicketSystem;
use super::{now_millis, numeric_id, Replies};

/// Highest `TICKET-<N>` already on disk under `<dir>/tickets/`, or 0 if
/// none. Only needed for a system built via `new()`, which never reads
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

impl TicketSystem {
    /// Insert `ticket`, stamping system fields. The ticket is always born
    /// `Todo`; to pin it to a specific agent, label it with the agent's
    /// name. Returns the inserted ticket's key.
    pub(crate) fn insert(&self, mut ticket: Ticket, reporter: String) -> String {
        let id = {
            let mut next = self.next_ticket_id.lock().unwrap();
            let base = next.get_or_insert_with(|| max_existing_ticket_id(&self.dir_value()));
            *base += 1;
            *base
        };
        let mut store = self.tickets.lock().unwrap();
        ticket.key = format!("TICKET-{id}");
        ticket.created_at = now_millis();
        ticket.reporter = reporter;
        ticket.result = None;
        ticket.status = Status::Todo;
        // A ticket without its own schema inherits the default registered for the
        // first of its labels, so a result contract follows the label regardless
        // of how the ticket was created (direct, labeled, or handover).
        if ticket.schema.is_none() {
            let defaults = self.label_schemas.lock().unwrap();
            if let Some(schema) = ticket.labels.iter().find_map(|l| defaults.get(l)) {
                ticket.schema = Some(schema.clone());
            }
        }
        let key = ticket.key.clone();
        let labels = ticket.labels.clone();
        let reporter = ticket.reporter.clone();
        let task = ticket.task.clone();
        let created_at = ticket.created_at;
        let parent = ticket.parent.clone();
        store.insert(key.clone(), ticket);
        drop(store);
        self.save_ticket(&key);
        self.stats.record_created();
        for l in &labels {
            self.stats.stats_for_label(l).record_created();
        }
        let mut event = serde_json::json!({
            "event": "created",
            "ts": created_at,
            "key": key,
            "reporter": reporter,
            "labels": labels,
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
            let _ = t.save(&self.dir_value());
        }
    }

    /// Append one JSON line to `<dir>/tickets.jsonl` and refresh
    /// `<dir>/stats.json` from the current counters. Both writes happen
    /// under the same lock so a concurrent reader sees consistent
    /// observational state. Errors are swallowed: persistence is
    /// best-effort, not load-bearing for run correctness.
    pub(crate) fn append_ticket_event(&self, event: serde_json::Value) {
        let dir = self.dir_value();
        let _guard = self.tickets_log_lock.lock().unwrap();
        let _ = TicketEvents::append(&dir, &event);
        let _ = self.stats.save(&dir);
    }

    /// Write a tool's full output to `<dir>/tickets/<key>/outputs/<tool_use_id>.txt`.
    /// Returns the path relative to the configured `dir` on success,
    /// `None` when the write fails. The relative form keeps the reply
    /// transcript portable across moves of the tickets dir; join with
    /// [`Self::dir_value`] to recover the on-disk path. Best-effort,
    /// matching the surrounding observational-persistence contract.
    pub(crate) fn write_tool_output(
        &self,
        key: &str,
        tool_use_id: &str,
        content: &str,
    ) -> Option<PathBuf> {
        let rel = crate::persistence::output_path(key, tool_use_id);
        let absolute = self.dir_value().join(&rel);
        crate::persistence::write_atomic(&absolute, content.as_bytes())
            .ok()
            .map(|_| rel)
    }

    /// Atomically find a `Todo` ticket matching `predicate`, label it
    /// with `agent_name`, and transition to `InProgress`.
    pub(crate) fn claim<F>(&self, predicate: F, agent_name: &str) -> Option<String>
    where
        F: Fn(&Ticket) -> bool,
    {
        let now = now_millis();
        let (key, prev, durations, labels) = {
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
            if !ticket.labels.iter().any(|l| l == agent_name) {
                ticket.labels.push(agent_name.to_string());
            }
            let prev = ticket.status;
            ticket.stamp_transition(Status::InProgress, now);
            ticket.status = Status::InProgress;
            let durations = ticket.terminal_durations();
            let labels = ticket.labels.clone();
            (key, prev, durations, labels)
        };
        self.record_transition(&key, prev, Status::InProgress, now, durations, &labels);
        self.save_ticket(&key);
        Some(key)
    }

    /// Append `reply` to the ticket's transcript. No-op when the
    /// ticket is missing: the loop drops out shortly afterwards on the
    /// same condition. The header file is not rewritten; the transcript
    /// lives only in `replies.jsonl`.
    pub(crate) fn add_reply(&self, key: &str, reply: Reply) {
        {
            let mut store = self.tickets.lock().unwrap();
            let Some(t) = store.get_mut(key) else { return };
            t.replies.push(reply.clone());
        }
        let _ = Replies::append(&self.dir_value(), key, &reply);
    }

    /// Transition a ticket to `Finished`, emitting `TicketFinished`
    /// under `agent`'s name.
    pub(crate) fn set_finished(&self, key: &str, agent: &str) -> Result<(), TicketError> {
        self.set_final_status(key, Status::Finished, agent)
    }

    /// Transition a ticket to `Failed`. The one host-facing status
    /// mutation: finishing is reserved for the agent's finish tools,
    /// which validate and record the result before the transition. The
    /// emitted `TicketFailed` carries an empty agent name, like the
    /// run-level events no single agent causes.
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
        let (prev, durations, labels) = {
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
            let labels = ticket.labels.clone();
            (prev, durations, labels)
        };
        self.record_transition(key, prev, status, now, durations, &labels);
        self.save_ticket(key);
        if prev != status && !matches!(prev, Status::Finished | Status::Failed) {
            let kind = match status {
                Status::Finished => EventKind::TicketFinished,
                _ => EventKind::TicketFailed,
            };
            self.emit(key, agent, kind);
        }
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
        labels: &[String],
    ) {
        self.stats.record_transition(prev, next, now, durations);
        self.stats.record_transition_for(labels, next, durations);
        self.log_transition(key, prev, next, now, durations, labels);
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
        labels: &[String],
    ) {
        if prev == next {
            return;
        }
        if prev == Status::Todo && next == Status::InProgress {
            self.append_ticket_event(serde_json::json!({
                "event": "started",
                "ts": ts,
                "key": key,
                "labels": labels,
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
            let _ = Results::append(&self.dir_value(), &log_line);
            let _ = ticket_copy.save(&self.dir_value());
        }
        Ok(result)
    }

    /// Collapse the ticket's transcript to `summary` and write the
    /// compaction pair. No-op when the ticket is missing.
    pub(crate) fn summarize(&self, key: &str, summary: String) {
        let ticket_copy = {
            let mut store = self.tickets.lock().unwrap();
            let Some(ticket) = store.get_mut(key) else {
                return;
            };
            ticket.summarize(summary);
            ticket.clone()
        };
        self.save_compaction(key, &ticket_copy);
        self.stats.reset_usage(key);
    }

    /// Replies file first, then header as commit marker. A crash in
    /// between leaves an orphan `replies.<ts>.jsonl` that `Replies::load`
    /// skips via the paired-check rule.
    fn save_compaction(&self, key: &str, ticket: &Ticket) {
        let dir = self.dir_value();
        let at = now_millis();
        let ticket_dir = dir.join("tickets").join(key);

        let replies_path = ticket_dir.join(format!("replies.{at}.jsonl"));
        let mut replies_body = String::new();
        for reply in &ticket.replies {
            if let Ok(line) = serde_json::to_string(reply) {
                replies_body.push_str(&line);
                replies_body.push('\n');
            }
        }
        let _ = crate::persistence::write_atomic(&replies_path, replies_body.as_bytes());

        let header_path = ticket_dir.join(format!("ticket.{at}.json"));
        if let Ok(header_body) = serde_json::to_vec_pretty(ticket) {
            let _ = crate::persistence::write_atomic(&header_path, &header_body);
        }
    }

    /// Edit caller-settable fields. Each `Some` overwrites; `None`
    /// leaves the field untouched. The `Option<Option<Schema>>` shape on
    /// `schema` lets callers explicitly clear it via `Some(None)`.
    pub(crate) fn edit(
        &self,
        key: &str,
        task: Option<serde_json::Value>,
        labels: Option<Vec<String>>,
        schema: Option<Option<crate::schemas::Schema>>,
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
        if let Some(l) = labels {
            ticket.labels = l;
        }
        if let Some(s) = schema {
            ticket.schema = s;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::test_util::*;
    use super::*;
    use crate::providers::ContentBlock;

    #[test]
    fn task_creates_ticket_with_user_reporter() {
        let (sys, _tmp) = test_system();
        sys.task("hello");
        let t = sys.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.task, serde_json::Value::String("hello".into()));
        assert_eq!(t.reporter, "user");
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn labeled_ticket_attaches_label_and_leaves_status_todo() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("hello").label("research"));
        let t = sys.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.labels, vec!["research".to_string()]);
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn create_with_named_label_is_born_todo_and_carries_label() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("specific work for alice").label("alice"));
        let t = sys.get_ticket("TICKET-1").unwrap();
        assert!(t.labels.iter().any(|l| l == "alice"));
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn create_with_label_and_schema_is_stored_verbatim() {
        let (sys, _tmp) = test_system();
        let schema = crate::schemas::Schema::parse(serde_json::json!({"type": "string"})).unwrap();
        sys.ticket(Ticket::new("x").label("urgent").schema(schema));
        let t = sys.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.labels, vec!["urgent".to_string()]);
        assert!(t.schema.is_some());
    }

    #[test]
    fn label_default_schema_stamps_a_schemaless_ticket() {
        let (sys, _tmp) = test_system();
        let schema = crate::schemas::Schema::parse(serde_json::json!({"type": "object"})).unwrap();
        sys.schema_for_label("analysis", schema);
        sys.ticket(Ticket::new("triage this").label("analysis"));
        let t = sys.get_ticket("TICKET-1").unwrap();
        assert!(
            t.schema.is_some(),
            "a schemaless ticket should inherit its label's default schema"
        );
    }

    #[test]
    fn an_explicit_ticket_schema_overrides_the_label_default() {
        let (sys, _tmp) = test_system();
        let default = crate::schemas::Schema::parse(serde_json::json!({"type": "object"})).unwrap();
        sys.schema_for_label("analysis", default);
        let own = crate::schemas::Schema::parse(serde_json::json!({"type": "string"})).unwrap();
        sys.ticket(Ticket::new("triage this").label("analysis").schema(own));
        let t = sys.get_ticket("TICKET-1").unwrap();
        let stored = serde_json::to_value(t.schema.unwrap()).unwrap();
        assert_eq!(
            stored["type"], "string",
            "an explicitly attached schema must win over the label default"
        );
    }

    #[test]
    fn set_result_updates_ticket() {
        let (sys, _tmp) = test_system();
        sys.task("hi");
        sys.set_result("TICKET-1", serde_json::Value::String("answer".into()))
            .unwrap();
        let stored = sys.get_ticket("TICKET-1").unwrap();
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
        let (sys, _tmp) = test_system();
        sys.task("ok");
        sys.task("oops");
        sys.task("pending");
        sys.claim(|t| t.key == "TICKET-1", "agent");
        sys.set_finished("TICKET-1", "agent").unwrap();
        sys.set_failed("TICKET-2").unwrap();
        let done = sys.find_tickets(|t| t.status == Status::Finished);
        let failed = sys.find_tickets(|t| t.status == Status::Failed);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].key, "TICKET-1");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].key, "TICKET-2");
    }

    #[test]
    fn ticket_status_transitions_record_stats() {
        let (sys, _tmp) = test_system();
        sys.task("a");
        sys.task("b");
        sys.task("c");
        assert_eq!(sys.stats().tickets_created(), 3);
        sys.claim(|t| t.key == "TICKET-1", "agent");
        sys.set_finished("TICKET-1", "agent").unwrap();
        sys.claim(|t| t.key == "TICKET-2", "agent");
        sys.set_failed("TICKET-2").unwrap();
        assert_eq!(sys.stats().tickets_finished(), 1);
        assert_eq!(sys.stats().tickets_failed(), 1);
    }

    #[test]
    fn stats_for_label_counts_creation_per_label() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("a").labels(["scan", "high"]));
        sys.ticket(Ticket::new("b").label("scan"));
        sys.ticket(Ticket::new("c"));
        let stats = sys.stats();
        assert_eq!(stats.tickets_created(), 3);
        assert_eq!(stats.stats_for_label("scan").tickets_created(), 2);
        assert_eq!(stats.stats_for_label("high").tickets_created(), 1);
        assert_eq!(stats.stats_for_label("never-used").tickets_created(), 0);
    }

    #[test]
    fn stats_for_label_counts_terminal_transitions_per_label() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("a").labels(["scan", "high"]));
        sys.ticket(Ticket::new("b").label("scan"));
        sys.claim(|t| t.key == "TICKET-1", "agent");
        sys.set_finished("TICKET-1", "agent").unwrap();
        sys.claim(|t| t.key == "TICKET-2", "agent");
        sys.set_failed("TICKET-2").unwrap();
        let stats = sys.stats();
        let scan = stats.stats_for_label("scan");
        let high = stats.stats_for_label("high");
        assert_eq!(scan.tickets_finished(), 1);
        assert_eq!(scan.tickets_failed(), 1);
        assert_eq!(high.tickets_finished(), 1);
        assert_eq!(high.tickets_failed(), 0);
    }

    #[test]
    fn stats_for_label_set_failed_path_records_per_label() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("a").label("scan"));
        sys.set_failed("TICKET-1").unwrap();
        assert_eq!(sys.stats().stats_for_label("scan").tickets_failed(), 1);
    }

    #[test]
    fn stats_for_label_unaffected_by_no_label_ticket() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("a"));
        sys.claim(|t| t.key == "TICKET-1", "agent");
        sys.set_finished("TICKET-1", "agent").unwrap();
        assert_eq!(sys.stats().tickets_finished(), 1);
        assert_eq!(sys.stats().stats_for_label("scan").tickets_finished(), 0);
        assert_eq!(sys.stats().stats_for_label("scan").tickets_created(), 0);
    }

    #[test]
    fn workspace_emits_created_started_done_in_order() {
        let (sys, dir) = test_system();
        sys.task("hello");
        sys.claim(|t| t.key == "TICKET-1", "agent");
        sys.set_finished("TICKET-1", "agent").unwrap();
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
        let (sys, dir) = test_system();
        sys.task("hello");
        sys.set_failed("TICKET-1").unwrap();
        let lines = read_tickets_log(dir.path());
        // created + failed (no started since Todo→Failed via set_failed)
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "created");
        assert_eq!(lines[1]["event"], "failed");
        assert_eq!(lines[1]["key"], "TICKET-1");
    }

    #[test]
    fn workspace_created_event_carries_labels_when_pinned() {
        let (sys, dir) = test_system();
        sys.ticket(Ticket::new("specific").label("alice"));
        let lines = read_tickets_log(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["event"], "created");
        assert_eq!(lines[0]["labels"], serde_json::json!(["alice"]));
    }

    #[test]
    fn workspace_logs_one_line_per_lifecycle_turn_for_multiple_tickets() {
        let (sys, dir) = test_system();
        sys.task("a");
        sys.task("b");
        sys.claim(|t| t.key == "TICKET-1", "agent");
        sys.set_finished("TICKET-1", "agent").unwrap();
        sys.claim(|t| t.key == "TICKET-2", "agent");
        sys.set_failed("TICKET-2").unwrap();
        let lines = read_tickets_log(dir.path());
        // 2 created + 2 started + 1 done + 1 failed
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn claim_transitions_todo_to_in_progress_and_adds_label() {
        let (sys, _tmp) = test_system();
        sys.task("hello");
        let key = sys.claim(|t| t.status == Status::Todo, "alice").unwrap();
        assert_eq!(key, "TICKET-1");
        let t = sys.get_ticket(&key).unwrap();
        assert_eq!(t.status, Status::InProgress);
        assert!(t.labels.iter().any(|l| l == "alice"));
        assert!(t.started_at.is_some());
    }

    #[test]
    fn claim_returns_none_when_no_ticket_matches() {
        let (sys, _tmp) = test_system();
        sys.task("hello");
        assert!(sys
            .claim(|t| t.labels.iter().any(|l| l == "nonexistent"), "alice")
            .is_none());
    }

    #[test]
    fn second_claim_of_same_ticket_returns_none() {
        let (sys, _tmp) = test_system();
        sys.task("hello");
        let first = sys.claim(|t| t.key == "TICKET-1", "alice");
        assert!(first.is_some());
        // Second claim: ticket is now InProgress, not Todo.
        let second = sys.claim(|t| t.key == "TICKET-1", "bob");
        assert!(second.is_none());
    }

    #[test]
    fn claim_picks_earliest_eligible_ticket() {
        let (sys, _tmp) = test_system();
        sys.task("a");
        sys.task("b");
        sys.task("c");
        let key = sys.claim(|t| t.status == Status::Todo, "alice").unwrap();
        assert_eq!(key, "TICKET-1");
    }

    #[test]
    fn claim_emits_started_event_in_workspace_log() {
        let (sys, dir) = test_system();
        sys.task("hello");
        sys.claim(|t| t.status == Status::Todo, "alice");
        let lines = read_tickets_log(dir.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "created");
        assert_eq!(lines[1]["event"], "started");
        assert_eq!(lines[1]["key"], "TICKET-1");
    }

    #[test]
    fn set_finished_transitions_to_finished() {
        let (sys, _tmp) = test_system();
        sys.task("hello");
        sys.claim(|t| t.status == Status::Todo, "alice");
        sys.set_finished("TICKET-1", "agent").unwrap();
        let t = sys.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.status, Status::Finished);
        assert!(t.finished_at.is_some());
    }

    #[test]
    fn set_failed_transitions_to_failed() {
        let (sys, _tmp) = test_system();
        sys.task("hello");
        sys.claim(|t| t.status == Status::Todo, "alice");
        sys.set_failed("TICKET-1").unwrap();
        let t = sys.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.status, Status::Failed);
        assert!(t.failed_at.is_some());
    }

    #[test]
    fn ticket_parent_builder_round_trips() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("child body").parent("TICKET-1"));
        let stored = sys.get_ticket("TICKET-1").unwrap();
        assert_eq!(stored.parent.as_deref(), Some("TICKET-1"));
    }

    #[test]
    fn write_tool_output_returns_relative_path_and_writes_absolute() {
        let (sys, dir) = test_system();
        sys.task("seed");
        let rel = sys
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
        let (sys, dir) = test_system();
        sys.task("seed");
        let outputs = dir.path().join("tickets").join("TICKET-1").join("outputs");
        assert!(!outputs.exists());
        sys.write_tool_output("TICKET-1", "call-1", "payload")
            .unwrap();
        assert!(outputs.is_dir());
    }

    #[test]
    fn parent_field_renders_in_created_event() {
        let (sys, dir) = test_system();
        sys.task("first");
        sys.ticket(Ticket::new("child").parent("TICKET-1"));
        let lines = read_tickets_log(dir.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "created");
        assert!(lines[0].get("parent").is_none());
        assert_eq!(lines[1]["event"], "created");
        assert_eq!(lines[1]["parent"], "TICKET-1");
    }

    // ---- resumption: TicketSystem::load ----

    #[test]
    fn load_creates_tickets_dir_when_missing() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let sys = TicketSystem::load(dir.path()).unwrap();
        assert!(sys.tickets().is_empty());
        assert!(dir.path().join("tickets").is_dir());
    }

    #[test]
    fn load_restores_done_ticket_with_result_and_replies() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketSystem::new();
        original.dir(dir.path().to_path_buf());
        original.task("seed work");
        original
            .set_result("TICKET-1", serde_json::json!({"ok": true}))
            .unwrap();
        original.set_finished("TICKET-1", "agent").unwrap();
        drop(original);

        let resumed = TicketSystem::load(dir.path()).unwrap();
        let t = resumed.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref(), Some(&serde_json::json!({"ok": true})));
        assert_eq!(t.task, serde_json::Value::String("seed work".into()));
    }

    #[test]
    fn insert_after_load_never_reuses_an_existing_key() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketSystem::new();
        original.dir(dir.path().to_path_buf());
        original.task("seed work");
        drop(original);

        let resumed = TicketSystem::load(dir.path()).unwrap();
        assert_eq!(resumed.task("more work"), "TICKET-2");
    }

    #[test]
    fn insert_after_new_plus_dir_never_reuses_an_existing_key() {
        // The pattern a fresh process actually uses against a directory a
        // prior run already wrote into: `new()` (not `load()`) plus `.dir(..)`.
        let dir = crate::test_util::TempDir::new().unwrap();
        let first = TicketSystem::new();
        first.dir(dir.path().to_path_buf());
        first.task("seed work");
        drop(first);

        let second = TicketSystem::new();
        second.dir(dir.path().to_path_buf());
        assert_eq!(second.task("more work"), "TICKET-2");
    }

    #[test]
    fn load_seeds_next_ticket_id_without_rescanning_tickets_dir() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketSystem::new();
        original.dir(dir.path().to_path_buf());
        original.task("seed work");
        drop(original);

        let resumed = TicketSystem::load(dir.path()).unwrap();
        // `load()` already knows the highest key from what it just read
        // into memory; removing the directory here proves `insert()` does
        // not rescan it, since a rescan would find nothing and wrongly
        // restart numbering at 1.
        std::fs::remove_dir_all(dir.path().join("tickets")).unwrap();
        assert_eq!(resumed.task("more work"), "TICKET-2");
    }

    #[test]
    fn load_restores_in_progress_transcript() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketSystem::new();
        original.dir(dir.path().to_path_buf());
        original.task("mid flight");
        original
            .claim(|t| t.status == Status::Todo, "alice")
            .unwrap();
        drop(original);

        let resumed = TicketSystem::load(dir.path()).unwrap();
        let t = resumed.get_ticket("TICKET-1").unwrap();
        assert_eq!(t.status, Status::InProgress);
        assert!(t.labels.iter().any(|l| l == "alice"));
    }

    #[test]
    fn load_derives_stats_from_ticket_files_when_stats_file_missing() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketSystem::new();
        original.dir(dir.path().to_path_buf());
        original.ticket(Ticket::new("a").label("scan"));
        original.ticket(Ticket::new("b").label("scan"));
        original.ticket(Ticket::new("c").label("scan"));
        original
            .set_result("TICKET-1", serde_json::Value::Null)
            .unwrap();
        original.set_finished("TICKET-1", "agent").unwrap();
        original.set_failed("TICKET-2").unwrap();
        drop(original);

        std::fs::remove_file(dir.path().join("stats.json")).unwrap();

        let resumed = TicketSystem::load(dir.path()).unwrap();
        let s = resumed.stats();
        assert_eq!(s.tickets_created(), 3);
        assert_eq!(s.tickets_finished(), 1);
        assert_eq!(s.tickets_failed(), 1);
        let scan = s.stats_for_label("scan");
        assert_eq!(scan.tickets_created(), 3);
        assert_eq!(scan.tickets_finished(), 1);
        assert_eq!(scan.tickets_failed(), 1);
    }

    #[test]
    fn load_skips_dir_without_ticket_json() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketSystem::new();
        original.dir(dir.path().to_path_buf());
        original.task("valid");
        drop(original);

        // A leftover directory with no `ticket.json` is ignored by the
        // loader: there is no migration from the pre-split layout.
        let stray_dir = dir.path().join("tickets").join("TICKET-99");
        std::fs::create_dir_all(&stray_dir).unwrap();
        std::fs::write(stray_dir.join("anything.json"), "not json").unwrap();

        let resumed = TicketSystem::load(dir.path()).unwrap();
        assert!(resumed.get_ticket("TICKET-1").is_some());
        assert!(resumed.get_ticket("TICKET-99").is_none());
    }

    #[test]
    fn load_skips_dir_with_malformed_ticket_json() {
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
        let sys = TicketSystem::load(dir.path()).unwrap();
        assert!(sys.get_ticket("TICKET-7").is_none());
    }

    #[test]
    fn ticket_json_does_not_carry_replies_field() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let sys = TicketSystem::new();
        sys.dir(dir.path().to_path_buf());
        sys.task("hello");
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
        let (sys, dir) = test_system();
        sys.task("hello");
        sys.reply("TICKET-1", "first");
        sys.reply("TICKET-1", "second");
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
            let sys = TicketSystem::new();
            sys.dir(dir.path().to_path_buf());
            sys.task("hello");
            sys.reply("TICKET-1", "first");
            sys.reply("TICKET-1", "second");
        }
        let resumed = TicketSystem::load(dir.path()).unwrap();
        let t = resumed.get_ticket("TICKET-1").unwrap();
        let texts: Vec<_> = t
            .replies
            .iter()
            .filter_map(|r| match r.content.first()? {
                ReplyContent::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["first", "second"]);
    }

    #[test]
    fn compaction_pair_without_ticket_header_is_ignored_on_load() {
        use super::super::reply::ReplyContent;
        let dir = crate::test_util::TempDir::new().unwrap();
        {
            let sys = TicketSystem::new();
            sys.dir(dir.path().to_path_buf());
            sys.task("hello");
            sys.reply("TICKET-1", "first");
        }
        use super::super::reply::Author;

        // Drop a stray `replies.<ts>.jsonl` with NO paired `ticket.<ts>.json`.
        // The loader's paired-check rule must ignore it and fall back to the
        // running `replies.jsonl`.
        let key_dir = dir.path().join("tickets").join("TICKET-1");
        let orphan = Reply {
            author: Author::User,
            content: vec![ReplyContent::Text("orphan".into())],
            created_at: 9_999_999_999_999,
        };
        std::fs::write(
            key_dir.join("replies.9999999999999.jsonl"),
            format!("{}\n", serde_json::to_string(&orphan).unwrap()),
        )
        .unwrap();

        let resumed = TicketSystem::load(dir.path()).unwrap();
        let t = resumed.get_ticket("TICKET-1").unwrap();
        let texts: Vec<_> = t
            .replies
            .iter()
            .filter_map(|r| match r.content.first()? {
                ReplyContent::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["first"]);
        assert!(
            !texts.contains(&"orphan"),
            "orphan compaction file must not be selected as base"
        );
    }

    #[test]
    fn load_replays_via_newest_compaction_pair_plus_tail() {
        use super::super::reply::ReplyContent;
        let dir = crate::test_util::TempDir::new().unwrap();
        let key_dir = dir.path().join("tickets").join("TICKET-1");
        std::fs::create_dir_all(&key_dir).unwrap();

        // Active header.
        let header = serde_json::json!({
            "task": "hello", "labels": [], "key": "TICKET-1",
            "status": "Todo", "reporter": "user", "created_at": 1,
            "started_at": null, "finished_at": null, "failed_at": null,
            "result": null, "parent": null
        });
        std::fs::write(
            key_dir.join("ticket.json"),
            serde_json::to_string(&header).unwrap(),
        )
        .unwrap();

        // Running log carries the full history including pre-compaction entries.
        for c in [50, 150, 250, 350].iter() {
            let line = serde_json::json!({
                "author": "user",
                "content": [{ "Text": format!("c{c}") }],
                "created_at": c,
            });
            crate::persistence::append_line(
                &key_dir.join("replies.jsonl"),
                &serde_json::to_string(&line).unwrap(),
            )
            .unwrap();
        }

        // Compaction file pair captured at ts=200 contains only the summary.
        let summary = serde_json::json!({
            "author": "user",
            "content": [{ "Text": "summary" }],
            "created_at": 200,
        });
        std::fs::write(
            key_dir.join("replies.200.jsonl"),
            format!("{}\n", serde_json::to_string(&summary).unwrap()),
        )
        .unwrap();
        std::fs::write(
            key_dir.join("ticket.200.json"),
            serde_json::to_string(&header).unwrap(),
        )
        .unwrap();

        let sys = TicketSystem::load(dir.path()).unwrap();
        let t = sys.get_ticket("TICKET-1").unwrap();
        let texts: Vec<_> = t
            .replies
            .iter()
            .filter_map(|r| match r.content.first()? {
                ReplyContent::Text(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        // Base = "summary"; tail = entries with created_at > 200 → c250, c350.
        assert_eq!(
            texts,
            vec!["summary".to_string(), "c250".into(), "c350".into()]
        );
    }

    #[test]
    fn ticket_lifecycle_event_writes_stats_file() {
        let (sys, dir) = test_system();
        sys.task("seed");
        sys.claim(|t| t.status == Status::Todo, "alice").unwrap();
        sys.set_result("TICKET-1", serde_json::Value::Null).unwrap();
        sys.set_finished("TICKET-1", "agent").unwrap();

        let bytes = std::fs::read(dir.path().join("stats.json")).expect("stats file written");
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["tickets_created"], 1);
        assert_eq!(body["tickets_finished"], 1);
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

        let sys = TicketSystem::load(dir.path()).unwrap();
        assert_eq!(sys.stats().turns(), 42);
        assert_eq!(sys.stats().requests(), 7);
    }

    #[test]
    fn load_falls_back_to_derivation_when_stats_file_malformed() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketSystem::new();
        original.dir(dir.path().to_path_buf());
        original.task("seed");
        original
            .set_result("TICKET-1", serde_json::Value::Null)
            .unwrap();
        original.set_finished("TICKET-1", "agent").unwrap();
        drop(original);

        std::fs::write(dir.path().join("stats.json"), "not json").unwrap();

        let sys = TicketSystem::load(dir.path()).unwrap();
        assert_eq!(sys.stats().tickets_created(), 1);
        assert_eq!(sys.stats().tickets_finished(), 1);
    }

    #[test]
    fn ticket_with_json_schema_round_trips_through_load() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketSystem::new();
        original.dir(dir.path().to_path_buf());
        let schema_doc = serde_json::json!({
            "type": "object",
            "properties": { "n": { "type": "integer" } },
            "required": ["n"],
        });
        let schema = crate::schemas::Schema::parse(schema_doc.clone()).unwrap();
        original.ticket(Ticket::new("counted").schema(schema));
        drop(original);

        let resumed = TicketSystem::load(dir.path()).unwrap();
        let t = resumed.get_ticket("TICKET-1").unwrap();
        let restored = t.schema.expect("JSON schema must restore");
        assert!(restored.validate(serde_json::json!({"n": 3})).is_ok());
        assert!(restored.validate(serde_json::json!({})).is_err());
    }

    #[test]
    fn summarize_writes_compaction_pair_with_matching_timestamps() {
        let (sys, _tmp) = test_system();
        let key = sys.task("hello");
        sys.add_reply(&key, Reply::user_text("turn one"));
        sys.add_reply(
            &key,
            Reply::assistant(&[ContentBlock::Text {
                text: "first reply".into(),
            }]),
        );

        sys.summarize(&key, "SUMMARY-TEXT".into());

        let ticket_dir = sys.dir_value().join("tickets").join(&key);
        let mut header_ts: Option<u64> = None;
        let mut replies_ts: Option<u64> = None;
        for entry in std::fs::read_dir(&ticket_dir).unwrap().flatten() {
            let name = entry.file_name().into_string().unwrap();
            if let Some(rest) = name
                .strip_prefix("ticket.")
                .and_then(|s| s.strip_suffix(".json"))
            {
                if let Ok(ts) = rest.parse::<u64>() {
                    header_ts = Some(ts);
                }
            }
            if let Some(rest) = name
                .strip_prefix("replies.")
                .and_then(|s| s.strip_suffix(".jsonl"))
            {
                if let Ok(ts) = rest.parse::<u64>() {
                    replies_ts = Some(ts);
                }
            }
        }
        assert!(
            header_ts.is_some() && replies_ts.is_some(),
            "save_compaction must write both ticket.<ts>.json and replies.<ts>.jsonl",
        );
        assert_eq!(
            header_ts, replies_ts,
            "pair timestamps must match so Replies::load picks them up as a committed pair",
        );
    }

    #[test]
    fn summarize_pair_round_trips_through_replies_load() {
        use super::super::reply::ReplyContent;
        let (sys, _tmp) = test_system();
        let key = sys.task("hello");
        sys.add_reply(&key, Reply::user_text("noise that gets compacted"));
        sys.add_reply(
            &key,
            Reply::assistant(&[ContentBlock::Text {
                text: "more noise".into(),
            }]),
        );

        sys.summarize(&key, "SUMMARY-TEXT".into());

        let reloaded = Replies::load(&sys.dir_value(), &key).unwrap();
        let texts: Vec<String> = reloaded
            .iter()
            .filter_map(|r| match &r.content[..] {
                [ReplyContent::Text(t)] => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec!["SUMMARY-TEXT".to_string()],
            "Replies::load must reconstruct the post-compaction transcript from the pair",
        );
    }

    #[test]
    fn replies_load_skips_orphan_replies_file_without_matching_header() {
        use super::super::reply::ReplyContent;
        let (sys, _tmp) = test_system();
        let key = sys.task("hello");
        sys.add_reply(&key, Reply::user_text("running entry"));

        let ticket_dir = sys.dir_value().join("tickets").join(&key);
        let orphan = ticket_dir.join("replies.42.jsonl");
        std::fs::write(
            &orphan,
            b"{\"author\":\"user\",\"content\":[{\"Text\":\"GHOST\"}],\"created_at\":1}\n",
        )
        .unwrap();

        let reloaded = Replies::load(&sys.dir_value(), &key).unwrap();
        let texts: Vec<String> = reloaded
            .iter()
            .filter_map(|r| match &r.content[..] {
                [ReplyContent::Text(t)] => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert!(
            !texts.iter().any(|t| t == "GHOST"),
            "Replies::load must skip a replies.<ts>.jsonl with no matching ticket.<ts>.json commit marker",
        );
        assert!(
            texts.iter().any(|t| t == "running entry"),
            "the running log must still be returned even when an orphan pair half exists",
        );
    }

    fn jsonl_line_count(path: &Path) -> usize {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty())
            .count()
    }

    fn latest_compaction_at(ticket_dir: &Path) -> u64 {
        std::fs::read_dir(ticket_dir)
            .unwrap()
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .into_string()
                    .ok()?
                    .strip_prefix("replies.")?
                    .strip_suffix(".jsonl")?
                    .parse::<u64>()
                    .ok()
            })
            .max()
            .expect("expected a replies.<ts>.jsonl file in the ticket dir")
    }

    #[test]
    fn add_reply_after_compaction_appends_to_current_replies_file_not_initial() {
        let (sys, _tmp) = test_system();
        let key = sys.task("hello");
        sys.add_reply(&key, Reply::user_text("pre-1"));
        sys.add_reply(&key, Reply::user_text("pre-2"));

        let ticket_dir = sys.dir_value().join("tickets").join(&key);
        let initial = ticket_dir.join("replies.jsonl");
        let initial_before = jsonl_line_count(&initial);

        sys.summarize(&key, "SUMMARY".into());
        let at = latest_compaction_at(&ticket_dir);
        let compaction_replies = ticket_dir.join(format!("replies.{at}.jsonl"));
        let compaction_before = jsonl_line_count(&compaction_replies);

        sys.add_reply(&key, Reply::user_text("post-1"));

        assert_eq!(
            jsonl_line_count(&initial),
            initial_before,
            "post-compaction append must not grow the initial replies.jsonl",
        );
        assert_eq!(
            jsonl_line_count(&compaction_replies),
            compaction_before + 1,
            "post-compaction append must grow the current compaction's replies file by one line",
        );
        let body = std::fs::read_to_string(&compaction_replies).unwrap();
        assert!(
            body.contains("post-1"),
            "the appended payload must land in the current compaction's replies file, got: {body}",
        );
    }

    #[test]
    fn second_compaction_redirects_appends_to_newest_compaction_replies_file() {
        let (sys, _tmp) = test_system();
        let key = sys.task("hello");
        sys.add_reply(&key, Reply::user_text("turn one"));

        sys.summarize(&key, "SUMMARY-1".into());
        let ticket_dir = sys.dir_value().join("tickets").join(&key);
        let at1 = latest_compaction_at(&ticket_dir);

        sys.add_reply(&key, Reply::user_text("between"));

        // Force the second compaction onto a different millisecond so
        // the pair gets a distinct timestamp.
        std::thread::sleep(std::time::Duration::from_millis(2));
        sys.summarize(&key, "SUMMARY-2".into());

        let at2 = latest_compaction_at(&ticket_dir);
        assert!(
            at2 > at1,
            "second compaction must produce a strictly later timestamp (got at1={at1}, at2={at2})",
        );

        sys.add_reply(&key, Reply::user_text("after-2"));

        let first_compaction =
            std::fs::read_to_string(ticket_dir.join(format!("replies.{at1}.jsonl"))).unwrap();
        let second_compaction =
            std::fs::read_to_string(ticket_dir.join(format!("replies.{at2}.jsonl"))).unwrap();
        let initial = std::fs::read_to_string(ticket_dir.join("replies.jsonl")).unwrap();

        assert!(
            second_compaction.contains("after-2"),
            "post-second-compaction append must land in replies.<at2>.jsonl",
        );
        assert!(
            !first_compaction.contains("after-2"),
            "post-second-compaction append must NOT land in the older compaction's replies file",
        );
        assert!(
            !initial.contains("after-2"),
            "post-second-compaction append must NOT land in the initial replies file",
        );
    }

    #[test]
    fn summarize_rewrites_task_to_summary_and_shrinks_persisted_header() {
        let (sys, _tmp) = test_system();
        let huge = "x".repeat(500_000);
        let key = sys.task(&huge);
        sys.add_reply(&key, Reply::user_text("turn one"));

        let ticket_dir = sys.dir_value().join("tickets").join(&key);
        let header_before = std::fs::metadata(ticket_dir.join("ticket.json"))
            .unwrap()
            .len();
        assert!(
            header_before > 500_000,
            "pre-compaction header should carry the huge task ({header_before} bytes)",
        );

        sys.summarize(&key, "SUMMARY".into());

        let ticket = sys.get_ticket(&key).unwrap();
        assert_eq!(
            ticket.task,
            serde_json::Value::String("SUMMARY".into()),
            "summarize must rewrite task to the summary text",
        );

        let at = latest_compaction_at(&ticket_dir);
        let compaction_header = ticket_dir.join(format!("ticket.{at}.json"));
        let compaction_size = std::fs::metadata(&compaction_header).unwrap().len();
        assert!(
            compaction_size < 2_000,
            "compaction header must be small (got {compaction_size} bytes): it should reflect post-compaction state, not duplicate the original task",
        );
        let body = std::fs::read_to_string(&compaction_header).unwrap();
        assert!(
            body.contains("SUMMARY"),
            "compaction header must carry the summary as task, got: {body}",
        );
        assert!(
            !body.contains(&huge),
            "compaction header must NOT contain the original task body",
        );
    }

    #[test]
    fn orphan_compaction_replies_without_header_does_not_divert_appends() {
        let (sys, _tmp) = test_system();
        let key = sys.task("hello");

        // Drop a stray replies.<ts>.jsonl with no matching ticket.<ts>.json
        // commit marker. The writer must apply the same paired-check rule
        // Replies::load uses on the read side and ignore the orphan.
        let ticket_dir = sys.dir_value().join("tickets").join(&key);
        let orphan = ticket_dir.join("replies.42.jsonl");
        std::fs::write(&orphan, b"").unwrap();

        sys.add_reply(&key, Reply::user_text("first"));

        let initial = std::fs::read_to_string(ticket_dir.join("replies.jsonl")).unwrap();
        let orphan_body = std::fs::read_to_string(&orphan).unwrap();
        assert!(
            initial.contains("first"),
            "append must go to the initial replies file when no committed compaction exists",
        );
        assert!(
            !orphan_body.contains("first"),
            "append must NOT land in an orphan replies file without a paired header",
        );
    }
}
