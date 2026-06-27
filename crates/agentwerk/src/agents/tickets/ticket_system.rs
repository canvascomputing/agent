//! The [`TicketSystem`] orchestrator: owns the shared ticket store,
//! registered agents, policies, cancellation signals, and run stats.
//! This file holds construction, configuration, the ticket-creation
//! API, agent binding, the background-run lifecycle, and queries.
//! Mutation impls (`claim`, `set_finished`, `summarize`, etc.) live
//! next door in `store.rs`.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::Serialize;
use tokio::task::JoinHandle;

use crate::event::{default_logger, Event, EventKind, FinishReason};
use crate::persistence::Persist;

use super::super::agent::{Agent, TicketSystemRef};
use super::super::policy::Policies;
use super::super::r#loop::run_main_loop;
use super::super::stats::Stats;
use super::ticket::{Status, Ticket};
use super::{now_millis, numeric_id, policy_violated_kind, Reply};

type EventHandler = dyn Fn(Event) + Send + Sync;

/// The shared work queue. Owns the ticket store, the registered
/// agents, the policies, and the run statistics. Many agents share one
/// `TicketSystem` and pick up tickets concurrently; labels and names
/// assign work to the right agent.
///
/// ```no_run
/// use agentwerk::{Agent, TicketSystem};
/// use agentwerk::tools::FetchUrlTool;
///
/// # async fn run() {
/// let tickets = TicketSystem::new();
/// for i in 0..4 {
///     tickets.agent(
///         Agent::new()
///             .name(format!("researcher_{i}"))
///             .label("research")
///             .from_env()
///             .tool(FetchUrlTool)
///             .build(),
///     );
/// }
/// tickets.task_labeled("Summarize https://canvascomputing.org", "research");
/// tickets.finish().await;
/// # }
/// ```
///
/// # Sessions
///
/// A `TicketSystem` writes every ticket, transcript, statistic, and
/// lifecycle event to its working directory (default `./.agentwerk`).
/// That directory is the session: stop the process, and `TicketSystem::load(dir)`
/// reopens it from disk and continues from where it stopped.
///
/// ```no_run
/// use agentwerk::TicketSystem;
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let tickets = TicketSystem::load(".agentwerk")?;
/// // Re-register the agents, then call .start() or .finish().await.
/// # let _ = tickets;
/// # Ok(())
/// # }
/// ```
///
/// On-disk layout:
///
/// ```text
/// .agentwerk/
/// ├── stats.json                            run statistics
/// ├── tickets.jsonl                         lifecycle events (one per line)
/// ├── results.jsonl                         finished results (one per line)
/// ├── tickets/
/// │   └── TICKET-1/
/// │       ├── ticket.json                   the ticket without its transcript
/// │       ├── ticket.<ts>.json              the ticket saved at each compaction; the timestamp matches replies.<ts>.jsonl
/// │       ├── replies.jsonl                 pre-compaction transcript
/// │       ├── replies.<ts>.jsonl            post-compaction transcript
/// │       └── outputs/<tool_use_id>.txt     full tool outputs spilled out of the transcript
/// ├── pages/<slug>.md                       knowledge pages
/// └── index.md                              knowledge index
/// ```
pub struct TicketSystem {
    pub(super) weak_self: Weak<TicketSystem>,
    pub(crate) tickets: Mutex<HashMap<String, Ticket>>,
    pub(super) agents: Mutex<Vec<Agent>>,
    pub(super) policies: Mutex<Policies>,
    /// Set when `finish()` should stop polling: external cancel, policy
    /// trip, or clean drain. Workers and tools also poll this; flipping
    /// it walks them off the queue.
    pub(crate) stop_signal: Mutex<Arc<AtomicBool>>,
    /// Set only by `cancel()`, `cancel_on`, and `cancel_on_event`. Read
    /// by `is_cancelled()` so observers can tell external cancel apart
    /// from policy stops and clean drains.
    pub(crate) cancel_signal: Mutex<Arc<AtomicBool>>,
    /// Reason the most recent `finish()` returned. `None` before the
    /// first `finish()` and between `start()` and the next `finish()`.
    pub(crate) finish_reason: Mutex<Option<FinishReason>>,
    pub(crate) stats: Stats,
    pub(super) event_handlers: Mutex<Vec<Arc<EventHandler>>>,
    pub(super) dir: Mutex<PathBuf>,
    pub(super) tickets_log_lock: Mutex<()>,
    pub(super) join_handle: Mutex<Option<JoinHandle<()>>>,
}

impl TicketSystem {
    /// Build a fresh `TicketSystem` and return it inside an `Arc`. The
    /// system captures its own `Weak<Self>` via `Arc::new_cyclic` so
    /// `bind_agent` can hand out the back-reference each `Agent` needs
    /// at run time.
    pub fn new() -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            weak_self: weak.clone(),
            tickets: Mutex::new(HashMap::new()),
            agents: Mutex::new(Vec::new()),
            policies: Mutex::new(Policies::default()),
            stop_signal: Mutex::new(Arc::new(AtomicBool::new(false))),
            cancel_signal: Mutex::new(Arc::new(AtomicBool::new(false))),
            finish_reason: Mutex::new(None),
            stats: Stats::new(),
            event_handlers: Mutex::new(Vec::new()),
            dir: Mutex::new(PathBuf::from(".agentwerk")),
            tickets_log_lock: Mutex::new(()),
            join_handle: Mutex::new(None),
        })
    }

    /// Open or create a ticket system rooted at `tickets_dir`. Loads
    /// the newest `ticket.<ts>.json` per key under `<tickets_dir>/tickets/`
    /// into the in-memory store and seeds `Stats` from
    /// `<tickets_dir>/stats.json` (or, when that file is missing or
    /// malformed, by deriving from the loaded tickets) so success rate
    /// and counters stay continuous across restarts.
    ///
    /// Pointing this and `Knowledge::load` at the same dir co-locates
    /// knowledge pages with `results.jsonl` and `tickets.jsonl`.
    ///
    /// `InProgress` tickets keep their status and their transcript; the
    /// loop's resume path (`agents/loop.rs`) picks them back up under
    /// the agent whose name is already in the ticket's `labels`.
    ///
    /// Caller contracts:
    /// - Tickets dirs deleted by hand break the `TICKET-N` counter:
    ///   the next inserted ticket may collide with an existing key.
    /// - Agent names must stay stable across restarts; agentwerk
    ///   matches `InProgress` tickets by name via the ticket's labels.
    pub fn load(tickets_dir: impl Into<PathBuf>) -> io::Result<Arc<Self>> {
        let tickets_dir = tickets_dir.into();
        std::fs::create_dir_all(tickets_dir.join("tickets"))?;

        let mut tickets = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(tickets_dir.join("tickets")) {
            for entry in entries.flatten() {
                let key_dir = entry.path();
                if !key_dir.is_dir() || !key_dir.join("ticket.json").is_file() {
                    continue;
                }
                let Some(key) = key_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                let Ok(ticket) = Ticket::load(&tickets_dir, &key) else {
                    continue;
                };
                tickets.insert(ticket.key.clone(), ticket);
            }
        }

        let stats = Stats::load(&tickets_dir).unwrap_or_else(|_| Stats::derive(&tickets));

        Ok(Arc::new_cyclic(|weak| Self {
            weak_self: weak.clone(),
            tickets: Mutex::new(tickets),
            agents: Mutex::new(Vec::new()),
            policies: Mutex::new(Policies::default()),
            stop_signal: Mutex::new(Arc::new(AtomicBool::new(false))),
            cancel_signal: Mutex::new(Arc::new(AtomicBool::new(false))),
            finish_reason: Mutex::new(None),
            stats,
            event_handlers: Mutex::new(Vec::new()),
            dir: Mutex::new(tickets_dir),
            tickets_log_lock: Mutex::new(()),
            join_handle: Mutex::new(None),
        }))
    }

    /// Run-time counters. Read after `run` / `finish` returns.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Push an event observer onto the handler chain. Every installed
    /// handler fires on every event, in installation order. Handlers
    /// must be cheap and non-blocking. When no handler has been
    /// installed, [`default_logger`] runs in its place.
    pub fn on_event(&self, h: impl Fn(Event) + Send + Sync + 'static) -> &Self {
        self.event_handlers.lock().unwrap().push(Arc::new(h));
        self
    }

    pub(crate) fn emit(&self, key: &str, agent: &str, kind: EventKind) {
        let labels = self.labels_for(key);
        match &kind {
            EventKind::TurnStarted => self.stats.record_turn_for(&labels),
            EventKind::ToolCallsRecorded { count } => {
                (0..*count).for_each(|_| self.stats.record_tool_call_for(&labels))
            }
            EventKind::RequestFinished { usage, .. } => {
                self.stats
                    .record_request_for(&labels, usage.input_tokens, usage.output_tokens);
                self.stats.record_usage(key, usage.clone());
            }
            EventKind::RequestFailed { .. } => self.stats.record_error_for(&labels),
            EventKind::ToolCallStarted { tool_name, .. } => {
                self.stats.record_tool_call_named(tool_name)
            }
            EventKind::ToolCallFailed {
                tool_name, kind, ..
            } => self.stats.record_tool_error_named(tool_name, *kind),
            _ => {}
        }
        let handlers: Vec<Arc<EventHandler>> = self.event_handlers.lock().unwrap().clone();
        let event = Event::new(agent, kind);
        if handlers.is_empty() {
            default_logger()(event);
            return;
        }
        for h in &handlers {
            h(event.clone());
        }
    }

    fn labels_for(&self, key: &str) -> Vec<String> {
        self.tickets
            .lock()
            .unwrap()
            .get(key)
            .map(|t| t.labels.clone())
            .unwrap_or_default()
    }

    pub(crate) fn policies(&self) -> Policies {
        self.policies.lock().unwrap().clone()
    }

    // ---- policy builders ----

    pub fn max_turns(&self, n: u32) -> &Self {
        self.policies.lock().unwrap().max_turns = Some(n);
        self
    }

    pub fn max_input_tokens(&self, n: u64) -> &Self {
        self.policies.lock().unwrap().max_input_tokens = Some(n);
        self
    }

    pub fn max_output_tokens(&self, n: u64) -> &Self {
        self.policies.lock().unwrap().max_output_tokens = Some(n);
        self
    }

    pub fn max_request_tokens(&self, n: u32) -> &Self {
        self.policies.lock().unwrap().max_request_tokens = Some(n);
        self
    }

    pub fn max_schema_retries(&self, n: u32) -> &Self {
        self.policies.lock().unwrap().max_schema_retries = Some(n);
        self
    }

    pub fn max_request_retries(&self, n: u32) -> &Self {
        self.policies.lock().unwrap().max_request_retries = n;
        self
    }

    pub fn request_retry_delay(&self, d: Duration) -> &Self {
        self.policies.lock().unwrap().request_retry_delay = d;
        self
    }

    /// Maximum elapsed duration the run is allowed to span. When the
    /// elapsed duration reaches the limit, `finish` stops with
    /// `FinishReason::PolicyViolated(PolicyKind::Time)` and emits the
    /// matching `PolicyViolated` event.
    pub fn max_time(&self, d: Duration) -> &Self {
        self.policies.lock().unwrap().max_time = Some(d);
        self
    }

    /// Cancel the run when `trigger` resolves. The future's output is
    /// discarded; only completion matters. Composes with any cancellation
    /// source: ctrl-c, a deadline, a channel receive, an external signal.
    pub fn cancel_on<F>(&self, trigger: F) -> &Self
    where
        F: Future + Send + 'static,
        F::Output: Send,
    {
        let supervisor = self
            .weak_self
            .upgrade()
            .expect("TicketSystem dropped during cancel_on");
        tokio::spawn(async move {
            let _ = trigger.await;
            supervisor.cancel();
        });
        self
    }

    /// Cancel the run when `predicate(&event)` first returns true.
    /// Implemented as one more entry on the [`Self::on_event`] chain;
    /// composes with any logger the caller installed.
    pub fn cancel_on_event<F>(&self, predicate: F) -> &Self
    where
        F: Fn(&Event) -> bool + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            if !predicate(&event) {
                return;
            }
            if let Some(s) = supervisor.upgrade() {
                s.cancel();
            }
        })
    }

    /// Override the directory under which the system writes
    /// `results.jsonl`, `tickets.jsonl`, and per-ticket
    /// `tickets/<key>/ticket.<ts>.json` files. Defaults to `./.agentwerk`.
    /// Knowledge co-locates with these files when `Knowledge::open`
    /// points at the same directory.
    pub fn dir(&self, dir: impl Into<PathBuf>) -> &Self {
        *self.dir.lock().unwrap() = dir.into();
        self
    }

    pub(crate) fn dir_value(&self) -> PathBuf {
        self.dir.lock().unwrap().clone()
    }

    // ---- ticket-creation API mirrored on Agent ----

    /// Enqueue a ticket carrying `task` as its body. Returns the new
    /// ticket's key.
    pub fn task<T: Serialize>(&self, task: T) -> String {
        self.dispatch(Ticket::new(task))
    }

    /// Enqueue a ticket carrying `task`, attached to `label` for Path B
    /// assignment. Returns the new ticket's key.
    pub fn task_labeled<T: Serialize>(&self, task: T, label: impl Into<String>) -> String {
        self.dispatch(Ticket::new(task).label(label))
    }

    /// Enqueue a fully-built `Ticket`. System-managed fields (key,
    /// reporter, created_at, status, result) are overwritten. To pin the
    /// ticket to a specific agent, label it with the agent's name.
    /// Compose schema and label via `Ticket::new(...).schema(...).label(...)`.
    /// Returns the inserted ticket's key.
    pub fn ticket(&self, ticket: Ticket) -> String {
        self.dispatch(ticket)
    }

    /// Append a user-side text reply to an existing ticket. After the
    /// assistant has spoken, the agent pauses on the ticket; this call
    /// flips the gate by appending a non-assistant reply, and the next
    /// turn is sent to the provider. Use this to continue multi-turn
    /// chats on one ticket instead of creating a new ticket per turn.
    pub fn reply(&self, key: &str, content: impl Into<String>) -> &Self {
        self.add_reply(key, Reply::user_text(content));
        self
    }

    fn dispatch(&self, ticket: Ticket) -> String {
        self.insert(ticket, "user".to_string())
    }

    // ---- query methods ----

    /// Clone of the ticket at `key`, if any.
    pub fn get_ticket(&self, key: &str) -> Option<Ticket> {
        self.tickets.lock().unwrap().get(key).cloned()
    }

    /// Every ticket, sorted by creation time then numeric key.
    pub fn tickets(&self) -> Vec<Ticket> {
        let tickets = self.tickets.lock().unwrap();
        let mut out: Vec<Ticket> = tickets.values().cloned().collect();
        out.sort_by_key(|t| (t.created_at, numeric_id(&t.key)));
        out
    }

    /// Earliest ticket by creation time, if any.
    pub fn first_ticket(&self) -> Option<Ticket> {
        self.tickets().into_iter().next()
    }

    /// Latest ticket by creation time, if any.
    pub fn last_ticket(&self) -> Option<Ticket> {
        self.tickets().into_iter().next_back()
    }

    /// Substring search over the task body, case-insensitive.
    pub fn search_tickets(&self, query: &str) -> Vec<Ticket> {
        let needle = query.to_lowercase();
        let store = self.tickets.lock().unwrap();
        let mut out: Vec<Ticket> = store
            .values()
            .filter(|t| match &t.task {
                serde_json::Value::String(s) => s.to_lowercase().contains(&needle),
                other => other.to_string().to_lowercase().contains(&needle),
            })
            .cloned()
            .collect();
        out.sort_by_key(|t| (t.created_at, numeric_id(&t.key)));
        out
    }

    /// Tickets matching `predicate`, sorted by creation time then numeric key.
    ///
    /// The predicate runs while `self.tickets` is locked. It MUST NOT call
    /// other `TicketSystem` methods that lock the same `Mutex`: deadlock.
    pub fn find_tickets<F>(&self, predicate: F) -> Vec<Ticket>
    where
        F: Fn(&Ticket) -> bool,
    {
        let store = self.tickets.lock().unwrap();
        let mut out: Vec<Ticket> = store.values().filter(|t| predicate(t)).cloned().collect();
        out.sort_by_key(|t| (t.created_at, numeric_id(&t.key)));
        out
    }

    /// First ticket matching `predicate`, by creation order. Short-circuits.
    ///
    /// The predicate runs while `self.tickets` is locked. It MUST NOT call
    /// other `TicketSystem` methods that lock the same `Mutex`: deadlock.
    pub fn find_ticket<F>(&self, predicate: F) -> Option<Ticket>
    where
        F: Fn(&Ticket) -> bool,
    {
        let store = self.tickets.lock().unwrap();
        let mut matching: Vec<&Ticket> = store.values().filter(|t| predicate(t)).collect();
        matching.sort_by_key(|t| (t.created_at, numeric_id(&t.key)));
        matching.into_iter().next().cloned()
    }

    /// Count of tickets matching `predicate`. Does not allocate.
    ///
    /// The predicate runs while `self.tickets` is locked. It MUST NOT call
    /// other `TicketSystem` methods that lock the same `Mutex`: deadlock.
    pub fn count_tickets<F>(&self, predicate: F) -> usize
    where
        F: Fn(&Ticket) -> bool,
    {
        self.tickets
            .lock()
            .unwrap()
            .values()
            .filter(|t| predicate(t))
            .count()
    }

    /// Count of tickets the run watcher still considers in flight: every
    /// ticket whose status is `Todo` or `InProgress`.
    pub(crate) fn pending_count(&self) -> usize {
        self.tickets
            .lock()
            .unwrap()
            .values()
            .filter(|t| matches!(t.status, Status::Todo | Status::InProgress))
            .count()
    }

    // ---- agent binding ----

    /// Wire `agent` to this system. Drains any tickets the agent had
    /// queued in a prior private system into this one, then switches the
    /// agent's `TicketSystemRef` to `Shared(weak_self)`. Any prior
    /// `Private` arm is dropped at the reassignment, so the prior system
    /// is freed once no other strong reference holds it.
    pub(crate) fn bind_agent(&self, agent: &mut Agent) {
        if let Some(prior) = agent.ticket_system.upgrade() {
            if !Arc::ptr_eq(
                &prior,
                &self
                    .weak_self
                    .upgrade()
                    .expect("self Arc dropped during bind"),
            ) {
                let drained: Vec<Ticket> = {
                    let mut old = prior.tickets.lock().unwrap();
                    std::mem::take(&mut *old).into_values().collect()
                };
                let reporter = agent.name.clone();
                for ticket in drained {
                    self.insert(ticket, reporter.clone());
                }
            }
        }
        agent.ticket_system = TicketSystemRef::Shared(self.weak_self.clone());
        self.agents.lock().unwrap().push(agent.clone());
    }

    /// Clone of the currently registered agent list. The list is
    /// append-only by invariant: `bind_agent` is the sole mutator and
    /// only calls `push`. `run_main_loop` relies on element indices
    /// being stable across calls. Any new mutator that removes or
    /// reorders entries would silently break late-add detection: route
    /// additions through `bind_agent` only.
    pub(crate) fn clone_agents(&self) -> Vec<Agent> {
        self.agents.lock().unwrap().clone()
    }

    /// Bind `agent` to this system: drain any tickets it queued in its
    /// default system into this one and push a clone onto this system's
    /// agents list. Returns the wired agent for chaining (`.task(...)`
    /// etc.).
    ///
    /// May be called before or after `run()` / `finish()`. When called
    /// after `run()`, the new agent starts polling for tickets within
    /// roughly one `IDLE_POLL_INTERVAL` (~100 ms).
    pub fn agent(&self, mut agent: Agent) -> Agent {
        self.bind_agent(&mut agent);
        agent
    }

    // ---- run lifecycle ----

    /// Start the agent loop on a background tokio task. Tickets queued
    /// afterwards are picked up within ~`IDLE_POLL_INTERVAL`. Pair with
    /// [`Self::finish`] to wait for the queue to empty, or with
    /// [`Self::cancel`] to signal an early exit.
    pub fn start(&self) -> &Self {
        // Reset both signals so a system can be re-started after a
        // previous finish left flags set, and clear the prior reason so
        // `finish_reason()` returns None during the live run.
        self.stop_signal
            .lock()
            .unwrap()
            .store(false, Ordering::Relaxed);
        self.cancel_signal
            .lock()
            .unwrap()
            .store(false, Ordering::Relaxed);
        self.finish_reason.lock().unwrap().take();
        let supervisor = self
            .weak_self
            .upgrade()
            .expect("TicketSystem dropped during start");
        self.emit("", "", EventKind::RunStarted);
        let join = tokio::spawn(async move {
            run_main_loop(&supervisor).await;
            supervisor.stats.mark_finished(now_millis());
        });
        *self.join_handle.lock().unwrap() = Some(join);
        self
    }

    /// Process every queued ticket, then return. Starts a run if none
    /// is in flight; otherwise watches the in-flight one. Polls every
    /// 20 ms; the loop exits on cancel, policy violation, or clean
    /// drain, in that precedence. The chosen reason is stashed for
    /// [`Self::finish_reason`] and announced via
    /// [`EventKind::RunFinished`]. Returns `&self` so callers can chain
    /// [`Self::last_result`], [`Self::results`], or
    /// [`Self::tickets`] without rebinding.
    pub async fn finish(&self) -> &Self {
        if self.join_handle.lock().unwrap().is_none() {
            self.start();
        }
        let policies = self.policies();
        let stop = Arc::clone(&self.stop_signal.lock().unwrap());
        let cancel = Arc::clone(&self.cancel_signal.lock().unwrap());
        let reason: FinishReason = loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if cancel.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
                break FinishReason::Cancelled;
            }
            if let Some((kind, _)) = policy_violated_kind(&policies, &self.stats) {
                stop.store(true, Ordering::Relaxed);
                break FinishReason::PolicyViolated(kind);
            }
            if self.pending_count() == 0 {
                stop.store(true, Ordering::Relaxed);
                break FinishReason::Drained;
            }
        };
        self.take_join_handle().await;
        self.stats.mark_finished(now_millis());
        *self.finish_reason.lock().unwrap() = Some(reason);
        self.emit("", "", EventKind::RunFinished { reason });
        self
    }

    /// Request cancellation. Sync, so it composes with ctrl-c handlers,
    /// drop guards, and other sync callers. Flips both the cancel
    /// signal (read by [`Self::is_cancelled`]) and the stop signal
    /// (read by every worker and tool). [`Self::finish`] returns
    /// shortly after with `FinishReason::Cancelled`.
    pub fn cancel(&self) {
        self.cancel_signal
            .lock()
            .unwrap()
            .store(true, Ordering::Relaxed);
        self.stop_signal
            .lock()
            .unwrap()
            .store(true, Ordering::Relaxed);
    }

    async fn take_join_handle(&self) {
        let handle = self.join_handle.lock().unwrap().take();
        if let Some(h) = handle {
            let _ = h.await;
        }
    }

    /// True once external cancellation has been requested through
    /// [`Self::cancel`], [`Self::cancel_on`], or
    /// [`Self::cancel_on_event`]. Clean drains and policy stops do not
    /// flip this signal.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_signal.lock().unwrap().load(Ordering::Relaxed)
    }

    /// Reason the most recent `finish()` returned. `None` before the
    /// first `finish()` call and between `start()` and the next
    /// `finish()`.
    pub fn finish_reason(&self) -> Option<FinishReason> {
        *self.finish_reason.lock().unwrap()
    }

    /// Most recent finished ticket's result rendered as a String.
    pub fn last_result(&self) -> Option<String> {
        self.results().into_iter().next_back()
    }

    /// Every finished ticket's result rendered as a String, in creation
    /// order.
    pub fn results(&self) -> Vec<String> {
        self.find_tickets(|t| t.status == Status::Finished && t.result.is_some())
            .iter()
            .filter_map(|t| {
                t.result.as_ref().map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
            })
            .collect()
    }

    /// Finished tickets carrying `label`, with each result deserialized
    /// into `T`. Tickets whose result fails to deserialize are silently
    /// skipped — matches the precedent of [`Self::results`], which drops
    /// rows without an attached result.
    pub fn collect_results_by_label<T>(&self, label: &str) -> Vec<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.find_tickets(|t| t.is_finished() && t.has_label(label))
            .into_iter()
            .filter_map(|t| t.result.and_then(|v| serde_json::from_value(v).ok()))
            .collect()
    }

    /// Earliest finished ticket carrying `label`, with its result
    /// deserialized into `T`. `None` when no finished ticket has the
    /// label or the result fails to deserialize — matches the
    /// skip-on-mismatch precedent of [`Self::collect_results_by_label`].
    pub fn result_by_label<T>(&self, label: &str) -> Option<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.find_ticket(|t| t.is_finished() && t.has_label(label))
            .and_then(|t| t.result)
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Resolve to the earliest ticket matching `predicate`, polling every
    /// ~50 ms. Resolves to `None` if the run stops (cancel, policy, or
    /// clean drain) before any ticket matches. Call after [`Self::start`].
    ///
    /// The predicate runs while `self.tickets` is locked (via
    /// `find_ticket`); it MUST NOT call other `TicketSystem` methods that
    /// lock the same `Mutex`: deadlock.
    pub async fn wait_for_ticket<F>(&self, predicate: F) -> Option<Ticket>
    where
        F: Fn(&Ticket) -> bool,
    {
        loop {
            if let Some(ticket) = self.find_ticket(&predicate) {
                return Some(ticket);
            }
            if self.stop_signal.lock().unwrap().load(Ordering::Relaxed) {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    #[test]
    fn ticket_system_handle_is_shared_between_caller_and_added_agent() {
        let (sys, _tmp) = test_system();
        let alice = sys.agent(minimal_agent("alice"));
        // Alice's task lands in the same queue.
        alice.task("from alice");
        sys.task("from system");
        let all_keys: Vec<String> = sys
            .find_tickets(|t| t.status == Status::Todo)
            .iter()
            .map(|t| t.key.clone())
            .collect();
        assert_eq!(all_keys.len(), 2);
    }

    #[test]
    fn repeated_task_calls_route_to_shared_queue_after_rebind() {
        let alice = minimal_agent("alice");
        let (sys, _tmp) = test_system();
        let alice = sys.agent(alice);
        alice.task("first");
        alice.task("second");
        assert_eq!(sys.count_tickets(|t| t.status == Status::Todo), 2);
    }

    #[test]
    fn search_matches_string_task_case_insensitively() {
        let (sys, _tmp) = test_system();
        sys.task("Fix Login");
        sys.task("Other thing");
        let hits = sys.search_tickets("login");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn first_returns_none_on_empty_system() {
        let (sys, _tmp) = test_system();
        assert!(sys.first_ticket().is_none());
        assert!(sys.tickets().is_empty());
    }

    #[test]
    fn first_returns_earliest_ticket_by_creation() {
        let (sys, _tmp) = test_system();
        sys.task("first");
        sys.task("second");
        sys.task("third");
        let first = sys.first_ticket().unwrap();
        assert_eq!(first.key, "TICKET-1");
        assert_eq!(first.task, serde_json::Value::String("first".into()));
    }

    #[test]
    fn last_returns_latest_ticket_by_creation() {
        let (sys, _tmp) = test_system();
        assert!(sys.last_ticket().is_none());
        sys.task("first");
        sys.task("second");
        sys.task("third");
        let last = sys.last_ticket().unwrap();
        assert_eq!(last.key, "TICKET-3");
        assert_eq!(last.task, serde_json::Value::String("third".into()));
    }

    #[test]
    fn tickets_returns_all_in_creation_order() {
        let (sys, _tmp) = test_system();
        sys.task("a");
        sys.task("b");
        sys.task("c");
        let all = sys.tickets();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].key, "TICKET-1");
        assert_eq!(all[1].key, "TICKET-2");
        assert_eq!(all[2].key, "TICKET-3");
    }

    #[test]
    fn results_return_done_payloads_in_creation_order() {
        let (sys, _tmp) = test_system();
        sys.task("a");
        sys.task("b");
        sys.task("c");
        attach_done_result(&sys, "TICKET-1", "first");
        attach_done_result(&sys, "TICKET-3", "third");
        assert_eq!(sys.results(), vec!["first", "third"]);
    }

    #[test]
    fn last_result_returns_last_done_payload() {
        let (sys, _tmp) = test_system();
        sys.task("a");
        sys.task("b");
        attach_done_result(&sys, "TICKET-2", "second");
        attach_done_result(&sys, "TICKET-1", "first");
        assert_eq!(sys.last_result().as_deref(), Some("second"));
    }

    #[test]
    fn results_order_by_creation_regardless_of_done_order() {
        let (sys, _tmp) = test_system();
        sys.task("a");
        sys.task("b");
        sys.task("c");
        attach_done_result(&sys, "TICKET-3", "third");
        attach_done_result(&sys, "TICKET-1", "first");
        attach_done_result(&sys, "TICKET-2", "second");
        assert_eq!(sys.results(), vec!["first", "second", "third"]);
    }

    #[test]
    fn results_are_empty_when_nothing_finished() {
        let (sys, _tmp) = test_system();
        sys.task("pending");
        assert!(sys.last_result().is_none());
        assert!(sys.results().is_empty());
    }

    #[test]
    fn pending_count_counts_todo() {
        let (sys, _tmp) = test_system();
        sys.task("a");
        sys.task("b");
        assert_eq!(sys.pending_count(), 2);
    }

    #[test]
    fn pending_count_counts_inprogress_waiting_for_response() {
        let (sys, _tmp) = test_system();
        sys.task("x");
        sys.claim(|t| t.status == Status::Todo, "agent").unwrap();
        assert_eq!(sys.pending_count(), 1);
    }

    #[test]
    fn pending_count_counts_inprogress_with_text_only_last_reply() {
        let (sys, _tmp) = test_system();
        sys.task("x");
        let key = sys.claim(|t| t.status == Status::Todo, "agent").unwrap();
        sys.add_reply(
            &key,
            Reply::assistant(&[crate::providers::ContentBlock::Text {
                text: "hello".into(),
            }]),
        );
        assert_eq!(sys.pending_count(), 1);
    }

    #[test]
    fn pending_count_counts_inprogress_with_empty_content_last_reply() {
        let (sys, _tmp) = test_system();
        sys.task("x");
        let key = sys.claim(|t| t.status == Status::Todo, "agent").unwrap();
        sys.add_reply(&key, Reply::assistant(&[]));
        assert_eq!(sys.pending_count(), 1);
    }

    #[test]
    fn pending_count_excludes_finished_and_failed() {
        let (sys, _tmp) = test_system();
        sys.task("a");
        sys.task("b");
        let key_a = sys.claim(|t| t.key == "TICKET-1", "agent").unwrap();
        let key_b = sys.claim(|t| t.key == "TICKET-2", "agent").unwrap();
        sys.set_finished(&key_a).unwrap();
        sys.set_failed(&key_b).unwrap();
        assert_eq!(sys.pending_count(), 0);
    }

    #[test]
    fn on_event_appends_handlers_in_installation_order() {
        use std::sync::Mutex;
        let (sys, _tmp) = test_system();
        let log: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let l1 = Arc::clone(&log);
        let l2 = Arc::clone(&log);
        sys.on_event(move |_| l1.lock().unwrap().push(1));
        sys.on_event(move |_| l2.lock().unwrap().push(2));
        sys.emit("KEY", "agent", EventKind::TurnStarted);
        assert_eq!(*log.lock().unwrap(), vec![1, 2]);
    }

    #[test]
    fn on_event_falls_back_to_default_logger_when_empty() {
        // No assertion target beyond "does not panic": with no installed
        // handlers, emit() must run default_logger without crashing.
        let (sys, _tmp) = test_system();
        sys.emit("KEY", "agent", EventKind::TurnStarted);
    }

    #[test]
    fn collect_results_by_label_returns_only_matching_label() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("a").label("analysis"));
        sys.ticket(Ticket::new("b").label("other"));
        let key_a = sys
            .claim(|t| t.task == serde_json::json!("a"), "agent")
            .unwrap();
        let key_b = sys
            .claim(|t| t.task == serde_json::json!("b"), "agent")
            .unwrap();
        sys.set_result(&key_a, serde_json::json!({"score": 7}))
            .unwrap();
        sys.set_finished(&key_a).unwrap();
        sys.set_result(&key_b, serde_json::json!({"score": 99}))
            .unwrap();
        sys.set_finished(&key_b).unwrap();
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct Finding {
            score: i32,
        }
        let hits: Vec<Finding> = sys.collect_results_by_label("analysis");
        assert_eq!(hits, vec![Finding { score: 7 }]);
    }

    #[test]
    fn collect_results_by_label_skips_unparseable_payloads() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("good").label("L"));
        sys.ticket(Ticket::new("bad").label("L"));
        let key_good = sys
            .claim(|t| t.task == serde_json::json!("good"), "agent")
            .unwrap();
        let key_bad = sys
            .claim(|t| t.task == serde_json::json!("bad"), "agent")
            .unwrap();
        sys.set_result(&key_good, serde_json::json!({"n": 1}))
            .unwrap();
        sys.set_finished(&key_good).unwrap();
        sys.set_result(&key_bad, serde_json::json!("not an object"))
            .unwrap();
        sys.set_finished(&key_bad).unwrap();
        #[derive(serde::Deserialize)]
        struct N {
            n: i32,
        }
        let hits: Vec<N> = sys.collect_results_by_label("L");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].n, 1);
    }

    #[test]
    fn collect_results_by_label_empty_when_no_label_match() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("x").label("other"));
        let key = sys.claim(|t| t.has_label("other"), "agent").unwrap();
        sys.set_result(&key, serde_json::json!({"n": 1})).unwrap();
        sys.set_finished(&key).unwrap();
        let hits: Vec<serde_json::Value> = sys.collect_results_by_label("missing");
        assert!(hits.is_empty());
    }

    #[test]
    fn result_by_label_returns_earliest_finished_match() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("a").label("analysis"));
        sys.ticket(Ticket::new("b").label("analysis"));
        let key_a = sys
            .claim(|t| t.task == serde_json::json!("a"), "agent")
            .unwrap();
        sys.set_result(&key_a, serde_json::json!({"score": 7}))
            .unwrap();
        sys.set_finished(&key_a).unwrap();
        let key_b = sys
            .claim(|t| t.task == serde_json::json!("b"), "agent")
            .unwrap();
        sys.set_result(&key_b, serde_json::json!({"score": 9}))
            .unwrap();
        sys.set_finished(&key_b).unwrap();
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct Finding {
            score: i32,
        }
        let hit: Option<Finding> = sys.result_by_label("analysis");
        assert_eq!(hit, Some(Finding { score: 7 }));
    }

    #[test]
    fn result_by_label_none_when_no_finished_label() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("x").label("other"));
        let hit: Option<serde_json::Value> = sys.result_by_label("analysis");
        assert!(hit.is_none());
    }

    #[tokio::test]
    async fn wait_for_ticket_resolves_when_ticket_matches() {
        let (sys, _tmp) = test_system();
        let key = sys.task("work");
        let writer = Arc::clone(&sys);
        let claimed = key.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            attach_done_result(&writer, &claimed, "done");
        });
        let found = sys.wait_for_ticket(|t| t.is_finished()).await;
        assert_eq!(found.map(|t| t.key), Some(key));
    }

    #[tokio::test]
    async fn wait_for_ticket_none_after_cancel() {
        let (sys, _tmp) = test_system();
        sys.task("never matches");
        sys.cancel();
        let found = sys.wait_for_ticket(|t| t.is_finished()).await;
        assert!(found.is_none());
    }

    #[test]
    fn cancel_on_event_trips_signal_when_predicate_matches() {
        let (sys, _tmp) = test_system();
        assert!(!sys.is_cancelled());
        sys.cancel_on_event(|e| matches!(e.kind, EventKind::TicketFailed { .. }));
        sys.emit("KEY", "agent", EventKind::TurnStarted);
        assert!(!sys.is_cancelled());
        sys.emit(
            "KEY",
            "agent",
            EventKind::TicketFailed { key: "KEY".into() },
        );
        assert!(sys.is_cancelled());
    }

    #[test]
    fn cancel_on_event_coexists_with_user_handler() {
        use std::sync::atomic::AtomicU32;
        let (sys, _tmp) = test_system();
        let count = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&count);
        sys.on_event(move |_| {
            c.fetch_add(1, Ordering::Relaxed);
        });
        sys.cancel_on_event(|e| matches!(e.kind, EventKind::TurnStarted));
        sys.emit("KEY", "agent", EventKind::TurnStarted);
        assert_eq!(count.load(Ordering::Relaxed), 1, "user handler should fire");
        assert!(sys.is_cancelled(), "predicate should trip cancel");
    }

    #[test]
    fn on_event_fires_every_handler_per_event() {
        use std::sync::atomic::AtomicU32;
        let (sys, _tmp) = test_system();
        let count = Arc::new(AtomicU32::new(0));
        let c1 = Arc::clone(&count);
        let c2 = Arc::clone(&count);
        sys.on_event(move |_| {
            c1.fetch_add(1, Ordering::Relaxed);
        });
        sys.on_event(move |_| {
            c2.fetch_add(10, Ordering::Relaxed);
        });
        sys.emit("KEY", "agent", EventKind::TurnStarted);
        sys.emit("KEY", "agent", EventKind::TurnStarted);
        assert_eq!(count.load(Ordering::Relaxed), 22);
    }

    #[test]
    fn finish_reason_is_none_before_first_finish() {
        let (sys, _tmp) = test_system();
        assert_eq!(sys.finish_reason(), None);
    }

    #[tokio::test]
    async fn finish_reason_drained_on_empty_queue() {
        let (sys, _tmp) = test_system();
        sys.finish().await;
        assert_eq!(sys.finish_reason(), Some(FinishReason::Drained));
    }

    #[tokio::test]
    async fn is_cancelled_stays_false_after_clean_drain() {
        let (sys, _tmp) = test_system();
        sys.finish().await;
        assert!(
            !sys.is_cancelled(),
            "clean drain must not flip the cancel signal",
        );
    }

    #[tokio::test]
    async fn finish_reason_cancelled_when_cancel_fires_during_run() {
        let (sys, _tmp) = test_system();
        sys.start();
        sys.cancel();
        sys.finish().await;
        assert_eq!(sys.finish_reason(), Some(FinishReason::Cancelled));
        assert!(sys.is_cancelled());
    }

    #[tokio::test]
    async fn finish_reason_policy_violated_when_max_turns_zero() {
        let (sys, _tmp) = test_system();
        sys.max_turns(0);
        sys.finish().await;
        assert_eq!(
            sys.finish_reason(),
            Some(FinishReason::PolicyViolated(
                crate::event::PolicyKind::Turns
            )),
        );
    }

    #[tokio::test]
    async fn finish_reason_resets_after_restart() {
        let (sys, _tmp) = test_system();
        sys.finish().await;
        assert_eq!(sys.finish_reason(), Some(FinishReason::Drained));
        sys.start();
        assert_eq!(sys.finish_reason(), None);
        sys.finish().await;
        assert_eq!(sys.finish_reason(), Some(FinishReason::Drained));
    }

    #[tokio::test]
    async fn run_started_emitted_before_run_finished() {
        let (sys, _tmp) = test_system();
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        sys.on_event(move |e| {
            if matches!(
                e.kind,
                EventKind::RunStarted | EventKind::RunFinished { .. }
            ) {
                sink.lock().unwrap().push(format!("{:?}", e.kind));
            }
        });
        sys.finish().await;
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2, "expected RunStarted then RunFinished");
        assert!(entries[0].starts_with("RunStarted"));
        assert!(entries[1].starts_with("RunFinished"));
    }
}
