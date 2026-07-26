//! The [`TicketSystem`] orchestrator: owns the shared ticket store,
//! registered agents, policies, cancellation signals, and run stats.
//! This file holds construction, configuration, the ticket-creation
//! API, agent binding, the background-run lifecycle, and queries.
//! Mutation impls (`claim`, `set_finished`, `summarize`, etc.) live
//! next door in `store.rs`.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::Serialize;
use tokio::task::JoinHandle;

use crate::event::{default_logger, Event, EventKind, FinishReason};
use crate::persistence::Persist;
use crate::schemas::Schema;

use super::super::agent::{Agent, TicketSystemRef};
use super::super::policy::Policies;
use super::super::r#loop::run_main_loop;
use super::super::stats::Stats;
use super::ticket::{Status, Ticket};
use super::{now_millis, numeric_id, policy_violated_kind, Reply, Trajectory};

type EventHandler = dyn Fn(Event) + Send + Sync;
type MessageEditor = dyn Fn(&[Event], &mut Vec<Reply>) + Send + Sync;

/// The message-editing state, always touched together: the registered
/// editors and the per-ticket events buffered for them since each ticket's
/// previous request. An `on_event` handler installed with the first editor
/// fills `pending`; `run_message_editors` drains it.
#[derive(Default)]
pub(super) struct MessageEditing {
    pub(super) editors: Vec<Arc<MessageEditor>>,
    pub(super) pending: HashMap<String, Vec<Event>>,
}

/// The shared work queue. Owns the ticket store, the registered
/// agents, the policies, and the run statistics. Many agents share one
/// `TicketSystem` and pick up tickets concurrently; labels and names
/// assign work to the right agent.
///
/// ```no_run
/// use agentwerk::{Agent, Ticket, TicketSystem};
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
/// tickets.ticket(Ticket::new("Summarize https://canvascomputing.org").label("research"));
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
/// │       ├── replies.jsonl                 the messages, one reply per line
/// │       └── outputs/<tool_use_id>.txt     full tool outputs spilled out of the transcript
/// └── knowledge/
///     ├── pages/<slug>.md                   knowledge pages
///     └── index.md                          knowledge index
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
    /// Labels whose pool has been called off via `cancel_label`. The loop
    /// skips claiming or resuming a ticket carrying one of these, and walks
    /// an agent off a ticket whose label lands here mid-flight, stopping one
    /// pool while the rest of the run continues.
    pub(crate) cancelled_labels: Mutex<HashSet<String>>,
    /// Result schema stamped onto every ticket carrying a registered label
    /// that was created without a schema of its own. Set via `schema_for_label`
    /// and applied at insert time, so a ticket's result contract follows its
    /// label no matter how the ticket was created.
    pub(crate) label_schemas: Mutex<HashMap<String, Schema>>,
    /// Count of `set_final_status` calls between their status flip and
    /// the return of the terminal event's handlers. The drain check in
    /// `finish()` treats a non-zero count as pending work, so a handler
    /// minting a follow-up ticket always beats the drain.
    pub(crate) terminal_transitions_in_flight: AtomicUsize,
    /// Reason the most recent `finish()` returned. `None` before the
    /// first `finish()` and between `start()` and the next `finish()`.
    pub(crate) finish_reason: Mutex<Option<FinishReason>>,
    pub(crate) stats: Stats,
    pub(super) event_handlers: Mutex<Vec<Arc<EventHandler>>>,
    /// Editors that rewrite a ticket's transcript before each provider
    /// request, plus the per-ticket events buffered for them; see
    /// `edit_messages_on_event`. Empty editors short-circuits buffering.
    pub(super) message_editing: Mutex<MessageEditing>,
    pub(super) dir: Mutex<PathBuf>,
    pub(super) tickets_log_lock: Mutex<()>,
    pub(super) results_log_lock: Mutex<()>,
    pub(super) join_handle: Mutex<Option<JoinHandle<()>>>,
    /// Next `TICKET-<N>` id to hand out, or `None` until it's known.
    /// `load()` seeds it directly from the tickets it just read off disk.
    /// A system built via `new()` (with or without a later `.dir(path)`)
    /// leaves it `None`; the first `insert()` scans for the highest
    /// existing id then, since `new()` never reads the directory itself.
    pub(super) next_ticket_id: Mutex<Option<u64>>,
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
            cancelled_labels: Mutex::new(HashSet::new()),
            label_schemas: Mutex::new(HashMap::new()),
            terminal_transitions_in_flight: AtomicUsize::new(0),
            finish_reason: Mutex::new(None),
            stats: Stats::new(),
            event_handlers: Mutex::new(Vec::new()),
            message_editing: Mutex::new(MessageEditing::default()),
            dir: Mutex::new(PathBuf::from(".agentwerk")),
            tickets_log_lock: Mutex::new(()),
            results_log_lock: Mutex::new(()),
            join_handle: Mutex::new(None),
            next_ticket_id: Mutex::new(None),
        })
    }

    /// Open or create a ticket system rooted at `tickets_dir`. Loads each
    /// `ticket.json` under `<tickets_dir>/tickets/` into the in-memory
    /// store and seeds `Stats` from
    /// `<tickets_dir>/stats.json` (or, when that file is missing or
    /// malformed, by deriving from the loaded tickets) so success rate
    /// and counters stay continuous across restarts.
    ///
    /// Pointing this and `Knowledge::load` at the same dir co-locates the
    /// `knowledge/` bundle with `results.jsonl` and `tickets.jsonl`.
    ///
    /// `InProgress` tickets keep their status and their transcript; the
    /// loop's resume path (`agents/loop.rs`) picks them back up under
    /// the agent whose name is already in the ticket's `labels`.
    ///
    /// Caller contracts:
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
        let next_id = tickets
            .keys()
            .map(|k| numeric_id(k) as u64)
            .filter(|&n| n != u32::MAX as u64)
            .max()
            .unwrap_or(0);

        Ok(Arc::new_cyclic(|weak| Self {
            weak_self: weak.clone(),
            tickets: Mutex::new(tickets),
            agents: Mutex::new(Vec::new()),
            policies: Mutex::new(Policies::default()),
            stop_signal: Mutex::new(Arc::new(AtomicBool::new(false))),
            cancel_signal: Mutex::new(Arc::new(AtomicBool::new(false))),
            cancelled_labels: Mutex::new(HashSet::new()),
            label_schemas: Mutex::new(HashMap::new()),
            terminal_transitions_in_flight: AtomicUsize::new(0),
            finish_reason: Mutex::new(None),
            stats,
            event_handlers: Mutex::new(Vec::new()),
            message_editing: Mutex::new(MessageEditing::default()),
            dir: Mutex::new(tickets_dir),
            tickets_log_lock: Mutex::new(()),
            results_log_lock: Mutex::new(()),
            join_handle: Mutex::new(None),
            next_ticket_id: Mutex::new(Some(next_id)),
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

    /// Register an editor that rewrites or drops a ticket's messages
    /// before its next provider request. The editor receives the events
    /// emitted for that ticket since its previous request and a mutable
    /// view of its full transcript, and mutates the `Vec` in place: drop
    /// a reply, rewrite one, or push a new one. Match on `event.kind` to
    /// act only on the triggers you care about (a tool failure, a stalled
    /// turn); keep the editor cheap, it runs inline in the loop.
    ///
    /// The edit is persistent: it mutates the stored transcript and
    /// rewrites the on-disk transcript in place, so a dropped message
    /// stays gone from the model's transcript, now and across resumption,
    /// and is left behind in no superseded file on disk. The editor must
    /// keep the transcript well-formed: a `tool_use` and its `tool_result`
    /// span two replies, so drop both sides together or the provider
    /// rejects the unpaired block.
    ///
    /// Editors run in registration order over the same transcript. The
    /// editor should be event-gated: on a reactive-compaction retry the
    /// request is reassembled, so an editor that ignores the (now empty)
    /// event batch would act twice.
    ///
    /// ```no_run
    /// use agentwerk::TicketSystem;
    /// use agentwerk::event::EventKind;
    /// use agentwerk::agents::tickets::{Reply, ReplyContent};
    ///
    /// let tickets = TicketSystem::new();
    /// tickets.edit_messages_on_event(|events, messages| {
    ///     let failed = events
    ///         .iter()
    ///         .any(|e| matches!(e.kind, EventKind::ToolCallFailed { .. }));
    ///     if !failed {
    ///         return;
    ///     }
    ///     // Drop both sides of the failed exchange: the assistant's tool_use
    ///     // and the failed tool_result, so no unpaired block is left behind.
    ///     messages.retain(|reply| {
    ///         !reply.content.iter().any(|b| {
    ///             matches!(
    ///                 b,
    ///                 ReplyContent::ToolUse { .. }
    ///                     | ReplyContent::ToolResult { succeeded: false, .. }
    ///             )
    ///         })
    ///     });
    ///     messages.push(Reply::user_text("That approach failed. Re-read the file first."));
    /// });
    /// ```
    pub fn edit_messages_on_event(
        &self,
        editor: impl Fn(&[Event], &mut Vec<Reply>) + Send + Sync + 'static,
    ) -> &Self {
        let is_first_editor = {
            let mut editing = self.message_editing.lock().unwrap();
            let was_empty = editing.editors.is_empty();
            editing.editors.push(Arc::new(editor));
            was_empty
        };
        if !is_first_editor {
            return self;
        }

        // Buffer each ticket's events for the editors as one more `on_event`
        // handler (like `cancel_on_event`), installed once with the first editor.
        let supervisor = self.weak_self.clone();
        let buffer_event = move |event: Event| {
            // Streaming chunks carry no editing signal; run-lifecycle events
            // (empty key) belong to no ticket.
            if event.ticket_key.is_empty()
                || matches!(event.kind, EventKind::TextChunkReceived { .. })
            {
                return;
            }
            let Some(system) = supervisor.upgrade() else {
                return;
            };
            system
                .message_editing
                .lock()
                .unwrap()
                .pending
                .entry(event.ticket_key.clone())
                .or_default()
                .push(event);
        };
        self.on_event(buffer_event);
        self
    }

    pub(crate) fn emit(&self, key: &str, agent: &str, kind: EventKind) {
        self.stats.record_event(&kind, key, &self.labels_for(key));
        let event = Event::new(agent, key, kind);
        let handlers: Vec<Arc<EventHandler>> = self.event_handlers.lock().unwrap().clone();
        if handlers.is_empty() {
            default_logger()(event);
            return;
        }
        for h in &handlers {
            h(event.clone());
        }
    }

    /// Apply the registered editors to `key`'s transcript, handing them
    /// the events buffered since the ticket's previous request and
    /// draining that batch. Called at the top of the request round-trip;
    /// a no-op until an editor is registered or when no events are
    /// pending.
    pub(crate) fn run_message_editors(&self, key: &str) {
        let (editors, events) = {
            let mut editing = self.message_editing.lock().unwrap();
            if editing.editors.is_empty() {
                return;
            }
            let events = editing.pending.remove(key).unwrap_or_default();
            (editing.editors.clone(), events)
        };
        if events.is_empty() {
            return;
        }
        self.edit_messages(key, |replies| {
            for editor in &editors {
                editor(&events, replies);
            }
        });
    }

    fn labels_for(&self, key: &str) -> Vec<String> {
        self.tickets
            .lock()
            .unwrap()
            .get(key)
            .map(|t| t.labels.clone())
            .unwrap_or_default()
    }

    /// Name of the model the currently bound agent named `agent_name`
    /// runs. `None` when no such agent is bound.
    fn model_for_agent(&self, agent_name: &str) -> Option<String> {
        self.agents
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.name == agent_name)
            .map(|a| a.model.name.clone())
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

    /// Call off the `label` pool when `predicate(&event)` first returns true.
    /// The label-scoped sibling of [`Self::cancel_on_event`]: instead of stopping
    /// the whole run it invokes [`Self::cancel_label`], so the other pools keep
    /// going. Implemented as one more entry on the [`Self::on_event`] chain.
    pub fn cancel_label_on_event<F>(&self, label: impl Into<String>, predicate: F) -> &Self
    where
        F: Fn(&Event) -> bool + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        let label = label.into();
        self.on_event(move |event| {
            if !predicate(&event) {
                return;
            }
            if let Some(s) = supervisor.upgrade() {
                s.cancel_label(label.clone());
            }
        })
    }

    /// Cancel the run when a finished ticket's result matches `predicate`.
    /// Fires on `TicketFinished`, so the value passed is the stored,
    /// schema-validated result: callers never reach into the finish tool's
    /// input shape. Fail-fast for "stop the whole run on the first result
    /// that means stop".
    pub fn cancel_on_result<F>(&self, predicate: F) -> &Self
    where
        F: Fn(&serde_json::Value) -> bool + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            if !matches!(event.kind, EventKind::TicketFinished) {
                return;
            }
            let key = &event.ticket_key;
            let Some(system) = supervisor.upgrade() else {
                return;
            };
            let Some(result) = system.get_ticket(key).and_then(|t| t.result) else {
                return;
            };
            if predicate(&result) {
                system.cancel();
            }
        })
    }

    /// Enqueue a follow-up ticket whenever a finished ticket makes `make`
    /// return one. Fires on `TicketFinished`, passing the finished ticket
    /// so `make` reads its key, labels, task, and schema-validated result,
    /// and can chain the follow-up via `Ticket::parent`. Guard against a
    /// follow-up that itself re-triggers `make`, or the run never drains.
    pub fn create_ticket_on_result<F>(&self, make: F) -> &Self
    where
        F: Fn(&Ticket) -> Option<Ticket> + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            if !matches!(event.kind, EventKind::TicketFinished) {
                return;
            }
            let key = &event.ticket_key;
            let Some(system) = supervisor.upgrade() else {
                return;
            };
            let Some(finished) = system.get_ticket(key) else {
                return;
            };
            if let Some(ticket) = make(&finished) {
                system.ticket(ticket);
            }
        })
    }

    /// Save a ticket's transcript as `trajectories/<agent>-<key>.json` under
    /// the system's output dir each time `predicate(&event)` returns true.
    /// One more entry on the [`Self::on_event`] chain, like
    /// [`Self::cancel_on_event`]. The write is observational: a failed write
    /// is dropped, matching the ticket lifecycle log. Events with an empty
    /// `ticket_key` (run lifecycle) name no ticket and are skipped.
    pub fn save_trajectory_on_event<F>(&self, predicate: F) -> &Self
    where
        F: Fn(&Event) -> bool + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            if !predicate(&event) {
                return;
            }
            let Some(system) = supervisor.upgrade() else {
                return;
            };
            let Some(ticket) = system.get_ticket(&event.ticket_key) else {
                return;
            };
            let model = system.model_for_agent(&event.agent_name);
            let _ = Trajectory::from_ticket(&event.agent_name, model.as_deref(), &ticket)
                .save(&system.dir_value());
        })
    }

    /// Override the directory under which the system writes
    /// `results.jsonl`, `tickets.jsonl`, and per-ticket
    /// `tickets/<key>/{ticket.json,replies.jsonl}`. Defaults to `./.agentwerk`.
    /// Knowledge co-locates with these files when `Knowledge::open`
    /// points at the same directory.
    pub fn dir(&self, dir: impl Into<PathBuf>) -> &Self {
        *self.dir.lock().unwrap() = dir.into();
        self
    }

    pub(crate) fn dir_value(&self) -> PathBuf {
        self.dir.lock().unwrap().clone()
    }

    /// Register the result schema every ticket carrying `label` validates
    /// against, unless the ticket was created with a schema of its own. The
    /// schema is stamped at creation, so the contract follows the label whether
    /// the ticket came from `task`, `ticket`, or a `finish` handover
    /// child. Mirrors `Stats::stats_for_label`.
    pub fn schema_for_label(&self, label: impl Into<String>, schema: Schema) -> &Self {
        self.label_schemas
            .lock()
            .unwrap()
            .insert(label.into(), schema);
        self
    }

    // ---- ticket-creation API mirrored on Agent ----

    /// Enqueue a ticket carrying `task` as its body. Returns the new
    /// ticket's key.
    pub fn task<T: Serialize>(&self, task: T) -> String {
        self.dispatch(Ticket::new(task))
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

    /// Call off the pool carrying `label`: the loop stops claiming or resuming its
    /// tickets and walks any agent off one it already holds, leaving that ticket
    /// `InProgress` (abandoned), just as [`Self::cancel`] leaves in-flight tickets.
    /// Stops one pool while the rest of the run continues.
    pub fn cancel_label(&self, label: impl Into<String>) -> &Self {
        self.cancelled_labels.lock().unwrap().insert(label.into());
        self
    }

    /// True when any of `labels` names a pool called off via [`Self::cancel_label`].
    /// The loop's claim/resume path reads this to keep a cancelled pool's tickets
    /// off the queue. Locks only `cancelled_labels`, so it is safe to call from a
    /// claim predicate that already holds the `tickets` lock.
    pub(crate) fn labels_cancelled(&self, labels: &[String]) -> bool {
        let cancelled = self.cancelled_labels.lock().unwrap();
        labels.iter().any(|l| cancelled.contains(l))
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
    /// agents list. Returns `&self` so registration chains with
    /// `.task(...)` and the policy builders. To keep a bound handle to
    /// the agent itself, use [`Agent::ticket_system`] instead.
    ///
    /// May be called before or after `run()` / `finish()`. When called
    /// after `run()`, the new agent starts polling for tickets within
    /// roughly one `IDLE_POLL_INTERVAL` (~100 ms).
    pub fn agent(&self, mut agent: Agent) -> &Self {
        self.bind_agent(&mut agent);
        self
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
        self.cancelled_labels.lock().unwrap().clear();
        self.message_editing.lock().unwrap().pending.clear();
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
            let transitions_in_flight = self.terminal_transitions_in_flight.load(Ordering::SeqCst);
            if self.pending_count() == 0 && transitions_in_flight == 0 {
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
    pub fn cancel(&self) -> &Self {
        self.cancel_signal
            .lock()
            .unwrap()
            .store(true, Ordering::Relaxed);
        self.stop_signal
            .lock()
            .unwrap()
            .store(true, Ordering::Relaxed);
        self
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

    /// Most recent finished ticket's result. Deserialize structured
    /// results with `serde_json::from_value`.
    pub fn last_result(&self) -> Option<serde_json::Value> {
        self.results().pop()
    }

    /// Every finished ticket's result, in creation order.
    pub fn results(&self) -> Vec<serde_json::Value> {
        self.find_tickets(|t| t.status == Status::Finished && t.result.is_some())
            .into_iter()
            .filter_map(|t| t.result)
            .collect()
    }

    /// Every finished ticket carrying `label`'s result, in creation
    /// order. Mirrors [`Self::schema_for_label`] and `Stats::stats_for_label`.
    pub fn results_for_label(&self, label: &str) -> Vec<serde_json::Value> {
        self.find_tickets(|t| t.is_finished() && t.has_label(label))
            .into_iter()
            .filter_map(|t| t.result)
            .collect()
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
        let (sys, _tmp) = test_system();
        let alice = minimal_agent("alice").ticket_system(&sys);
        alice.task("first");
        alice.task("second");
        assert_eq!(sys.find_tickets(|t| t.status == Status::Todo).len(), 2);
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
        assert_eq!(
            sys.results(),
            vec![serde_json::json!("first"), serde_json::json!("third")]
        );
    }

    #[test]
    fn last_result_returns_last_done_payload() {
        let (sys, _tmp) = test_system();
        sys.task("a");
        sys.task("b");
        attach_done_result(&sys, "TICKET-2", "second");
        attach_done_result(&sys, "TICKET-1", "first");
        assert_eq!(sys.last_result(), Some(serde_json::json!("second")));
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
        assert_eq!(
            sys.results(),
            vec![
                serde_json::json!("first"),
                serde_json::json!("second"),
                serde_json::json!("third")
            ]
        );
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
        sys.set_finished(&key_a, "agent").unwrap();
        sys.set_failed(&key_b).unwrap();
        assert_eq!(sys.pending_count(), 0);
    }

    #[test]
    fn cancel_label_flags_only_that_label() {
        let (sys, _tmp) = test_system();
        sys.cancel_label("research");

        assert!(sys.labels_cancelled(&["research".into()]));
        // A ticket carries its agent name too once claimed; the pool label still hits.
        assert!(sys.labels_cancelled(&["research".into(), "Threat Researcher 1".into()]));
        assert!(
            !sys.labels_cancelled(&["analysis".into()]),
            "other pools are untouched",
        );
        assert!(!sys.labels_cancelled(&[]));
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
    fn results_for_label_returns_only_matching_label() {
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
        sys.set_finished(&key_a, "agent").unwrap();
        sys.set_result(&key_b, serde_json::json!({"score": 99}))
            .unwrap();
        sys.set_finished(&key_b, "agent").unwrap();
        assert_eq!(
            sys.results_for_label("analysis"),
            vec![serde_json::json!({"score": 7})]
        );
    }

    #[test]
    fn results_for_label_empty_when_no_label_match() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("x").label("other"));
        let key = sys.claim(|t| t.has_label("other"), "agent").unwrap();
        sys.set_result(&key, serde_json::json!({"n": 1})).unwrap();
        sys.set_finished(&key, "agent").unwrap();
        assert!(sys.results_for_label("missing").is_empty());
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
        sys.cancel_on_event(|e| matches!(e.kind, EventKind::TicketFailed));
        sys.emit("KEY", "agent", EventKind::TurnStarted);
        assert!(!sys.is_cancelled());
        sys.emit("KEY", "agent", EventKind::TicketFailed);
        assert!(sys.is_cancelled());
    }

    #[test]
    fn message_editor_receives_buffered_events_excluding_text_chunks() {
        use crate::event::ToolFailureKind;
        let (sys, _tmp) = test_system();
        let key = sys.task("go");

        let seen: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        sys.edit_messages_on_event(move |events, _messages| {
            recorder.lock().unwrap().extend(events.iter().cloned());
        });

        sys.emit(&key, "agent", EventKind::TurnStarted);
        sys.emit(
            &key,
            "agent",
            EventKind::TextChunkReceived {
                content: "hi".into(),
            },
        );
        sys.emit(
            &key,
            "agent",
            EventKind::ToolCallFailed {
                tool_name: "boom".into(),
                call_id: "c1".into(),
                kind: ToolFailureKind::ExecutionFailed,
                message: "boom".into(),
            },
        );
        sys.run_message_editors(&key);

        let events = seen.lock().unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::TurnStarted)));
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ToolCallFailed { .. })));
        assert!(!events
            .iter()
            .any(|e| matches!(e.kind, EventKind::TextChunkReceived { .. })));
    }

    #[test]
    fn message_editor_batch_drains_after_it_runs() {
        let (sys, _tmp) = test_system();
        let key = sys.task("go");

        let runs = Arc::new(Mutex::new(0u32));
        let counter = Arc::clone(&runs);
        sys.edit_messages_on_event(move |_events, _messages| {
            *counter.lock().unwrap() += 1;
        });

        sys.emit(&key, "agent", EventKind::TurnStarted);
        sys.run_message_editors(&key);
        // The batch is drained, so a second run has nothing to react to.
        sys.run_message_editors(&key);

        assert_eq!(*runs.lock().unwrap(), 1);
    }

    #[test]
    fn message_editors_run_in_registration_order() {
        use crate::agents::tickets::ReplyContent;
        let (sys, _tmp) = test_system();
        let key = sys.task("go");
        sys.edit_messages_on_event(|_events, messages| {
            messages.push(Reply::user_text("first"));
        });
        sys.edit_messages_on_event(|_events, messages| {
            messages.push(Reply::user_text("second"));
        });

        sys.emit(&key, "agent", EventKind::TurnStarted);
        sys.run_message_editors(&key);

        let texts: Vec<String> = sys
            .get_ticket(&key)
            .unwrap()
            .replies
            .iter()
            .filter_map(|r| match r.content.first() {
                Some(ReplyContent::Text(t)) => Some(t.clone()),
                _ => None,
            })
            .collect();
        let first = texts.iter().position(|t| t == "first");
        let second = texts.iter().position(|t| t == "second");
        assert!(
            first < second,
            "editors must run in registration order: {texts:?}"
        );
    }

    #[test]
    fn message_editor_sees_only_the_events_of_the_ticket_it_edits() {
        let (sys, _tmp) = test_system();
        let a = sys.task("a");
        let b = sys.task("b");

        let seen: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        sys.edit_messages_on_event(move |events, _messages| {
            recorder.lock().unwrap().extend(events.iter().cloned());
        });

        sys.emit(&a, "agent", EventKind::TurnStarted);
        sys.emit(&b, "agent", EventKind::TicketFailed);
        sys.run_message_editors(&a);

        let events = seen.lock().unwrap();
        assert!(
            events.iter().all(|e| e.ticket_key == a),
            "editor for ticket {a} must not see another ticket's events: {events:?}"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, EventKind::TurnStarted)));
    }

    #[test]
    fn edit_messages_edits_the_transcript_on_demand() {
        use crate::agents::tickets::ReplyContent;
        let (sys, _tmp) = test_system();
        let key = sys.task("go");
        sys.add_reply(&key, Reply::user_text("keep me"));
        sys.add_reply(&key, Reply::user_text("drop me"));

        sys.edit_messages(&key, |messages| {
            messages.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text(t)) if t == "drop me")
            });
        });

        let replies = sys.get_ticket(&key).unwrap().replies;
        assert!(replies
            .iter()
            .any(|r| matches!(r.content.first(), Some(ReplyContent::Text(t)) if t == "keep me")));
        assert!(replies
            .iter()
            .all(|r| !matches!(r.content.first(), Some(ReplyContent::Text(t)) if t == "drop me")));
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
    fn cancel_on_result_trips_when_finished_result_matches() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("x").label("L"));
        let key = sys.claim(|t| t.has_label("L"), "agent").unwrap();
        sys.set_result(&key, serde_json::json!({"status": "malicious"}))
            .unwrap();
        sys.cancel_on_result(|r| r.get("status").and_then(|v| v.as_str()) == Some("malicious"));
        assert!(!sys.is_cancelled());
        sys.emit(&key, "agent", EventKind::TicketFinished);
        assert!(sys.is_cancelled());
    }

    #[test]
    fn cancel_on_result_ignores_nonmatching_result() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("x").label("L"));
        let key = sys.claim(|t| t.has_label("L"), "agent").unwrap();
        sys.set_result(&key, serde_json::json!({"status": "benign"}))
            .unwrap();
        sys.cancel_on_result(|r| r.get("status").and_then(|v| v.as_str()) == Some("malicious"));
        sys.emit(&key, "agent", EventKind::TicketFinished);
        assert!(!sys.is_cancelled());
    }

    #[test]
    fn create_ticket_on_result_enqueues_follow_up_for_finished_ticket() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("scout").label("scout"));
        let key = sys.claim(|t| t.has_label("scout"), "agent").unwrap();
        sys.set_result(&key, serde_json::json!("lead")).unwrap();
        sys.set_finished(&key, "agent").unwrap();
        sys.create_ticket_on_result(|done| {
            done.has_label("scout")
                .then(|| Ticket::new("hunt").label("sniper"))
        });
        sys.emit(&key, "agent", EventKind::TicketFinished);
        assert_eq!(sys.find_tickets(|t| t.has_label("sniper")).len(), 1);
    }

    #[test]
    fn create_ticket_on_result_links_follow_up_to_finished_parent() {
        let (sys, _tmp) = test_system();
        sys.ticket(Ticket::new("scout").label("scout"));
        let key = sys.claim(|t| t.has_label("scout"), "agent").unwrap();
        sys.set_result(&key, serde_json::json!("lead")).unwrap();
        sys.set_finished(&key, "agent").unwrap();
        sys.create_ticket_on_result(|done| {
            Some(Ticket::new("hunt").label("sniper").parent(&done.key))
        });
        sys.emit(&key, "agent", EventKind::TicketFinished);
        let spawned = sys.find_ticket(|t| t.has_label("sniper")).unwrap();
        assert_eq!(spawned.parent, Some(key));
    }

    #[test]
    fn create_ticket_on_result_ignores_unfinished_events() {
        let (sys, _tmp) = test_system();
        sys.create_ticket_on_result(|_| Some(Ticket::new("follow-up").label("next")));
        sys.emit("KEY", "agent", EventKind::TurnStarted);
        assert!(sys.tickets().is_empty());
    }

    #[test]
    fn create_ticket_on_result_inserts_follow_up_before_drain_is_observable() {
        let (sys, _tmp) = test_system();
        sys.create_ticket_on_result(|done| {
            done.has_label("scout")
                .then(|| Ticket::new("hunt").label("sniper"))
        });
        sys.ticket(Ticket::new("scout").label("scout"));
        let key = sys.claim(|t| t.has_label("scout"), "agent").unwrap();
        sys.set_result(&key, serde_json::json!("lead")).unwrap();
        sys.set_finished(&key, "agent").unwrap();
        // The handler ran inside `set_finished`, so the queue is never
        // observably empty between the parent finishing and the follow-up.
        assert_eq!(sys.pending_count(), 1);
    }

    #[test]
    fn save_trajectory_on_event_writes_on_matching_event() {
        let (sys, tmp) = test_system();
        sys.save_trajectory_on_event(|e| matches!(e.kind, EventKind::TicketFinished));
        sys.ticket(Ticket::new("scan").label("scan"));
        let key = sys.claim(|t| t.has_label("scan"), "analyst").unwrap();
        sys.add_reply(&key, Reply::user_text("hello"));
        sys.set_result(&key, serde_json::json!("done")).unwrap();
        sys.set_finished(&key, "analyst").unwrap();

        let path = tmp
            .path()
            .join("trajectories")
            .join(format!("analyst-{key}.json"));
        assert!(path.exists());
        let trajectory: Trajectory =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(trajectory.key, format!("analyst-{key}"));
        assert!(!trajectory.messages.is_empty());
    }

    #[test]
    fn save_trajectory_on_event_skips_non_matching_events() {
        let (sys, tmp) = test_system();
        sys.save_trajectory_on_event(|e| matches!(e.kind, EventKind::TicketFinished));
        sys.emit("KEY", "analyst", EventKind::TurnStarted);
        assert!(!tmp.path().join("trajectories").exists());
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
