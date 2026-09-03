//! Every change a [`Werk`] makes to its tasks, and the events each
//! change emits.

use std::path::{Path, PathBuf};

use crate::event::Event;
use crate::persistence::Persist;
use crate::schemas::SchemaViolations;

use super::super::query::{Origin, Query};
use super::error::TaskError;
use super::reply::Reply;
use super::task::{Status, Task};
use super::werk::Werk;
use super::{now_millis, numeric_id, Replies, TaskResult};

/// Highest `t-<N>` already on disk under `<dir>/tasks/`, or 0 if
/// none. Only needed for a Werk built via `new()`, which never reads
/// the directory itself; `load()` derives this from what it already read.
fn max_existing_task_id(dir: &Path) -> u64 {
    std::fs::read_dir(dir.join("tasks"))
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

impl Werk {
    /// Insert `task`, filling in the fields agentwerk owns. The task is always born
    /// `todo`; to pin it to a specific agent, label it with the agent's
    /// name. Returns the inserted task's ID.
    pub(crate) fn insert(&self, mut task: Task, reporter: String) -> String {
        let id = {
            let mut next = self.next_task_id.lock().unwrap();
            let base = next.get_or_insert_with(|| max_existing_task_id(&self.get_dir()));
            *base += 1;
            *base
        };
        task.id = format!("t-{id}");
        task.created_at = now_millis();
        task.reporter = reporter;
        task.result = None;
        task.status = Status::Todo;
        task.cancelled = self
            .cancel_filters
            .lock()
            .unwrap()
            .iter()
            .any(|filter| filter.matches(&task));
        let mut store = self.tasks.lock().unwrap();
        let id = task.id.clone();
        let reporter = task.reporter.clone();
        store.insert(id.clone(), task);
        drop(store);
        self.save_task(&id);
        self.emit_event(
            Event::new(Event::TASK_CREATED)
                .task_id(&id)
                .agent_id(&reporter),
        );
        id
    }

    /// Write the task at `id` to disk. No-op when the task is missing.
    fn save_task(&self, id: &str) {
        if let Some(t) = self.get_task(id) {
            let _ = t.save(&self.get_dir());
        }
    }

    /// Write a tool's full output and get its path relative to the configured `dir`, or `None` when the write fails. The relative path keeps replies portable when the task directory moves; join it with [`Self::get_dir`] to recover the on-disk path. The write is best-effort like the surrounding observational persistence.
    pub(crate) fn write_tool_output(
        &self,
        id: &str,
        tool_use_id: &str,
        content: &str,
    ) -> Option<PathBuf> {
        let rel = crate::persistence::output_path(id, tool_use_id);
        let absolute = self.get_dir().join(&rel);
        crate::persistence::write_atomic(&absolute, content.as_bytes())
            .ok()
            .map(|_| rel)
    }

    /// Atomically find a `todo` task the query selects, assign it to
    /// `agent_id`, and transition to `in_progress`.
    ///
    /// The earliest candidate must itself be `todo`, so a query naming no
    /// status never reaches past a task already claimed.
    pub(crate) fn claim(&self, query: &Query, agent_id: &str) -> Option<String> {
        let projected = match query.origin() {
            Origin::Task => None,
            Origin::Event | Origin::Joined => {
                Some(self.tasks_selected_by(query).into_iter().next()?.id)
            }
        };
        let now = now_millis();
        let id = {
            let mut store = self.tasks.lock().unwrap();
            let id = match projected {
                Some(id) => id,
                None => {
                    let mut candidates: Vec<&Task> = store
                        .values()
                        .filter(|task| query.matches_task(task))
                        .collect();
                    query.sort_tasks(&mut candidates);
                    candidates.first()?.id.clone()
                }
            };
            let task = store.get_mut(&id)?;
            if task.status != Status::Todo {
                return None;
            }
            task.assignee = Some(agent_id.to_string());
            task.stamp_transition(Status::InProgress, now);
            task.status = Status::InProgress;
            id
        };
        self.save_task(&id);
        // Emitted here rather than from the loop: the claim is the moment a
        // task starts, so a host claiming one records it the same way.
        self.emit_event(
            Event::new(Event::TASK_STARTED)
                .task_id(&id)
                .agent_id(agent_id),
        );
        Some(id)
    }

    /// Append `reply` to the task's replies. No-op when the
    /// task is missing: the loop drops out shortly afterwards on the
    /// same condition. The task record is not rewritten; the replies
    /// live only in `replies.jsonl`.
    pub(crate) fn append_reply(&self, id: &str, reply: Reply) {
        {
            let mut store = self.tasks.lock().unwrap();
            let Some(t) = store.get_mut(id) else { return };
            t.replies.push(reply.clone());
        }
        let _ = Replies::append(&self.get_dir(), id, &reply);
    }

    /// Transition a task to `finished`, emitting `task_finished`
    /// under `agent`'s name.
    pub(crate) fn set_finished_by(&self, id: &str, agent: &str) -> Result<(), TaskError> {
        self.set_final_status(id, Status::Finished, agent)
    }

    /// Attach `result` to the task and transition it to `finished`,
    /// resolving it from outside the run. Validates against the task's
    /// schema first, so a host finish and an agent finish record the same
    /// contract. The emitted `task_finished` carries an empty agent ID,
    /// like the run-level events no single agent causes.
    ///
    /// ```no_run
    /// # use agentwerk::Werk;
    /// # fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let werk = Werk::new();
    /// let id = werk.add_task("Look up the cached answer.");
    /// werk.set_task_finished(&id, "42")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_task_finished(
        &self,
        id: &str,
        result: impl serde::Serialize,
    ) -> Result<(), TaskError> {
        let value = serde_json::to_value(result).expect("result is serializable");
        self.set_result(id, value)
            .map_err(|violations| TaskError::ResultRejected {
                message: violations.to_string(),
            })?;
        self.set_final_status(id, Status::Finished, "")
    }

    /// Transition a task to `failed`. No result argument, unlike
    /// [`Self::set_task_finished`]: a failed task has none. The emitted
    /// `task_failed` carries an empty agent ID, like the run-level
    /// events no single agent causes.
    pub fn set_task_failed(&self, id: &str) -> Result<(), TaskError> {
        self.set_final_status(id, Status::Failed, "")
    }

    /// Transition a task to `failed`, emitting `task_failed` under
    /// `agent`'s name. All agent failures use this transition.
    pub(crate) fn set_failed_by(&self, id: &str, agent: &str) -> Result<(), TaskError> {
        self.set_final_status(id, Status::Failed, agent)
    }

    fn set_final_status(&self, id: &str, status: Status, agent: &str) -> Result<(), TaskError> {
        // Increment BEFORE the status flip and decrement only after the
        // terminal event has been emitted: the drain check in `finish_tasks()`
        // must never observe (empty Werk, zero counter) mid-transition,
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
        let result = {
            let mut store = self.tasks.lock().unwrap();
            let task = store
                .get_mut(id)
                .ok_or_else(|| TaskError::TaskNotFound { id: id.to_string() })?;
            // First outcome wins. The host resolving a task an agent is still
            // turning, and the agent giving up on one the host just resolved,
            // are the same race from either side; without this the loser's
            // status overwrites the winner's and leaves, say, a `failed` task
            // carrying a result. Checked under the lock the write happens
            // under, so two racing transitions cannot both pass it.
            if matches!(task.status, Status::Finished | Status::Failed) {
                return Ok(());
            }
            task.stamp_transition(status, now);
            task.status = status;
            task.result.clone()
        };
        let name = match status {
            Status::Finished => Event::TASK_FINISHED,
            _ => Event::TASK_FAILED,
        };
        let mut event = Event::new(name).task_id(id).agent_id(agent);
        if let (Status::Finished, Some(result)) = (status, result) {
            event = event.data(serde_json::json!({ "result": result }));
        }
        self.emit_event(event);
        self.save_task(id);
        Ok(())
    }

    /// Validate `result` against the task's schema, write it to the task's
    /// `result.json`, and store the validated result on the task, which it
    /// returns alongside the JSON pointer of every value validation repaired to
    /// accept it. Does not finish the task: the caller does.
    pub(crate) fn set_result(
        &self,
        id: &str,
        result: serde_json::Value,
    ) -> Result<(serde_json::Value, Vec<String>), SchemaViolations> {
        let schema = self
            .tasks
            .lock()
            .unwrap()
            .get(id)
            .and_then(|t| t.schema.clone());
        let (result, repairs) = match schema.as_ref() {
            Some(schema) => schema.validate(result)?,
            None => (result, Vec::new()),
        };
        let attached = {
            let mut store = self.tasks.lock().unwrap();
            // A task that already reached an outcome keeps the result that
            // outcome carried, the way its status does: the agent's result
            // arriving after the host resolved the task, or the reverse, is
            // the same race, and either way the late one is not the answer.
            match store.get_mut(id) {
                Some(task) if task.is_pending() => {
                    task.result = Some(result.clone());
                    true
                }
                _ => false,
            }
        };
        // A missing task records nothing: no phantom results line, no file.
        if attached {
            let record = TaskResult {
                id: id.to_string(),
                value: Some(result.clone()),
            };
            // Best-effort: the result is already attached in memory, so a
            // failed write is observational, not load-bearing.
            let _ = record.save(&self.get_dir());
        }
        Ok((result, repairs))
    }

    /// Apply `editor` to task `id`'s replies now, then rewrite them
    /// in place so the change survives resumption. Triggers no request.
    /// No-op when the task is missing. The edit must keep the replies
    /// well-formed (matched tool_use/tool_result pairs); they are sent
    /// as-is. Leaves the task and the token accounting untouched, which
    /// is why compaction resets the usage history itself.
    ///
    /// Inside [`Self::on_event`] it rewrites what the model reads next: the
    /// reply the event announces is stored, and the next request re-reads
    /// the task.
    ///
    /// ```no_run
    /// use agentwerk::Werk;
    /// use agentwerk::Event;
    /// use agentwerk::agents::tasks::{Reply, ReplyContent};
    ///
    /// let werk = Werk::new();
    /// werk.on_event(|werk, event| {
    ///     if event.get_name() != Event::TOOL_CALL_FAILED {
    ///         return;
    ///     }
    ///     werk.edit_replies(event.get_task_id(), |replies| {
    ///         // Drop both sides of the failed exchange: the assistant's tool_use
    ///         // and the failed tool_result, so no unpaired block is left behind.
    ///         replies.retain(|reply| {
    ///             !reply.get_content().iter().any(|b| {
    ///                 matches!(
    ///                     b,
    ///                     ReplyContent::ToolUse { .. }
    ///                         | ReplyContent::ToolResult { succeeded: false, .. }
    ///                 )
    ///             })
    ///         });
    ///         replies.push(Reply::user_text("That approach failed. Re-read the file first."));
    ///     });
    /// });
    /// ```
    pub fn edit_replies(&self, id: &str, editor: impl FnOnce(&mut Vec<Reply>)) -> &Self {
        let task_copy = {
            let mut store = self.tasks.lock().unwrap();
            let Some(task) = store.get_mut(id) else {
                return self;
            };
            let before = task.replies.clone();
            editor(&mut task.replies);
            // An editor that inspected without changing anything must not
            // trigger a rewrite: leave the on-disk files alone.
            if task.replies == before {
                return self;
            }
            task.clone()
        };
        let replies = Replies {
            id: id.to_string(),
            entries: task_copy.replies,
        };
        let _ = replies.save(&self.get_dir());
        self
    }

    /// Edit caller-settable fields. Each `Some` overwrites; `None`
    /// leaves the field untouched. A label can be replaced but not removed.
    pub(crate) fn edit(
        &self,
        id: &str,
        new_task: Option<serde_json::Value>,
        label: Option<String>,
    ) -> Result<(), TaskError> {
        let mut store = self.tasks.lock().unwrap();
        let task = store
            .get_mut(id)
            .ok_or_else(|| TaskError::TaskNotFound { id: id.to_string() })?;
        if let Some(t) = new_task {
            task.task = t;
        }
        if let Some(l) = label {
            task.label = Some(l);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;
    use std::sync::{Arc, Mutex};

    fn emit_event(werk: &Werk, id: &str, agent: &str, event: Event) -> Event {
        werk.emit_event(event.task_id(id).agent_id(agent))
    }

    #[test]
    fn task_creates_task_with_user_reporter() {
        let (werk, _tmp) = test_werk();
        werk.add_task("hello");
        let t = werk.get_task("t-1").unwrap();
        assert_eq!(t.task, serde_json::Value::String("hello".into()));
        assert_eq!(t.reporter, "user");
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn labeled_task_attaches_label_and_leaves_status_todo() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("hello").label("research"));
        let t = werk.get_task("t-1").unwrap();
        assert_eq!(t.label.as_deref(), Some("research"));
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn create_with_named_label_is_born_todo_and_carries_label() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("specific work for alice").label("alice"));
        let t = werk.get_task("t-1").unwrap();
        assert_eq!(t.label.as_deref(), Some("alice"));
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn create_with_label_and_schema_is_stored_verbatim() {
        let (werk, _tmp) = test_werk();
        let schema = crate::schemas::Schema::new(serde_json::json!({"type": "string"})).unwrap();
        werk.add_task(Task::new("x").label("urgent").schema(schema));
        let t = werk.get_task("t-1").unwrap();
        assert_eq!(t.label.as_deref(), Some("urgent"));
        assert!(t.schema.is_some());
    }

    #[test]
    fn set_result_updates_task() {
        let (werk, _tmp) = test_werk();
        werk.add_task("hi");
        werk.set_result("t-1", serde_json::Value::String("answer".into()))
            .unwrap();
        let stored = werk.get_task("t-1").unwrap();
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
        let (werk, _tmp) = test_werk();
        werk.add_task("ok");
        werk.add_task("oops");
        werk.add_task("pending");
        werk.claim(&Query::from("t-1"), "agent");
        werk.set_finished_by("t-1", "agent").unwrap();
        werk.set_task_failed("t-2").unwrap();
        let done = werk.find_tasks(|t: &Task| t.status == Status::Finished);
        let failed = werk.find_tasks(|t: &Task| t.status == Status::Failed);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].id, "t-1");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, "t-2");
    }

    #[test]
    fn task_status_transitions_record_stats() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        werk.add_task("c");
        assert_eq!(werk.stats.event_count(Event::TASK_CREATED), 3);
        werk.claim(&Query::from("t-1"), "agent");
        werk.set_finished_by("t-1", "agent").unwrap();
        werk.claim(&Query::from("t-2"), "agent");
        werk.set_task_failed("t-2").unwrap();
        assert_eq!(werk.stats.event_count(Event::TASK_FINISHED), 1);
        assert_eq!(werk.stats.event_count(Event::TASK_FAILED), 1);
    }

    #[test]
    fn a_task_logs_created_started_and_finished_in_order() {
        let (werk, dir) = test_werk();
        werk.add_task("hello");
        werk.claim(&Query::from("t-1"), "agent");
        werk.set_finished_by("t-1", "agent").unwrap();
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 3);
        let names: Vec<&str> = lines.iter().map(|l| l["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["task_created", "task_started", "task_finished"]);
        for line in &lines {
            assert_eq!(line["task_id"], "t-1");
            assert!(line["created_at"].is_u64());
        }
    }

    #[test]
    fn streamed_chunks_stay_out_of_the_log() {
        let (werk, dir) = test_werk();
        werk.add_task("seed");
        emit_event(
            &werk,
            "t-1",
            "agent",
            Event::new(Event::TEXT_CHUNK_RECEIVED)
                .data(serde_json::json!({ "content": "a piece of the reply" })),
        );
        // One line per token would outweigh every other line, and the replies
        // already hold the text.
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["name"], "task_created");
    }

    #[test]
    fn load_replays_the_token_totals_a_run_already_spent() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("seed");
        emit_event(
            &original,
            "t-1",
            "agent",
            Event::new(Event::REQUEST_FINISHED).data(serde_json::json!({
                "model": "m",
                "usage": crate::providers::TokenUsage {
                    input_tokens: 900,
                    output_tokens: 120,
                },
            })),
        );
        drop(original);

        // The token limits divide against these, so a resumed run that read
        // them back as zero would silently start its budget over.
        let resumed = Werk::load(dir.path()).unwrap();
        assert_eq!(resumed.stats.input_tokens(), 900);
        assert_eq!(resumed.stats.output_tokens(), 120);
    }

    #[test]
    fn set_failed_logs_a_failure_without_a_start() {
        let (werk, dir) = test_werk();
        werk.add_task("hello");
        werk.set_task_failed("t-1").unwrap();
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["name"], "task_created");
        assert_eq!(lines[1]["name"], "task_failed");
        assert_eq!(lines[1]["task_id"], "t-1");
    }

    #[test]
    fn a_logged_event_carries_the_task_label_when_pinned() {
        let (werk, dir) = test_werk();
        werk.add_task(Task::new("specific").label("alice"));
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["name"], "task_created");
        assert_eq!(lines[0]["label"], "alice");
    }

    #[test]
    fn the_log_holds_one_line_per_lifecycle_turn_across_tasks() {
        let (werk, dir) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        werk.claim(&Query::from("t-1"), "agent");
        werk.set_finished_by("t-1", "agent").unwrap();
        werk.claim(&Query::from("t-2"), "agent");
        werk.set_task_failed("t-2").unwrap();
        // 2 created + 2 started + 1 finished + 1 failed
        assert_eq!(read_events_log(dir.path()).len(), 6);
    }

    #[test]
    fn claim_transitions_todo_to_in_progress_and_sets_the_assignee() {
        let (werk, _tmp) = test_werk();
        werk.add_task("hello");
        let id = werk
            .claim(&Query::from("task.status = todo"), "alice")
            .unwrap();
        assert_eq!(id, "t-1");
        let t = werk.get_task(&id).unwrap();
        assert_eq!(t.status, Status::InProgress);
        assert_eq!(t.assignee.as_deref(), Some("alice"));
        assert!(t.started_at.is_some());
    }

    #[test]
    fn claim_leaves_the_label_the_task_was_filed_with() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("hello").label("analysis"));
        let id = werk
            .claim(&Query::from("task.label = analysis"), "alice")
            .unwrap();
        assert_eq!(
            werk.get_task(&id).unwrap().label.as_deref(),
            Some("analysis")
        );
    }

    #[test]
    fn claim_returns_none_when_no_task_matches() {
        let (werk, _tmp) = test_werk();
        werk.add_task("hello");
        assert!(werk
            .claim(&Query::from("task.label = nonexistent"), "alice")
            .is_none());
    }

    #[test]
    fn second_claim_of_same_task_returns_none() {
        let (werk, _tmp) = test_werk();
        werk.add_task("hello");
        let first = werk.claim(&Query::from("t-1"), "alice");
        assert!(first.is_some());
        // Second claim: task is now InProgress, not Todo.
        let second = werk.claim(&Query::from("t-1"), "bob");
        assert!(second.is_none());
    }

    #[test]
    fn claim_picks_earliest_eligible_task() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        werk.add_task("c");
        let id = werk
            .claim(&Query::from("task.status = todo"), "alice")
            .unwrap();
        assert_eq!(id, "t-1");
    }

    #[test]
    fn claim_logs_the_task_starting() {
        let (werk, dir) = test_werk();
        werk.add_task("hello");
        werk.claim(&Query::from("task.status = todo"), "alice");
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["name"], "task_created");
        assert_eq!(lines[1]["name"], "task_started");
        assert_eq!(lines[1]["task_id"], "t-1");
        assert_eq!(lines[1]["agent_id"], "alice");
    }

    #[test]
    fn set_finished_transitions_to_finished() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("hello");
        werk.claim(&Query::from("task.status = todo"), "alice");
        werk.set_finished_by(&id, "alice").unwrap();
        let t = werk.get_task(&id).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert!(t.finished_at.is_some());
    }

    #[test]
    fn task_finished_event_carries_the_stored_result() {
        let (werk, dir) = test_werk();
        let id = werk.add_task("hello");
        werk.set_task_finished(&id, serde_json::json!({"answer": 42}))
            .unwrap();

        let lines = read_events_log(dir.path());
        let event = lines.last().unwrap();
        assert_eq!(event["name"], Event::TASK_FINISHED);
        assert_eq!(event["data"]["result"], serde_json::json!({"answer": 42}));
    }

    #[test]
    fn task_finished_event_distinguishes_no_result_from_null() {
        let (werk, dir) = test_werk();
        let without = werk.add_task("without");
        werk.set_finished_by(&without, "alice").unwrap();
        let with_null = werk.add_task("with null");
        werk.set_task_finished(&with_null, ()).unwrap();

        let lines = read_events_log(dir.path());
        let finished: Vec<&serde_json::Value> = lines
            .iter()
            .filter(|event| event["name"] == Event::TASK_FINISHED)
            .collect();
        assert!(finished[0]["data"].get("result").is_none());
        assert_eq!(
            finished[1]["data"].get("result"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn task_finished_result_round_trips_through_the_event_log() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let id = werk.add_task("hello");
        werk.set_task_finished(&id, "done").unwrap();
        drop(werk);

        let resumed = Werk::load(dir.path()).unwrap();
        let event = resumed.find_event("event.name = task_finished").unwrap();
        assert_eq!(event.get_data()["result"], "done");
        assert_eq!(resumed.get_results(), vec![serde_json::json!("done")]);
    }

    #[test]
    fn set_failed_transitions_to_failed() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("hello");
        werk.claim(&Query::from("task.status = todo"), "alice");
        werk.set_task_failed(&id).unwrap();
        let t = werk.get_task(&id).unwrap();
        assert_eq!(t.status, Status::Failed);
        assert!(t.failed_at.is_some());
    }

    #[test]
    fn a_finished_task_is_not_reopened_by_a_later_failure() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("hello");
        werk.claim(&Query::from("task.status = todo"), "alice");
        werk.set_task_finished(&id, "host result").unwrap();

        // Alice was still turning the task and gives up after the host
        // resolved it.
        werk.set_failed_by(&id, "alice").unwrap();

        let task = werk.get_task(&id).unwrap();
        assert_eq!(task.status, Status::Finished);
        assert_eq!(task.result, Some(serde_json::json!("host result")));
        assert!(task.failed_at.is_none());
        let terminal = werk.find_events(|event: &Event| {
            matches!(event.get_name(), Event::TASK_FINISHED | Event::TASK_FAILED)
        });
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].get_data()["result"], "host result");
    }

    #[test]
    fn a_failed_task_is_not_reopened_by_a_later_finish() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("hello");
        werk.claim(&Query::from("task.status = todo"), "alice");
        werk.set_failed_by(&id, "alice").unwrap();

        werk.set_task_finished(&id, "late result").unwrap();

        let task = werk.get_task(&id).unwrap();
        assert_eq!(task.status, Status::Failed);
        assert!(task.finished_at.is_none());
        // The result too, or the task reads as failed while carrying an answer.
        assert_eq!(task.result, None);
    }

    #[test]
    fn set_finished_stores_the_result_and_resolves_the_task() {
        let (werk, _tmp) = test_werk();
        werk.add_task("hello");
        werk.set_task_finished("t-1", serde_json::json!({"answer": 42}))
            .unwrap();
        let t = werk.get_task("t-1").unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result, Some(serde_json::json!({"answer": 42})));
        assert_eq!(
            werk.get_results().pop(),
            Some(serde_json::json!({"answer": 42}))
        );
    }

    #[test]
    fn set_finished_rejects_a_result_that_misses_the_task_schema() {
        let (werk, _tmp) = test_werk();
        let schema = crate::schemas::Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
        }))
        .unwrap();
        werk.add_task(Task::new("write a report").schema(schema));

        let err = werk
            .set_task_finished("t-1", serde_json::json!({"body": "no title"}))
            .unwrap_err();
        assert!(matches!(err, TaskError::ResultRejected { .. }));
        assert_eq!(werk.get_task("t-1").unwrap().status, Status::Todo);
    }

    #[test]
    fn set_finished_errors_on_an_unknown_id() {
        let (werk, _tmp) = test_werk();
        let err = werk.set_task_finished("t-9", "done").unwrap_err();
        assert!(matches!(err, TaskError::TaskNotFound { .. }));
    }

    #[test]
    fn write_tool_output_returns_relative_path_and_writes_absolute() {
        let (werk, dir) = test_werk();
        werk.add_task("seed");
        let rel = werk
            .write_tool_output("t-1", "call-1", "the full content")
            .expect("write succeeds when dir exists");
        let expected_rel: PathBuf = ["tasks", "t-1", "outputs", "call-1.txt"].iter().collect();
        assert_eq!(rel, expected_rel);
        let body = std::fs::read_to_string(dir.path().join(&rel)).unwrap();
        assert_eq!(body, "the full content");
    }

    #[test]
    fn write_tool_output_creates_outputs_subdir_lazily() {
        let (werk, dir) = test_werk();
        werk.add_task("seed");
        let outputs = dir.path().join("tasks").join("t-1").join("outputs");
        assert!(!outputs.exists());
        werk.write_tool_output("t-1", "call-1", "payload").unwrap();
        assert!(outputs.is_dir());
    }

    #[test]
    fn a_logged_event_names_the_agent_that_caused_it() {
        let (werk, dir) = test_werk();
        werk.add_task("first");
        werk.add_task("second");
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 2);
        // The reporter, since a task is created by whoever filed it.
        assert_eq!(lines[0]["agent_id"], "user");
        assert_eq!(lines[1]["agent_id"], "user");
    }

    // Resumption: Werk::load

    #[test]
    fn load_creates_tasks_dir_when_missing() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::load(dir.path()).unwrap();
        assert!(werk.get_tasks().is_empty());
        assert!(dir.path().join("tasks").is_dir());
    }

    /// A replies file written by an older version no longer parses. The
    /// task must not be skipped: its status and result would vanish with
    /// it, and the caller would receive a short store reporting success.
    #[test]
    fn load_reports_a_task_whose_replies_cannot_be_read() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("seed work");
        original.append_reply("t-1", Reply::user_text("hello"));
        original.set_finished_by("t-1", "agent").unwrap();
        drop(original);

        let replies = dir.path().join("tasks/t-1/replies.jsonl");
        std::fs::write(
            &replies,
            "{\"author\":\"user\",\"content\":[{\"Text\":\"hello\"}]}\n",
        )
        .unwrap();

        let Err(error) = Werk::load(dir.path()) else {
            panic!("load must fail when a task's replies cannot be read");
        };
        assert!(
            error.to_string().contains("t-1"),
            "the failure must name the task: {error}",
        );
    }

    #[test]
    fn load_restores_done_task_with_result_and_replies() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("seed work");
        original
            .set_result("t-1", serde_json::json!({"ok": true}))
            .unwrap();
        original.set_finished_by("t-1", "agent").unwrap();
        drop(original);

        let resumed = Werk::load(dir.path()).unwrap();
        let t = resumed.get_task("t-1").unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref(), Some(&serde_json::json!({"ok": true})));
        assert_eq!(t.task, serde_json::Value::String("seed work".into()));
    }

    #[test]
    fn insert_after_load_never_reuses_an_existing_id() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("seed work");
        drop(original);

        let resumed = Werk::load(dir.path()).unwrap();
        assert_eq!(resumed.add_task("more work"), "t-2");
    }

    #[test]
    fn insert_after_new_plus_dir_never_reuses_an_existing_id() {
        // The pattern a fresh process actually uses against a directory a
        // prior run already wrote into: `new()` (not `load()`) plus `.dir(..)`.
        let dir = crate::test_util::TempDir::new().unwrap();
        let first = Werk::new();
        first.set_dir(dir.path().to_path_buf());
        first.add_task("seed work");
        drop(first);

        let second = Werk::new();
        second.set_dir(dir.path().to_path_buf());
        assert_eq!(second.add_task("more work"), "t-2");
    }

    #[test]
    fn load_seeds_next_task_id_without_rescanning_tasks_dir() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("seed work");
        drop(original);

        let resumed = Werk::load(dir.path()).unwrap();
        // `load()` already knows the highest ID from what it just read
        // into memory; removing the directory here proves `insert()` does
        // not rescan it, since a rescan would find nothing and wrongly
        // restart numbering at 1.
        std::fs::remove_dir_all(dir.path().join("tasks")).unwrap();
        assert_eq!(resumed.add_task("more work"), "t-2");
    }

    #[test]
    fn load_restores_in_progress_replies() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("mid flight");
        original
            .claim(&Query::from("task.status = todo"), "alice")
            .unwrap();
        drop(original);

        let resumed = Werk::load(dir.path()).unwrap();
        let t = resumed.get_task("t-1").unwrap();
        assert_eq!(t.status, Status::InProgress);
        assert_eq!(t.assignee.as_deref(), Some("alice"));
    }

    #[test]
    fn load_replays_the_event_log_into_the_counters() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("a");
        original.add_task("b");
        original.set_result("t-1", serde_json::Value::Null).unwrap();
        original.set_finished_by("t-1", "agent").unwrap();
        original.set_task_failed("t-2").unwrap();
        drop(original);

        let resumed = Werk::load(dir.path()).unwrap();
        assert_eq!(resumed.stats.event_count(Event::TASK_CREATED), 2);
        assert_eq!(resumed.stats.event_count(Event::TASK_FINISHED), 1);
        assert_eq!(resumed.stats.event_count(Event::TASK_FAILED), 1);
    }
    #[test]
    fn load_skips_dir_without_task_json() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("valid");
        drop(original);

        // A leftover directory with no `task.json` is ignored by the
        // loader: there is no migration from the pre-split layout.
        let stray_dir = dir.path().join("tasks").join("t-99");
        std::fs::create_dir_all(&stray_dir).unwrap();
        std::fs::write(stray_dir.join("anything.json"), "not json").unwrap();

        let resumed = Werk::load(dir.path()).unwrap();
        assert!(resumed.get_task("t-1").is_some());
        assert!(resumed.get_task("t-99").is_none());
    }

    #[test]
    fn load_drops_the_label_of_a_task_written_before_it_was_singular() {
        // The accepted cost of the clean break: a pre-singular `task.json`
        // still loads, but its label is gone, so the task falls to the
        // default scope instead of its pool. Documented in architecture.md.
        let dir = crate::test_util::TempDir::new().unwrap();
        let task_dir = dir.path().join("tasks").join("t-1");
        std::fs::create_dir_all(&task_dir).unwrap();
        let body = serde_json::json!({
            "task": "scan the tree",
            "labels": ["scan"],
            "id": "t-1",
            "status": "Todo",
            "reporter": "user",
            "created_at": 1,
            "started_at": null,
            "finished_at": null,
            "failed_at": null,
            "result": null,
        });
        std::fs::write(
            task_dir.join("task.json"),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();

        let resumed = Werk::load(dir.path()).unwrap();
        let task = resumed.get_task("t-1").expect("the task loads");
        assert_eq!(task.label, None);
    }

    #[test]
    fn load_reports_a_malformed_task_json() {
        let dir = crate::test_util::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("tasks").join("t-7")).unwrap();
        std::fs::write(
            dir.path().join("tasks").join("t-7").join("task.json"),
            "not json",
        )
        .unwrap();
        let Err(error) = Werk::load(dir.path()) else {
            panic!("load must fail on a malformed task.json");
        };
        assert!(
            error.to_string().contains("t-7"),
            "the failure must name the task: {error}",
        );
    }

    #[test]
    fn load_rejects_a_task_record_that_only_carries_key() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let task_dir = dir.path().join("tasks").join("t-1");
        std::fs::create_dir_all(&task_dir).unwrap();
        let body = serde_json::json!({
            "task": "scan the tree",
            "key": "t-1",
            "status": "Todo",
            "reporter": "user",
            "created_at": 1,
            "started_at": null,
            "finished_at": null,
            "failed_at": null,
        });
        std::fs::write(
            task_dir.join("task.json"),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();

        assert!(Werk::load(dir.path()).is_err());
    }

    #[test]
    fn task_json_uses_id_without_a_key_alias() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        werk.add_task("hello");
        let stored =
            std::fs::read_to_string(dir.path().join("tasks").join("t-1").join("task.json"))
                .unwrap();
        let record: serde_json::Value = serde_json::from_str(&stored).unwrap();

        assert_eq!(record["id"], "t-1");
        assert!(record.get("key").is_none());
        assert!(record.get("parent").is_none());
    }

    #[test]
    fn load_ignores_the_removed_parent_field() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("hello");
        drop(original);

        let task_file = dir.path().join("tasks").join("t-1").join("task.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&task_file).unwrap()).unwrap();
        record["parent"] = serde_json::json!("t-0");
        std::fs::write(&task_file, serde_json::to_vec(&record).unwrap()).unwrap();

        let resumed = Werk::load(dir.path()).unwrap();
        assert_eq!(resumed.get_task("t-1").unwrap().get_task(), "hello");
    }

    #[test]
    fn task_json_does_not_carry_replies_field() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        werk.add_task("hello");
        let stored =
            std::fs::read_to_string(dir.path().join("tasks").join("t-1").join("task.json"))
                .unwrap();
        let v: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert!(
            v.as_object().unwrap().get("replies").is_none(),
            "task.json must not carry a `replies` field; got: {stored}",
        );
    }

    #[test]
    fn task_json_does_not_persist_cancellation() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let id = werk.add_task(Task::new("hello").label("scan"));
        werk.cancel_tasks("task.label = scan");
        werk.set_task_failed(&id).unwrap();

        let stored =
            std::fs::read_to_string(dir.path().join("tasks").join(&id).join("task.json")).unwrap();
        let record: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert!(record.get("cancelled").is_none());

        let resumed = Werk::load(dir.path()).unwrap();
        assert!(resumed.find_tasks("task.cancelled = true").is_empty());
        assert_eq!(resumed.find_tasks("task.cancelled = false").len(), 1);
    }

    #[test]
    fn add_reply_appends_one_line_to_replies_jsonl() {
        let (werk, dir) = test_werk();
        werk.add_task("hello");
        werk.add_reply("t-1", "first");
        werk.add_reply("t-1", "second");
        let body =
            std::fs::read_to_string(dir.path().join("tasks").join("t-1").join("replies.jsonl"))
                .unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let _: Reply = serde_json::from_str(lines[0]).unwrap();
        let _: Reply = serde_json::from_str(lines[1]).unwrap();
    }

    #[test]
    fn load_replays_replies_jsonl_into_in_memory_task() {
        use super::super::reply::ReplyContent;
        let dir = crate::test_util::TempDir::new().unwrap();
        {
            let werk = Werk::new();
            werk.set_dir(dir.path().to_path_buf());
            werk.add_task("hello");
            werk.add_reply("t-1", "first");
            werk.add_reply("t-1", "second");
        }
        let resumed = Werk::load(dir.path()).unwrap();
        let t = resumed.get_task("t-1").unwrap();
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
    fn task_lifecycle_writes_its_events_to_the_log() {
        let (werk, _dir) = test_werk();
        werk.add_task("seed");
        werk.claim(&Query::from("task.status = todo"), "alice")
            .unwrap();
        werk.set_result("t-1", serde_json::Value::Null).unwrap();
        werk.set_finished_by("t-1", "agent").unwrap();

        assert_eq!(werk.find_events("event.name = task_created").len(), 1);
        assert_eq!(werk.find_events("event.name = task_finished").len(), 1);
    }

    #[test]
    fn creating_a_task_names_the_reporter_on_its_event() {
        let (werk, _tmp) = test_werk();
        let reporters = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&reporters);
        werk.on_event(move |_, event| {
            if event.get_name() == Event::TASK_CREATED {
                seen.lock().unwrap().push(event.agent_id.clone());
            }
        });

        werk.add_task("seed");

        assert_eq!(werk.stats.event_count(Event::TASK_CREATED), 1);
        assert_eq!(*reporters.lock().unwrap(), vec!["user".to_string()]);
    }

    #[test]
    fn a_finished_task_failed_afterwards_is_not_counted_twice() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("seed");
        werk.set_finished_by(&id, "alice").unwrap();
        // Refused before the transition, so nothing is emitted to count.
        werk.set_task_failed(&id).unwrap();

        let stats = &werk.stats;
        assert_eq!(stats.event_count(Event::TASK_FINISHED), 1);
        assert_eq!(stats.event_count(Event::TASK_FAILED), 0);
    }

    #[test]
    fn task_with_json_schema_round_trips_through_load() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        let schema_doc = serde_json::json!({
            "type": "object",
            "properties": { "n": { "type": "integer" } },
            "required": ["n"],
        });
        let schema = crate::schemas::Schema::new(schema_doc.clone()).unwrap();
        original.add_task(Task::new("counted").schema(schema));
        drop(original);

        let resumed = Werk::load(dir.path()).unwrap();
        let t = resumed.get_task("t-1").unwrap();
        let restored = t.schema.expect("JSON schema must restore");
        assert!(restored.validate(serde_json::json!({"n": 3})).is_ok());
        assert!(restored.validate(serde_json::json!({})).is_err());
    }

    #[test]
    fn edit_replies_rewrites_replies_without_touching_task() {
        use crate::agents::tasks::ReplyContent;
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("original task");
        werk.append_reply(&id, Reply::user_text("keep me"));
        werk.append_reply(&id, Reply::user_text("drop me"));

        werk.edit_replies(&id, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
            });
        });

        let task = werk.get_task(&id).unwrap();
        assert_eq!(task.task, serde_json::Value::String("original task".into()));

        let reloaded = Werk::load(werk.get_dir()).unwrap();
        let replies = reloaded.get_task(&id).unwrap().replies;
        assert!(replies.iter().any(
            |r| matches!(r.content.first(), Some(ReplyContent::Text { text: t }) if t == "keep me")
        ));
        assert!(replies.iter().all(
            |r| !matches!(r.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
        ));
    }

    #[test]
    fn edit_replies_that_changes_nothing_writes_nothing() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("go");
        werk.append_reply(&id, Reply::user_text("keep me"));
        let task_dir = werk.get_dir().join("tasks").join(&id);

        werk.edit_replies(&id, |_replies| {}); // inspect, change nothing

        let rewrites = std::fs::read_dir(&task_dir)
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
        use crate::agents::tasks::ReplyContent;
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("go");
        werk.append_reply(&id, Reply::user_text("keep me"));
        werk.append_reply(&id, Reply::user_text("drop me"));
        let task_dir = werk.get_dir().join("tasks").join(&id);

        werk.edit_replies(&id, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
            });
        });

        // The edit changed the base file without creating a replies.<ts>.jsonl copy.
        let names: Vec<String> = std::fs::read_dir(&task_dir)
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
        let body = std::fs::read_to_string(task_dir.join("replies.jsonl")).unwrap();
        assert!(body.contains("keep me"));
        assert!(!body.contains("drop me"));
    }

    #[test]
    fn compaction_then_edit_leaves_one_replies_file_and_no_leak() {
        use crate::agents::tasks::ReplyContent;
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("go");
        werk.append_reply(&id, Reply::user_text("SECRET"));
        // What compaction applies: the replies wholesale, rewritten in place.
        werk.edit_replies(&id, |replies| *replies = vec![Reply::user_text("summary")]);
        werk.append_reply(&id, Reply::user_text("after"));
        werk.edit_replies(&id, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "after")
            });
        });

        // Rewriting replaces the one replies file, so removed content cannot survive in another copy.
        let task_dir = werk.get_dir().join("tasks").join(&id);
        let replies_files: Vec<String> = std::fs::read_dir(&task_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("replies."))
            .collect();
        assert_eq!(replies_files, vec!["replies.jsonl".to_string()]);
        let body = std::fs::read_to_string(task_dir.join("replies.jsonl")).unwrap();
        assert!(
            !body.contains("SECRET") && !body.contains("after"),
            "leaked: {body}"
        );
    }
}
