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
use crate::schemas::SchemaStore;

use super::super::agent::{Agent, TicketQueueRef};
use super::super::policy::Policy;
use super::super::query::{Matcher, Query};
use super::super::r#loop::run_main_loop;
use super::super::stats::Stats;
use super::ticket::{Status, Ticket};
use super::{numeric_id, policy_violated, Reply};

/// The queue arrives first so a handler selects tickets and files follow-up work
/// without capturing an `Arc` into the queue that holds it.
type EventHandler = dyn Fn(&Arc<TicketQueue>, &Event) + Send + Sync;

/// How many events a `finish` waiter may fall behind before it starts
/// missing them. `TextChunkReceived` fires once per streaming delta and sets
/// the volume this has to absorb.
const EVENT_STREAM_CAPACITY: usize = 1024;

/// One shape for every awaited hook: the ticket is `None` for the kinds no
/// ticket-shaped hook accepts, and the wrapper each `on_*_async` installs picks
/// out what its own handler takes.
type AsyncHandler = dyn Fn(Arc<TicketQueue>, Event, Option<Ticket>) -> HandlerWork + Send + Sync;

/// Boxed so the queue can hold handlers with different future types.
type HandlerWork = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The kinds that name a ticket the whole hook is about, as opposed to one
/// step inside it.
fn is_ticket_kind(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::TicketStarted | EventKind::TicketFinished | EventKind::TicketFailed
    )
}

/// `TicketFailed` is left out: it is the outcome, already carried by the
/// ticket's status and `failed_at`, not a cause.
fn is_recorded_failure(kind: &EventKind) -> bool {
    kind.is_failure() && !matches!(kind, EventKind::TicketFailed)
}

/// An awaited handler and the kinds it accepts. The filter is read twice: once
/// to decide whether the event is worth queueing at all, once at handover.
struct AwaitedHandler {
    matches: fn(&EventKind) -> bool,
    call: Arc<AsyncHandler>,
}

/// An event held for the awaited handlers, with its ticket resolved as it was
/// when the event landed.
type Delivery = (Event, Option<Ticket>);

/// `emit` runs on an agent that has to carry on, so an event an awaited handler
/// wants is only queued here; whichever `finish` is waiting drains it and awaits
/// the handlers.
///
/// `queued` and `draining` are separate locks because `emit` pushes without
/// awaiting, while the drain guard is held across handler awaits.
#[derive(Default)]
pub(super) struct AwaitedEvents {
    handlers: Mutex<Vec<AwaitedHandler>>,
    queued: Mutex<VecDeque<Delivery>>,
    draining: tokio::sync::Mutex<()>,
    /// A concurrent registration waits for the hook rather than adding a second.
    queueing: OnceLock<()>,
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
///             .tool(FetchUrlTool::new()),
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
    pub(super) policy: Mutex<Policy>,
    /// Why the run ended, once the main loop decides. The agent tasks, the
    /// tools, and every `finish` read it to know the run is over.
    pub(crate) run: Arc<Run>,
    /// What `cancel` has taken off the queue. A matching ticket is neither
    /// claimed nor resumed, and an agent already holding one is taken off it,
    /// while the rest of the run continues.
    pub(crate) cancel_filters: Mutex<Vec<Query>>,
    /// How many terminal status transitions are between their status change and
    /// the return of their event handlers. `pending` counts a non-zero value as
    /// pending work, so a handler creating a follow-up ticket always beats the drain.
    pub(crate) terminal_transitions_in_flight: AtomicUsize,
    pub(crate) stats: Stats,
    pub(super) event_handlers: Mutex<Vec<Arc<EventHandler>>>,
    pub(super) awaited_events: AwaitedEvents,
    /// Every emitted event, for `finish` to wake on. A separate channel rather
    /// than one more `on_event` entry: a handler stays on the chain for the
    /// life of the queue, so one registered per call would grow without bound
    /// in a host that awaits in a loop.
    pub(super) event_stream: broadcast::Sender<Event>,
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
            policy: Mutex::new(Policy::default()),
            run: Arc::new(Run::default()),
            cancel_filters: Mutex::new(Vec::new()),
            terminal_transitions_in_flight: AtomicUsize::new(0),
            stats: Stats::new(),
            event_handlers: Mutex::new(Vec::new()),
            awaited_events: AwaitedEvents::default(),
            event_stream: broadcast::Sender::new(EVENT_STREAM_CAPACITY),
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
    /// limit checks stay continuous across restarts. Pointing this and
    /// `Knowledge::load` at the same directory keeps the knowledge pages beside
    /// the session.
    ///
    /// An unfinished ticket is picked up again by the agent whose id it carries
    /// as its assignee. Ids are numbered per label as agents take them, so
    /// create the same agents in the same order after a restart.
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

        // One pass over the log fills both: the figures the run resumes on, and
        // the failures each ticket saw, which no ticket file carries.
        let stats = Stats::new();
        let _ = Stats::for_each_event(&tickets_dir, |event| {
            stats.record(event);
            if is_recorded_failure(&event.kind) {
                if let Some(ticket) = tickets.get_mut(&event.ticket_key) {
                    ticket.errors.push(event.clone());
                }
            }
        });
        // The clock starts over: `max_time` bounds this run, not the one that
        // wrote the log.
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
            policy: Mutex::new(Policy::default()),
            run: Arc::new(Run::default()),
            cancel_filters: Mutex::new(Vec::new()),
            terminal_transitions_in_flight: AtomicUsize::new(0),
            stats,
            event_handlers: Mutex::new(Vec::new()),
            awaited_events: AwaitedEvents::default(),
            event_stream: broadcast::Sender::new(EVENT_STREAM_CAPACITY),
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
    ///
    /// The queue arrives with the event, so a handler selects tickets and
    /// results and files follow-up work without holding one of its own.
    ///
    /// ```no_run
    /// # use agentwerk::{Ticket, TicketQueue};
    /// # use agentwerk::event::EventKind;
    /// let tickets = TicketQueue::new();
    /// tickets.on_event(|queue, event| {
    ///     if matches!(event.kind, EventKind::TicketFailed) {
    ///         queue.ticket(Ticket::labeled("triage", "Look into the failure."));
    ///     }
    /// });
    /// ```
    pub fn on_event(
        &self,
        handler: impl Fn(&Arc<TicketQueue>, &Event) + Send + Sync + 'static,
    ) -> &Self {
        self.event_handlers.lock().unwrap().push(Arc::new(handler));
        self
    }

    /// Read every event as it is emitted, in a handler [`Self::finish`] waits
    /// for before it returns.
    ///
    /// [`Self::on_event`] cannot await: it runs on the agent task that emitted
    /// the event, and that task has to carry on. This one hands the work to
    /// whichever `finish` is waiting, which awaits each handler as the events
    /// land. In Python that puts the handler on the caller's event loop, so
    /// work that has to stay serialized against the caller's own, such as a
    /// commit, can be.
    ///
    /// Every kind reaches it, `TextChunkReceived` included, and each event
    /// waits in memory until a `finish` drains it. A host that streams a long
    /// reply and only calls [`Self::start`] uses `on_event`.
    ///
    /// Your handler MUST NOT call `finish` or [`Self::finish_all`], or it waits
    /// forever on the handover it is running inside.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// # async fn run() {
    /// let tickets = TicketQueue::new();
    /// tickets.on_event_async(|_, event| async move {
    ///     println!("{:?}", event.kind);
    /// });
    /// tickets.finish_all().await;
    /// # }
    /// ```
    pub fn on_event_async<F, Fut>(&self, handler: F) -> &Self
    where
        F: Fn(Arc<TicketQueue>, Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_awaited(
            |_| true,
            move |queue, event, _| Box::pin(handler(queue, event)),
        )
    }

    /// The filter-resolve-call shape the ticket handlers share. The queue is
    /// handed in so one that files follow-up work needs no upgrade of its own.
    fn on_ticket_event<F>(&self, matches: fn(&EventKind) -> bool, handler: F) -> &Self
    where
        F: Fn(&Arc<Self>, &Event, &Ticket) + Send + Sync + 'static,
    {
        self.on_event(move |queue, event| {
            if !matches(&event.kind) {
                return;
            }
            let Some(ticket) = queue.get_ticket(&event.ticket_key) else {
                return;
            };
            handler(queue, event, &ticket);
        })
    }

    /// Read every finished ticket together with its result.
    ///
    /// The value handed over is the stored, schema-validated result, so a
    /// handler never reaches into the finish tool's input shape. This is one
    /// more entry on the [`Self::on_event`] chain.
    ///
    /// ```no_run
    /// # use agentwerk::{Ticket, TicketQueue};
    /// let tickets = TicketQueue::new();
    /// tickets.on_result(|queue, done, result| {
    ///     if result["needs_review"] == true {
    ///         queue.ticket(Ticket::labeled("review", done.task.clone()).parent(&done.key));
    ///     }
    /// });
    /// ```
    pub fn on_result<F>(&self, handler: F) -> &Self
    where
        F: Fn(&Arc<TicketQueue>, &Ticket, &serde_json::Value) + Send + Sync + 'static,
    {
        self.on_ticket_event(
            |kind| matches!(kind, EventKind::TicketFinished),
            move |queue, _, finished| {
                let Some(result) = &finished.result else {
                    return;
                };
                handler(queue, finished, result);
            },
        )
    }

    /// Read every finished ticket together with its result, in a handler
    /// [`Self::finish`] waits for before it returns.
    ///
    /// [`Self::on_result`] cannot await: it runs on the agent task that just
    /// finished the ticket, and that task has to carry on. This one hands the
    /// work to whichever `finish` is waiting, on the terms
    /// [`Self::on_event_async`] sets, and each result waiting to be handed over
    /// holds a copy of its ticket and every reply in it.
    ///
    /// Your handler MUST NOT call `finish` or [`Self::finish_all`], or it waits
    /// forever on the handover it is running inside.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// # async fn run() {
    /// let tickets = TicketQueue::new();
    /// tickets.on_result_async(|_, ticket, result| async move {
    ///     println!("{} produced {result}", ticket.key);
    /// });
    /// tickets.finish_all().await;
    /// # }
    /// ```
    pub fn on_result_async<F, Fut>(&self, handler: F) -> &Self
    where
        F: Fn(Arc<TicketQueue>, Ticket, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_awaited(
            |kind| matches!(kind, EventKind::TicketFinished),
            move |queue, _, finished| match finished.and_then(|t| t.result.clone().map(|r| (t, r)))
            {
                Some((ticket, result)) => Box::pin(handler(queue, ticket, result)),
                None => Box::pin(std::future::ready(())),
            },
        )
    }

    /// Read every failure together with the ticket it happened in:
    /// `TicketFailed`, `RequestFailed`, `ToolCallFailed`, `FileOpenFailed`,
    /// `KnowledgeFailed`, and `CompactionFailed`.
    ///
    /// Match on `event.kind` to tell a failure that ends the ticket from one
    /// the agent works around. Each call copies the ticket's replies, so an
    /// agent that fails many tool calls pays that copy once per failure.
    ///
    /// ```no_run
    /// # use agentwerk::{Ticket, TicketQueue};
    /// # use agentwerk::event::EventKind;
    /// let tickets = TicketQueue::new();
    /// tickets.on_failure(|queue, event, failed| {
    ///     // Count the attempts yourself, or a ticket that fails every time
    ///     // re-queues itself forever.
    ///     if matches!(event.kind, EventKind::TicketFailed) && failed.parent.is_none() {
    ///         queue.ticket(Ticket::new(failed.task.clone()).parent(&failed.key));
    ///     }
    /// });
    /// ```
    pub fn on_failure<F>(&self, handler: F) -> &Self
    where
        F: Fn(&Arc<TicketQueue>, &Event, &Ticket) + Send + Sync + 'static,
    {
        self.on_ticket_event(EventKind::is_failure, handler)
    }

    /// Read every failure together with the ticket it happened in, in a handler
    /// [`Self::finish`] waits for before it returns.
    ///
    /// [`Self::on_failure`] on the terms [`Self::on_event_async`] sets.
    ///
    /// Your handler MUST NOT call `finish` or [`Self::finish_all`], or it waits
    /// forever on the handover it is running inside.
    pub fn on_failure_async<F, Fut>(&self, handler: F) -> &Self
    where
        F: Fn(Arc<TicketQueue>, Event, Ticket) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_awaited(
            EventKind::is_failure,
            move |queue, event, ticket| match ticket {
                Some(ticket) => Box::pin(handler(queue, event, ticket)),
                None => Box::pin(std::future::ready(())),
            },
        )
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
        F: Fn(&Arc<TicketQueue>, &Event, &Ticket) + Send + Sync + 'static,
    {
        self.on_ticket_event(is_ticket_kind, handler)
    }

    /// Read a ticket as it starts, finishes, or fails, in a handler
    /// [`Self::finish`] waits for before it returns.
    ///
    /// [`Self::on_ticket`] on the terms [`Self::on_event_async`] sets.
    ///
    /// Your handler MUST NOT call `finish` or [`Self::finish_all`], or it waits
    /// forever on the handover it is running inside.
    pub fn on_ticket_async<F, Fut>(&self, handler: F) -> &Self
    where
        F: Fn(Arc<TicketQueue>, Event, Ticket) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_awaited(is_ticket_kind, move |queue, event, ticket| match ticket {
            Some(ticket) => Box::pin(handler(queue, event, ticket)),
            None => Box::pin(std::future::ready(())),
        })
    }

    /// Register an awaited handler and make sure the kinds it accepts are being
    /// queued for it.
    fn on_awaited<F>(&self, matches: fn(&EventKind) -> bool, call: F) -> &Self
    where
        F: Fn(Arc<TicketQueue>, Event, Option<Ticket>) -> HandlerWork + Send + Sync + 'static,
    {
        self.queue_events();
        self.awaited_events
            .handlers
            .lock()
            .unwrap()
            .push(AwaitedHandler {
                matches,
                call: Arc::new(call),
            });
        self
    }

    /// Installed only once, or a second registration would queue every event
    /// twice.
    fn queue_events(&self) {
        self.awaited_events.queueing.get_or_init(|| {
            self.on_event(|queue, event| {
                let anyone_wants_it = queue
                    .awaited_events
                    .handlers
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|handler| (handler.matches)(&event.kind));
                if !anyone_wants_it {
                    return;
                }
                // Resolved now rather than at handover, so a handler sees the
                // ticket as it was when the event landed. Only for the kinds a
                // ticket-shaped hook accepts: resolving copies every reply,
                // which on `TextChunkReceived` would cost once per piece.
                let ticket = match is_ticket_kind(&event.kind) || EventKind::is_failure(&event.kind)
                {
                    true => queue.get_ticket(&event.ticket_key),
                    false => None,
                };
                queue
                    .awaited_events
                    .queued
                    .lock()
                    .unwrap()
                    .push_back((event.clone(), ticket));
            });
        });
    }

    /// Loops because a handler that takes a while lets more events queue up
    /// behind it. One is taken per lock, so a `finish` dropped mid-handover, by
    /// a timeout or a panic, loses only the event it was on.
    async fn await_handlers(&self) {
        let handlers: Vec<(fn(&EventKind) -> bool, Arc<AsyncHandler>)> = self
            .awaited_events
            .handlers
            .lock()
            .unwrap()
            .iter()
            .map(|handler| (handler.matches, handler.call.clone()))
            .collect();
        if handlers.is_empty() {
            return;
        }
        let Some(queue) = self.weak_self.upgrade() else {
            return;
        };
        // Waits rather than skips, or a second `finish` could return while its
        // own events were still being handed over.
        let _draining = self.awaited_events.draining.lock().await;
        loop {
            let next = self.awaited_events.queued.lock().unwrap().pop_front();
            let Some((event, ticket)) = next else {
                return;
            };
            for (matches, call) in &handlers {
                if matches(&event.kind) {
                    call(Arc::clone(&queue), event.clone(), ticket.clone()).await;
                }
            }
        }
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
        // Pushed before the handlers run, so one receiving the ticket sees
        // it. Nothing is written: the line is already in `events.jsonl`, and
        // `load` reads it back from there.
        if is_recorded_failure(&event.kind) {
            let mut store = self.tickets.lock().unwrap();
            if let Some(ticket) = store.get_mut(key) {
                ticket.errors.push(event.clone());
            }
        }
        let handlers: Vec<Arc<EventHandler>> = self.event_handlers.lock().unwrap().clone();
        if handlers.is_empty() {
            default_logger()(&event);
            return event;
        }
        // Handed to every handler, so one that files follow-up work needs no
        // reference of its own. Gone only while the queue is being dropped,
        // when there is nothing left for a handler to act on.
        let Some(queue) = self.weak_self.upgrade() else {
            return event;
        };
        for h in &handlers {
            h(&queue, &event);
        }
        event
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
            .find(|a| a.id() == agent_id)
            .map(|a| a.get_model().name.clone())
    }

    /// Set the execution limits and retry tuning.
    ///
    /// The whole `Policy` is replaced, so build one from the fields you want:
    /// `Policy { max_turns: Some(40), ..Default::default() }`. A
    /// `compaction_threshold` outside `0.0..=1.0` is clamped into it.
    pub fn policy(&self, mut policy: Policy) -> &Self {
        // NaN survives `clamp` and would put the threshold at zero, compacting
        // every turn. A full window is the harmless reading of nonsense.
        policy.compaction_threshold = policy.compaction_threshold.map(|fraction| {
            if fraction.is_nan() {
                1.0
            } else {
                fraction.clamp(0.0, 1.0)
            }
        });
        *self.policy.lock().unwrap() = policy;
        self
    }

    /// Get the execution limits and retry tuning in force.
    pub fn get_policy(&self) -> Policy {
        self.policy.lock().unwrap().clone()
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
        self.matching_tickets(&Query::all())
    }

    /// Get every ticket matching a condition, in creation order unless the
    /// query names another with `ORDER BY`.
    ///
    /// Your condition MUST NOT call another `TicketQueue` method that reads
    /// the ticket store, or the call deadlocks.
    pub fn find_tickets(&self, predicate: impl Matcher<Ticket>) -> Vec<Ticket> {
        self.matching_tickets(&predicate.into_query())
    }

    /// Get the first ticket matching a condition, the earliest one unless the
    /// query names another order.
    ///
    /// Your condition MUST NOT call another `TicketQueue` method that reads
    /// the ticket store, or the call deadlocks.
    pub fn find_ticket(&self, predicate: impl Matcher<Ticket>) -> Option<Ticket> {
        self.first_matching_ticket(&predicate.into_query())
    }

    fn matching_tickets(&self, query: &Query) -> Vec<Ticket> {
        let store = self.tickets.lock().unwrap();
        let mut matching: Vec<&Ticket> = store.values().filter(|t| query.matches(t)).collect();
        query.sort(&mut matching);
        matching.into_iter().cloned().collect()
    }

    /// The first of those, and the only ticket copied.
    fn first_matching_ticket(&self, query: &Query) -> Option<Ticket> {
        let store = self.tickets.lock().unwrap();
        let mut matching: Vec<&Ticket> = store.values().filter(|t| query.matches(t)).collect();
        query.sort(&mut matching);
        matching.into_iter().next().cloned()
    }

    /// Get every recorded event matching a condition, oldest first, or in the
    /// order an `ORDER BY` names.
    ///
    /// The condition is an AQL string, a [`Query<Event>`](crate::Query), or a
    /// closure, the way [`Self::find_tickets`] takes any of the three.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// let tickets = TicketQueue::new();
    /// tickets.find_events("tool_call_failed AND created > -1h");
    /// ```
    ///
    /// Read from the session's `events.jsonl`, so this answers for a run that
    /// has finished as readily as one still working. Counting is `.len()`, and
    /// a total is a fold over the events themselves. A log that cannot be read
    /// finds nothing, and `TextChunkReceived` is never recorded, so a condition
    /// naming it never matches.
    pub fn find_events(&self, matcher: impl Matcher<Event>) -> Vec<Event> {
        let query = matcher.into_query();
        let mut out = self.collect_events(&query, usize::MAX);
        query.sort(&mut out);
        out
    }

    /// Get the earliest recorded event matching a condition, or the first in
    /// the order an `ORDER BY` names.
    pub fn find_event(&self, matcher: impl Matcher<Event>) -> Option<Event> {
        let query = matcher.into_query();
        // Without an order the log's own is the answer, so one match ends the
        // read instead of the whole log being copied to be sorted.
        let wanted = match query.is_ordered() {
            true => usize::MAX,
            false => 1,
        };
        let mut found = self.collect_events(&query, wanted);
        query.sort(&mut found);
        found.into_iter().next()
    }

    /// In log order: the caller sorts if its query named one.
    fn collect_events(&self, query: &Query<Event>, wanted: usize) -> Vec<Event> {
        let mut out = Vec::new();
        let _ = Stats::for_each_event(&self.get_dir(), |event| {
            if out.len() < wanted && query.matches(event) {
                out.push(event.clone());
            }
        });
        out
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
    /// # use agentwerk::TicketQueue;
    /// let tickets = TicketQueue::new();
    /// tickets.cancel("scan");
    /// ```
    pub fn cancel(&self, matches: impl Matcher<Ticket>) -> &Self {
        self.cancel_filters
            .lock()
            .unwrap()
            .push(matches.into_query());
        self
    }

    /// Take every ticket off the queue, which ends the run.
    ///
    /// [`Self::finish_all`] then reports `FinishReason::Cancelled`. Like
    /// [`Self::cancel`], nothing waits, so a ctrl-c handler can call it.
    pub fn cancel_all(&self) -> &Self {
        self.cancel(Query::all())
    }

    /// Check whether a ticket has been cancelled.
    ///
    /// Ask before creating follow-up work: a cancelled ticket is never claimed.
    /// It reads no ticket state, so a condition passed to [`Self::find_ticket`]
    /// or [`Self::find_tickets`] may call it.
    pub fn is_cancelled(&self, ticket: &Ticket) -> bool {
        // Cloned out first: a filter may hold a closure, and one that reaches
        // back here would meet a lock it already holds.
        let filters: Vec<Query> = self.cancel_filters.lock().unwrap().clone();
        filters.iter().any(|matches| matches.matches(ticket))
    }

    /// True while any matching ticket still has work for an agent.
    ///
    /// The one definition of "not done yet": the main loop asks it of every
    /// ticket to decide the run is over, and [`Self::finish`] asks it of a
    /// subset. A ticket is pending while it is todo or in progress,
    /// uncancelled, and not paused for a caller reply.
    pub(crate) fn pending(&self, matches: &Query) -> bool {
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
        if let Some((violation, _)) = policy_violated(&self.get_policy(), &self.stats) {
            return Some(FinishReason::PolicyViolated(violation));
        }
        if self.anything_claimable() {
            return None;
        }
        // A cancel is a statement that work should stop, so it ends a run with
        // nothing claimable left even when the queue was already empty.
        let cancelled = !self.cancel_filters.lock().unwrap().is_empty();
        cancelled.then_some(FinishReason::Cancelled)
    }

    /// The one definition of a ticket an agent could still take, which both the
    /// ending check and [`Self::anything_pending`] ask for.
    fn anything_claimable(&self) -> bool {
        let tickets = self.tickets.lock().unwrap();
        tickets
            .values()
            .any(|t| t.is_pending() && !self.is_cancelled(t))
    }

    /// True while any ticket is still open. Stricter than [`Self::pending`]:
    /// an interactive agent's paused ticket has no work for it right now, but a
    /// reply revives it, so the run is not over.
    fn anything_pending(&self) -> bool {
        // A terminal transition mid-flight may still add a follow-up ticket
        // from a handler, so it counts as work whatever the store says.
        self.terminal_transitions_in_flight.load(Ordering::SeqCst) > 0 || self.anything_claimable()
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
        agent.require_provider_and_model();
        agent.register_finish_tool();
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
                let reporter = agent.id().to_string();
                for ticket in drained {
                    self.insert(ticket, reporter.clone());
                }
            }
        }
        agent.ticket_queue = TicketQueueRef::Shared(self.weak_self.clone());
        self.agents.lock().unwrap().push(agent.clone());
    }

    /// Whether an agent under this id is registered. `Agent::start` asks
    /// before binding, so starting twice runs one agent, not two.
    pub(crate) fn has_agent(&self, id: &str) -> bool {
        self.agents.lock().unwrap().iter().any(|a| a.id() == id)
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
    /// # use agentwerk::TicketQueue;
    /// # async fn run() {
    /// let tickets = TicketQueue::new();
    /// for finding in tickets.finish("research").await {
    ///     println!("{finding}");
    /// }
    /// # }
    /// ```
    pub async fn finish(&self, matches: impl Matcher<Ticket>) -> Vec<serde_json::Value> {
        let query = matches.into_query();
        if self.join_handle.lock().unwrap().is_none() {
            self.start();
        }
        let mut stream = self.event_stream.subscribe();
        while self.pending(&query) {
            let ended = self.next_event_or_end(&mut stream).await;
            self.await_handlers().await;
            if ended {
                break;
            }
        }
        // Again after the wait, for whatever landed in its last turn.
        self.await_handlers().await;
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
        // `and_status`, not the `find_results` default: a caller who waited on
        // `status = Todo` is handed the results that landed, not the todos.
        self.matching_tickets(&query.and_status(Status::Finished))
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
        self.finish(Query::all()).await
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
        self.find_results(Query::all())
    }

    /// Get every result whose ticket matches the query, in creation order.
    ///
    /// Status defaults to `Finished` when the filter names none, which is
    /// every closure and any query that leaves it unset. A caller that sets
    /// `.status(Status::Failed)` keeps that filter. A ticket contributes a
    /// result only when it has one, so this is shorter than the set the
    /// filter named.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// let tickets = TicketQueue::new();
    /// let scans = tickets.find_results("scan");
    /// ```
    pub fn find_results(&self, matches: impl Matcher<Ticket>) -> Vec<serde_json::Value> {
        self.matching_tickets(&results_of(matches))
            .into_iter()
            .filter_map(|t| t.result)
            .collect()
    }

    /// Get the first result whose ticket matches the query.
    ///
    /// Status defaults to `Finished` as in [`Self::find_results`], and the
    /// order is that method's too.
    pub fn find_result(&self, matches: impl Matcher<Ticket>) -> Option<serde_json::Value> {
        self.first_matching_ticket(&results_of(matches))
            .and_then(|t| t.result)
    }
}

/// The tickets that contribute to `find_results`. The result term is why
/// `find_result` answers with the first match carrying one, not the first
/// match.
fn results_of(matches: impl Matcher<Ticket>) -> Query {
    matches
        .into_query()
        .default_status(Status::Finished)
        .and_result()
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
    fn find_tickets_answers_in_creation_order() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        queue.ticket("c");
        let keys: Vec<String> = queue
            .find_tickets(|t: &Ticket| t.is_todo())
            .into_iter()
            .map(|t| t.key)
            .collect();
        assert_eq!(keys, ["TICKET-1", "TICKET-2", "TICKET-3"]);
    }

    #[test]
    fn cancel_ignores_an_order_by_and_takes_every_ticket() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        queue.cancel("ORDER BY key DESC");
        assert!(queue.tickets().iter().all(|t| queue.is_cancelled(t)));
    }

    #[test]
    fn find_tickets_answers_in_the_order_the_query_names() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        queue.ticket("c");
        let keys: Vec<String> = queue
            .find_tickets("ORDER BY key DESC")
            .into_iter()
            .map(|t| t.key)
            .collect();
        assert_eq!(keys, ["TICKET-3", "TICKET-2", "TICKET-1"]);
    }

    #[test]
    fn find_ticket_answers_the_first_in_the_order_the_query_names() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        let found = queue.find_ticket("ORDER BY key DESC").expect("a ticket");
        assert_eq!(found.key, "TICKET-2");
    }

    #[test]
    fn find_results_answers_in_the_order_the_query_names() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        attach_done_result(&queue, "TICKET-1", "first");
        attach_done_result(&queue, "TICKET-2", "second");
        assert_eq!(
            queue.find_results("ORDER BY key DESC"),
            vec![serde_json::json!("second"), serde_json::json!("first")]
        );
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
    fn find_results_defaults_to_finished_when_the_query_names_no_status() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::labeled("scan", "a"));
        queue.ticket(Ticket::labeled("scan", "b"));
        attach_done_result(&queue, "TICKET-1", "scanned");
        assert_eq!(
            queue.find_results("scan"),
            vec![serde_json::json!("scanned")]
        );
    }

    #[test]
    fn find_results_keeps_the_status_the_query_names() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::labeled("scan", "a"));
        attach_done_result(&queue, "TICKET-1", "scanned");
        assert!(queue
            .find_results("label = scan AND status = Todo")
            .is_empty());
    }

    #[test]
    fn find_results_takes_a_closure_in_place_of_a_query() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::labeled("scan", "a"));
        queue.ticket(Ticket::labeled("report", "b"));
        attach_done_result(&queue, "TICKET-1", "scanned");
        attach_done_result(&queue, "TICKET-2", "reported");
        assert_eq!(
            queue.find_results(|t: &Ticket| t.has_label("scan")),
            vec![serde_json::json!("scanned")]
        );
    }

    #[test]
    fn find_results_defaults_a_closure_to_finished_tickets() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::labeled("scan", "a"));
        // A result attached without the finish transition, which the default
        // leaves out because a closure names no status of its own.
        queue
            .set_result("TICKET-1", serde_json::json!("mid-flight"))
            .unwrap();
        assert!(queue
            .find_results(|t: &Ticket| t.has_label("scan"))
            .is_empty());
    }

    #[test]
    fn find_tickets_compiles_the_string_as_a_query() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::labeled("scan", "a"));
        queue.ticket(Ticket::labeled("report", "b"));
        let found = queue.find_tickets("label = report AND status = Todo");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].task, serde_json::json!("b"));
    }

    #[test]
    fn pending_on_a_todo_ticket() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        assert!(queue.pending(&Query::all()));
    }

    #[test]
    fn pending_only_for_the_matching_tickets() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("research"));
        assert!(queue.pending(&Query::from("research")));
        assert!(!queue.pending(&Query::from("report")));
    }

    #[test]
    fn pending_while_a_claimed_ticket_awaits_the_model() {
        let (queue, _tmp) = test_queue();
        queue.ticket("x");
        queue.claim(&Query::from("status = Todo"), "agent").unwrap();
        assert!(queue.pending(&Query::all()));
    }

    #[test]
    fn pending_when_a_text_only_reply_pauses_a_non_interactive_agent() {
        let (queue, _tmp) = test_queue();
        queue.ticket("x");
        let key = queue.claim(&Query::from("status = Todo"), "agent").unwrap();
        queue.add_reply(
            &key,
            Reply::assistant(&[crate::providers::ContentBlock::Text {
                text: "hello".into(),
            }]),
        );
        // Only an interactive agent waits on the caller; the rest are retried.
        assert!(queue.pending(&Query::all()));
    }

    #[test]
    fn not_pending_once_every_ticket_is_finished_or_failed() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        let key_a = queue.claim(&Query::from("TICKET-1"), "agent").unwrap();
        let key_b = queue.claim(&Query::from("TICKET-2"), "agent").unwrap();
        queue.set_finished_by(&key_a, "agent").unwrap();
        queue.set_failed(&key_b).unwrap();
        assert!(!queue.pending(&Query::all()));
    }

    #[test]
    fn not_pending_on_a_cancelled_ticket() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("research"));
        queue.cancel(|t: &Ticket| t.has_label("research"));
        assert!(!queue.pending(&Query::all()));
    }

    #[test]
    fn policy_round_trips_through_get_policy() {
        let (queue, _tmp) = test_queue();
        let policy = Policy {
            max_turns: Some(40),
            max_input_tokens: Some(200_000),
            max_output_tokens: Some(50_000),
            max_request_tokens: Some(8_000),
            max_schema_retries: Some(3),
            max_request_retries: 5,
            request_retry_delay: Duration::from_millis(250),
            max_time: Some(Duration::from_secs(300)),
            compaction_threshold: Some(0.75),
        };
        queue.policy(policy.clone());

        assert_eq!(queue.get_policy(), policy);
    }

    #[test]
    fn get_policy_returns_the_defaults_before_policy_is_called() {
        let (queue, _tmp) = test_queue();
        assert_eq!(queue.get_policy(), Policy::default());
    }

    #[test]
    fn compaction_threshold_clamps_a_fraction_outside_the_unit_range() {
        let (queue, _tmp) = test_queue();
        for (given, expected) in [(1.5, 1.0), (-0.2, 0.0), (f64::NAN, 1.0)] {
            queue.policy(Policy {
                compaction_threshold: Some(given),
                ..Default::default()
            });
            assert_eq!(
                queue.get_policy().compaction_threshold,
                Some(expected),
                "given {given}"
            );
        }
    }

    #[test]
    fn find_events_returns_the_matching_events_oldest_first() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");
        queue.claim(&Query::from("TICKET-1"), "alice");

        let created = queue.find_events(|e: &Event| matches!(e.kind, EventKind::TicketCreated));
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
            .find_events(|e: &Event| matches!(e.kind, EventKind::RunFinished { .. }))
            .is_empty());
    }

    #[test]
    fn find_events_without_a_log_is_empty() {
        let (queue, _tmp) = test_queue();
        assert!(queue.find_events(|_: &Event| true).is_empty());
    }

    #[test]
    fn find_event_returns_the_earliest_match() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");

        let first = queue.find_event(|e: &Event| matches!(e.kind, EventKind::TicketCreated));
        assert_eq!(first.unwrap().ticket_key, "TICKET-1");
        assert!(queue
            .find_event(|e: &Event| matches!(e.kind, EventKind::TicketFailed))
            .is_none());
    }

    #[test]
    fn find_events_takes_the_same_syntax_a_ticket_query_is_written_in() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("scan").label("scout"));
        queue.ticket("b");
        queue.claim(&Query::from("TICKET-1"), "scout-1");

        assert_eq!(queue.find_events("ticket_created").len(), 2);
        assert_eq!(queue.find_events("TICKET-1").len(), 2);
        assert_eq!(queue.find_events("event = ticket_started").len(), 1);
        assert_eq!(queue.find_events("agent = scout-1").len(), 1);
        assert!(queue.find_events("run_finished").is_empty());
    }

    #[test]
    fn find_event_answers_the_first_in_the_order_the_query_names() {
        let (queue, _tmp) = test_queue();
        queue.ticket("a");
        queue.ticket("b");

        let newest = queue.find_event("ticket_created ORDER BY created DESC");
        assert_eq!(newest.unwrap().ticket_key, "TICKET-2");
        let oldest = queue.find_event("ticket_created");
        assert_eq!(oldest.unwrap().ticket_key, "TICKET-1");
    }

    #[test]
    fn a_condition_reads_the_label_and_the_agent_that_caused_the_event() {
        // What makes a per-label or per-agent breakdown possible without the
        // crate keeping one.
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("scan").label("scout"));
        queue.claim(&Query::from("scout"), "scout-1");

        assert_eq!(
            queue
                .find_events(|e: &Event| e.label.as_deref() == Some("scout"))
                .len(),
            2
        );
        assert_eq!(
            queue.find_events(|e: &Event| e.agent_id == "scout-1").len(),
            1
        );
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
            .find_events(|e: &Event| matches!(e.kind, EventKind::TextChunkReceived { .. }))
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
        assert!(queue.find_events(|_: &Event| true).is_empty());
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
        queue.on_event(move |_, _| l1.lock().unwrap().push(1));
        queue.on_event(move |_, _| l2.lock().unwrap().push(2));
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
        queue.on_event(move |_, event| {
            if matches!(event.kind, EventKind::TicketFinished) {
                seen.lock()
                    .unwrap()
                    .push((event.agent_id.clone(), event.label.clone()));
            }
        });
        queue.ticket(Ticket::new("a").label("scan"));
        let key = queue.claim(&Query::from("scan"), "scout").unwrap();
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
        queue.on_result(move |_, ticket, result| {
            record
                .lock()
                .unwrap()
                .push((ticket.key.clone(), result.clone()))
        });
        queue.ticket(Ticket::new("x").label("L"));
        let key = queue.claim(&Query::from("L"), "agent").unwrap();

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
        queue.on_failure(move |_, event, ticket| {
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
    fn failures_accumulate_on_the_ticket_in_order() {
        let (queue, dir) = test_queue();
        let key = queue.ticket("work");

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
        queue.emit(
            &key,
            "agent",
            EventKind::RequestFailed {
                model: "mock".into(),
                reason: crate::providers::RequestErrorKind::ConnectionFailed,
                message: "dns lookup failed".into(),
            },
        );

        let ticket = queue.get_ticket(&key).unwrap();
        let names: Vec<&str> = ticket.errors.iter().map(|e| e.kind.name()).collect();
        assert_eq!(names, ["tool_call_failed", "request_failed"]);

        // Written once, to the session log, as a full event per line.
        let body = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let logged: Vec<String> = body
            .lines()
            .map(|line| serde_json::from_str::<crate::event::Event>(line).unwrap())
            .filter(|event| is_recorded_failure(&event.kind))
            .map(|event| event.kind.name().to_string())
            .collect();
        assert_eq!(logged, names);
        assert!(!dir
            .path()
            .join("tickets")
            .join(&key)
            .join("errors.jsonl")
            .exists());
    }

    #[test]
    fn a_recoverable_failure_stays_on_a_finished_ticket() {
        let (queue, _tmp) = test_queue();
        let key = queue.ticket("work");

        // A failed tool call the model recovered from: the ticket finishes.
        queue.emit(
            &key,
            "agent",
            EventKind::ToolCallFailed {
                tool_name: "grep".into(),
                call_id: "c1".into(),
                reason: ToolFailureKind::ExecutionFailed,
                message: "boom".into(),
            },
        );
        queue.set_finished(&key, "done").unwrap();

        let ticket = queue.get_ticket(&key).unwrap();
        assert_eq!(ticket.status, Status::Finished);
        assert_eq!(ticket.errors.len(), 1);
        assert_eq!(ticket.errors[0].kind.name(), "tool_call_failed");
    }

    #[test]
    fn the_terminal_ticket_failed_is_not_recorded_as_an_error() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let queue = TicketQueue::new();
        queue.dir(dir.path().to_path_buf());
        let key = queue.ticket("work");
        queue.set_failed(&key).unwrap();
        assert!(queue.get_ticket(&key).unwrap().errors.is_empty());

        // The log carries `ticket_failed` either way, so a resumed session that
        // read it back as a failure would disagree with the run that wrote it.
        drop(queue);
        let resumed = TicketQueue::load(dir.path()).unwrap();
        assert!(resumed.get_ticket(&key).unwrap().errors.is_empty());
    }

    #[test]
    fn a_failure_naming_a_ticket_the_directory_lost_is_skipped() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        let key = original.ticket("work");
        original.emit(
            &key,
            "agent",
            EventKind::ToolCallFailed {
                tool_name: "grep".into(),
                call_id: "c1".into(),
                reason: ToolFailureKind::ExecutionFailed,
                message: "boom".into(),
            },
        );
        drop(original);
        std::fs::remove_dir_all(dir.path().join("tickets").join(&key)).unwrap();

        let resumed = TicketQueue::load(dir.path()).unwrap();
        assert!(resumed.get_ticket(&key).is_none());
        assert_eq!(resumed.input_tokens(), 0);
    }

    #[test]
    fn failures_round_trip_through_load() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = TicketQueue::new();
        original.dir(dir.path().to_path_buf());
        let key = original.ticket("work");
        original.emit(
            &key,
            "agent",
            EventKind::ToolCallFailed {
                tool_name: "grep".into(),
                call_id: "c1".into(),
                reason: ToolFailureKind::ExecutionFailed,
                message: "boom".into(),
            },
        );
        drop(original);

        let resumed = TicketQueue::load(dir.path()).unwrap();
        let ticket = resumed.get_ticket(&key).unwrap();
        assert_eq!(ticket.errors.len(), 1);
        assert_eq!(ticket.errors[0].kind.name(), "tool_call_failed");
    }

    #[test]
    fn on_failure_files_a_retry_through_the_queue_it_is_handed() {
        let (queue, _tmp) = test_queue();
        queue.on_failure(|queue, _, failed| {
            if failed.parent.is_none() {
                queue.ticket(Ticket::new(failed.task.clone()).parent(&failed.key));
            }
        });
        let key = queue.ticket("work");

        queue.set_failed(&key).unwrap();

        let retry = queue.find_ticket(format!("parent = {key}")).unwrap();
        assert_eq!(retry.task, serde_json::json!("work"));
    }

    #[test]
    fn on_event_files_a_follow_up_for_any_kind() {
        let (queue, _tmp) = test_queue();
        queue.on_event(|queue, event| {
            if matches!(event.kind, EventKind::TurnStarted) {
                queue.ticket(Ticket::new("report").label("report"));
            }
        });
        let key = queue.ticket("work");

        queue.emit(&key, "agent", EventKind::TurnStarted);

        assert_eq!(
            queue.find_tickets(|t: &Ticket| t.has_label("report")).len(),
            1
        );
    }

    #[test]
    fn on_result_links_a_follow_up_to_the_finished_parent() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("scout").label("scout"));
        let key = queue.claim(&Query::from("scout"), "agent").unwrap();
        queue.set_result(&key, serde_json::json!("lead")).unwrap();
        queue.set_finished_by(&key, "agent").unwrap();
        queue.on_result(|queue, done, _| {
            queue.ticket(Ticket::new("hunt").label("sniper").parent(&done.key));
        });
        queue.emit(&key, "agent", EventKind::TicketFinished);
        let spawned = queue
            .find_ticket(|t: &Ticket| t.has_label("sniper"))
            .unwrap();
        assert_eq!(spawned.parent, Some(key));
    }

    #[test]
    fn on_result_ignores_unfinished_events() {
        let (queue, _tmp) = test_queue();
        queue.on_result(|queue, _, _| {
            queue.ticket(Ticket::new("follow-up").label("next"));
        });
        queue.emit("KEY", "agent", EventKind::TurnStarted);
        assert!(queue.tickets().is_empty());
    }

    #[test]
    fn on_result_reads_the_results_that_landed_before_it() {
        // A condition across results, which the handler selects for itself.
        let (queue, _tmp) = test_queue();
        let counts = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&counts);
        queue.on_result(move |queue, _, _| {
            record
                .lock()
                .unwrap()
                .push(queue.find_results("scan").len());
        });
        for key in scans(&queue, 2) {
            queue.set_finished(&key, "clean").unwrap();
        }

        assert_eq!(*counts.lock().unwrap(), vec![1, 2]);
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
        queue.on_result_async(move |_, ticket, result| {
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
            queue.on_result_async(move |_, _, _| {
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
                    queue.on_result_async(move |_, _, _| {
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
        queue.on_result_async(move |_, ticket, _| {
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
        queue.on_result_async(move |_, ticket, _| {
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
    async fn every_kind_of_async_handler_sees_its_event_once() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let one = Arc::clone(&seen);
        queue.on_result_async(move |_, _, result| {
            let one = Arc::clone(&one);
            async move { one.lock().unwrap().push(format!("result {result}")) }
        });
        let each = Arc::clone(&seen);
        queue.on_ticket_async(move |_, event, _| {
            let each = Arc::clone(&each);
            async move {
                each.lock()
                    .unwrap()
                    .push(format!("ticket {}", event.kind.name()))
            }
        });
        let key = queue.ticket("scan the corpus");
        queue.set_finished(&key, 1).unwrap();

        queue.finish_all().await;

        // One entry each: the two kinds share one queueing hook.
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["result 1", "ticket ticket_finished"]
        );
    }

    #[tokio::test]
    async fn on_event_async_sees_the_kinds_no_ticket_hook_accepts() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_event_async(move |_, event| {
            let record = Arc::clone(&record);
            async move { record.lock().unwrap().push(event.kind.name()) }
        });
        queue.emit("KEY", "agent", EventKind::TurnStarted);

        queue.finish_all().await;

        assert!(seen.lock().unwrap().contains(&"turn_started"));
    }

    #[tokio::test]
    async fn on_failure_async_hands_over_the_failed_ticket() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_failure_async(move |_, event, ticket| {
            let record = Arc::clone(&record);
            async move {
                record
                    .lock()
                    .unwrap()
                    .push((event.kind.name(), ticket.key.clone()))
            }
        });
        let key = queue.ticket("scan the corpus");
        queue.set_failed(&key).unwrap();

        queue.finish_all().await;

        assert_eq!(*seen.lock().unwrap(), vec![("ticket_failed", key)]);
    }

    #[tokio::test]
    async fn an_async_handler_files_a_follow_up_through_the_queue_it_is_handed() {
        let (queue, _tmp) = test_queue();
        queue.on_result_async(|queue, done, _| async move {
            queue.ticket(Ticket::new("hunt").label("sniper").parent(&done.key));
        });
        let key = queue.ticket(Ticket::new("scout").label("scout"));
        queue.set_finished(&key, "lead").unwrap();

        queue.finish_all().await;

        let spawned = queue
            .find_ticket(|t: &Ticket| t.has_label("sniper"))
            .unwrap();
        assert_eq!(spawned.parent, Some(key));
    }

    #[tokio::test]
    async fn an_async_handler_waits_for_a_finish_to_run_it() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_result_async(move |_, ticket, _| {
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
        queue.on_result_async(move |_, ticket, _| {
            let record = Arc::clone(&record);
            async move { record.lock().unwrap().push(ticket.key) }
        });
        let key = queue.ticket("scan the corpus");
        queue.set_failed(&key).unwrap();

        queue.finish_all().await;

        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_hook_waits_for_the_results_it_needs_before_filing_the_next_step() {
        let (queue, _tmp) = test_queue();
        queue.on_result(|queue, _, _| {
            if queue.find_results("scan").len() == 2 {
                queue.ticket(Ticket::new("write the report").label("report"));
            }
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
    fn on_result_inserts_a_follow_up_before_drain_is_observable() {
        let (queue, _tmp) = test_queue();
        queue.on_result(|queue, done, _| {
            if done.has_label("scout") {
                queue.ticket(Ticket::new("hunt").label("sniper"));
            }
        });
        queue.ticket(Ticket::new("scout").label("scout"));
        let key = queue.claim(&Query::from("scout"), "agent").unwrap();
        queue.set_result(&key, serde_json::json!("lead")).unwrap();
        queue.set_finished_by(&key, "agent").unwrap();
        // The handler ran inside `set_finished_by`, so the queue is never
        // observably empty between the parent finishing and the follow-up.
        assert!(queue.pending(&Query::all()));
    }

    #[test]
    fn on_ticket_hands_the_handler_the_finished_ticket() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_ticket(move |_, event, ticket| {
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
        let key = queue.claim(&Query::from("scan"), "analyst").unwrap();
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
        let key = queue.claim(&Query::from("scan"), "analyst").unwrap();
        // Installed after the claim, so only the turn is in the handler's view.
        queue.on_ticket(move |_, _, ticket| record.lock().unwrap().push(ticket.key.clone()));
        queue.emit(&key, "analyst", EventKind::TurnStarted);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn on_ticket_skips_events_that_name_no_ticket() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_ticket(move |_, _, ticket| record.lock().unwrap().push(ticket.key.clone()));
        queue.emit("", "", EventKind::RunStarted);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn on_ticket_coexists_with_a_user_handler() {
        let (queue, _tmp) = test_queue();
        let logged = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&logged);
        queue.on_event(move |_, e| log.lock().unwrap().push(e.kind.name()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_ticket(move |_, _, ticket| record.lock().unwrap().push(ticket.key.clone()));
        queue.ticket(Ticket::new("scan").label("scan"));
        // The claim is the lifecycle event; no second emit needed.
        let key = queue.claim(&Query::from("scan"), "analyst").unwrap();

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
        queue.on_event(move |_, _| {
            c1.fetch_add(1, Ordering::Relaxed);
        });
        queue.on_event(move |_, _| {
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
        queue.policy(Policy {
            max_turns: Some(0),
            ..Default::default()
        });
        queue.finish_all().await;
        assert_eq!(
            *reasons.lock().unwrap(),
            vec![FinishReason::PolicyViolated(
                crate::event::PolicyViolation::Turns
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
        queue.on_event(move |_, e| {
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
