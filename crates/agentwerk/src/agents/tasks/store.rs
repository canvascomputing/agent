//! Every change a [`Queue`] makes to its tasks, and the events each
//! change emits.

use std::path::{Path, PathBuf};

use crate::event::EventKind;
use crate::persistence::Persist;
use crate::schemas::SchemaViolations;

use super::super::query::Query;
use super::error::TaskError;
use super::queue::Queue;
use super::reply::Reply;
use super::task::{Status, Task};
use super::{now_millis, numeric_id, Replies, TaskResult};

/// Highest `t-<N>` already on disk under `<dir>/tasks/`, or 0 if
/// none. Only needed for a queue built via `new()`, which never reads
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

impl Queue {
    /// Insert `task`, filling in the fields agentwerk owns. The task is always born
    /// `Todo`; to pin it to a specific agent, label it with the agent's
    /// name. Returns the inserted task's key.
    pub(crate) fn insert(&self, mut task: Task, reporter: String) -> String {
        let id = {
            let mut next = self.next_task_id.lock().unwrap();
            let base = next.get_or_insert_with(|| max_existing_task_id(&self.get_dir()));
            *base += 1;
            *base
        };
        task.key = format!("t-{id}");
        task.created_at = now_millis();
        task.reporter = reporter;
        task.result = None;
        task.status = Status::Todo;
        task.cancelled = self
            .cancel_filters
            .lock()
            .unwrap()
            .iter()
            .any(|query| query.matches(&task));
        let mut store = self.tasks.lock().unwrap();
        let key = task.key.clone();
        let reporter = task.reporter.clone();
        store.insert(key.clone(), task);
        drop(store);
        self.save_task(&key);
        self.emit(&key, &reporter, EventKind::TaskCreated);
        key
    }

    /// Write the task at `key` to disk. No-op when the task is missing.
    fn save_task(&self, key: &str) {
        if let Some(t) = self.get_task(key) {
            let _ = t.save(&self.get_dir());
        }
    }

    /// Write a tool's full output to `<dir>/tasks/<key>/outputs/<tool_use_id>.txt`.
    /// Returns the path relative to the configured `dir` on success,
    /// `None` when the write fails. The relative form keeps the
    /// replies portable across moves of the tasks dir; join with
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

    /// Atomically find a `Todo` task the query selects, assign it to
    /// `agent_id`, and transition to `InProgress`.
    ///
    /// The earliest candidate must itself be `Todo`, so a query naming no
    /// status never reaches past a task already claimed.
    pub(crate) fn claim(&self, query: &Query, agent_id: &str) -> Option<String> {
        let now = now_millis();
        let schemas = self.schemas.lock().unwrap().clone();
        let key = {
            let mut store = self.tasks.lock().unwrap();
            let mut candidates: Vec<&Task> = store.values().filter(|t| query.matches(t)).collect();
            query.sort(&mut candidates);
            let key = candidates.first()?.key.clone();
            let task = store.get_mut(&key)?;
            if task.status != Status::Todo {
                return None;
            }
            task.assignee = Some(agent_id.to_string());
            if task.schema.is_none() {
                let bound = schemas
                    .as_ref()
                    .zip(task.label.as_deref())
                    .and_then(|(s, label)| s.get(label));
                task.schema = bound;
            }
            task.stamp_transition(Status::InProgress, now);
            task.status = Status::InProgress;
            key
        };
        self.save_task(&key);
        // Emitted here rather than from the loop: the claim is the moment a
        // task starts, so a host claiming one records it the same way.
        self.emit(&key, agent_id, EventKind::TaskStarted);
        Some(key)
    }

    /// Append `reply` to the task's replies. No-op when the
    /// task is missing: the loop drops out shortly afterwards on the
    /// same condition. The task record is not rewritten; the replies
    /// live only in `replies.jsonl`.
    pub(crate) fn append_reply(&self, key: &str, reply: Reply) {
        {
            let mut store = self.tasks.lock().unwrap();
            let Some(t) = store.get_mut(key) else { return };
            t.replies.push(reply.clone());
        }
        let _ = Replies::append(&self.get_dir(), key, &reply);
    }

    /// Transition a task to `Finished`, emitting `TaskFinished`
    /// under `agent`'s name.
    pub(crate) fn set_finished_by(&self, key: &str, agent: &str) -> Result<(), TaskError> {
        self.set_final_status(key, Status::Finished, agent)
    }

    /// Attach `result` to the task and transition it to `Finished`,
    /// resolving it from outside the run. Validates against the task's
    /// schema first, so a host finish and an agent finish record the same
    /// contract. The emitted `TaskFinished` carries an empty agent id,
    /// like the run-level events no single agent causes.
    ///
    /// ```no_run
    /// # use agentwerk::Queue;
    /// # fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let tasks = Queue::new();
    /// let key = tasks.add_task("Look up the cached answer.");
    /// tasks.set_task_finished(&key, "42")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_task_finished(
        &self,
        key: &str,
        result: impl serde::Serialize,
    ) -> Result<(), TaskError> {
        let value = serde_json::to_value(result).expect("result is serializable");
        self.set_result(key, value)
            .map_err(|violations| TaskError::ResultRejected {
                message: violations.to_string(),
            })?;
        self.set_final_status(key, Status::Finished, "")
    }

    /// Transition a task to `Failed`. No result argument, unlike
    /// [`Self::set_task_finished`]: a failed task has none. The emitted
    /// `TaskFailed` carries an empty agent id, like the run-level
    /// events no single agent causes.
    pub fn set_task_failed(&self, key: &str) -> Result<(), TaskError> {
        self.set_final_status(key, Status::Failed, "")
    }

    /// Transition a task to `Failed`, emitting `TaskFailed` under
    /// `agent`'s name. The loop's failure paths route through this.
    pub(crate) fn set_failed_by(&self, key: &str, agent: &str) -> Result<(), TaskError> {
        self.set_final_status(key, Status::Failed, agent)
    }

    fn set_final_status(&self, key: &str, status: Status, agent: &str) -> Result<(), TaskError> {
        // Increment BEFORE the status flip and decrement only after the
        // terminal event has been emitted: the drain check in `finish_results()`
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
        let transitioned = {
            let mut store = self.tasks.lock().unwrap();
            let task = store.get_mut(key).ok_or_else(|| TaskError::TaskMissing {
                key: key.to_string(),
            })?;
            // First outcome wins. The host resolving a task an agent is still
            // turning, and the agent giving up on one the host just resolved,
            // are the same race from either side; without this the loser's
            // status overwrites the winner's and leaves, say, a `Failed` task
            // carrying a result. Checked under the lock the write happens
            // under, so two racing transitions cannot both pass it.
            match task.status {
                Status::Finished | Status::Failed => false,
                _ => {
                    task.stamp_transition(status, now);
                    task.status = status;
                    true
                }
            }
        };
        if !transitioned {
            return Ok(());
        }
        let kind = match status {
            Status::Finished => EventKind::TaskFinished,
            _ => EventKind::TaskFailed,
        };
        self.emit(key, agent, kind);
        self.save_task(key);
        Ok(())
    }

    /// Validate `result` against the task's schema, write it to the task's
    /// `result.json`, and store the validated result on the task, which it
    /// returns alongside the JSON pointer of every value validation repaired to
    /// accept it. Does not finish the task: the caller does.
    pub(crate) fn set_result(
        &self,
        key: &str,
        result: serde_json::Value,
    ) -> Result<(serde_json::Value, Vec<String>), SchemaViolations> {
        let schema = self
            .tasks
            .lock()
            .unwrap()
            .get(key)
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
            match store.get_mut(key) {
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
                key: key.to_string(),
                value: Some(result.clone()),
            };
            // Best-effort: the result is already attached in memory, so a
            // failed write is observational, not load-bearing.
            let _ = record.save(&self.get_dir());
        }
        Ok((result, repairs))
    }

    /// Apply `editor` to task `key`'s replies now, then rewrite them
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
    /// use agentwerk::Queue;
    /// use agentwerk::event::EventKind;
    /// use agentwerk::agents::tasks::{Reply, ReplyContent};
    ///
    /// let tasks = Queue::new();
    /// tasks.on_event(|queue, event| {
    ///     if !matches!(event.kind, EventKind::ToolCallFailed { .. }) {
    ///         return;
    ///     }
    ///     queue.edit_replies(&event.task_key, |replies| {
    ///         // Drop both sides of the failed exchange: the assistant's tool_use
    ///         // and the failed tool_result, so no unpaired block is left behind.
    ///         replies.retain(|reply| {
    ///             !reply.content.iter().any(|b| {
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
    pub fn edit_replies(&self, key: &str, editor: impl FnOnce(&mut Vec<Reply>)) -> &Self {
        let task_copy = {
            let mut store = self.tasks.lock().unwrap();
            let Some(task) = store.get_mut(key) else {
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
            key: key.to_string(),
            entries: task_copy.replies,
        };
        let _ = replies.save(&self.get_dir());
        self
    }

    /// Edit caller-settable fields. Each `Some` overwrites; `None`
    /// leaves the field untouched. A label can be replaced but not removed.
    pub(crate) fn edit(
        &self,
        key: &str,
        new_task: Option<serde_json::Value>,
        label: Option<String>,
    ) -> Result<(), TaskError> {
        let mut store = self.tasks.lock().unwrap();
        let task = store.get_mut(key).ok_or_else(|| TaskError::TaskMissing {
            key: key.to_string(),
        })?;
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
    use crate::event::EventName;
    use std::sync::{Arc, Mutex};

    #[test]
    fn task_creates_task_with_user_reporter() {
        let (queue, _tmp) = test_queue();
        queue.add_task("hello");
        let t = queue.get_task("t-1").unwrap();
        assert_eq!(t.task, serde_json::Value::String("hello".into()));
        assert_eq!(t.reporter, "user");
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn labeled_task_attaches_label_and_leaves_status_todo() {
        let (queue, _tmp) = test_queue();
        queue.add_task(Task::new("hello").label("research"));
        let t = queue.get_task("t-1").unwrap();
        assert_eq!(t.label.as_deref(), Some("research"));
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn create_with_named_label_is_born_todo_and_carries_label() {
        let (queue, _tmp) = test_queue();
        queue.add_task(Task::new("specific work for alice").label("alice"));
        let t = queue.get_task("t-1").unwrap();
        assert_eq!(t.label.as_deref(), Some("alice"));
        assert_eq!(t.status, Status::Todo);
    }

    #[test]
    fn create_with_label_and_schema_is_stored_verbatim() {
        let (queue, _tmp) = test_queue();
        let schema = crate::schemas::Schema::new(serde_json::json!({"type": "string"})).unwrap();
        queue.add_task(Task::new("x").label("urgent").schema(schema));
        let t = queue.get_task("t-1").unwrap();
        assert_eq!(t.label.as_deref(), Some("urgent"));
        assert!(t.schema.is_some());
    }

    #[test]
    fn set_result_updates_task() {
        let (queue, _tmp) = test_queue();
        queue.add_task("hi");
        queue
            .set_result("t-1", serde_json::Value::String("answer".into()))
            .unwrap();
        let stored = queue.get_task("t-1").unwrap();
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
        queue.add_task("ok");
        queue.add_task("oops");
        queue.add_task("pending");
        queue.claim(&Query::from("t-1"), "agent");
        queue.set_finished_by("t-1", "agent").unwrap();
        queue.set_task_failed("t-2").unwrap();
        let done = queue.find_tasks(|t: &Task| t.status == Status::Finished);
        let failed = queue.find_tasks(|t: &Task| t.status == Status::Failed);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].key, "t-1");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].key, "t-2");
    }

    #[test]
    fn task_status_transitions_record_stats() {
        let (queue, _tmp) = test_queue();
        queue.add_task("a");
        queue.add_task("b");
        queue.add_task("c");
        assert_eq!(queue.stats.event_count(EventName::TaskCreated), 3);
        queue.claim(&Query::from("t-1"), "agent");
        queue.set_finished_by("t-1", "agent").unwrap();
        queue.claim(&Query::from("t-2"), "agent");
        queue.set_task_failed("t-2").unwrap();
        assert_eq!(queue.stats.event_count(EventName::TaskFinished), 1);
        assert_eq!(queue.stats.event_count(EventName::TaskFailed), 1);
    }

    #[test]
    fn a_task_logs_created_started_and_finished_in_order() {
        let (queue, dir) = test_queue();
        queue.add_task("hello");
        queue.claim(&Query::from("t-1"), "agent");
        queue.set_finished_by("t-1", "agent").unwrap();
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 3);
        let names: Vec<&str> = lines.iter().map(|l| l["event"].as_str().unwrap()).collect();
        assert_eq!(names, ["task_created", "task_started", "task_finished"]);
        for line in &lines {
            assert_eq!(line["task_key"], "t-1");
            assert!(line["created_at"].is_u64());
        }
    }

    #[test]
    fn streamed_chunks_stay_out_of_the_log() {
        let (queue, dir) = test_queue();
        queue.add_task("seed");
        queue.emit(
            "t-1",
            "agent",
            EventKind::TextChunkReceived {
                content: "a piece of the reply".into(),
            },
        );
        // One line per token would outweigh every other line, and the replies
        // already hold the text.
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["event"], "task_created");
    }

    #[test]
    fn load_replays_the_token_totals_a_run_already_spent() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Queue::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("seed");
        original.emit(
            "t-1",
            "agent",
            EventKind::RequestFinished {
                model: "m".into(),
                usage: crate::providers::TokenUsage {
                    input_tokens: 900,
                    output_tokens: 120,
                },
            },
        );
        drop(original);

        // The token limits divide against these, so a resumed run that read
        // them back as zero would silently start its budget over.
        let resumed = Queue::load(dir.path()).unwrap();
        assert_eq!(resumed.stats.input_tokens(), 900);
        assert_eq!(resumed.stats.output_tokens(), 120);
    }

    #[test]
    fn set_failed_logs_a_failure_without_a_start() {
        let (queue, dir) = test_queue();
        queue.add_task("hello");
        queue.set_task_failed("t-1").unwrap();
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "task_created");
        assert_eq!(lines[1]["event"], "task_failed");
        assert_eq!(lines[1]["task_key"], "t-1");
    }

    #[test]
    fn a_logged_event_carries_the_task_label_when_pinned() {
        let (queue, dir) = test_queue();
        queue.add_task(Task::new("specific").label("alice"));
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["event"], "task_created");
        assert_eq!(lines[0]["label"], "alice");
    }

    #[test]
    fn the_log_holds_one_line_per_lifecycle_turn_across_tasks() {
        let (queue, dir) = test_queue();
        queue.add_task("a");
        queue.add_task("b");
        queue.claim(&Query::from("t-1"), "agent");
        queue.set_finished_by("t-1", "agent").unwrap();
        queue.claim(&Query::from("t-2"), "agent");
        queue.set_task_failed("t-2").unwrap();
        // 2 created + 2 started + 1 finished + 1 failed
        assert_eq!(read_events_log(dir.path()).len(), 6);
    }

    #[test]
    fn claim_transitions_todo_to_in_progress_and_sets_the_assignee() {
        let (queue, _tmp) = test_queue();
        queue.add_task("hello");
        let key = queue.claim(&Query::from("status = Todo"), "alice").unwrap();
        assert_eq!(key, "t-1");
        let t = queue.get_task(&key).unwrap();
        assert_eq!(t.status, Status::InProgress);
        assert_eq!(t.assignee.as_deref(), Some("alice"));
        assert!(t.started_at.is_some());
    }

    #[test]
    fn claim_leaves_the_label_the_task_was_filed_with() {
        let (queue, _tmp) = test_queue();
        queue.add_task(Task::new("hello").label("analysis"));
        let key = queue.claim(&Query::from("analysis"), "alice").unwrap();
        assert_eq!(
            queue.get_task(&key).unwrap().label.as_deref(),
            Some("analysis")
        );
    }

    #[test]
    fn claim_returns_none_when_no_task_matches() {
        let (queue, _tmp) = test_queue();
        queue.add_task("hello");
        assert!(queue.claim(&Query::from("nonexistent"), "alice").is_none());
    }

    #[test]
    fn second_claim_of_same_task_returns_none() {
        let (queue, _tmp) = test_queue();
        queue.add_task("hello");
        let first = queue.claim(&Query::from("t-1"), "alice");
        assert!(first.is_some());
        // Second claim: task is now InProgress, not Todo.
        let second = queue.claim(&Query::from("t-1"), "bob");
        assert!(second.is_none());
    }

    #[test]
    fn claim_picks_earliest_eligible_task() {
        let (queue, _tmp) = test_queue();
        queue.add_task("a");
        queue.add_task("b");
        queue.add_task("c");
        let key = queue.claim(&Query::from("status = Todo"), "alice").unwrap();
        assert_eq!(key, "t-1");
    }

    fn queue_with_analysis_schema() -> (std::sync::Arc<Queue>, crate::test_util::TempDir) {
        let (queue, dir) = test_queue();
        let schemas = crate::schemas::SchemaStore::new();
        schemas.label("analysis", document("verdict")).unwrap();
        queue.set_schemas(&schemas);
        (queue, dir)
    }

    /// A schema document the assertions can tell apart by its `title`.
    fn document(title: &str) -> serde_json::Value {
        serde_json::json!({ "type": "object", "title": title })
    }

    /// The `title` of the schema bound to a task, naming which one it took.
    fn bound_title(queue: &Queue, key: &str) -> Option<String> {
        let schema = queue.get_task(key)?.schema?;
        let document = serde_json::to_value(schema).ok()?;
        Some(document["title"].as_str().unwrap_or_default().to_string())
    }

    #[test]
    fn claim_takes_the_schema_bound_to_the_tasks_label() {
        let (queue, _tmp) = queue_with_analysis_schema();
        queue.add_task(Task::new("audit").label("analysis"));
        assert_eq!(bound_title(&queue, "t-1"), None);

        queue.claim(&Query::from("analysis"), "alice");
        assert_eq!(bound_title(&queue, "t-1").as_deref(), Some("verdict"));
    }

    #[test]
    fn claim_leaves_a_schema_the_task_already_carries() {
        let (queue, _tmp) = queue_with_analysis_schema();
        let own = crate::schemas::Schema::new(document("its own")).unwrap();
        queue.add_task(Task::new("audit").label("analysis").schema(own));
        assert_eq!(bound_title(&queue, "t-1").as_deref(), Some("its own"));

        queue.claim(&Query::from("analysis"), "alice");
        assert_eq!(bound_title(&queue, "t-1").as_deref(), Some("its own"));
    }

    #[test]
    fn claim_prefers_the_label_the_task_was_filed_under_to_the_agent_id() {
        let (queue, _tmp) = test_queue();
        let schemas = crate::schemas::SchemaStore::new();
        schemas.label("analysis", document("by scope")).unwrap();
        schemas.label("alice", document("by agent")).unwrap();
        queue.set_schemas(&schemas);
        queue.add_task(Task::new("audit").label("analysis"));
        assert_eq!(bound_title(&queue, "t-1"), None);

        queue.claim(&Query::from("analysis"), "alice");
        assert_eq!(bound_title(&queue, "t-1").as_deref(), Some("by scope"));
    }

    #[test]
    fn claim_binds_no_schema_when_the_tasks_label_has_none() {
        let (queue, _tmp) = queue_with_analysis_schema();
        queue.add_task(Task::new("search").label("discovery"));
        assert_eq!(bound_title(&queue, "t-1"), None);

        queue.claim(&Query::from("discovery"), "alice");
        assert_eq!(bound_title(&queue, "t-1"), None);
    }

    #[test]
    fn a_task_already_in_progress_keeps_the_schema_its_claim_bound() {
        let (queue, _tmp) = test_queue();
        let schemas = crate::schemas::SchemaStore::new();
        schemas.label("analysis", document("first")).unwrap();
        queue.set_schemas(&schemas);
        queue.add_task(Task::new("audit").label("analysis"));
        queue.claim(&Query::from("analysis"), "alice");
        assert_eq!(bound_title(&queue, "t-1").as_deref(), Some("first"));

        schemas.label("analysis", document("second")).unwrap();

        // The loop resumes an `InProgress` task through `find_task`, never
        // through a second `claim`, so a later binding cannot reach it.
        assert!(queue.claim(&Query::from("analysis"), "alice").is_none());
        assert_eq!(bound_title(&queue, "t-1").as_deref(), Some("first"));
    }

    #[test]
    fn a_schema_bound_at_claim_survives_load() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Queue::new();
        original.set_dir(dir.path().to_path_buf());
        let schemas = crate::schemas::SchemaStore::new();
        schemas.label("analysis", document("verdict")).unwrap();
        original.set_schemas(&schemas);
        original.add_task(Task::new("audit").label("analysis"));
        original.claim(&Query::from("analysis"), "alice");
        drop(original);

        let resumed = Queue::load(dir.path()).unwrap();
        assert_eq!(bound_title(&resumed, "t-1").as_deref(), Some("verdict"));
    }

    #[test]
    fn claim_logs_the_task_starting() {
        let (queue, dir) = test_queue();
        queue.add_task("hello");
        queue.claim(&Query::from("status = Todo"), "alice");
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "task_created");
        assert_eq!(lines[1]["event"], "task_started");
        assert_eq!(lines[1]["task_key"], "t-1");
        assert_eq!(lines[1]["agent_id"], "alice");
    }

    #[test]
    fn set_finished_transitions_to_finished() {
        let (queue, _tmp) = test_queue();
        let key = queue.add_task("hello");
        queue.claim(&Query::from("status = Todo"), "alice");
        queue.set_finished_by(&key, "alice").unwrap();
        let t = queue.get_task(&key).unwrap();
        assert_eq!(t.status, Status::Finished);
        assert!(t.finished_at.is_some());
    }

    #[test]
    fn set_failed_transitions_to_failed() {
        let (queue, _tmp) = test_queue();
        let key = queue.add_task("hello");
        queue.claim(&Query::from("status = Todo"), "alice");
        queue.set_task_failed(&key).unwrap();
        let t = queue.get_task(&key).unwrap();
        assert_eq!(t.status, Status::Failed);
        assert!(t.failed_at.is_some());
    }

    #[test]
    fn a_finished_task_is_not_reopened_by_a_later_failure() {
        let (queue, _tmp) = test_queue();
        let key = queue.add_task("hello");
        queue.claim(&Query::from("status = Todo"), "alice");
        queue.set_task_finished(&key, "host result").unwrap();

        // Alice was still turning the task and gives up after the host
        // resolved it.
        queue.set_failed_by(&key, "alice").unwrap();

        let task = queue.get_task(&key).unwrap();
        assert_eq!(task.status, Status::Finished);
        assert_eq!(task.result, Some(serde_json::json!("host result")));
        assert!(task.failed_at.is_none());
    }

    #[test]
    fn a_failed_task_is_not_reopened_by_a_later_finish() {
        let (queue, _tmp) = test_queue();
        let key = queue.add_task("hello");
        queue.claim(&Query::from("status = Todo"), "alice");
        queue.set_failed_by(&key, "alice").unwrap();

        queue.set_task_finished(&key, "late result").unwrap();

        let task = queue.get_task(&key).unwrap();
        assert_eq!(task.status, Status::Failed);
        assert!(task.finished_at.is_none());
        // The result too, or the task reads as failed while carrying an answer.
        assert_eq!(task.result, None);
    }

    #[test]
    fn set_finished_stores_the_result_and_resolves_the_task() {
        let (queue, _tmp) = test_queue();
        queue.add_task("hello");
        queue
            .set_task_finished("t-1", serde_json::json!({"answer": 42}))
            .unwrap();
        let t = queue.get_task("t-1").unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result, Some(serde_json::json!({"answer": 42})));
        assert_eq!(
            queue.get_results().pop(),
            Some(serde_json::json!({"answer": 42}))
        );
    }

    #[test]
    fn set_finished_rejects_a_result_that_misses_the_task_schema() {
        let (queue, _tmp) = test_queue();
        let schema = crate::schemas::Schema::new(serde_json::json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
        }))
        .unwrap();
        queue.add_task(Task::new("write a report").schema(schema));

        let err = queue
            .set_task_finished("t-1", serde_json::json!({"body": "no title"}))
            .unwrap_err();
        assert!(matches!(err, TaskError::ResultRejected { .. }));
        assert_eq!(queue.get_task("t-1").unwrap().status, Status::Todo);
    }

    #[test]
    fn set_finished_errors_on_an_unknown_key() {
        let (queue, _tmp) = test_queue();
        let err = queue.set_task_finished("t-9", "done").unwrap_err();
        assert!(matches!(err, TaskError::TaskMissing { .. }));
    }

    #[test]
    fn task_parent_builder_round_trips() {
        let (queue, _tmp) = test_queue();
        queue.add_task(Task::new("child body").parent("t-1"));
        let stored = queue.get_task("t-1").unwrap();
        assert_eq!(stored.parent.as_deref(), Some("t-1"));
    }

    #[test]
    fn write_tool_output_returns_relative_path_and_writes_absolute() {
        let (queue, dir) = test_queue();
        queue.add_task("seed");
        let rel = queue
            .write_tool_output("t-1", "call-1", "the full content")
            .expect("write succeeds when dir exists");
        let expected_rel: PathBuf = ["tasks", "t-1", "outputs", "call-1.txt"].iter().collect();
        assert_eq!(rel, expected_rel);
        let body = std::fs::read_to_string(dir.path().join(&rel)).unwrap();
        assert_eq!(body, "the full content");
    }

    #[test]
    fn write_tool_output_creates_outputs_subdir_lazily() {
        let (queue, dir) = test_queue();
        queue.add_task("seed");
        let outputs = dir.path().join("tasks").join("t-1").join("outputs");
        assert!(!outputs.exists());
        queue.write_tool_output("t-1", "call-1", "payload").unwrap();
        assert!(outputs.is_dir());
    }

    #[test]
    fn a_logged_event_names_the_agent_that_caused_it() {
        let (queue, dir) = test_queue();
        queue.add_task("first");
        queue.add_task(Task::new("child").parent("t-1"));
        let lines = read_events_log(dir.path());
        assert_eq!(lines.len(), 2);
        // The reporter, since a task is created by whoever filed it.
        assert_eq!(lines[0]["agent_id"], "user");
        assert_eq!(lines[1]["agent_id"], "user");
    }

    // Resumption: Queue::load

    #[test]
    fn load_creates_tasks_dir_when_missing() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::load(dir.path()).unwrap();
        assert!(queue.get_tasks().is_empty());
        assert!(dir.path().join("tasks").is_dir());
    }

    /// A replies file written by an older version no longer parses. The
    /// task must not be skipped: its status and result would vanish with
    /// it, and the caller would receive a short store reporting success.
    #[test]
    fn load_reports_a_task_whose_replies_cannot_be_read() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Queue::new();
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

        let Err(error) = Queue::load(dir.path()) else {
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
        let original = Queue::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("seed work");
        original
            .set_result("t-1", serde_json::json!({"ok": true}))
            .unwrap();
        original.set_finished_by("t-1", "agent").unwrap();
        drop(original);

        let resumed = Queue::load(dir.path()).unwrap();
        let t = resumed.get_task("t-1").unwrap();
        assert_eq!(t.status, Status::Finished);
        assert_eq!(t.result.as_ref(), Some(&serde_json::json!({"ok": true})));
        assert_eq!(t.task, serde_json::Value::String("seed work".into()));
    }

    #[test]
    fn insert_after_load_never_reuses_an_existing_key() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Queue::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("seed work");
        drop(original);

        let resumed = Queue::load(dir.path()).unwrap();
        assert_eq!(resumed.add_task("more work"), "t-2");
    }

    #[test]
    fn insert_after_new_plus_dir_never_reuses_an_existing_key() {
        // The pattern a fresh process actually uses against a directory a
        // prior run already wrote into: `new()` (not `load()`) plus `.dir(..)`.
        let dir = crate::test_util::TempDir::new().unwrap();
        let first = Queue::new();
        first.set_dir(dir.path().to_path_buf());
        first.add_task("seed work");
        drop(first);

        let second = Queue::new();
        second.set_dir(dir.path().to_path_buf());
        assert_eq!(second.add_task("more work"), "t-2");
    }

    #[test]
    fn load_seeds_next_task_id_without_rescanning_tasks_dir() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Queue::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("seed work");
        drop(original);

        let resumed = Queue::load(dir.path()).unwrap();
        // `load()` already knows the highest key from what it just read
        // into memory; removing the directory here proves `insert()` does
        // not rescan it, since a rescan would find nothing and wrongly
        // restart numbering at 1.
        std::fs::remove_dir_all(dir.path().join("tasks")).unwrap();
        assert_eq!(resumed.add_task("more work"), "t-2");
    }

    #[test]
    fn load_restores_in_progress_replies() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Queue::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("mid flight");
        original
            .claim(&Query::from("status = Todo"), "alice")
            .unwrap();
        drop(original);

        let resumed = Queue::load(dir.path()).unwrap();
        let t = resumed.get_task("t-1").unwrap();
        assert_eq!(t.status, Status::InProgress);
        assert_eq!(t.assignee.as_deref(), Some("alice"));
    }

    #[test]
    fn load_replays_the_event_log_into_the_counters() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Queue::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("a");
        original.add_task("b");
        original.set_result("t-1", serde_json::Value::Null).unwrap();
        original.set_finished_by("t-1", "agent").unwrap();
        original.set_task_failed("t-2").unwrap();
        drop(original);

        let resumed = Queue::load(dir.path()).unwrap();
        assert_eq!(resumed.stats.event_count(EventName::TaskCreated), 2);
        assert_eq!(resumed.stats.event_count(EventName::TaskFinished), 1);
        assert_eq!(resumed.stats.event_count(EventName::TaskFailed), 1);
    }
    #[test]
    fn load_skips_dir_without_task_json() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Queue::new();
        original.set_dir(dir.path().to_path_buf());
        original.add_task("valid");
        drop(original);

        // A leftover directory with no `task.json` is ignored by the
        // loader: there is no migration from the pre-split layout.
        let stray_dir = dir.path().join("tasks").join("t-99");
        std::fs::create_dir_all(&stray_dir).unwrap();
        std::fs::write(stray_dir.join("anything.json"), "not json").unwrap();

        let resumed = Queue::load(dir.path()).unwrap();
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
            "key": "t-1",
            "status": "Todo",
            "reporter": "user",
            "created_at": 1,
            "started_at": null,
            "finished_at": null,
            "failed_at": null,
            "result": null,
            "parent": null,
        });
        std::fs::write(
            task_dir.join("task.json"),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();

        let resumed = Queue::load(dir.path()).unwrap();
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
        let Err(error) = Queue::load(dir.path()) else {
            panic!("load must fail on a malformed task.json");
        };
        assert!(
            error.to_string().contains("t-7"),
            "the failure must name the task: {error}",
        );
    }

    #[test]
    fn task_json_does_not_carry_replies_field() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = Queue::new();
        queue.set_dir(dir.path().to_path_buf());
        queue.add_task("hello");
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
        let queue = Queue::new();
        queue.set_dir(dir.path().to_path_buf());
        let key = queue.add_task(Task::new("hello").label("scan"));
        queue.cancel_tasks("label = scan");
        queue.set_task_failed(&key).unwrap();

        let stored =
            std::fs::read_to_string(dir.path().join("tasks").join(&key).join("task.json")).unwrap();
        let record: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert!(record.get("cancelled").is_none());

        let resumed = Queue::load(dir.path()).unwrap();
        assert!(resumed.find_tasks("cancelled = true").is_empty());
        assert_eq!(resumed.find_tasks("cancelled = false").len(), 1);
    }

    #[test]
    fn add_reply_appends_one_line_to_replies_jsonl() {
        let (queue, dir) = test_queue();
        queue.add_task("hello");
        queue.add_reply("t-1", "first");
        queue.add_reply("t-1", "second");
        let body =
            std::fs::read_to_string(dir.path().join("tasks").join("t-1").join("replies.jsonl"))
                .unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        // Each line is a single, parseable JSON object.
        let _: Reply = serde_json::from_str(lines[0]).unwrap();
        let _: Reply = serde_json::from_str(lines[1]).unwrap();
    }

    #[test]
    fn load_replays_replies_jsonl_into_in_memory_task() {
        use super::super::reply::ReplyContent;
        let dir = crate::test_util::TempDir::new().unwrap();
        {
            let queue = Queue::new();
            queue.set_dir(dir.path().to_path_buf());
            queue.add_task("hello");
            queue.add_reply("t-1", "first");
            queue.add_reply("t-1", "second");
        }
        let resumed = Queue::load(dir.path()).unwrap();
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
        let (queue, _dir) = test_queue();
        queue.add_task("seed");
        queue.claim(&Query::from("status = Todo"), "alice").unwrap();
        queue.set_result("t-1", serde_json::Value::Null).unwrap();
        queue.set_finished_by("t-1", "agent").unwrap();

        assert_eq!(queue.find_events("task_created").len(), 1);
        assert_eq!(queue.find_events("task_finished").len(), 1);
    }

    #[test]
    fn creating_a_task_names_the_reporter_on_its_event() {
        let (queue, _tmp) = test_queue();
        let reporters = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&reporters);
        queue.on_event(move |_, event| {
            if matches!(event.kind, EventKind::TaskCreated) {
                seen.lock().unwrap().push(event.agent_id.clone());
            }
        });

        queue.add_task("seed");

        assert_eq!(queue.stats.event_count(EventName::TaskCreated), 1);
        assert_eq!(*reporters.lock().unwrap(), vec!["user".to_string()]);
    }

    #[test]
    fn a_finished_task_failed_afterwards_is_not_counted_twice() {
        let (queue, _tmp) = test_queue();
        let key = queue.add_task("seed");
        queue.set_finished_by(&key, "alice").unwrap();
        // Refused before the transition, so nothing is emitted to count.
        queue.set_task_failed(&key).unwrap();

        let stats = &queue.stats;
        assert_eq!(stats.event_count(EventName::TaskFinished), 1);
        assert_eq!(stats.event_count(EventName::TaskFailed), 0);
    }

    #[test]
    fn task_with_json_schema_round_trips_through_load() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Queue::new();
        original.set_dir(dir.path().to_path_buf());
        let schema_doc = serde_json::json!({
            "type": "object",
            "properties": { "n": { "type": "integer" } },
            "required": ["n"],
        });
        let schema = crate::schemas::Schema::new(schema_doc.clone()).unwrap();
        original.add_task(Task::new("counted").schema(schema));
        drop(original);

        let resumed = Queue::load(dir.path()).unwrap();
        let t = resumed.get_task("t-1").unwrap();
        let restored = t.schema.expect("JSON schema must restore");
        assert!(restored.validate(serde_json::json!({"n": 3})).is_ok());
        assert!(restored.validate(serde_json::json!({})).is_err());
    }

    #[test]
    fn edit_replies_rewrites_replies_without_touching_task() {
        use crate::agents::tasks::ReplyContent;
        let (queue, _tmp) = test_queue();
        let key = queue.add_task("original task");
        queue.append_reply(&key, Reply::user_text("keep me"));
        queue.append_reply(&key, Reply::user_text("drop me"));

        queue.edit_replies(&key, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
            });
        });

        // Unlike summarize, the task is left as it was.
        let task = queue.get_task(&key).unwrap();
        assert_eq!(task.task, serde_json::Value::String("original task".into()));

        // The drop is committed and reloads from disk.
        let reloaded = Queue::load(queue.get_dir()).unwrap();
        let replies = reloaded.get_task(&key).unwrap().replies;
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
        let key = queue.add_task("go");
        queue.append_reply(&key, Reply::user_text("keep me"));
        let task_dir = queue.get_dir().join("tasks").join(&key);

        queue.edit_replies(&key, |_replies| {}); // inspect, change nothing

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
        let (queue, _tmp) = test_queue();
        let key = queue.add_task("go");
        queue.append_reply(&key, Reply::user_text("keep me"));
        queue.append_reply(&key, Reply::user_text("drop me"));
        let task_dir = queue.get_dir().join("tasks").join(&key);

        queue.edit_replies(&key, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
            });
        });

        // The edit landed in the base file, and minted no replies.<ts>.jsonl.
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
        let (queue, _tmp) = test_queue();
        let key = queue.add_task("go");
        queue.append_reply(&key, Reply::user_text("SECRET"));
        // What compaction applies: the replies wholesale, rewritten in place.
        queue.edit_replies(&key, |replies| *replies = vec![Reply::user_text("summary")]);
        queue.append_reply(&key, Reply::user_text("after"));
        queue.edit_replies(&key, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "after")
            });
        });

        // One replies file, no snapshots, and neither dropped string survives.
        let task_dir = queue.get_dir().join("tasks").join(&key);
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
