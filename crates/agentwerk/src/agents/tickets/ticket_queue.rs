//! The shared queue agents claim tickets from, and the lifecycle that drives them.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::event::{default_logger, Event, EventKind, FinishReason};
use crate::persistence::Persist;
use crate::providers::ProviderResult;
use crate::schemas::SchemaStore;

use super::super::agent::{Agent, TicketQueueRef};
use super::super::compaction::Compaction;
use super::super::policy::Policies;
use super::super::r#loop::run_main_loop;
use super::super::stats::Stats;
use super::query::{Query, TicketMatcher};
use super::ticket::{Status, Ticket};
use super::{numeric_id, policy_violated_kind, Reply};

type EventHandler = dyn Fn(&Event) + Send + Sync;

/// Which tickets a lifecycle call speaks for. `cancel` stores one; `finish`
/// borrows one for the length of the wait.
pub(crate) type TicketFilter = dyn Fn(&Ticket) -> bool + Send + Sync;

/// How many events a `finish` waiter may fall behind before it starts
/// missing them. `TextChunkReceived` fires once per streaming delta and sets
/// the volume this has to absorb.
const EVENT_STREAM_CAPACITY: usize = 1024;

type AsyncResultHandler = dyn Fn(Ticket, serde_json::Value) -> HandlerWork + Send + Sync;

type AsyncResultsHandler = dyn Fn(Vec<serde_json::Value>) -> HandlerWork + Send + Sync;

/// Boxed so the queue can hold handlers with different future types.
type HandlerWork = Pin<Box<dyn Future<Output = ()> + Send>>;

type ReplyEditor = dyn Fn(&[Event], &mut Vec<Reply>) + Send + Sync;

type CompactionEditor = dyn Fn(Compaction, Vec<Reply>) -> EditedReplies + Send + Sync;

/// The replies an editor hands back, once its work finishes.
type EditedReplies = Pin<Box<dyn Future<Output = ProviderResult<Vec<Reply>>> + Send>>;

/// A finished ticket and its result, with every result there was when it landed.
type Delivery = (Ticket, serde_json::Value, Vec<serde_json::Value>);

/// `emit` runs on an agent that has to carry on, so a finished result is only
/// queued here; whichever `finish` is waiting drains it and awaits the
/// handlers.
///
/// `queued` and `draining` are separate locks because `emit` pushes without
/// awaiting, while the drain guard is held across handler awaits.
#[derive(Default)]
pub(super) struct AwaitedResults {
    pub(super) per_result: Mutex<Vec<Arc<AsyncResultHandler>>>,
    pub(super) all_results: Mutex<Vec<Arc<AsyncResultsHandler>>>,
    pub(super) queued: Mutex<VecDeque<Delivery>>,
    pub(super) draining: tokio::sync::Mutex<()>,
    /// A concurrent registration waits for the hook rather than adding a second.
    pub(super) queueing: OnceLock<()>,
}

/// The registered reply editor and the per-ticket events buffered for it since
/// each ticket's previous request. The two are always touched together.
#[derive(Default)]
pub(super) struct ReplyEditing {
    pub(super) editor: Option<Arc<ReplyEditor>>,
    pub(super) pending: HashMap<String, Vec<Event>>,
}

/// Where a run is, and the wake for anything waiting on it.
///
/// A `watch` channel is both the value and the notification, and its sender
/// takes `&self`, so this needs no lock and no separate flag. Naming the three
/// phases keeps the contradiction the two-flag version allowed, a run complete
/// without a reason, out of the type.
pub(crate) struct Run {
    phase: watch::Sender<Phase>,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Agents may claim.
    Working,
    /// The reason is known and the agents are stopping.
    Draining(FinishReason),
    /// Every agent has stopped and `RunFinished` has been announced.
    Finished(FinishReason),
}

impl Default for Run {
    fn default() -> Self {
        Self {
            phase: watch::Sender::new(Phase::Working),
        }
    }
}

impl Run {
    /// Name the ending, keeping the first reason given.
    pub(crate) fn set_draining(&self, reason: FinishReason) {
        self.phase.send_if_modified(|phase| match phase {
            Phase::Working => {
                *phase = Phase::Draining(reason);
                true
            }
            _ => false,
        });
    }

    /// Record that every agent has stopped and the reason is announced.
    pub(crate) fn set_finished(&self) {
        self.phase.send_if_modified(|phase| match *phase {
            Phase::Draining(reason) => {
                *phase = Phase::Finished(reason);
                true
            }
            _ => false,
        });
    }

    pub(crate) fn is_working(&self) -> bool {
        *self.phase.borrow() == Phase::Working
    }

    pub(crate) fn is_finished(&self) -> bool {
        matches!(*self.phase.borrow(), Phase::Finished(_))
    }

    /// Why the run ended, or `None` while it is still working.
    pub(crate) fn reason(&self) -> Option<FinishReason> {
        match *self.phase.borrow() {
            Phase::Working => None,
            Phase::Draining(reason) | Phase::Finished(reason) => Some(reason),
        }
    }

    /// Resolves once the run is no longer working, whatever the reason.
    pub(crate) async fn until_draining(&self) {
        // `wait_for` reads the current value before it waits, so a phase
        // landing between the two cannot be missed.
        let _ = self
            .phase
            .subscribe()
            .wait_for(|p| *p != Phase::Working)
            .await;
    }

    /// Resolves once every agent has stopped. A caller that starts another run
    /// awaits this first, or the two overlap.
    pub(crate) async fn until_finished(&self) {
        let _ = self
            .phase
            .subscribe()
            .wait_for(|p| matches!(p, Phase::Finished(_)))
            .await;
    }

    fn reset(&self) {
        self.phase.send_replace(Phase::Working);
    }
}

/// The core data structure of agentwerk, coordinating complex work across
/// agents. Many agents share one `TicketQueue` and pick up tickets
/// concurrently; a label assigns work to the right agents.
///
/// ```no_run
/// use agentwerk::{Agent, Ticket, TicketQueue};
/// use agentwerk::tools::FetchUrlTool;
///
/// # async fn run() {
/// let tickets = TicketQueue::new();
/// for _ in 0..4 {
///     tickets.agent(
///         Agent::from_env()
///             .label("research")
///             .tool(FetchUrlTool::new())
///             .build(),
///     );
/// }
/// tickets.ticket(Ticket::labeled("research", "Summarize https://canvascomputing.org"));
/// tickets.finish_all().await;
/// # }
/// ```
///
/// # Sessions
///
/// A `TicketQueue` writes every ticket, reply, statistic, and lifecycle
/// event to its working directory (default `./.agentwerk`). That directory is
/// the session: stop the process, and `TicketQueue::load(dir)` reopens it
/// from disk and continues from where it stopped.
///
/// ```no_run
/// use agentwerk::TicketQueue;
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let tickets = TicketQueue::load(".agentwerk")?;
/// // Re-register the agents, then call .start() or .finish_all().await.
/// # let _ = tickets;
/// # Ok(())
/// # }
/// ```
///
/// On-disk layout:
///
/// ```text
/// .agentwerk/
/// ├── events.jsonl                          every event (one per line)
/// ├── tickets/
/// │   └── TICKET-1/
/// │       ├── ticket.json                   the ticket without its messages or result
/// │       ├── result.json                   the result the agent produced
/// │       ├── replies.jsonl                 every message exchanged with the model, one per line
/// │       └── outputs/<tool_use_id>.txt     full tool outputs spilled out of the messages
/// └── knowledge/
///     ├── pages/<slug>.md                   knowledge pages
///     └── index.md                          knowledge index
/// ```
pub struct TicketQueue {
    pub(super) weak_self: Weak<TicketQueue>,
    pub(crate) tickets: Mutex<HashMap<String, Ticket>>,
    pub(super) agents: Mutex<Vec<Agent>>,
    pub(super) policies: Mutex<Policies>,
    /// Why the run ended, once the main loop decides. The agent tasks, the
    /// tools, and every `finish` read it to know the run is over.
    pub(crate) run: Arc<Run>,
    /// What `cancel` has taken off the queue. A matching ticket is neither
    /// claimed nor resumed, and an agent already holding one is taken off it,
    /// while the rest of the run continues.
    pub(crate) cancel_filters: Mutex<Vec<Arc<TicketFilter>>>,
    /// How many terminal status transitions are between their status change and
    /// the return of their event handlers. `pending` counts a non-zero value as
    /// pending work, so a handler creating a follow-up ticket always beats the drain.
    pub(crate) terminal_transitions_in_flight: AtomicUsize,
    pub(crate) stats: Stats,
    pub(super) event_handlers: Mutex<Vec<Arc<EventHandler>>>,
    pub(super) awaited_results: AwaitedResults,
    /// Every emitted event, for `finish` to wake on. A separate channel rather
    /// than one more `on_event` entry: a handler stays on the chain for the
    /// life of the queue, so one registered per call would grow without bound
    /// in a host that awaits in a loop.
    pub(super) event_stream: broadcast::Sender<Event>,
    /// The editor that rewrites a ticket's replies before each request, plus the
    /// per-ticket events buffered for it. Buffering runs whether or not an
    /// editor is installed.
    pub(super) reply_editing: Mutex<ReplyEditing>,
    /// What compaction does with a ticket's replies. `None` summarizes them.
    pub(super) compaction_editor: Mutex<Option<Arc<CompactionEditor>>>,
    /// What corrects an agent asked to try again. `None` keeps the built-in
    /// directive.
    /// The result contracts bound to labels, read once per claim. `None` leaves
    /// every ticket with whatever schema it was built with.
    pub(super) schemas: Mutex<Option<Arc<SchemaStore>>>,
    pub(super) dir: Mutex<PathBuf>,
    pub(super) events_lock: Mutex<()>,
    /// The main loop, held so `start()` can join a previous one before starting
    /// the next.
    pub(super) join_handle: Mutex<Option<JoinHandle<()>>>,
    /// Next `TICKET-<N>` key to hand out, or `None` until it is known.
    /// `load()` seeds it from the tickets it just read off disk. `new()` leaves
    /// it `None` and the first `insert()` scans for the highest existing key,
    /// since `new()` never reads the directory itself.
    pub(super) next_ticket_id: Mutex<Option<u64>>,
}

impl TicketQueue {
    /// Create an empty ticket queue, shared through an `Arc`.
    pub fn new() -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            weak_self: weak.clone(),
            tickets: Mutex::new(HashMap::new()),
            agents: Mutex::new(Vec::new()),
            policies: Mutex::new(Policies::default()),
            run: Arc::new(Run::default()),
            cancel_filters: Mutex::new(Vec::new()),
            terminal_transitions_in_flight: AtomicUsize::new(0),
            stats: Stats::new(),
            event_handlers: Mutex::new(Vec::new()),
            awaited_results: AwaitedResults::default(),
            event_stream: broadcast::Sender::new(EVENT_STREAM_CAPACITY),
            reply_editing: Mutex::new(ReplyEditing::default()),
            compaction_editor: Mutex::new(None),
            schemas: Mutex::new(None),
            dir: Mutex::new(PathBuf::from(".agentwerk")),
            events_lock: Mutex::new(()),
            join_handle: Mutex::new(None),
            next_ticket_id: Mutex::new(None),
        })
    }

    /// Continue a session from `tickets_dir`, or start one there when it is empty.
    ///
    /// Every ticket is read back with its status, result, and messages, and the
    /// statistics resume from `events.jsonl`, so the turn and token budgets
    /// policies check stay continuous across restarts. Pointing this and
    /// `Knowledge::load` at the same directory keeps the knowledge pages beside
    /// the session.
    ///
    /// An unfinished ticket is picked up again by the agent whose id it carries
    /// as its assignee. Ids are numbered per label as agents are built, so
    /// build the same agents in the same order after a restart.
    ///
    /// A ticket that cannot be read stops the load and the returned error names
    /// it, rather than handing back a store quietly missing that ticket. Files
    /// written by an older version are the usual cause: delete the session
    /// directory, or migrate it, and load again.
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
                // Skipping an unreadable ticket would drop its status, result
                // and timestamps with it, leaving the queue to resume work it
                // has no record of.
                let ticket = Ticket::load(&tickets_dir, &key).map_err(|source| {
                    io::Error::new(
                        source.kind(),
                        format!("ticket {key} could not be read: {source}"),
                    )
                })?;
                tickets.insert(ticket.key.clone(), ticket);
            }
        }

        // The clock starts over: `max_time` bounds this run, not the one that
        // wrote the log.
        let stats = Stats::load(&tickets_dir).unwrap_or_else(|_| Stats::new());
        stats.restart_clock();
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
            run: Arc::new(Run::default()),
            cancel_filters: Mutex::new(Vec::new()),
            terminal_transitions_in_flight: AtomicUsize::new(0),
            stats,
            event_handlers: Mutex::new(Vec::new()),
            awaited_results: AwaitedResults::default(),
            event_stream: broadcast::Sender::new(EVENT_STREAM_CAPACITY),
            reply_editing: Mutex::new(ReplyEditing::default()),
            compaction_editor: Mutex::new(None),
            schemas: Mutex::new(None),
            dir: Mutex::new(tickets_dir),
            events_lock: Mutex::new(()),
            join_handle: Mutex::new(None),
            next_ticket_id: Mutex::new(Some(next_id)),
        }))
    }

    /// Get the input tokens across the run's finished requests.
    ///
    /// Counted as the requests finish, so this reports what the run has spent
    /// even when the log it wrote is gone.
    pub fn input_tokens(&self) -> u64 {
        self.stats.input_tokens()
    }

    /// Get the output tokens across the run's finished requests.
    pub fn output_tokens(&self) -> u64 {
        self.stats.output_tokens()
    }

    /// Get the elapsed duration, which keeps growing while agents work and
    /// stops when execution ends. `None` until the first ticket starts.
    pub fn execution_duration(&self) -> Option<Duration> {
        self.stats.execution_duration()
    }

    /// Push an event observer onto the handler chain. Every installed
    /// handler fires on every event, in installation order. Handlers
    /// must be cheap and non-blocking. When no handler has been
    /// installed, [`default_logger`] runs in its place.
    pub fn on_event(&self, handler: impl Fn(&Event) + Send + Sync + 'static) -> &Self {
        self.event_handlers.lock().unwrap().push(Arc::new(handler));
        self
    }

    /// The filter-resolve-call shape the ticket handlers share. The queue is
    /// handed in so one that files follow-up work needs no upgrade of its own.
    fn on_ticket_event<F>(&self, wanted: fn(&EventKind) -> bool, handler: F) -> &Self
    where
        F: Fn(&Arc<Self>, &Event, &Ticket) + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            if !wanted(&event.kind) {
                return;
            }
            let Some(queue) = supervisor.upgrade() else {
                return;
            };
            let Some(ticket) = queue.get_ticket(&event.ticket_key) else {
                return;
            };
            handler(&queue, event, &ticket);
        })
    }

    /// Read every finished ticket together with its result.
    ///
    /// The value handed over is the stored, schema-validated result, so a
    /// handler never reaches into the finish tool's input shape. This is one
    /// more entry on the [`Self::on_event`] chain.
    pub fn on_result<F>(&self, handler: F) -> &Self
    where
        F: Fn(&Ticket, &serde_json::Value) + Send + Sync + 'static,
    {
        self.on_ticket_event(
            |kind| matches!(kind, EventKind::TicketFinished),
            move |_, _, finished| {
                let Some(result) = &finished.result else {
                    return;
                };
                handler(finished, result);
            },
        )
    }

    /// Read every finished ticket together with its result, in a handler
    /// [`Self::finish`] waits for before it returns.
    ///
    /// [`Self::on_result`] cannot await: it runs on the agent task that just
    /// finished the ticket, and that task has to carry on. This one hands the
    /// work to whichever [`Self::finish`] is waiting, which awaits each
    /// handler as the results land. In Python that puts the handler on the
    /// caller's event loop, so work that has to stay serialized against the
    /// caller's own, such as a commit, can be.
    ///
    /// Handlers therefore run only while a `finish` or [`Self::finish_all`] is
    /// awaited. A host that only calls [`Self::start`] uses `on_result`: each
    /// result waiting to be handed over holds a copy of its ticket and every
    /// reply in it, so a run that never drains grows for as long as it lasts.
    ///
    /// Your handler MUST NOT call `finish` or `finish_all`, or it waits forever
    /// on the handover it is running inside.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// # async fn run() {
    /// let tickets = TicketQueue::new();
    /// tickets.on_result_async(|ticket, result| async move {
    ///     println!("{} produced {result}", ticket.key);
    /// });
    /// tickets.finish_all().await;
    /// # }
    /// ```
    pub fn on_result_async<F, Fut>(&self, handler: F) -> &Self
    where
        F: Fn(Ticket, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let boxed: Arc<AsyncResultHandler> =
            Arc::new(move |ticket, result| Box::pin(handler(ticket, result)));
        self.queue_finished_results();
        self.awaited_results.per_result.lock().unwrap().push(boxed);
        self
    }

    /// Read every result the run has produced so far, each time one lands, in
    /// a handler [`Self::finish`] waits for before it returns.
    ///
    /// [`Self::on_results`] on the terms [`Self::on_result_async`] sets: the
    /// same list [`Self::results`] gives after the run, handed over while it
    /// is still going. Use it to act on a condition across results with work
    /// that has to be awaited.
    ///
    /// Your handler MUST NOT call `finish` or [`Self::finish_all`], or it waits
    /// forever on the handover it is running inside.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// # async fn run() {
    /// let tickets = TicketQueue::new();
    /// tickets.on_results_async(|results| async move {
    ///     println!("{} results so far", results.len());
    /// });
    /// tickets.finish_all().await;
    /// # }
    /// ```
    pub fn on_results_async<F, Fut>(&self, handler: F) -> &Self
    where
        F: Fn(Vec<serde_json::Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let boxed: Arc<AsyncResultsHandler> = Arc::new(move |results| Box::pin(handler(results)));
        self.queue_finished_results();
        self.awaited_results.all_results.lock().unwrap().push(boxed);
        self
    }

    /// Installed only once, or a second registration would queue every result
    /// twice.
    fn queue_finished_results(&self) {
        self.awaited_results.queueing.get_or_init(|| {
            self.on_ticket_event(
                |kind| matches!(kind, EventKind::TicketFinished),
                |queue, _, finished| {
                    let Some(result) = &finished.result else {
                        return;
                    };
                    // Taken now rather than at handoff, so a handler sees what had
                    // landed when its result did, as the synchronous `on_results`
                    // does. Skipped while nothing wants it, since it copies them all.
                    let wants_all = !queue.awaited_results.all_results.lock().unwrap().is_empty();
                    let so_far = match wants_all {
                        true => queue.results(),
                        false => Vec::new(),
                    };
                    queue.awaited_results.queued.lock().unwrap().push_back((
                        finished.clone(),
                        result.clone(),
                        so_far,
                    ));
                },
            );
        });
    }

    /// Loops because a handler that takes a while lets more results queue up
    /// behind it. One is taken per lock, so a `finish` dropped mid-handover, by
    /// a timeout or a panic, loses only the result it was on.
    async fn await_result_handlers(&self) {
        let per_result = self.awaited_results.per_result.lock().unwrap().clone();
        let all_results = self.awaited_results.all_results.lock().unwrap().clone();
        if per_result.is_empty() && all_results.is_empty() {
            return;
        }
        // Waits rather than skips, or a second `finish` could return while its
        // own results were still being handed over.
        let _draining = self.awaited_results.draining.lock().await;
        loop {
            let next = self.awaited_results.queued.lock().unwrap().pop_front();
            let Some((ticket, result, so_far)) = next else {
                return;
            };
            for handler in &per_result {
                handler(ticket.clone(), result.clone()).await;
            }
            for handler in &all_results {
                handler(so_far.clone()).await;
            }
        }
    }

    /// Read every result the run has produced so far, each time one lands.
    ///
    /// Where [`Self::on_result`] hands over the one ticket that just finished,
    /// this hands over all of them: the same [`Self::results`] a caller reads
    /// after the run, in creation order, delivered while it is still going.
    /// That is what lets a handler act on a condition across results rather
    /// than on any single one, such as every result it was waiting for having
    /// arrived.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// let tickets = TicketQueue::new();
    /// tickets.on_results(|results| {
    ///     if results.len() == 3 {
    ///         println!("every scan landed");
    ///     }
    /// });
    /// ```
    pub fn on_results<H>(&self, handler: H) -> &Self
    where
        H: Fn(&[serde_json::Value]) + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            if !matches!(event.kind, EventKind::TicketFinished) {
                return;
            }
            let Some(queue) = supervisor.upgrade() else {
                return;
            };
            handler(&queue.results());
        })
    }

    /// Read every failure together with the ticket it happened in:
    /// `TicketFailed`, `RequestFailed`, `ToolCallFailed`, `FileOpenFailed`,
    /// `KnowledgeFailed`, and `CompactionFailed`.
    ///
    /// Match on `event.kind` to tell a failure that ends the ticket from one
    /// the agent works around. Each call copies the ticket's replies, so an
    /// agent that fails many tool calls pays that copy once per failure.
    pub fn on_failure<F>(&self, handler: F) -> &Self
    where
        F: Fn(&Event, &Ticket) + Send + Sync + 'static,
    {
        self.on_ticket_event(EventKind::is_failure, move |_, event, ticket| {
            handler(event, ticket)
        })
    }

    /// Rewrite a ticket's replies before its next request.
    ///
    /// Your function receives the events emitted for that ticket since its
    /// previous request and the replies themselves, and changes them in place:
    /// drop a reply, rewrite one, or add one. Match on `event.kind` to act only
    /// on what you care about, such as a tool failure or a stalled turn, and
    /// keep the work cheap, since it runs between requests.
    ///
    /// The edit is permanent. It changes the stored replies and rewrites them
    /// on disk, so a dropped message stays gone from what the model sees, now
    /// and after the session is continued, and no superseded file is left
    /// behind. Keep the replies well-formed: a `tool_use` and its `tool_result`
    /// span two replies, so drop both sides together or the LLM provider
    /// rejects the unpaired block.
    ///
    /// One editor is held at a time, and installing a second replaces the
    /// first, the way [`Self::dir`] or [`Self::max_turns`] do. Two rewriters of
    /// one ticket's replies would each see the other's output, so stack edits
    /// inside a single editor instead.
    ///
    /// Act only when the event batch says so. A retry after compaction
    /// reassembles the request, so an editor that ignores the now empty batch
    /// would edit the same replies twice.
    ///
    /// ```no_run
    /// use agentwerk::TicketQueue;
    /// use agentwerk::event::EventKind;
    /// use agentwerk::agents::tickets::{Reply, ReplyContent};
    ///
    /// let tickets = TicketQueue::new();
    /// tickets.edit_replies_on_event(|events, replies| {
    ///     let failed = events
    ///         .iter()
    ///         .any(|e| matches!(e.kind, EventKind::ToolCallFailed { .. }));
    ///     if !failed {
    ///         return;
    ///     }
    ///     // Drop both sides of the failed exchange: the assistant's tool_use
    ///     // and the failed tool_result, so no unpaired block is left behind.
    ///     replies.retain(|reply| {
    ///         !reply.content.iter().any(|b| {
    ///             matches!(
    ///                 b,
    ///                 ReplyContent::ToolUse { .. }
    ///                     | ReplyContent::ToolResult { succeeded: false, .. }
    ///             )
    ///         })
    ///     });
    ///     replies.push(Reply::user_text("That approach failed. Re-read the file first."));
    /// });
    /// ```
    pub fn edit_replies_on_event(
        &self,
        editor: impl Fn(&[Event], &mut Vec<Reply>) + Send + Sync + 'static,
    ) -> &Self {
        let had_editor = {
            let mut editing = self.reply_editing.lock().unwrap();
            editing.editor.replace(Arc::new(editor)).is_some()
        };
        if had_editor {
            return self;
        }

        // Buffer each ticket's events for the editors as one more `on_event`
        // handler, installed once with the first editor.
        let supervisor = self.weak_self.clone();
        let buffer_event = move |event: &Event| {
            // Streaming chunks carry no editing signal; run-lifecycle events
            // (empty key) belong to no ticket.
            if event.ticket_key.is_empty()
                || matches!(event.kind, EventKind::TextChunkReceived { .. })
            {
                return;
            }
            let Some(queue) = supervisor.upgrade() else {
                return;
            };
            queue
                .reply_editing
                .lock()
                .unwrap()
                .pending
                .entry(event.ticket_key.clone())
                .or_default()
                .push(event.clone());
        };
        self.on_event(buffer_event);
        self
    }

    /// Decide what compaction does with a ticket's replies.
    ///
    /// Compaction fires when the next request would not fit the model's context
    /// window, from [`Self::compact_at`] or the built-in distance below it. Your
    /// function receives the [`Compaction`] and the replies as they stand, and
    /// returns the ones to keep. Summarizing them into a single message is what
    /// happens without this hook, and [`Compaction::summarize`] is that
    /// summarizer, so an editor can call it for part of the replies and handle
    /// the rest itself.
    ///
    /// Returning the replies unchanged says compaction found nothing to drop.
    /// After an overflow the ticket then fails, because the next request would
    /// overflow again. Returning an error fails the ticket and emits
    /// `EventKind::CompactionFailed`.
    ///
    /// The edit is permanent, like [`Self::edit_replies_on_event`]: it rewrites
    /// the stored replies on disk. Keep them well-formed, since a `tool_use` and
    /// its `tool_result` span two replies and the LLM provider rejects an
    /// unpaired block.
    ///
    /// One editor is held at a time, and installing a second replaces the first.
    ///
    /// ```no_run
    /// use agentwerk::TicketQueue;
    ///
    /// let tickets = TicketQueue::new();
    /// // Summarize what falls off, keep the last two turns as they were.
    /// tickets.edit_replies_on_compaction(|compaction, replies| async move {
    ///     let cut = replies.len().saturating_sub(4);
    ///     let summary = compaction.summarize(&replies[..cut]).await?;
    ///     let mut kept = vec![agentwerk::Reply::user_text(summary)];
    ///     kept.extend_from_slice(&replies[cut..]);
    ///     Ok(kept)
    /// });
    /// ```
    pub fn edit_replies_on_compaction<F, Fut>(&self, editor: F) -> &Self
    where
        F: Fn(Compaction, Vec<Reply>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ProviderResult<Vec<Reply>>> + Send + 'static,
    {
        let boxed: Arc<CompactionEditor> =
            Arc::new(move |compaction, replies| Box::pin(editor(compaction, replies)));
        *self.compaction_editor.lock().unwrap() = Some(boxed);
        self
    }

    /// The installed compaction editor, cloned out so the lock is released
    /// before the loop awaits it.
    pub(crate) fn compaction_editor(&self) -> Option<Arc<CompactionEditor>> {
        self.compaction_editor.lock().unwrap().clone()
    }

    /// Publish `kind` and hand back the event it became, so a caller that also
    /// acts on it works from what every observer saw.
    pub(crate) fn emit(&self, key: &str, agent: &str, kind: EventKind) -> Event {
        let event = Event::new(agent, key, self.label_for(key), kind);
        self.stats.record(&event);
        // Published before the handlers run: a `finish` waiter competes
        // with them for nothing, and no handler can swallow the event. The
        // receiver count is checked first so a run with no waiter never pays the
        // clone, which `TextChunkReceived` would otherwise charge per token.
        if self.event_stream.receiver_count() > 0 {
            let _ = self.event_stream.send(event.clone());
        }
        // The chunk kinds are the exception: one per streamed token would
        // outweigh every other line and repeats what `replies.jsonl` holds.
        if !matches!(event.kind, EventKind::TextChunkReceived { .. }) {
            let _guard = self.events_lock.lock().unwrap();
            let _ = Stats::append(&self.get_dir(), &event);
        }
        let handlers: Vec<Arc<EventHandler>> = self.event_handlers.lock().unwrap().clone();
        if handlers.is_empty() {
            default_logger()(&event);
            return event;
        }
        for h in &handlers {
            h(&event);
        }
        event
    }

    /// Apply the registered editor to `key`'s replies, handing it the events
    /// buffered since the ticket's previous request and draining that batch.
    /// Does nothing until an editor is registered, or while no events are pending.
    pub(crate) fn run_reply_editor(&self, key: &str) {
        let (editor, events) = {
            let mut editing = self.reply_editing.lock().unwrap();
            let Some(editor) = editing.editor.clone() else {
                return;
            };
            let events = editing.pending.remove(key).unwrap_or_default();
            (editor, events)
        };
        if events.is_empty() {
            return;
        }
        self.edit_replies(key, |replies| editor(&events, replies));
    }

    fn label_for(&self, key: &str) -> Option<String> {
        self.tickets
            .lock()
            .unwrap()
            .get(key)
            .and_then(|t| t.label.clone())
    }

    /// Get the model that agent runs, or `None` when no agent of that name is bound.
    ///
    /// Pairs with [`Self::on_ticket`]: the event names the agent, this names
    /// its model, and [`Trajectory::from_ticket`] needs both.
    ///
    /// [`Trajectory::from_ticket`]: super::Trajectory::from_ticket
    pub fn model_for_agent(&self, agent_id: &str) -> Option<String> {
        self.agents
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.id == agent_id)
            .map(|a| a.model.name.clone())
    }

    pub(crate) fn policies(&self) -> Policies {
        self.policies.lock().unwrap().clone()
    }

    /// Limit the total number of turns.
    pub fn max_turns(&self, count: u32) -> &Self {
        self.policies.lock().unwrap().max_turns = Some(count);
        self
    }

    /// Limit the total input tokens.
    pub fn max_input_tokens(&self, count: u64) -> &Self {
        self.policies.lock().unwrap().max_input_tokens = Some(count);
        self
    }

    /// Limit the total output tokens.
    pub fn max_output_tokens(&self, count: u64) -> &Self {
        self.policies.lock().unwrap().max_output_tokens = Some(count);
        self
    }

    /// Limit the output tokens of a single request.
    pub fn max_request_tokens(&self, count: u32) -> &Self {
        self.policies.lock().unwrap().max_request_tokens = Some(count);
        self
    }

    /// Limit the consecutive turns without a valid tool call; any successful
    /// call resets the count.
    pub fn max_schema_retries(&self, count: u32) -> &Self {
        self.policies.lock().unwrap().max_schema_retries = Some(count);
        self
    }

    /// Limit how often a failing request is retried.
    pub fn max_request_retries(&self, count: u32) -> &Self {
        self.policies.lock().unwrap().max_request_retries = count;
        self
    }

    /// Wait this long between retries.
    pub fn request_retry_delay(&self, duration: Duration) -> &Self {
        self.policies.lock().unwrap().request_retry_delay = duration;
        self
    }

    /// Limit the total elapsed duration.
    ///
    /// On reaching it, `finish` stops with
    /// `FinishReason::PolicyViolated(PolicyKind::Time)` and emits the matching
    /// `PolicyViolated` event.
    pub fn max_time(&self, duration: Duration) -> &Self {
        self.policies.lock().unwrap().max_time = Some(duration);
        self
    }

    /// Compact once the model's context window is this full.
    ///
    /// Unset, a built-in fraction applies. The value is clamped to `0.0..=1.0`,
    /// and a fraction near zero compacts every turn. Nothing happens on a model
    /// whose window is unknown.
    ///
    /// This is a trigger, not a limit: reaching it costs a summary, not the run.
    pub fn compact_at(&self, fraction: f64) -> &Self {
        // NaN survives `clamp` and would put the threshold at zero, compacting
        // every turn. A full window is the harmless reading of nonsense.
        let fraction = if fraction.is_nan() {
            1.0
        } else {
            fraction.clamp(0.0, 1.0)
        };
        self.policies.lock().unwrap().compact_at = Some(fraction);
        self
    }

    /// Get the turn limit, or `None` when there is none.
    pub fn get_max_turns(&self) -> Option<u32> {
        self.policies.lock().unwrap().max_turns
    }

    /// Get the input-token limit, or `None` when there is none.
    pub fn get_max_input_tokens(&self) -> Option<u64> {
        self.policies.lock().unwrap().max_input_tokens
    }

    /// Get the output-token limit, or `None` when there is none.
    pub fn get_max_output_tokens(&self) -> Option<u64> {
        self.policies.lock().unwrap().max_output_tokens
    }

    /// Get the per-request output-token limit, or `None` when there is none.
    pub fn get_max_request_tokens(&self) -> Option<u32> {
        self.policies.lock().unwrap().max_request_tokens
    }

    /// Get the schema-retry limit, or `None` when there is none.
    pub fn get_max_schema_retries(&self) -> Option<u32> {
        self.policies.lock().unwrap().max_schema_retries
    }

    /// Get the request-retry limit, which always holds a value.
    pub fn get_max_request_retries(&self) -> u32 {
        self.policies.lock().unwrap().max_request_retries
    }

    /// Get the delay between retries, which always holds a value.
    pub fn get_request_retry_delay(&self) -> Duration {
        self.policies.lock().unwrap().request_retry_delay
    }

    /// Get the elapsed-duration limit, or `None` when there is none.
    pub fn get_max_time(&self) -> Option<Duration> {
        self.policies.lock().unwrap().max_time
    }

    /// Get how full the context window may get before compaction fires, or
    /// `None` when the built-in default applies.
    pub fn get_compact_at(&self) -> Option<f64> {
        self.policies.lock().unwrap().compact_at
    }

    /// Enqueue a follow-up ticket from any event.
    ///
    /// The broadest of the three: your function sees every kind, including each
    /// piece of a reply as it arrives, so match on `event.kind` first. Guard
    /// against a follow-up that triggers itself, or execution never ends.
    pub fn create_ticket_on_event<F>(&self, make: F) -> &Self
    where
        F: Fn(&Event) -> Option<Ticket> + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            let Some(ticket) = make(event) else {
                return;
            };
            if let Some(queue) = supervisor.upgrade() {
                queue.ticket(ticket);
            }
        })
    }

    /// Enqueue a follow-up ticket from a finished ticket.
    ///
    /// Your function reads the finished ticket's key, label, and task
    /// alongside its result, and can chain the follow-up through
    /// `Ticket::parent`. Guard against a follow-up that triggers itself, or
    /// execution never ends.
    pub fn create_ticket_on_result<F>(&self, make: F) -> &Self
    where
        F: Fn(&Ticket, &serde_json::Value) -> Option<Ticket> + Send + Sync + 'static,
    {
        self.on_ticket_event(
            |kind| matches!(kind, EventKind::TicketFinished),
            move |queue, _, finished| {
                let Some(result) = &finished.result else {
                    return;
                };
                if let Some(ticket) = make(finished, result) {
                    queue.ticket(ticket);
                }
            },
        )
    }

    /// Enqueue follow-up tickets once a condition across every result holds.
    ///
    /// The many-to-many counterpart to [`Self::create_ticket_on_result`], on
    /// the terms [`Self::on_results`] sets: every result so far in, as many
    /// follow-ups as you hand back out. Your function is the condition, so
    /// hand back an empty `Vec` until the results call for the work, which is
    /// how it waits for the ones it needs. That also guards against a
    /// follow-up whose own result triggers it again, or execution never ends.
    ///
    /// ```no_run
    /// # use agentwerk::{Ticket, TicketQueue};
    /// let tickets = TicketQueue::new();
    /// tickets.create_tickets_on_results(|results| {
    ///     match results.iter().filter(|r| r["scanned"] == true).count() == 3 {
    ///         true => vec![Ticket::new("Write the report.").label("report")],
    ///         false => Vec::new(),
    ///     }
    /// });
    /// ```
    pub fn create_tickets_on_results<M>(&self, make: M) -> &Self
    where
        M: Fn(&[serde_json::Value]) -> Vec<Ticket> + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_results(move |results| {
            let follow_ups = make(results);
            if follow_ups.is_empty() {
                return;
            }
            if let Some(queue) = supervisor.upgrade() {
                for ticket in follow_ups {
                    queue.ticket(ticket);
                }
            }
        })
    }

    /// Enqueue a retry for a ticket that failed.
    ///
    /// Your function reads the failed ticket's task and label and hands back a
    /// fresh attempt, chained through `Ticket::parent`. Count the attempts
    /// yourself, or a ticket that fails every time re-queues itself forever.
    ///
    /// ```no_run
    /// # use agentwerk::{Ticket, TicketQueue};
    /// # use agentwerk::event::EventKind;
    /// let tickets = TicketQueue::new();
    /// tickets.create_ticket_on_failure(|event, failed| {
    ///     if !matches!(event.kind, EventKind::TicketFailed) || failed.parent.is_some() {
    ///         return None;
    ///     }
    ///     Some(Ticket::new(failed.task.clone()).parent(&failed.key))
    /// });
    /// ```
    pub fn create_ticket_on_failure<F>(&self, make: F) -> &Self
    where
        F: Fn(&Event, &Ticket) -> Option<Ticket> + Send + Sync + 'static,
    {
        self.on_ticket_event(EventKind::is_failure, move |queue, event, failed| {
            if let Some(ticket) = make(event, failed) {
                queue.ticket(ticket);
            }
        })
    }

    /// Read a ticket as it starts, finishes, or fails.
    ///
    /// The handler receives the event plus the ticket it names, already
    /// resolved, so it reads the result, label, and replies without a second
    /// lookup. No other kind reaches the handler: resolving a ticket copies its
    /// replies, which on `TextChunkReceived` would cost once per piece of the
    /// reply.
    pub fn on_ticket<F>(&self, handler: F) -> &Self
    where
        F: Fn(&Event, &Ticket) + Send + Sync + 'static,
    {
        self.on_ticket_event(
            |kind| {
                matches!(
                    kind,
                    EventKind::TicketStarted | EventKind::TicketFinished | EventKind::TicketFailed
                )
            },
            move |_, event, ticket| handler(event, ticket),
        )
    }

    /// Define where a session is stored, `./.agentwerk` by default.
    ///
    /// Pointing `Knowledge::load` at the same directory keeps the knowledge
    /// pages beside the session.
    pub fn dir(&self, dir: impl Into<PathBuf>) -> &Self {
        *self.dir.lock().unwrap() = dir.into();
        self
    }

    /// Get the session directory.
    pub fn get_dir(&self) -> PathBuf {
        self.dir.lock().unwrap().clone()
    }

    /// Where ticket `key`'s result is stored. Named for agents, which read a
    /// result through this path or hand it to the next ticket.
    pub(crate) fn result_path(&self, key: &str) -> PathBuf {
        super::ticket::result_path(&self.get_dir(), key)
    }

    /// Enforce schemas for ticket results.
    ///
    /// A ticket claimed under a label the store knows takes that schema, unless
    /// it already carries one of its own. It is how a ticket nobody could
    /// attach a schema to gets one: a handover child, or a ticket the model
    /// filed through `tickets`.
    ///
    /// ```no_run
    /// # use agentwerk::{SchemaStore, Ticket, TicketQueue};
    /// # use serde_json::json;
    /// let schemas = SchemaStore::new();
    /// schemas.label("analysis", json!({ "type": "object" }))?;
    ///
    /// let tickets = TicketQueue::new();
    /// tickets.schemas(&schemas);
    /// tickets.ticket(Ticket::labeled("analysis", "Audit src/db."));
    /// # Ok::<(), agentwerk::schemas::SchemaParseError>(())
    /// ```
    pub fn schemas(&self, store: &Arc<SchemaStore>) -> &Self {
        *self.schemas.lock().unwrap() = Some(Arc::clone(store));
        self
    }

    /// Submit a task and return its ticket key.
    ///
    /// A string is the task itself, and a `&Path` or `PathBuf` names the file
    /// holding it. A [`Ticket`] carries a custom label or
    /// schema with it. Key, reporter, creation time, status, and result are set
    /// at insertion and overwrite whatever the ticket carried. A label decides
    /// which agents may claim it, so give an agent a label of its own to
    /// address it alone.
    pub fn ticket(&self, ticket: impl Into<Ticket>) -> String {
        self.dispatch(ticket.into())
    }

    /// Add a reply to a ticket.
    ///
    /// An agent that has just spoken waits on the ticket, and this reply is
    /// what sends the next turn. Use it to continue a conversation on one
    /// ticket instead of creating a new ticket per turn.
    pub fn reply(&self, key: &str, content: impl Into<String>) -> &Self {
        self.add_reply(key, Reply::user_text(content));
        self
    }

    fn dispatch(&self, ticket: Ticket) -> String {
        self.insert(ticket, "user".to_string())
    }

    /// Get one ticket by key.
    pub fn get_ticket(&self, key: &str) -> Option<Ticket> {
        self.tickets.lock().unwrap().get(key).cloned()
    }

    /// Get every ticket in creation order.
    pub fn tickets(&self) -> Vec<Ticket> {
        let tickets = self.tickets.lock().unwrap();
        let mut out: Vec<Ticket> = tickets.values().cloned().collect();
        out.sort_by_key(|t| (t.created_at, numeric_id(&t.key)));
        out
    }

    /// Get every ticket matching a condition, in creation order.
    ///
    /// Your condition MUST NOT call another `TicketQueue` method that reads
    /// the ticket store, or the call deadlocks.
    pub fn find_tickets(&self, predicate: impl TicketMatcher) -> Vec<Ticket> {
        let store = self.tickets.lock().unwrap();
        let mut out: Vec<Ticket> = store
            .values()
            .filter(|t| predicate.matches(t))
            .cloned()
            .collect();
        out.sort_by_key(|t| (t.created_at, numeric_id(&t.key)));
        out
    }

    /// Get the earliest ticket matching a condition.
    ///
    /// Your condition MUST NOT call another `TicketQueue` method that reads
    /// the ticket store, or the call deadlocks.
    pub fn find_ticket(&self, predicate: impl TicketMatcher) -> Option<Ticket> {
        let store = self.tickets.lock().unwrap();
        let mut matching: Vec<&Ticket> = store.values().filter(|t| predicate.matches(t)).collect();
        matching.sort_by_key(|t| (t.created_at, numeric_id(&t.key)));
        matching.into_iter().next().cloned()
    }

    /// Get every recorded event matching a condition, oldest first.
    ///
    /// Read from the session's `events.jsonl`, so this answers for a run that
    /// has finished as readily as one still working. Counting is `.len()`, and
    /// a total is a fold over the events themselves. A log that cannot be read
    /// finds nothing, and `TextChunkReceived` is never recorded, so a condition
    /// naming it never matches.
    pub fn find_events<F>(&self, predicate: F) -> Vec<Event>
    where
        F: Fn(&Event) -> bool,
    {
        let mut out = Vec::new();
        let _ = Stats::for_each_event(&self.get_dir(), |event| {
            if predicate(event) {
                out.push(event.clone());
            }
        });
        out
    }

    /// Get the earliest recorded event matching a condition.
    pub fn find_event<F>(&self, predicate: F) -> Option<Event>
    where
        F: Fn(&Event) -> bool,
    {
        let mut found = None;
        let _ = Stats::for_each_event(&self.get_dir(), |event| {
            if found.is_none() && predicate(event) {
                found = Some(event.clone());
            }
        });
        found
    }

    /// Take every matching ticket off the queue.
    ///
    /// A match is neither claimed nor resumed, and an agent already holding one
    /// is taken off it; the ticket stays `InProgress`. Nothing waits: this is
    /// not async, so it can be called from a ctrl-c handler, a drop guard, or
    /// anywhere else. Use [`Self::cancel_all`] to stop the whole run.
    ///
    /// Your filter MUST NOT call another `TicketQueue` method that reads the
    /// ticket store, or the claim path deadlocks.
    ///
    /// ```no_run
    /// # use agentwerk::{Query, TicketQueue};
    /// let tickets = TicketQueue::new();
    /// tickets.cancel(Query::new().label("scan"));
    /// ```
    pub fn cancel(&self, matches: impl TicketMatcher + 'static) -> &Self {
        self.cancel_filters
            .lock()
            .unwrap()
            .push(Arc::new(move |t: &Ticket| matches.matches(t)));
        self
    }

    /// Take every ticket off the queue, which ends the run.
    ///
    /// [`Self::finish_all`] then reports `FinishReason::Cancelled`. Like
    /// [`Self::cancel`], nothing waits, so a ctrl-c handler can call it.
    pub fn cancel_all(&self) -> &Self {
        self.cancel(|_: &Ticket| true)
    }

    /// Check whether a ticket has been cancelled.
    ///
    /// Ask before creating follow-up work: a cancelled ticket is never claimed.
    /// It reads no ticket state, so a condition passed to [`Self::find_ticket`]
    /// or [`Self::find_tickets`] may call it.
    pub fn is_cancelled(&self, ticket: &Ticket) -> bool {
        let filters = self.cancel_filters.lock().unwrap();
        filters.iter().any(|matches| matches(ticket))
    }

    /// True while any matching ticket still has work for an agent.
    ///
    /// The one definition of "not done yet": the main loop asks it of every
    /// ticket to decide the run is over, and [`Self::finish`] asks it of a
    /// subset. A ticket is pending while it is todo or in progress,
    /// uncancelled, and not paused for a caller reply.
    pub(crate) fn pending(&self, matches: &impl TicketMatcher) -> bool {
        // A terminal transition mid-flight may still add a follow-up ticket
        // from a handler, so it counts as work whatever the store says.
        if self.terminal_transitions_in_flight.load(Ordering::SeqCst) > 0 {
            return true;
        }
        let interactive = self.interactive_agents();
        let tickets = self.tickets.lock().unwrap();
        tickets.values().any(|t| {
            matches.matches(t)
                && t.is_pending()
                && !self.is_cancelled(t)
                && !(t.is_paused()
                    && t.assignee
                        .as_deref()
                        .is_some_and(|a| interactive.contains(a)))
        })
    }

    /// Why the run is over, or `None` while it should keep going.
    ///
    /// An empty queue is not an ending: a host that called [`Self::start`] may
    /// still be filing work, and a paused ticket revives on the next reply.
    /// Only a breached limit or a cancel that leaves nothing claimable ends a
    /// run here; the drained ending is named by the [`Self::finish`] that waited
    /// for it.
    pub(crate) fn ending_reason(&self) -> Option<FinishReason> {
        if let Some((policy, _)) = policy_violated_kind(&self.policies(), &self.stats) {
            return Some(FinishReason::PolicyViolated(policy));
        }
        let claimable = {
            let tickets = self.tickets.lock().unwrap();
            tickets
                .values()
                .any(|t| t.is_pending() && !self.is_cancelled(t))
        };
        if claimable {
            return None;
        }
        // A cancel is a statement that work should stop, so it ends a run with
        // nothing claimable left even when the queue was already empty.
        let cancelled = !self.cancel_filters.lock().unwrap().is_empty();
        cancelled.then_some(FinishReason::Cancelled)
    }

    /// True while any ticket is still open. Stricter than [`Self::pending`]:
    /// an interactive agent's paused ticket has no work for it right now, but a
    /// reply revives it, so the run is not over.
    fn anything_pending(&self) -> bool {
        if self.terminal_transitions_in_flight.load(Ordering::SeqCst) > 0 {
            return true;
        }
        let tickets = self.tickets.lock().unwrap();
        tickets
            .values()
            .any(|t| t.is_pending() && !self.is_cancelled(t))
    }

    /// True while the main loop is up. `start()` is a no-op then, so a second
    /// caller never starts a run alongside the first.
    fn is_running(&self) -> bool {
        self.join_handle.lock().unwrap().is_some() && !self.run.is_finished()
    }

    /// The names of the added agents that wait for a caller reply. Read before
    /// the ticket store is locked: `bind_agent` takes the two in that order.
    fn interactive_agents(&self) -> HashSet<String> {
        self.agents
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.is_interactive())
            .map(|a| a.id().to_string())
            .collect()
    }

    /// Attach `agent` to this queue, moving any tickets it queued in its own
    /// private queue across first. The prior queue is freed once nothing else
    /// holds it.
    pub(crate) fn bind_agent(&self, agent: &mut Agent) {
        if let Some(prior) = agent.ticket_queue.upgrade() {
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
                let reporter = agent.id.clone();
                for ticket in drained {
                    self.insert(ticket, reporter.clone());
                }
            }
        }
        agent.ticket_queue = TicketQueueRef::Shared(self.weak_self.clone());
        self.agents.lock().unwrap().push(agent.clone());
    }

    /// A copy of the registered agents.
    ///
    /// The list is append-only, and `bind_agent` is its only writer.
    /// `run_main_loop` needs the positions to stay stable, so a writer that
    /// removed or reordered entries would silently break detection of agents
    /// added while execution is under way.
    pub(crate) fn clone_agents(&self) -> Vec<Agent> {
        self.agents.lock().unwrap().clone()
    }

    /// Add an agent to this ticket queue.
    ///
    /// Any tickets the agent queued on its own move into this queue. An agent
    /// added while execution is under way picks up its first ticket within
    /// about 100 ms.
    pub fn agent(&self, mut agent: Agent) -> &Self {
        self.bind_agent(&mut agent);
        self
    }

    /// Begin processing tickets, on a background task.
    ///
    /// A ticket queued afterwards is picked up within about 100 ms, and an
    /// empty queue keeps the run alive: only [`Self::finish`] and
    /// [`Self::cancel`] end one. Calling this while a run is under way does
    /// nothing; calling it after one ended starts a fresh run, which is how a
    /// host resumes after a cancel.
    pub fn start(&self) -> &Self {
        if self.is_running() {
            return self;
        }
        self.run.reset();
        self.cancel_filters.lock().unwrap().clear();
        self.reply_editing.lock().unwrap().pending.clear();
        let supervisor = self
            .weak_self
            .upgrade()
            .expect("TicketQueue dropped during start");
        self.emit("", "", EventKind::RunStarted);
        let join = tokio::spawn(async move { run_main_loop(&supervisor).await });
        *self.join_handle.lock().unwrap() = Some(join);
        self
    }

    /// Wait for the matching tickets to be done, then get their results in
    /// creation order.
    ///
    /// Name a label to wait for one pool, or a key to wait for one ticket;
    /// [`Self::finish_all`] waits for the whole run. The wait ends once no
    /// matching ticket has work left for an agent, which covers one that
    /// finished, failed, was cancelled, or is paused awaiting your reply.
    ///
    /// A ticket contributes a result only when it finished with one, so this is
    /// shorter than the set the filter named rather than aligned with it, as
    /// with [`Self::results`]. Read why the wait ended with
    /// [`Self::finish_reason`].
    ///
    /// Execution begins here when the queue has never run, and otherwise this
    /// waits on what is already under way. Once a run has ended it returns at
    /// once: only [`Self::start`] starts another. Your filter MUST NOT call
    /// another `TicketQueue` method that reads the ticket store, or the call
    /// deadlocks.
    ///
    /// ```no_run
    /// # use agentwerk::{Query, TicketQueue};
    /// # async fn run() {
    /// let tickets = TicketQueue::new();
    /// for finding in tickets.finish(Query::new().label("research")).await {
    ///     println!("{finding}");
    /// }
    /// # }
    /// ```
    pub async fn finish(&self, matches: impl TicketMatcher) -> Vec<serde_json::Value> {
        if self.join_handle.lock().unwrap().is_none() {
            self.start();
        }
        let mut stream = self.event_stream.subscribe();
        while self.pending(&matches) {
            let ended = self.next_event_or_end(&mut stream).await;
            self.await_result_handlers().await;
            if ended {
                break;
            }
        }
        // Again after the wait, for whatever landed in its last turn.
        self.await_result_handlers().await;
        // Nothing this filter named is left. When no ticket at all is open, the
        // run is over too, so start it finishing and let it announce the reason.
        if !self.anything_pending() {
            self.run
                .set_draining(self.ending_reason().unwrap_or(FinishReason::Drained));
            self.run.until_finished().await;
        }
        // Releasing the handle lets a later finish start a fresh run, the way a
        // host adds more work once the queue has run dry.
        if self.run.is_finished() {
            self.join_handle.lock().unwrap().take();
        }
        self.find_tickets(|t: &Ticket| matches.matches(t) && t.is_finished())
            .into_iter()
            .filter_map(|t| t.result)
            .collect()
    }

    /// Wait for every ticket to be done, then get every result in creation
    /// order.
    ///
    /// This is how a host waits for work it started: it returns once no ticket
    /// has work left for an agent. [`Self::finish`] waits for one pool or one
    /// ticket instead, and everything it says about starting, restarting, and
    /// which tickets contribute a result holds here too.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// # async fn run() {
    /// let tickets = TicketQueue::new();
    /// for finding in tickets.finish_all().await {
    ///     println!("{finding}");
    /// }
    /// # }
    /// ```
    pub async fn finish_all(&self) -> Vec<serde_json::Value> {
        self.finish(|_: &Ticket| true).await
    }

    /// Wait for every ticket to be done, then get the last result in creation
    /// order.
    ///
    /// The one-result form of [`Self::finish_all`], for a run whose answer is a
    /// single value. `None` means no ticket finished with a result.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// # async fn run() {
    /// let tickets = TicketQueue::new();
    /// if let Some(answer) = tickets.finish_last().await {
    ///     println!("{answer}");
    /// }
    /// # }
    /// ```
    pub async fn finish_last(&self) -> Option<serde_json::Value> {
        self.finish_all().await.pop()
    }

    /// Get why the last run ended, or `None` while one is still going.
    ///
    /// Cleared by [`Self::start`], so a re-started queue does not report the
    /// previous run. A [`Self::finish`] over a subset can return while the run
    /// carries on, and this reads `None` until it ends.
    pub fn finish_reason(&self) -> Option<FinishReason> {
        self.run.reason()
    }

    /// Waits for the next event, giving back true once the ending is complete
    /// or nothing can emit again. One subscription spans the whole wait, so an
    /// event arriving between two reads of the store is never missed. It waits
    /// for the ending to be complete rather than begun, so a caller that starts
    /// another run never overlaps the previous one.
    async fn next_event_or_end(&self, stream: &mut broadcast::Receiver<Event>) -> bool {
        tokio::select! {
            // A lagging reader misses events rather than the run stalling to
            // wait for it, so only a closed channel ends the wait.
            received = stream.recv() => matches!(received, Err(broadcast::error::RecvError::Closed)),
            _ = self.run.until_finished() => true,
        }
    }

    /// Get the result of every finished ticket, in creation order.
    ///
    /// Read a structured result back with `serde_json::from_value`. A ticket
    /// still running, or finished without a result, contributes nothing, so
    /// this is shorter than [`Self::tickets`] rather than aligned with it.
    pub fn results(&self) -> Vec<serde_json::Value> {
        self.find_tickets(|t: &Ticket| t.status == Status::Finished && t.result.is_some())
            .into_iter()
            .filter_map(|t| t.result)
            .collect()
    }

    /// Get every result whose ticket matches the query, in creation order.
    ///
    /// Status defaults to `Finished` when the query leaves it unset.
    /// A caller that sets `.status(Status::Failed)` keeps that filter.
    /// A ticket contributes a result only when it has one, so this is
    /// shorter than the set the query named.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// let tickets = TicketQueue::new();
    /// let scans = tickets.find_results("scan");
    /// ```
    pub fn find_results(&self, query: impl Into<Query>) -> Vec<serde_json::Value> {
        let query = query.into().default_status(Status::Finished);
        self.find_tickets(|t: &Ticket| query.matches(t) && t.result.is_some())
            .into_iter()
            .filter_map(|t| t.result)
            .collect()
    }

    /// Get the earliest result whose ticket matches the query.
    ///
    /// Status defaults to `Finished` as in [`Self::find_results`].
    pub fn find_result(&self, query: impl Into<Query>) -> Option<serde_json::Value> {
        let query = query.into().default_status(Status::Finished);
        self.find_ticket(|t: &Ticket| query.matches(t) && t.result.is_some())
            .and_then(|t| t.result)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;
    use crate::event::ToolFailureKind;

    #[test]
    fn ticket_queue_handle_is_shared_between_caller_and_added_agent() {
        let (queue, _tmp) = test_queue();
        let alice = queue.agent(minimal_agent("alice"));
        // Alice's task lands in the same queue.
        alice.ticket("from alice");
        queue.ticket("from queue");
        let all_keys: Vec<String> = queue
            .find_tickets(|t: &Ticket| t.status == Status::Todo)
            .iter()
            .map(|t| t.key.clone())
            .collect();
        assert_eq!(all_keys.len(), 2);
    }

    #[test]
    fn repeated_task_calls_route_to_shared_queue_after_rebind() {
        let (queue, _tmp) = test_queue();
        let mut alice = minimal_agent("alice");
        queue.bind_agent(&mut alice);
        alice.ticket("first");
        alice.ticket("second");
        assert_eq!(
            queue
                .find_tickets(|t: &Ticket| t.status == Status::Todo)
                .len(),
            2
        );
    }

    #[test]
    fn tickets_returns_all_in_creation_order() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        queue.ticket("c");
        let all = queue.tickets();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].key, "TICKET-1");
        assert_eq!(all[1].key, "TICKET-2");
        assert_eq!(all[2].key, "TICKET-3");
    }

    #[test]
    fn results_return_done_payloads_in_creation_order() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        queue.ticket("c");
        attach_done_result(&queue, "TICKET-1", "first");
        attach_done_result(&queue, "TICKET-3", "third");
        assert_eq!(
            queue.results(),
            vec![serde_json::json!("first"), serde_json::json!("third")]
        );
    }

    #[test]
    fn results_order_by_creation_regardless_of_done_order() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        queue.ticket("c");
        attach_done_result(&queue, "TICKET-3", "third");
        attach_done_result(&queue, "TICKET-1", "first");
        attach_done_result(&queue, "TICKET-2", "second");
        assert_eq!(
            queue.results(),
            vec![
                serde_json::json!("first"),
                serde_json::json!("second"),
                serde_json::json!("third")
            ]
        );
    }

    #[test]
    fn results_are_empty_when_nothing_finished() {
        let (queue, _tmp) = test_queue();
        queue.ticket("pending");
        assert!(queue.results().is_empty());
    }

    #[test]
    fn find_results_takes_a_label_in_place_of_a_query() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::labeled("scan", "a"));
        queue.ticket(Ticket::labeled("report", "b"));
        attach_done_result(&queue, "TICKET-1", "scanned");
        attach_done_result(&queue, "TICKET-2", "reported");
        assert_eq!(
            queue.find_results("scan"),
            vec![serde_json::json!("scanned")]
        );
    }

    #[test]
    fn find_tickets_takes_a_label_in_place_of_a_query() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::labeled("scan", "a"));
        queue.ticket(Ticket::labeled("report", "b"));
        let found = queue.find_tickets("scan");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].task, serde_json::json!("a"));
    }

    #[test]
    fn pending_on_a_todo_ticket() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        assert!(queue.pending(&|_: &Ticket| true));
    }

    #[test]
    fn pending_only_for_the_matching_tickets() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("research"));
        assert!(queue.pending(&|t: &Ticket| t.has_label("research")));
        assert!(!queue.pending(&|t: &Ticket| t.has_label("report")));
    }

    #[test]
    fn pending_while_a_claimed_ticket_awaits_the_model() {
        let (queue, _tmp) = test_queue();
        queue.ticket("x");
        queue.claim(|t| t.status == Status::Todo, "agent").unwrap();
        assert!(queue.pending(&|_: &Ticket| true));
    }

    #[test]
    fn pending_when_a_text_only_reply_pauses_a_non_interactive_agent() {
        let (queue, _tmp) = test_queue();
        queue.ticket("x");
        let key = queue.claim(|t| t.status == Status::Todo, "agent").unwrap();
        queue.add_reply(
            &key,
            Reply::assistant(&[crate::providers::ContentBlock::Text {
                text: "hello".into(),
            }]),
        );
        // Only an interactive agent waits on the caller; the rest are retried.
        assert!(queue.pending(&|_: &Ticket| true));
    }

    #[test]
    fn not_pending_once_every_ticket_is_finished_or_failed() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        let key_a = queue.claim(|t| t.key == "TICKET-1", "agent").unwrap();
        let key_b = queue.claim(|t| t.key == "TICKET-2", "agent").unwrap();
        queue.set_finished_by(&key_a, "agent").unwrap();
        queue.set_failed(&key_b).unwrap();
        assert!(!queue.pending(&|_: &Ticket| true));
    }

    #[test]
    fn not_pending_on_a_cancelled_ticket() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("research"));
        queue.cancel(|t: &Ticket| t.has_label("research"));
        assert!(!queue.pending(&|_: &Ticket| true));
    }

    #[test]
    fn policy_readers_return_the_limits_that_were_set() {
        let (queue, _tmp) = test_queue();
        queue
            .max_turns(40)
            .max_input_tokens(200_000)
            .max_output_tokens(50_000)
            .max_request_tokens(8_000)
            .max_schema_retries(3)
            .max_request_retries(5)
            .request_retry_delay(Duration::from_millis(250))
            .max_time(Duration::from_secs(300))
            .compact_at(0.75);

        assert_eq!(queue.get_max_turns(), Some(40));
        assert_eq!(queue.get_max_input_tokens(), Some(200_000));
        assert_eq!(queue.get_max_output_tokens(), Some(50_000));
        assert_eq!(queue.get_max_request_tokens(), Some(8_000));
        assert_eq!(queue.get_max_schema_retries(), Some(3));
        assert_eq!(queue.get_max_request_retries(), 5);
        assert_eq!(queue.get_request_retry_delay(), Duration::from_millis(250));
        assert_eq!(queue.get_max_time(), Some(Duration::from_secs(300)));
        assert_eq!(queue.get_compact_at(), Some(0.75));
    }

    #[test]
    fn policy_readers_return_the_defaults_before_any_limit_is_set() {
        let (queue, _tmp) = test_queue();
        assert_eq!(queue.get_max_turns(), None);
        assert_eq!(queue.get_max_input_tokens(), None);
        assert_eq!(queue.get_max_output_tokens(), None);
        assert_eq!(queue.get_max_request_tokens(), None);
        assert_eq!(queue.get_max_time(), None);
        assert_eq!(queue.get_max_schema_retries(), Some(10));
        assert_eq!(queue.get_max_request_retries(), 10);
        assert_eq!(queue.get_request_retry_delay(), Duration::from_millis(500));
        assert_eq!(queue.get_compact_at(), None);
    }

    #[test]
    fn compact_at_clamps_a_fraction_outside_the_unit_range() {
        let (queue, _tmp) = test_queue();
        for (given, expected) in [(1.5, 1.0), (-0.2, 0.0), (f64::NAN, 1.0)] {
            queue.compact_at(given);
            assert_eq!(queue.get_compact_at(), Some(expected), "given {given}");
        }
    }

    #[test]
    fn find_events_returns_the_matching_events_oldest_first() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        queue.claim(|t| t.key == "TICKET-1", "alice");

        let created = queue.find_events(|e| matches!(e.kind, EventKind::TicketCreated));
        assert_eq!(created.len(), 2);
        assert_eq!(created[0].ticket_key, "TICKET-1");
        assert_eq!(created[1].ticket_key, "TICKET-2");
        assert!(created[0].created_at <= created[1].created_at);
    }

    #[test]
    fn find_events_matching_nothing_is_empty() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        assert!(queue
            .find_events(|e| matches!(e.kind, EventKind::RunFinished { .. }))
            .is_empty());
    }

    #[test]
    fn find_events_without_a_log_is_empty() {
        let (queue, _tmp) = test_queue();
        assert!(queue.find_events(|_| true).is_empty());
    }

    #[test]
    fn find_event_returns_the_earliest_match() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");

        let first = queue.find_event(|e| matches!(e.kind, EventKind::TicketCreated));
        assert_eq!(first.unwrap().ticket_key, "TICKET-1");
        assert!(queue
            .find_event(|e| matches!(e.kind, EventKind::TicketFailed))
            .is_none());
    }

    #[test]
    fn a_condition_reads_the_label_and_the_agent_that_caused_the_event() {
        // What makes a per-label or per-agent breakdown possible without the
        // crate keeping one.
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("scan").label("scout"));
        queue.claim(|t| t.has_label("scout"), "scout-1");

        assert_eq!(
            queue
                .find_events(|e| e.label.as_deref() == Some("scout"))
                .len(),
            2
        );
        assert_eq!(queue.find_events(|e| e.agent_id == "scout-1").len(), 1);
    }

    #[test]
    fn a_condition_naming_a_streamed_chunk_never_matches() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.emit(
            "TICKET-1",
            "alice",
            EventKind::TextChunkReceived {
                content: "a piece of the reply".into(),
            },
        );

        // Chunks are deliberately left out of the log, so nothing finds them.
        assert!(queue
            .find_events(|e| matches!(e.kind, EventKind::TextChunkReceived { .. }))
            .is_empty());
    }

    #[test]
    fn the_totals_keep_reporting_once_the_log_is_gone() {
        // The counters are what the run spent; the finders are what it wrote
        // down. Deleting the log separates the two.
        let (queue, dir) = test_queue();
        queue.ticket("a");
        queue.emit(
            "TICKET-1",
            "alice",
            EventKind::RequestFinished {
                model: "m".into(),
                usage: crate::providers::TokenUsage {
                    input_tokens: 900,
                    output_tokens: 120,
                },
            },
        );

        std::fs::remove_file(dir.path().join("events.jsonl")).unwrap();

        assert_eq!(queue.input_tokens(), 900);
        assert_eq!(queue.output_tokens(), 120);
        assert!(queue.find_events(|_| true).is_empty());
    }

    #[test]
    fn get_dir_reads_back_the_configured_directory() {
        let (queue, tmp) = test_queue();
        assert_eq!(queue.get_dir(), tmp.path());
    }

    #[test]
    fn cancel_takes_only_the_matching_tickets_off_the_queue() {
        let (queue, _tmp) = test_queue();
        queue.cancel(|t: &Ticket| t.has_label("research"));

        assert!(queue.is_cancelled(&Ticket::new("x").label("research")));
        assert!(
            !queue.is_cancelled(&Ticket::new("x").label("analysis")),
            "other pools are untouched",
        );
        assert!(!queue.is_cancelled(&Ticket::new("x")));
    }

    #[test]
    fn is_cancelled_reads_back_what_cancel_took_off_the_queue() {
        let (queue, _tmp) = test_queue();
        assert!(!queue.is_cancelled(&Ticket::new("x").label("research")));
        queue.cancel(|t: &Ticket| t.has_label("research"));
        assert!(queue.is_cancelled(&Ticket::new("x").label("research")));
        assert!(
            !queue.is_cancelled(&Ticket::new("x").label("analysis")),
            "other pools stay claimable",
        );
    }

    #[test]
    fn on_event_appends_handlers_in_installation_order() {
        use std::sync::Mutex;
        let (queue, _tmp) = test_queue();
        let log: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let l1 = Arc::clone(&log);
        let l2 = Arc::clone(&log);
        queue.on_event(move |_| l1.lock().unwrap().push(1));
        queue.on_event(move |_| l2.lock().unwrap().push(2));
        queue.emit("KEY", "agent", EventKind::TurnStarted);
        assert_eq!(*log.lock().unwrap(), vec![1, 2]);
    }

    #[test]
    fn on_event_falls_back_to_default_logger_when_empty() {
        // No assertion target beyond "does not panic": with no installed
        // handlers, emit() must run default_logger without crashing.
        let (queue, _tmp) = test_queue();
        queue.emit("KEY", "agent", EventKind::TurnStarted);
    }

    #[test]
    fn an_event_names_the_agent_and_the_tickets_label() {
        // What a handler needs to count per agent or per label, which is where
        // those figures live now that `Stats` counts the run as a whole.
        let (queue, _tmp) = test_queue();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&outcomes);
        queue.on_event(move |event| {
            if matches!(event.kind, EventKind::TicketFinished) {
                seen.lock()
                    .unwrap()
                    .push((event.agent_id.clone(), event.label.clone()));
            }
        });
        queue.ticket(Ticket::new("a").label("scan"));
        let key = queue
            .claim(|t| t.task == serde_json::json!("a"), "scout")
            .unwrap();
        queue.set_result(&key, serde_json::json!("done")).unwrap();
        queue.set_finished_by(&key, "scout").unwrap();

        assert_eq!(
            *outcomes.lock().unwrap(),
            vec![("scout".to_string(), Some("scan".to_string()))],
        );
    }

    #[tokio::test]
    async fn finish_returns_once_the_matching_ticket_resolves() {
        let (queue, _tmp) = test_queue();
        let key = queue.ticket("work");
        let writer = Arc::clone(&queue);
        let claimed = key.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            attach_done_result(&writer, &claimed, "done");
        });
        let target = key.clone();
        queue.finish(move |t: &Ticket| t.key == target).await;
        assert!(queue.get_ticket(&key).unwrap().is_finished());
    }

    #[tokio::test]
    async fn finish_returns_without_an_event_when_nothing_matches_yet() {
        let (queue, _tmp) = test_queue();
        let key = queue.ticket("work");
        attach_done_result(&queue, &key, "done");
        // Nothing emits from here on, so only the check before the wait can
        // resolve this.
        assert_eq!(
            queue.finish(move |t: &Ticket| t.key == key).await,
            vec![serde_json::json!("done")]
        );
    }

    #[test]
    fn message_editor_receives_buffered_events_excluding_text_chunks() {
        use crate::event::ToolFailureKind;
        let (queue, _tmp) = test_queue();
        let key = queue.ticket("go");

        let seen: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        queue.edit_replies_on_event(move |events, _replies| {
            recorder.lock().unwrap().extend(events.iter().cloned());
        });

        queue.emit(&key, "agent", EventKind::TurnStarted);
        queue.emit(
            &key,
            "agent",
            EventKind::TextChunkReceived {
                content: "hi".into(),
            },
        );
        queue.emit(
            &key,
            "agent",
            EventKind::ToolCallFailed {
                tool_name: "boom".into(),
                call_id: "c1".into(),
                reason: ToolFailureKind::ExecutionFailed,
                message: "boom".into(),
            },
        );
        queue.run_reply_editor(&key);

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
        let (queue, _tmp) = test_queue();
        let key = queue.ticket("go");

        let runs = Arc::new(Mutex::new(0u32));
        let counter = Arc::clone(&runs);
        queue.edit_replies_on_event(move |_events, _replies| {
            *counter.lock().unwrap() += 1;
        });

        queue.emit(&key, "agent", EventKind::TurnStarted);
        queue.run_reply_editor(&key);
        // The batch is drained, so a second run has nothing to react to.
        queue.run_reply_editor(&key);

        assert_eq!(*runs.lock().unwrap(), 1);
    }

    /// One editor at a time, the way `dir` or `max_turns` replace. Two
    /// rewriters of one ticket's replies would each see the other's output.
    #[test]
    fn a_second_reply_editor_replaces_the_first() {
        use crate::agents::tickets::ReplyContent;
        let (queue, _tmp) = test_queue();
        let key = queue.ticket("go");
        queue.edit_replies_on_event(|_events, replies| {
            replies.push(Reply::user_text("first"));
        });
        queue.edit_replies_on_event(|_events, replies| {
            replies.push(Reply::user_text("second"));
        });

        queue.emit(&key, "agent", EventKind::TurnStarted);
        queue.run_reply_editor(&key);

        let texts: Vec<String> = queue
            .get_ticket(&key)
            .unwrap()
            .replies
            .iter()
            .filter_map(|r| match r.content.first() {
                Some(ReplyContent::Text { text: t }) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert!(
            texts.contains(&"second".to_string()),
            "the last editor installed must run: {texts:?}"
        );
        assert!(
            !texts.contains(&"first".to_string()),
            "the replaced editor must not run: {texts:?}"
        );
    }

    #[test]
    fn message_editor_sees_only_the_events_of_the_ticket_it_edits() {
        let (queue, _tmp) = test_queue();
        let a = queue.ticket("a");
        let b = queue.ticket("b");

        let seen: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        queue.edit_replies_on_event(move |events, _replies| {
            recorder.lock().unwrap().extend(events.iter().cloned());
        });

        queue.emit(&a, "agent", EventKind::TurnStarted);
        queue.emit(&b, "agent", EventKind::TicketFailed);
        queue.run_reply_editor(&a);

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
    fn edit_replies_edits_the_transcript_on_demand() {
        use crate::agents::tickets::ReplyContent;
        let (queue, _tmp) = test_queue();
        let key = queue.ticket("go");
        queue.add_reply(&key, Reply::user_text("keep me"));
        queue.add_reply(&key, Reply::user_text("drop me"));

        queue.edit_replies(&key, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
            });
        });

        let replies = queue.get_ticket(&key).unwrap().replies;
        assert!(replies.iter().any(
            |r| matches!(r.content.first(), Some(ReplyContent::Text { text: t }) if t == "keep me")
        ));
        assert!(replies.iter().all(
            |r| !matches!(r.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
        ));
    }

    #[test]
    fn on_result_receives_the_finished_ticket_and_its_result() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_result(move |ticket, result| {
            record
                .lock()
                .unwrap()
                .push((ticket.key.clone(), result.clone()))
        });
        queue.ticket(Ticket::new("x").label("L"));
        let key = queue.claim(|t| t.has_label("L"), "agent").unwrap();

        attach_done_result(&queue, &key, "lead");

        assert_eq!(
            *seen.lock().unwrap(),
            vec![(key, serde_json::json!("lead"))]
        );
    }

    #[test]
    fn on_failure_fires_for_a_tool_call_failure_not_only_a_failed_ticket() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_failure(move |event, ticket| {
            record
                .lock()
                .unwrap()
                .push((event.kind.name(), ticket.key.clone()))
        });
        let key = queue.ticket("work");

        queue.emit(&key, "agent", EventKind::TurnStarted);
        queue.emit(
            &key,
            "agent",
            EventKind::ToolCallFailed {
                tool_name: "grep".into(),
                call_id: "c1".into(),
                reason: ToolFailureKind::ExecutionFailed,
                message: "no such directory".into(),
            },
        );
        queue.set_failed(&key).unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                ("tool_call_failed", key.clone()),
                ("ticket_failed", key.clone()),
            ]
        );
    }

    #[test]
    fn create_ticket_on_failure_enqueues_a_retry_for_the_failed_ticket() {
        let (queue, _tmp) = test_queue();
        queue.create_ticket_on_failure(|_, failed| {
            failed
                .parent
                .is_none()
                .then(|| Ticket::new(failed.task.clone()).parent(&failed.key))
        });
        let key = queue.ticket("work");

        queue.set_failed(&key).unwrap();

        let retry = queue
            .find_ticket(|t: &Ticket| t.parent.as_deref() == Some(&key))
            .unwrap();
        assert_eq!(retry.task, serde_json::json!("work"));
    }

    #[test]
    fn create_ticket_on_event_enqueues_a_follow_up_for_any_event() {
        let (queue, _tmp) = test_queue();
        queue.create_ticket_on_event(|event| {
            matches!(event.kind, EventKind::TurnStarted)
                .then(|| Ticket::new("report").label("report"))
        });
        let key = queue.ticket("work");

        queue.emit(&key, "agent", EventKind::TurnStarted);

        assert_eq!(
            queue.find_tickets(|t: &Ticket| t.has_label("report")).len(),
            1
        );
    }

    #[test]
    fn create_ticket_on_result_enqueues_follow_up_for_finished_ticket() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("scout").label("scout"));
        let key = queue.claim(|t| t.has_label("scout"), "agent").unwrap();
        queue.set_result(&key, serde_json::json!("lead")).unwrap();
        queue.set_finished_by(&key, "agent").unwrap();
        queue.create_ticket_on_result(|done, _| {
            done.has_label("scout")
                .then(|| Ticket::new("hunt").label("sniper"))
        });
        queue.emit(&key, "agent", EventKind::TicketFinished);
        assert_eq!(
            queue.find_tickets(|t: &Ticket| t.has_label("sniper")).len(),
            1
        );
    }

    #[test]
    fn create_ticket_on_result_links_follow_up_to_finished_parent() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("scout").label("scout"));
        let key = queue.claim(|t| t.has_label("scout"), "agent").unwrap();
        queue.set_result(&key, serde_json::json!("lead")).unwrap();
        queue.set_finished_by(&key, "agent").unwrap();
        queue.create_ticket_on_result(|done, _| {
            Some(Ticket::new("hunt").label("sniper").parent(&done.key))
        });
        queue.emit(&key, "agent", EventKind::TicketFinished);
        let spawned = queue
            .find_ticket(|t: &Ticket| t.has_label("sniper"))
            .unwrap();
        assert_eq!(spawned.parent, Some(key));
    }

    #[test]
    fn create_ticket_on_result_ignores_unfinished_events() {
        let (queue, _tmp) = test_queue();
        queue.create_ticket_on_result(|_, _| Some(Ticket::new("follow-up").label("next")));
        queue.emit("KEY", "agent", EventKind::TurnStarted);
        assert!(queue.tickets().is_empty());
    }

    /// File `count` tickets labelled `scan`, all `Todo`.
    fn scans(queue: &TicketQueue, count: usize) -> Vec<String> {
        (0..count)
            .map(|i| queue.ticket(Ticket::new(format!("scan {i}")).label("scan")))
            .collect()
    }

    #[tokio::test]
    async fn on_result_async_handlers_run_before_finish_all_returns() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_result_async(move |ticket, result| {
            let record = Arc::clone(&record);
            async move { record.lock().unwrap().push((ticket.key, result)) }
        });
        let key = queue.ticket("scan the corpus");
        queue.set_finished(&key, "clean").unwrap();

        queue.finish_all().await;

        assert_eq!(
            *seen.lock().unwrap(),
            vec![(key, serde_json::json!("clean"))]
        );
    }

    #[tokio::test]
    async fn on_result_async_runs_every_handler_once_per_result() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        for name in ["first", "second"] {
            let record = Arc::clone(&seen);
            queue.on_result_async(move |_, _| {
                let record = Arc::clone(&record);
                async move { record.lock().unwrap().push(name) }
            });
        }
        let key = queue.ticket("scan the corpus");
        queue.set_finished(&key, "clean").unwrap();

        queue.finish_all().await;

        // Two entries, not four: a second registration does not queue twice.
        assert_eq!(*seen.lock().unwrap(), vec!["first", "second"]);
    }

    #[tokio::test]
    async fn registering_from_two_threads_at_once_queues_each_result_once() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|threads| {
            for name in ["first", "second"] {
                let queue = Arc::clone(&queue);
                let record = Arc::clone(&seen);
                let start = &start;
                threads.spawn(move || {
                    start.wait();
                    queue.on_result_async(move |_, _| {
                        let record = Arc::clone(&record);
                        async move { record.lock().unwrap().push(name) }
                    });
                });
            }
        });
        let key = queue.ticket("scan the corpus");
        queue.set_finished(&key, "clean").unwrap();

        queue.finish_all().await;

        // Two entries, not four: neither thread installed a second hook.
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_cancelled_finish_leaves_the_rest_of_the_queue_for_the_next_one() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_result_async(move |ticket, _| {
            let record = Arc::clone(&record);
            async move {
                let first = record.lock().unwrap().is_empty();
                record.lock().unwrap().push(ticket.key);
                if first {
                    // Outlasts the timeout below, so the finish is dropped here.
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            }
        });
        let first = queue.ticket("scan the first half");
        let second = queue.ticket("scan the second half");
        queue.set_finished(&first, "clean").unwrap();
        queue.set_finished(&second, "clean").unwrap();

        let cancelled = tokio::time::timeout(Duration::from_millis(50), queue.finish_all());
        assert!(cancelled.await.is_err());
        queue.finish_all().await;

        assert_eq!(*seen.lock().unwrap(), vec![first, second]);
    }

    #[tokio::test]
    async fn on_result_async_finishes_one_handler_before_starting_the_next() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_result_async(move |ticket, _| {
            let record = Arc::clone(&record);
            async move {
                record.lock().unwrap().push(format!("start {}", ticket.key));
                // A spawned handler would let the next one start here.
                tokio::task::yield_now().await;
                record.lock().unwrap().push(format!("end {}", ticket.key));
            }
        });
        let first = queue.ticket("scan the first half");
        let second = queue.ticket("scan the second half");
        queue.set_finished(&first, "clean").unwrap();
        queue.set_finished(&second, "clean").unwrap();

        queue.finish_all().await;

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                format!("start {first}"),
                format!("end {first}"),
                format!("start {second}"),
                format!("end {second}"),
            ]
        );
    }

    #[tokio::test]
    async fn on_results_async_hands_over_every_result_so_far() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_results_async(move |results| {
            let record = Arc::clone(&record);
            async move { record.lock().unwrap().push(results) }
        });
        let first = queue.ticket("scan the first half");
        let second = queue.ticket("scan the second half");
        queue.set_finished(&first, 1).unwrap();
        queue.set_finished(&second, 4).unwrap();

        queue.finish_all().await;

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                vec![serde_json::json!(1)],
                vec![serde_json::json!(1), serde_json::json!(4)],
            ]
        );
    }

    #[tokio::test]
    async fn both_kinds_of_async_handler_see_each_result_once() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let one = Arc::clone(&seen);
        queue.on_result_async(move |_, result| {
            let one = Arc::clone(&one);
            async move { one.lock().unwrap().push(format!("one {result}")) }
        });
        let all = Arc::clone(&seen);
        queue.on_results_async(move |results| {
            let all = Arc::clone(&all);
            async move { all.lock().unwrap().push(format!("all {}", results.len())) }
        });
        let key = queue.ticket("scan the corpus");
        queue.set_finished(&key, 1).unwrap();

        queue.finish_all().await;

        // One entry each: the two kinds share one queueing hook.
        assert_eq!(*seen.lock().unwrap(), vec!["one 1", "all 1"]);
    }

    #[tokio::test]
    async fn an_async_handler_waits_for_a_finish_to_run_it() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_result_async(move |ticket, _| {
            let record = Arc::clone(&record);
            async move { record.lock().unwrap().push(ticket.key) }
        });
        let key = queue.ticket("scan the corpus");

        queue.set_finished(&key, "clean").unwrap();
        assert!(seen.lock().unwrap().is_empty());

        queue.finish_all().await;
        assert_eq!(*seen.lock().unwrap(), vec![key]);
    }

    #[tokio::test]
    async fn on_result_async_leaves_a_failed_ticket_alone() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_result_async(move |ticket, _| {
            let record = Arc::clone(&record);
            async move { record.lock().unwrap().push(ticket.key) }
        });
        let key = queue.ticket("scan the corpus");
        queue.set_failed(&key).unwrap();

        queue.finish_all().await;

        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn on_results_hands_over_every_result_so_far_each_time_one_lands() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_results(move |results| record.lock().unwrap().push(results.to_vec()));
        let scans = scans(&queue, 2);

        queue.set_finished(&scans[0], 1).unwrap();
        queue.set_finished(&scans[1], 4).unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                vec![serde_json::json!(1)],
                vec![serde_json::json!(1), serde_json::json!(4)],
            ]
        );
    }

    #[test]
    fn on_results_stays_quiet_for_a_ticket_that_failed() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_results(move |results| record.lock().unwrap().push(results.to_vec()));
        let scans = scans(&queue, 1);

        queue.set_failed(&scans[0]).unwrap();

        // A failed ticket produces no result, so nothing changed to report.
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn create_tickets_on_results_waits_until_the_results_call_for_the_work() {
        let (queue, _tmp) = test_queue();
        queue.create_tickets_on_results(|results| match results.len() == 2 {
            true => vec![Ticket::new("write the report").label("report")],
            false => Vec::new(),
        });
        let scans = scans(&queue, 2);

        queue.set_finished(&scans[0], "clean").unwrap();
        assert!(queue
            .find_tickets(|t: &Ticket| t.has_label("report"))
            .is_empty());

        queue.set_finished(&scans[1], "clean").unwrap();
        assert_eq!(
            queue.find_tickets(|t: &Ticket| t.has_label("report")).len(),
            1
        );
    }

    #[test]
    fn create_tickets_on_results_files_one_follow_up_per_result() {
        let (queue, _tmp) = test_queue();
        queue.create_tickets_on_results(|results| match results.len() == 2 {
            true => results
                .iter()
                .map(|r| Ticket::new(r.clone()).label("review"))
                .collect(),
            false => Vec::new(),
        });
        let scans = scans(&queue, 2);

        queue.set_finished(&scans[0], 1).unwrap();
        queue.set_finished(&scans[1], 4).unwrap();

        let filed: Vec<serde_json::Value> = queue
            .find_tickets(|t: &Ticket| t.has_label("review"))
            .into_iter()
            .map(|t| t.task)
            .collect();
        assert_eq!(filed, vec![serde_json::json!(1), serde_json::json!(4)]);
    }

    #[test]
    fn create_ticket_on_result_inserts_follow_up_before_drain_is_observable() {
        let (queue, _tmp) = test_queue();
        queue.create_ticket_on_result(|done, _| {
            done.has_label("scout")
                .then(|| Ticket::new("hunt").label("sniper"))
        });
        queue.ticket(Ticket::new("scout").label("scout"));
        let key = queue.claim(|t| t.has_label("scout"), "agent").unwrap();
        queue.set_result(&key, serde_json::json!("lead")).unwrap();
        queue.set_finished_by(&key, "agent").unwrap();
        // The handler ran inside `set_finished_by`, so the queue is never
        // observably empty between the parent finishing and the follow-up.
        assert!(queue.pending(&|_: &Ticket| true));
    }

    #[test]
    fn on_ticket_hands_the_handler_the_finished_ticket() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_ticket(move |event, ticket| {
            if matches!(event.kind, EventKind::TicketFinished) {
                record.lock().unwrap().push((
                    event.agent_id.clone(),
                    ticket.key.clone(),
                    ticket.replies.len(),
                    ticket.result.clone(),
                ));
            }
        });
        queue.ticket(Ticket::new("scan").label("scan"));
        let key = queue.claim(|t| t.has_label("scan"), "analyst").unwrap();
        queue.add_reply(&key, Reply::user_text("hello"));
        queue.set_result(&key, serde_json::json!("done")).unwrap();
        queue.set_finished_by(&key, "analyst").unwrap();

        // `replies` is `#[serde(skip)]`, so a transcript here proves the
        // handler holds the in-memory ticket, not a disk round-trip.
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(
                "analyst".to_string(),
                key,
                1,
                Some(serde_json::json!("done"))
            )]
        );
    }

    #[test]
    fn on_ticket_skips_events_that_are_not_lifecycle_transitions() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.ticket(Ticket::new("scan").label("scan"));
        let key = queue.claim(|t| t.has_label("scan"), "analyst").unwrap();
        // Installed after the claim, so only the turn is in the handler's view.
        queue.on_ticket(move |_, ticket| record.lock().unwrap().push(ticket.key.clone()));
        queue.emit(&key, "analyst", EventKind::TurnStarted);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn on_ticket_skips_events_that_name_no_ticket() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_ticket(move |_, ticket| record.lock().unwrap().push(ticket.key.clone()));
        queue.emit("", "", EventKind::RunStarted);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn on_ticket_coexists_with_a_user_handler() {
        let (queue, _tmp) = test_queue();
        let logged = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&logged);
        queue.on_event(move |e| log.lock().unwrap().push(e.kind.name()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_ticket(move |_, ticket| record.lock().unwrap().push(ticket.key.clone()));
        queue.ticket(Ticket::new("scan").label("scan"));
        // The claim is the lifecycle event; no second emit needed.
        let key = queue.claim(|t| t.has_label("scan"), "analyst").unwrap();

        assert_eq!(*seen.lock().unwrap(), vec![key]);
        assert!(logged.lock().unwrap().contains(&"ticket_started"));
    }

    #[test]
    fn on_event_fires_every_handler_per_event() {
        use std::sync::atomic::AtomicU32;
        let (queue, _tmp) = test_queue();
        let count = Arc::new(AtomicU32::new(0));
        let c1 = Arc::clone(&count);
        let c2 = Arc::clone(&count);
        queue.on_event(move |_| {
            c1.fetch_add(1, Ordering::Relaxed);
        });
        queue.on_event(move |_| {
            c2.fetch_add(10, Ordering::Relaxed);
        });
        queue.emit("KEY", "agent", EventKind::TurnStarted);
        queue.emit("KEY", "agent", EventKind::TurnStarted);
        assert_eq!(count.load(Ordering::Relaxed), 22);
    }

    #[tokio::test]
    async fn run_finished_reports_drained_on_empty_queue() {
        let (queue, _tmp) = test_queue();
        let reasons = collect_finish_reasons(&queue);
        queue.finish_all().await;
        assert_eq!(*reasons.lock().unwrap(), vec![FinishReason::Drained]);
    }

    #[tokio::test]
    async fn finish_reason_reports_nothing_until_the_run_ends() {
        let (queue, _tmp) = test_queue();
        queue.start();
        assert_eq!(queue.finish_reason(), None);
        queue.finish_all().await;
        assert_eq!(queue.finish_reason(), Some(FinishReason::Drained));
    }

    #[tokio::test]
    async fn the_finish_reason_is_cleared_by_a_restart() {
        let (queue, _tmp) = test_queue();
        queue.start();
        queue.cancel_all();
        queue.finish_all().await;
        assert_eq!(queue.finish_reason(), Some(FinishReason::Cancelled));
        queue.start();
        assert_eq!(queue.finish_reason(), None);
    }

    #[tokio::test]
    async fn a_clean_drain_is_not_reported_as_cancelled() {
        let (queue, _tmp) = test_queue();
        queue.finish_all().await;
        assert_eq!(queue.finish_reason(), Some(FinishReason::Drained));
    }

    #[tokio::test]
    async fn finish_hands_back_only_the_results_its_filter_named() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("scan"));
        queue.ticket(Ticket::new("b").label("report"));
        attach_done_result(&queue, "TICKET-1", "scanned");
        attach_done_result(&queue, "TICKET-2", "reported");

        assert_eq!(
            queue.finish(|t: &Ticket| t.has_label("scan")).await,
            vec![serde_json::json!("scanned")]
        );
    }

    #[tokio::test]
    async fn finish_all_hands_back_the_results_of_every_pool() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("scan"));
        queue.ticket(Ticket::new("b").label("report"));
        attach_done_result(&queue, "TICKET-1", "scanned");
        attach_done_result(&queue, "TICKET-2", "reported");

        assert_eq!(
            queue.finish_all().await,
            vec![serde_json::json!("scanned"), serde_json::json!("reported")]
        );
    }

    #[tokio::test]
    async fn finish_last_hands_back_the_last_result_in_creation_order() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("scan"));
        queue.ticket(Ticket::new("b").label("report"));
        // Resolved back to front, so the answer tells creation order from the
        // order the results landed in.
        attach_done_result(&queue, "TICKET-2", "reported");
        attach_done_result(&queue, "TICKET-1", "scanned");

        assert_eq!(
            queue.finish_last().await,
            Some(serde_json::json!("reported"))
        );
    }

    #[tokio::test]
    async fn finish_last_is_none_when_nothing_finished() {
        let (queue, _tmp) = test_queue();

        assert_eq!(queue.finish_last().await, None);
    }

    #[tokio::test]
    async fn run_finished_reports_cancelled_when_cancel_fires_during_run() {
        let (queue, _tmp) = test_queue();
        let reasons = collect_finish_reasons(&queue);
        queue.start();
        queue.cancel_all();
        queue.finish_all().await;
        assert_eq!(*reasons.lock().unwrap(), vec![FinishReason::Cancelled]);
        assert_eq!(queue.finish_reason(), Some(FinishReason::Cancelled));
    }

    #[tokio::test]
    async fn run_finished_reports_policy_violated_when_max_turns_zero() {
        let (queue, _tmp) = test_queue();
        let reasons = collect_finish_reasons(&queue);
        queue.max_turns(0);
        queue.finish_all().await;
        assert_eq!(
            *reasons.lock().unwrap(),
            vec![FinishReason::PolicyViolated(
                crate::event::PolicyKind::Turns
            )],
        );
    }

    #[tokio::test]
    async fn run_finished_is_emitted_again_after_a_restart() {
        let (queue, _tmp) = test_queue();
        let reasons = collect_finish_reasons(&queue);
        queue.finish_all().await;
        queue.start();
        queue.finish_all().await;
        assert_eq!(
            *reasons.lock().unwrap(),
            vec![FinishReason::Drained, FinishReason::Drained],
        );
    }

    #[tokio::test]
    async fn run_started_emitted_before_run_finished() {
        let (queue, _tmp) = test_queue();
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        queue.on_event(move |e| {
            if matches!(
                e.kind,
                EventKind::RunStarted | EventKind::RunFinished { .. }
            ) {
                sink.lock().unwrap().push(format!("{:?}", e.kind));
            }
        });
        queue.finish_all().await;
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2, "expected RunStarted then RunFinished");
        assert!(entries[0].starts_with("RunStarted"));
        assert!(entries[1].starts_with("RunFinished"));
    }
}
