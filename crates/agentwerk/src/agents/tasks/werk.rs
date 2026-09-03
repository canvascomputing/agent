//! Stores, assigns, and executes tasks for a shared `Werk`.

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

use super::super::agent::Agent;
use super::super::policy::Policy;
use super::super::query::{Matcher, Query};
use super::super::r#loop::run_main_loop;
use super::super::stats::Stats;
use super::task::{Status, Task};
use super::{numeric_id, policy_violated, Reply};
use crate::event::{default_logger, Event};
use crate::persistence::Persist;

/// Why execution ended.
///
/// Carried by a run-finished event, and handed back by
/// `Werk::finish_tasks` once the wait is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The Werk emptied; nothing more to do.
    Drained,
    /// A limit was breached.
    PolicyViolated(crate::agents::PolicyViolation),
    /// A `cancel` left nothing claimable.
    Cancelled,
}

impl std::fmt::Display for FinishReason {
    /// The violated limit is named inside the parentheses, as in
    /// `policy_violated(turns)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FinishReason::Drained => f.write_str("drained"),
            FinishReason::PolicyViolated(kind) => write!(f, "policy_violated({kind})"),
            FinishReason::Cancelled => f.write_str("cancelled"),
        }
    }
}

/// The Werk arrives first so a handler selects tasks and files follow-up work
/// without capturing an `Arc` into the Werk that holds it.
type EventHandler = dyn Fn(&Arc<Werk>, &Event) + Send + Sync;

/// How many events a `finish` waiter may fall behind before it starts
/// missing them. `TextChunkReceived` fires once per streaming delta and sets
/// the volume this has to absorb.
const EVENT_STREAM_CAPACITY: usize = 1024;

/// One shape for every awaited hook: the task is `None` for the kinds no
/// task-shaped hook accepts, and the wrapper each `on_*_async` installs picks
/// out what its own handler takes.
type AsyncHandler = dyn Fn(Arc<Werk>, Event, Option<Task>) -> HandlerWork + Send + Sync;

type AwaitedHandlerRef = (fn(&Event) -> bool, Arc<AsyncHandler>);

/// Boxed so the Werk can hold handlers with different future types.
type HandlerWork = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The names that identify events whose whole subject is a task, as opposed to one
/// step inside it.
fn is_task_event(event: &Event) -> bool {
    matches!(
        event.name.as_str(),
        Event::TASK_STARTED | Event::TASK_FINISHED | Event::TASK_FAILED
    )
}

fn is_failure(event: &Event) -> bool {
    matches!(
        event.name.as_str(),
        Event::TASK_FAILED
            | Event::REQUEST_FAILED
            | Event::TOOL_CALL_FAILED
            | Event::KNOWLEDGE_FAILED
            | Event::COMPACTION_FAILED
    )
}

/// `task_failed` is left out: it is the outcome, already carried by the
/// task's status and `failed_at`, not a cause.
fn is_recorded_failure(event: &Event) -> bool {
    is_failure(event) && event.name != Event::TASK_FAILED
}

/// An awaited handler and the events it accepts. The filter is read twice: once
/// to decide whether the event is worth queueing at all, once at handover.
struct AwaitedHandler {
    matches: fn(&Event) -> bool,
    call: Arc<AsyncHandler>,
}

/// An event held for the awaited handlers, with its task resolved as it was
/// when the event was emitted.
type Delivery = (Event, Option<Task>);

/// `emit_event` runs on an agent that has to carry on, so an event an awaited handler
/// wants is only queued here; whichever `finish` is waiting drains it and awaits
/// the handlers.
///
/// `queued` and `draining` are separate locks because `emit_event` pushes without
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
        // `wait_for` reads the current value before waiting, so it cannot miss
        // a phase change between those operations.
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

/// Store tasks, assign them to agents by label, and run agents concurrently.
///
/// ```no_run
/// use agentwerk::{Agent, Task, Werk};
/// use agentwerk::tools::FetchTool;
///
/// # async fn run() {
/// let werk = Werk::new();
/// for _ in 0..4 {
///     werk.add_agent(
///         Agent::from_env()
///             .label("research")
///             .tool(FetchTool::new()),
///     );
/// }
/// werk.add_task(Task::labeled("research", "Summarize https://canvascomputing.org"));
/// werk.finish_all_tasks().await;
/// # }
/// ```
///
/// # Sessions
///
/// A `Werk` writes every task, reply, statistic, and lifecycle
/// event to its working directory (default `./.agentwerk`). That directory is
/// the session: stop the process, and `Werk::load(dir)` reopens it
/// from disk and continues from where it stopped.
///
/// ```no_run
/// use agentwerk::Werk;
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let werk = Werk::load(".agentwerk")?;
/// // Re-register the agents, then call .start() or .finish_all_tasks().await.
/// # let _ = werk;
/// # Ok(())
/// # }
/// ```
///
/// On-disk layout:
///
/// ```text
/// .agentwerk/
/// ├── events.jsonl                          every event (one per line)
/// ├── tasks/
/// │   └── t-1/
/// │       ├── task.json                   the task without its messages or result
/// │       ├── result.json                   the result the agent produced
/// │       ├── replies.jsonl                 every message exchanged with the model, one per line
/// │       └── outputs/<tool_use_id>.txt     full tool outputs spilled out of the messages
/// └── knowledge/
///     ├── pages/<slug>.md                   knowledge pages
///     └── index.md                          knowledge index
/// ```
pub struct Werk {
    pub(super) weak_self: Weak<Werk>,
    pub(crate) tasks: Mutex<HashMap<String, Task>>,
    pub(super) agents: Mutex<Vec<Agent>>,
    pub(super) policy: Mutex<Policy>,
    /// Why the run ended, once the main loop decides. The agent tasks, the
    /// tools, and every `finish` read it to know the run is over.
    pub(crate) run: Arc<Run>,
    /// What `cancel` has taken off the Werk. A matching task is neither
    /// claimed nor resumed, and an agent already holding one is taken off it,
    /// while the rest of the run continues.
    pub(crate) cancel_filters: Mutex<Vec<Query>>,
    /// How many terminal status transitions are between their status change and
    /// the return of their event handlers. `pending` counts a non-zero value as
    /// pending work, so a handler creating a follow-up task always beats the drain.
    pub(crate) terminal_transitions_in_flight: AtomicUsize,
    pub(crate) stats: Stats,
    pub(super) event_handlers: Mutex<Vec<Arc<EventHandler>>>,
    pub(super) awaited_events: AwaitedEvents,
    /// Every emitted event, for `finish` to wake on. A separate channel rather
    /// than one more `on_event` entry: a handler stays on the chain for the
    /// life of the Werk, so one registered per call would grow without bound
    /// in a host that awaits in a loop.
    pub(super) event_stream: broadcast::Sender<Event>,
    pub(super) dir: Mutex<PathBuf>,
    pub(super) events_lock: Mutex<()>,
    /// The main loop, held so `start()` can join a previous one before starting
    /// the next.
    pub(super) join_handle: Mutex<Option<JoinHandle<()>>>,
    /// Next `t-<N>` ID to hand out, or `None` until it is known.
    /// `load()` seeds it from the tasks it just read off disk. `new()` leaves
    /// it `None` and the first `insert()` scans for the highest existing ID,
    /// since `new()` never reads the directory itself.
    pub(super) next_task_id: Mutex<Option<u64>>,
}

impl Werk {
    /// Create an empty Werk, shared through an `Arc`.
    pub fn new() -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            weak_self: weak.clone(),
            tasks: Mutex::new(HashMap::new()),
            agents: Mutex::new(Vec::new()),
            policy: Mutex::new(Policy::default()),
            run: Arc::new(Run::default()),
            cancel_filters: Mutex::new(Vec::new()),
            terminal_transitions_in_flight: AtomicUsize::new(0),
            stats: Stats::new(),
            event_handlers: Mutex::new(Vec::new()),
            awaited_events: AwaitedEvents::default(),
            event_stream: broadcast::Sender::new(EVENT_STREAM_CAPACITY),
            dir: Mutex::new(PathBuf::from(".agentwerk")),
            events_lock: Mutex::new(()),
            join_handle: Mutex::new(None),
            next_task_id: Mutex::new(None),
        })
    }

    /// Continue a session from `werk_dir`, or start one there when it is empty.
    ///
    /// Every task is read back with its status, result, and messages, and the
    /// statistics resume from `events.jsonl`, so the turn and token budgets
    /// limit checks stay continuous across restarts. Loading knowledge from
    /// `<werk_dir>/knowledge` keeps its pages beside the session.
    ///
    /// An unfinished task is picked up again by the agent whose ID it carries
    /// as its assignee. IDs are numbered per label as agents take them, so
    /// create the same agents in the same order after a restart.
    ///
    /// A task that cannot be read stops the load and the returned error names
    /// it, rather than handing back a store quietly missing that task. Files
    /// written by an older version are the usual cause: delete the session
    /// directory, or migrate it, and load again.
    pub fn load(werk_dir: impl Into<PathBuf>) -> io::Result<Arc<Self>> {
        let werk_dir = werk_dir.into();
        std::fs::create_dir_all(werk_dir.join("tasks"))?;

        let mut tasks = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(werk_dir.join("tasks")) {
            for entry in entries.flatten() {
                let task_dir = entry.path();
                if !task_dir.is_dir() || !task_dir.join("task.json").is_file() {
                    continue;
                }
                let Some(id) = task_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                // Skipping an unreadable task would drop its status, result
                // and timestamps with it, leaving the Werk to resume work it
                // has no record of.
                let task = Task::load(&werk_dir, &id).map_err(|source| {
                    io::Error::new(
                        source.kind(),
                        format!("task {id} could not be read: {source}"),
                    )
                })?;
                tasks.insert(task.id.clone(), task);
            }
        }

        // One pass over the log fills both: the figures the run resumes on, and
        // the failures each task saw, which no task file carries.
        let stats = Stats::new();
        let _ = Stats::for_each_event(&werk_dir, |event| {
            stats.record(event);
            if is_recorded_failure(event) {
                if let Some(task) = tasks.get_mut(&event.task_id) {
                    task.errors.push(event.clone());
                }
            }
        });
        // The clock starts over: `max_time` bounds this run, not the one that
        // wrote the log.
        stats.restart_clock();
        let next_id = tasks
            .keys()
            .map(|k| numeric_id(k) as u64)
            .filter(|&n| n != u32::MAX as u64)
            .max()
            .unwrap_or(0);

        Ok(Arc::new_cyclic(|weak| Self {
            weak_self: weak.clone(),
            tasks: Mutex::new(tasks),
            agents: Mutex::new(Vec::new()),
            policy: Mutex::new(Policy::default()),
            run: Arc::new(Run::default()),
            cancel_filters: Mutex::new(Vec::new()),
            terminal_transitions_in_flight: AtomicUsize::new(0),
            stats,
            event_handlers: Mutex::new(Vec::new()),
            awaited_events: AwaitedEvents::default(),
            event_stream: broadcast::Sender::new(EVENT_STREAM_CAPACITY),
            dir: Mutex::new(werk_dir),
            events_lock: Mutex::new(()),
            join_handle: Mutex::new(None),
            next_task_id: Mutex::new(Some(next_id)),
        }))
    }

    /// Get the input tokens across the run's finished requests.
    ///
    /// Counted as the requests finish, so this reports what the run has spent
    /// even when the log it wrote is gone.
    pub fn get_input_tokens(&self) -> u64 {
        self.stats.input_tokens()
    }

    /// Get the output tokens across the run's finished requests.
    pub fn get_output_tokens(&self) -> u64 {
        self.stats.output_tokens()
    }

    /// Get the elapsed duration, which keeps growing while agents work and
    /// stops when execution ends. `None` until the first task starts.
    pub fn get_duration(&self) -> Option<Duration> {
        self.stats.execution_duration()
    }

    /// Push an event observer onto the handler chain. Every installed
    /// handler fires on every event, in installation order. Handlers
    /// must be cheap and non-blocking. When no handler has been
    /// installed, [`default_logger`] runs in its place.
    ///
    /// The Werk arrives with the event, so a handler selects tasks and
    /// results and files follow-up work without holding one of its own.
    ///
    /// ```no_run
    /// # use agentwerk::{Event, Task, Werk};
    /// let werk = Werk::new();
    /// werk.on_event(|werk, event| {
    ///     if event.get_name() == Event::TASK_FAILED {
    ///         werk.add_task(Task::labeled("triage", "Look into the failure."));
    ///     }
    /// });
    /// ```
    pub fn on_event(&self, handler: impl Fn(&Arc<Werk>, &Event) + Send + Sync + 'static) -> &Self {
        self.event_handlers.lock().unwrap().push(Arc::new(handler));
        self
    }

    /// Read every event as it is emitted, in a handler [`Self::finish_tasks`] waits
    /// for before it returns.
    ///
    /// [`Self::on_event`] cannot await: it runs on the agent task that emitted
    /// the event, and that task has to carry on. This one hands the work to
    /// whichever `finish` is waiting, which awaits each handler as events
    /// arrive. In Python that puts the handler on the caller's event loop, so
    /// work that has to stay serialized against the caller's own, such as a
    /// commit, can be.
    ///
    /// Every event reaches it, `TextChunkReceived` included, and each event
    /// waits in memory until a `finish` drains it. A host that streams a long
    /// reply and only calls [`Self::start`] uses `on_event`.
    ///
    /// Your handler MUST NOT call `finish` or [`Self::finish_all_tasks`], or it waits
    /// forever on the handover it is running inside.
    ///
    /// ```no_run
    /// # use agentwerk::Werk;
    /// # async fn run() {
    /// let werk = Werk::new();
    /// werk.on_event_async(|_, event| async move {
    ///     println!("{}", event.get_name());
    /// });
    /// werk.finish_all_tasks().await;
    /// # }
    /// ```
    pub fn on_event_async<F, Fut>(&self, handler: F) -> &Self
    where
        F: Fn(Arc<Werk>, Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_awaited(
            |_| true,
            move |werk, event, _| Box::pin(handler(werk, event)),
        )
    }

    /// The filter-resolve-call shape the task handlers share. The Werk is
    /// handed in so one that files follow-up work needs no upgrade of its own.
    fn on_task_event<F>(&self, matches: fn(&Event) -> bool, handler: F) -> &Self
    where
        F: Fn(&Arc<Self>, &Event, &Task) + Send + Sync + 'static,
    {
        self.on_event(move |werk, event| {
            if !matches(event) {
                return;
            }
            let Some(task) = werk.get_task(&event.task_id) else {
                return;
            };
            handler(werk, event, &task);
        })
    }

    /// Read every finished task together with its result.
    ///
    /// The value handed over is the stored, schema-validated result, so a
    /// handler never reaches into the finish tool's input shape. This is one
    /// more entry on the [`Self::on_event`] chain.
    ///
    /// ```no_run
    /// # use agentwerk::{Task, Werk};
    /// let werk = Werk::new();
    /// werk.on_result(|werk, done, result| {
    ///     if result["needs_review"] == true {
    ///         werk.add_task(Task::labeled("review", done.get_task().clone()).parent(done.get_id()));
    ///     }
    /// });
    /// ```
    pub fn on_result<F>(&self, handler: F) -> &Self
    where
        F: Fn(&Arc<Werk>, &Task, &serde_json::Value) + Send + Sync + 'static,
    {
        self.on_task_event(
            |event| event.name == Event::TASK_FINISHED,
            move |werk, _, finished| {
                let Some(result) = &finished.result else {
                    return;
                };
                handler(werk, finished, result);
            },
        )
    }

    /// Read every finished task together with its result, in a handler
    /// [`Self::finish_tasks`] waits for before it returns.
    ///
    /// [`Self::on_result`] cannot await: it runs on the agent task that just
    /// finished the task, and that task has to carry on. This one hands the
    /// work to whichever `finish` is waiting, on the terms
    /// [`Self::on_event_async`] sets, and each result waiting to be handed over
    /// holds a copy of its task and every reply in it.
    ///
    /// Your handler MUST NOT call `finish` or [`Self::finish_all_tasks`], or it waits
    /// forever on the handover it is running inside.
    ///
    /// ```no_run
    /// # use agentwerk::Werk;
    /// # async fn run() {
    /// let werk = Werk::new();
    /// werk.on_result_async(|_, task, result| async move {
    ///     println!("{} produced {result}", task.get_id());
    /// });
    /// werk.finish_all_tasks().await;
    /// # }
    /// ```
    pub fn on_result_async<F, Fut>(&self, handler: F) -> &Self
    where
        F: Fn(Arc<Werk>, Task, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_awaited(
            |event| event.name == Event::TASK_FINISHED,
            move |werk, _, finished| match finished.and_then(|t| t.result.clone().map(|r| (t, r))) {
                Some((task, result)) => Box::pin(handler(werk, task, result)),
                None => Box::pin(std::future::ready(())),
            },
        )
    }

    /// Read every failure together with the task it happened in:
    /// `task_failed`, `request_failed`, `tool_call_failed`, `file_open_failed`,
    /// `knowledge_failed`, and `compaction_failed`.
    ///
    /// Read `event.get_name()` to tell a failure that ends the task from one
    /// the agent works around. Each call copies the task's replies, so an
    /// agent that fails many tool calls pays that copy once per failure.
    ///
    /// ```no_run
    /// # use agentwerk::{Event, Task, Werk};
    /// let werk = Werk::new();
    /// werk.on_failure(|werk, event, failed| {
    ///     // Count the attempts yourself, or a task that fails every time
    ///     // re-queues itself forever.
    ///     if event.get_name() == Event::TASK_FAILED && failed.get_parent().is_none() {
    ///         werk.add_task(Task::new(failed.get_task().clone()).parent(failed.get_id()));
    ///     }
    /// });
    /// ```
    pub fn on_failure<F>(&self, handler: F) -> &Self
    where
        F: Fn(&Arc<Werk>, &Event, &Task) + Send + Sync + 'static,
    {
        self.on_task_event(is_failure, handler)
    }

    /// Read every failure together with the task it happened in, in a handler
    /// [`Self::finish_tasks`] waits for before it returns.
    ///
    /// [`Self::on_failure`] on the terms [`Self::on_event_async`] sets.
    ///
    /// Your handler MUST NOT call `finish` or [`Self::finish_all_tasks`], or it waits
    /// forever on the handover it is running inside.
    pub fn on_failure_async<F, Fut>(&self, handler: F) -> &Self
    where
        F: Fn(Arc<Werk>, Event, Task) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_awaited(is_failure, move |werk, event, task| match task {
            Some(task) => Box::pin(handler(werk, event, task)),
            None => Box::pin(std::future::ready(())),
        })
    }

    /// Read a task as it starts, finishes, or fails.
    ///
    /// The handler receives the event plus the task it names, already
    /// resolved, so it reads the result, label, and replies without a second
    /// lookup. No other kind reaches the handler: resolving a task copies its
    /// replies, which on `TextChunkReceived` would cost once per piece of the
    /// reply.
    pub fn on_task<F>(&self, handler: F) -> &Self
    where
        F: Fn(&Arc<Werk>, &Event, &Task) + Send + Sync + 'static,
    {
        self.on_task_event(is_task_event, handler)
    }

    /// Read a task as it starts, finishes, or fails, in a handler
    /// [`Self::finish_tasks`] waits for before it returns.
    ///
    /// [`Self::on_task`] on the terms [`Self::on_event_async`] sets.
    ///
    /// Your handler MUST NOT call `finish` or [`Self::finish_all_tasks`], or it waits
    /// forever on the handover it is running inside.
    pub fn on_task_async<F, Fut>(&self, handler: F) -> &Self
    where
        F: Fn(Arc<Werk>, Event, Task) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_awaited(is_task_event, move |werk, event, task| match task {
            Some(task) => Box::pin(handler(werk, event, task)),
            None => Box::pin(std::future::ready(())),
        })
    }

    /// Register an awaited handler and make sure the events it accepts are being
    /// queued for it.
    fn on_awaited<F>(&self, matches: fn(&Event) -> bool, call: F) -> &Self
    where
        F: Fn(Arc<Werk>, Event, Option<Task>) -> HandlerWork + Send + Sync + 'static,
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
            self.on_event(|werk, event| {
                let anyone_wants_it = werk
                    .awaited_events
                    .handlers
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|handler| (handler.matches)(event));
                if !anyone_wants_it {
                    return;
                }
                // Resolved now rather than at handover, so a handler sees the
                // task as it was when the event arrived. Only for the kinds a
                // task-shaped hook accepts: resolving copies every reply,
                // which on `TextChunkReceived` would cost once per piece.
                let task = match is_task_event(event) || is_failure(event) {
                    true => werk.get_task(&event.task_id),
                    false => None,
                };
                werk.awaited_events
                    .queued
                    .lock()
                    .unwrap()
                    .push_back((event.clone(), task));
            });
        });
    }

    /// Loops because a handler that takes a while lets more events queue up
    /// behind it. One is taken per lock, so a `finish` dropped mid-handover, by
    /// a timeout or a panic, loses only the event it was on.
    async fn await_handlers(&self) {
        let handlers: Vec<AwaitedHandlerRef> = self
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
        let Some(werk) = self.weak_self.upgrade() else {
            return;
        };
        // Waits rather than skips, or a second `finish` could return while its
        // own events were still being handed over.
        let _draining = self.awaited_events.draining.lock().await;
        loop {
            let next = self.awaited_events.queued.lock().unwrap().pop_front();
            let Some((event, task)) = next else {
                return;
            };
            for (matches, call) in &handlers {
                if matches(&event) {
                    call(Arc::clone(&werk), event.clone(), task.clone()).await;
                }
            }
        }
    }

    /// Publish an event and hand back what every observer saw.
    pub fn emit_event(&self, mut event: Event) -> Event {
        event.created_at = super::now_millis();
        event.label = self.label_for(&event.task_id);
        self.stats.record(&event);
        // Published before the handlers run: a `finish` waiter competes
        // with them for nothing, and no handler can swallow the event. The
        // receiver count is checked first so a run with no waiter never pays the
        // clone, which `TextChunkReceived` would otherwise charge per token.
        if self.event_stream.receiver_count() > 0 {
            let _ = self.event_stream.send(event.clone());
        }
        // Text chunks are the exception: one per streamed token would
        // outweigh every other line and repeats what `replies.jsonl` holds.
        if event.name != Event::TEXT_CHUNK_RECEIVED {
            let _guard = self.events_lock.lock().unwrap();
            let _ = Stats::append(&self.get_dir(), &event);
        }
        // Pushed before the handlers run, so one receiving the task sees
        // it. Nothing is written: the line is already in `events.jsonl`, and
        // `load` reads it back from there.
        if is_recorded_failure(&event) {
            let mut store = self.tasks.lock().unwrap();
            if let Some(task) = store.get_mut(&event.task_id) {
                task.errors.push(event.clone());
            }
        }
        let handlers: Vec<Arc<EventHandler>> = self.event_handlers.lock().unwrap().clone();
        if handlers.is_empty() {
            default_logger()(&event);
            return event;
        }
        // Handed to every handler, so one that files follow-up work needs no
        // reference of its own. Gone only while the Werk is being dropped,
        // when there is nothing left for a handler to act on.
        let Some(werk) = self.weak_self.upgrade() else {
            return event;
        };
        for h in &handlers {
            h(&werk, &event);
        }
        event
    }

    fn label_for(&self, id: &str) -> Option<String> {
        self.tasks
            .lock()
            .unwrap()
            .get(id)
            .and_then(|t| t.label.clone())
    }

    /// Get the model that agent runs, or `None` when no agent of that name is bound.
    ///
    /// Pairs with [`Self::on_task`]: the event names the agent, this names
    /// its model, and [`Trajectory::from_task`] needs both.
    ///
    /// [`Trajectory::from_task`]: super::Trajectory::from_task
    pub fn get_model_for_agent(&self, agent_id: &str) -> Option<String> {
        self.agents
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.get_id() == agent_id)
            .map(|a| a.get_model().name.clone())
    }

    /// Set execution limits and retry settings.
    ///
    /// The whole `Policy` is replaced, so build one from the fields you want:
    /// `Policy { max_turns: Some(40), ..Default::default() }`. A
    /// `compaction_threshold` outside `0.0..=1.0` is clamped into it.
    pub fn set_policy(&self, mut policy: Policy) -> &Self {
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

    /// Get the execution limits and retry settings in force.
    pub fn get_policy(&self) -> Policy {
        self.policy.lock().unwrap().clone()
    }

    /// Define where a session is stored, `./.agentwerk` by default.
    ///
    /// Loading knowledge from `<dir>/knowledge` keeps its pages beside the
    /// session.
    pub fn set_dir(&self, dir: impl Into<PathBuf>) -> &Self {
        *self.dir.lock().unwrap() = dir.into();
        self
    }

    /// Get the session directory.
    pub fn get_dir(&self) -> PathBuf {
        self.dir.lock().unwrap().clone()
    }

    /// Where task `id`'s result is stored. Named for agents, which read a
    /// result through this path or hand it to the next task.
    pub(crate) fn result_path(&self, id: &str) -> PathBuf {
        super::task::result_path(&self.get_dir(), id)
    }

    /// Submit a task and return its task ID.
    ///
    /// A string is the task itself, and a `&Path` or `PathBuf` names the file
    /// holding it. A [`Task`] carries a custom label or
    /// schema with it. ID, reporter, creation time, status, and result are set
    /// at insertion and overwrite whatever the task carried. A label decides
    /// which agents may claim it, so give an agent a label of its own to
    /// address it alone.
    pub fn add_task(&self, task: impl Into<Task>) -> String {
        self.dispatch(task.into())
    }

    /// Add a reply to a task.
    ///
    /// An agent that has just spoken waits on the task, and this reply is
    /// what sends the next turn. Use it to continue a conversation on one
    /// task instead of creating a new task per turn.
    pub fn add_reply(&self, id: &str, content: impl Into<String>) -> &Self {
        self.append_reply(id, Reply::user_text(content));
        self
    }

    fn dispatch(&self, task: Task) -> String {
        self.insert(task, "user".to_string())
    }

    /// Get one task by ID.
    pub fn get_task(&self, id: &str) -> Option<Task> {
        self.tasks.lock().unwrap().get(id).cloned()
    }

    /// Get every task in creation order.
    pub fn get_tasks(&self) -> Vec<Task> {
        self.matching_tasks(&Query::all().task())
    }

    /// Get every task selected by a task query or referenced by an event query.
    /// The query's source order determines the returned order.
    ///
    /// Your condition MUST NOT call another `Werk` method that reads
    /// the task store, or the call deadlocks.
    pub fn find_tasks(&self, predicate: impl Matcher<Task>) -> Vec<Task> {
        let query = predicate.into_query().task_if_originless();
        self.tasks_selected_by(&query)
    }

    /// Get the first task selected by a task query or referenced by an event
    /// query.
    ///
    /// Your condition MUST NOT call another `Werk` method that reads
    /// the task store, or the call deadlocks.
    pub fn find_task(&self, predicate: impl Matcher<Task>) -> Option<Task> {
        let query = predicate.into_query().task_if_originless();
        match query.is_event() {
            true => self.tasks_referenced_by(&query).into_iter().next(),
            false => self.first_matching_task(&query),
        }
    }

    fn tasks_selected_by(&self, query: &Query) -> Vec<Task> {
        match query.is_event() {
            true => self.tasks_referenced_by(query),
            false => self.matching_tasks(query),
        }
    }

    fn matching_tasks(&self, query: &Query) -> Vec<Task> {
        let store = self.tasks.lock().unwrap();
        let mut matching: Vec<&Task> = store
            .values()
            .filter(|task| query.matches_task(task))
            .collect();
        query.sort_tasks(&mut matching);
        matching.into_iter().cloned().collect()
    }

    /// The first of those, and the only task copied.
    fn first_matching_task(&self, query: &Query) -> Option<Task> {
        let store = self.tasks.lock().unwrap();
        let mut matching: Vec<&Task> = store
            .values()
            .filter(|task| query.matches_task(task))
            .collect();
        query.sort_tasks(&mut matching);
        matching.into_iter().next().cloned()
    }

    fn tasks_referenced_by(&self, query: &Query) -> Vec<Task> {
        let events = self.matching_events(query);
        let store = self.tasks.lock().unwrap();
        let mut seen = HashSet::new();
        events
            .into_iter()
            .filter_map(|event| {
                let task = store.get(&event.task_id)?;
                seen.insert(event.task_id).then(|| task.clone())
            })
            .collect()
    }

    /// Get every event selected by an event query or attached to tasks selected
    /// by a task query, in source-query order.
    ///
    /// The condition is an AQL string, a [`Query`](crate::Query), or a
    /// closure, the way [`Self::find_tasks`] takes any of the three.
    ///
    /// ```no_run
    /// # use agentwerk::Werk;
    /// let werk = Werk::new();
    /// werk.find_events("event.name = tool_call_failed AND event.created > -1h");
    /// ```
    ///
    /// Read from the session's `events.jsonl`, so this answers for a run that
    /// has finished as readily as one still working. Counting is `.len()`, and
    /// a total is a fold over the events themselves. A log that cannot be read
    /// finds nothing, and `TextChunkReceived` is never recorded, so a condition
    /// naming it never matches.
    pub fn find_events(&self, matcher: impl Matcher<Event>) -> Vec<Event> {
        let query = matcher.into_query().event_if_originless();
        match query.is_event() {
            true => self.matching_events(&query),
            false => self.events_referencing_tasks(&query),
        }
    }

    /// Get the first event selected by an event query or attached to a task
    /// selected by a task query.
    pub fn find_event(&self, matcher: impl Matcher<Event>) -> Option<Event> {
        let query = matcher.into_query().event_if_originless();
        if !query.is_event() {
            return self.events_referencing_tasks(&query).into_iter().next();
        }
        // Without an order the log's own is the answer, so one match ends the
        // read instead of the whole log being copied to be sorted.
        let wanted = match query.is_ordered() {
            true => usize::MAX,
            false => 1,
        };
        let mut found = self.collect_events(&query, wanted);
        query.sort_events(&mut found);
        found.into_iter().next()
    }

    fn matching_events(&self, query: &Query) -> Vec<Event> {
        let mut events = self.collect_events(query, usize::MAX);
        query.sort_events(&mut events);
        events
    }

    fn events_referencing_tasks(&self, query: &Query) -> Vec<Event> {
        let tasks = self.matching_tasks(query);
        let positions: HashMap<&str, usize> = tasks
            .iter()
            .enumerate()
            .map(|(position, task)| (task.id.as_str(), position))
            .collect();
        let mut grouped = vec![Vec::new(); tasks.len()];
        let _ = Stats::for_each_event(&self.get_dir(), |event| {
            if let Some(position) = positions.get(event.task_id.as_str()) {
                grouped[*position].push(event.clone());
            }
        });
        grouped.into_iter().flatten().collect()
    }

    /// In log order: the caller sorts if its query named one.
    fn collect_events(&self, query: &Query, wanted: usize) -> Vec<Event> {
        let mut out = Vec::new();
        let _ = Stats::for_each_event(&self.get_dir(), |event| {
            if out.len() < wanted && query.matches_event(event) {
                out.push(event.clone());
            }
        });
        out
    }

    /// Take every matching task off the Werk.
    ///
    /// A match is neither claimed nor resumed, and an agent already holding one
    /// is taken off it; the task stays `in_progress`. Nothing waits: this is
    /// not async, so it can be called from a ctrl-c handler, a drop guard, or
    /// anywhere else. Use [`Self::cancel_all_tasks`] to stop the whole run.
    ///
    /// Your filter MUST NOT call another `Werk` method that reads the
    /// task store, or the claim path deadlocks.
    ///
    /// ```no_run
    /// # use agentwerk::Werk;
    /// let werk = Werk::new();
    /// werk.cancel_tasks("task.label = scan");
    /// ```
    pub fn cancel_tasks(&self, matches: impl Matcher<Task>) -> &Self {
        let query = matches.into_query().task();
        self.cancel_filters.lock().unwrap().push(query.clone());
        for task in self.tasks.lock().unwrap().values_mut() {
            if query.matches_task(task) {
                task.cancelled = true;
            }
        }
        self
    }

    /// Take every task off the Werk, which ends the run.
    ///
    /// [`Self::finish_all_tasks`] then reports `FinishReason::Cancelled`. Like
    /// [`Self::cancel_tasks`], nothing waits, so a ctrl-c handler can call it.
    pub fn cancel_all_tasks(&self) -> &Self {
        self.cancel_tasks(Query::all().task())
    }

    /// True while any matching task still has work for an agent.
    ///
    /// The one definition of "not done yet": the main loop asks it of every
    /// task to decide the run is over, and [`Self::finish_tasks`] asks it of a
    /// subset. A task is pending while it is todo or in progress,
    /// uncancelled, and not paused for a caller reply.
    pub(crate) fn pending(&self, matches: &Query) -> bool {
        // A terminal transition mid-flight may still add a follow-up task
        // from a handler, so it counts as work whatever the store says.
        if self.terminal_transitions_in_flight.load(Ordering::SeqCst) > 0 {
            return true;
        }
        let interactive = self.interactive_agents();
        let tasks = self.tasks.lock().unwrap();
        tasks.values().any(|t| {
            matches.matches_task(t)
                && t.is_pending()
                && !(t.is_paused()
                    && t.assignee
                        .as_deref()
                        .is_some_and(|a| interactive.contains(a)))
        })
    }

    /// Why the run is over, or `None` while it should keep going.
    ///
    /// An empty Werk is not an ending: a host that called [`Self::start`] may
    /// still be filing work, and a paused task revives on the next reply.
    /// Only a breached limit or a cancel that leaves nothing claimable ends a
    /// run here; the drained ending is named by the [`Self::finish_tasks`] that waited
    /// for it.
    pub(crate) fn ending_reason(&self) -> Option<FinishReason> {
        if let Some((violation, _)) = policy_violated(&self.get_policy(), &self.stats) {
            return Some(FinishReason::PolicyViolated(violation));
        }
        if self.anything_claimable() {
            return None;
        }
        // A cancel is a statement that work should stop, so it ends a run with
        // nothing claimable left even when the Werk was already empty.
        let cancelled = !self.cancel_filters.lock().unwrap().is_empty();
        cancelled.then_some(FinishReason::Cancelled)
    }

    /// The one definition of a task an agent could still take, which both the
    /// ending check and [`Self::anything_pending`] ask for.
    fn anything_claimable(&self) -> bool {
        let tasks = self.tasks.lock().unwrap();
        tasks.values().any(Task::is_pending)
    }

    /// True while any task is still open. Stricter than [`Self::pending`]:
    /// an interactive agent's paused task has no work for it right now, but a
    /// reply revives it, so the run is not over.
    fn anything_pending(&self) -> bool {
        // A terminal transition mid-flight may still add a follow-up task
        // from a handler, so it counts as work whatever the store says.
        self.terminal_transitions_in_flight.load(Ordering::SeqCst) > 0 || self.anything_claimable()
    }

    /// True while the main loop is up. `start()` is a no-op then, so a second
    /// caller never starts a run alongside the first.
    fn is_running(&self) -> bool {
        self.join_handle.lock().unwrap().is_some() && !self.run.is_finished()
    }

    /// The names of the added agents that wait for a caller reply. Read before
    /// the task store is locked: `bind_agent` takes the two in that order.
    fn interactive_agents(&self) -> HashSet<String> {
        self.agents
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.is_interactive())
            .map(|a| a.get_id().to_string())
            .collect()
    }

    /// Attach `agent` to this Werk, moving any tasks it queued in its own
    /// private Werk across first. The prior Werk is freed once nothing else
    /// holds it.
    pub(crate) fn bind_agent(&self, agent: &mut Agent) {
        agent.require_provider_and_model();
        agent.register_finish_tool();
        if let Some(prior) = agent.werk.upgrade() {
            if !Arc::ptr_eq(
                &prior,
                &self
                    .weak_self
                    .upgrade()
                    .expect("self Arc dropped during bind"),
            ) {
                let drained: Vec<Task> = {
                    let mut old = prior.tasks.lock().unwrap();
                    std::mem::take(&mut *old).into_values().collect()
                };
                let reporter = agent.get_id().to_string();
                for task in drained {
                    self.insert(task, reporter.clone());
                }
            }
        }
        agent.werk.bind(self.weak_self.clone());
        self.agents.lock().unwrap().push(agent.clone());
    }

    /// Whether an agent under this ID is registered. `Agent::start` asks
    /// before binding, so starting twice runs one agent, not two.
    pub(crate) fn has_agent(&self, id: &str) -> bool {
        self.agents.lock().unwrap().iter().any(|a| a.get_id() == id)
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

    /// Add an agent to this Werk.
    ///
    /// Any tasks the agent queued on its own move into this Werk. An agent
    /// added while execution is under way picks up its first task within
    /// about 100 ms.
    pub fn add_agent(&self, mut agent: Agent) -> &Self {
        self.bind_agent(&mut agent);
        self
    }

    /// Begin processing tasks, on a background task.
    ///
    /// A task queued afterwards is picked up within about 100 ms, and an
    /// empty Werk keeps the run alive: only [`Self::finish_tasks`] and
    /// [`Self::cancel_tasks`] end one. Calling this while a run is under way does
    /// nothing; calling it after one ended starts a fresh run, which is how a
    /// host resumes after a cancel.
    pub fn start(&self) -> &Self {
        if self.is_running() {
            return self;
        }
        self.run.reset();
        self.cancel_filters.lock().unwrap().clear();
        for task in self.tasks.lock().unwrap().values_mut() {
            task.cancelled = false;
        }
        let supervisor = self.weak_self.upgrade().expect("Werk dropped during start");
        self.emit_event(Event::new(Event::RUN_STARTED));
        let join = tokio::spawn(async move { run_main_loop(&supervisor).await });
        *self.join_handle.lock().unwrap() = Some(join);
        self
    }

    /// Wait for the matching tasks to be done, then get their results in
    /// query order, or creation order when the query names none.
    ///
    /// Name a label to wait for one pool, or an ID to wait for one task;
    /// [`Self::finish_all_tasks`] waits for the whole run. The wait ends once no
    /// matching task has work left for an agent, which covers one that
    /// finished, failed, was cancelled, or is paused awaiting your reply.
    ///
    /// A task contributes a result only when it finished with one, so this is
    /// shorter than the set the filter named rather than aligned with it, as
    /// with [`Self::get_results`]. Read why the wait ended with
    /// [`Self::get_finish_reason`].
    ///
    /// Execution begins here when the Werk has never run, and otherwise this
    /// waits on what is already under way. Once a run has ended it returns at
    /// once: only [`Self::start`] starts another. Your filter MUST NOT call
    /// another `Werk` method that reads the task store, or the call
    /// deadlocks.
    ///
    /// ```no_run
    /// # use agentwerk::Werk;
    /// # async fn run() {
    /// let werk = Werk::new();
    /// for finding in werk.finish_tasks("task.label = research").await {
    ///     println!("{finding}");
    /// }
    /// # }
    /// ```
    pub async fn finish_tasks(&self, matches: impl Matcher<Task>) -> Vec<serde_json::Value> {
        let query = matches.into_query().task();
        if self.join_handle.lock().unwrap().is_none()
            && (!self.run.is_finished() || self.anything_claimable())
        {
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
        // Check again after the wait for events emitted during the last turn.
        self.await_handlers().await;
        // Nothing this filter named is left. When no task at all is open, the
        // run is over too, so start it finishing and let it announce the reason.
        if !self.anything_pending() {
            self.run
                .set_draining(self.ending_reason().unwrap_or(FinishReason::Drained));
            self.run.until_finished().await;
        }
        // Releasing the handle lets a later finish start a fresh execution after the current tasks are done.
        if self.run.is_finished() {
            self.join_handle.lock().unwrap().take();
        }
        // `and_status`, not the `find_results` default: a caller who waited on
        // `task.status = todo` receives finished results, not the matching unfinished tasks.
        self.matching_tasks(&query.and_task_status(Status::Finished))
            .into_iter()
            .filter_map(|t| t.result)
            .collect()
    }

    /// Wait for every task to be done, then get every result in creation
    /// order.
    ///
    /// Wait until no task has work left for an agent. [`Self::finish_tasks`] applies the same behavior to one pool or task.
    ///
    /// ```no_run
    /// # use agentwerk::Werk;
    /// # async fn run() {
    /// let werk = Werk::new();
    /// for finding in werk.finish_all_tasks().await {
    ///     println!("{finding}");
    /// }
    /// # }
    /// ```
    pub async fn finish_all_tasks(&self) -> Vec<serde_json::Value> {
        self.finish_tasks(Query::all().task()).await
    }

    /// Wait for the matching tasks to be done, then get the first available
    /// result in query order.
    ///
    /// The one-result form of [`Self::finish_tasks`]. `None` means no
    /// matching task finished with a result.
    ///
    /// ```no_run
    /// # use agentwerk::Werk;
    /// # async fn run() {
    /// let werk = Werk::new();
    /// if let Some(answer) = werk.finish_task("ORDER BY task.created DESC").await {
    ///     println!("{answer}");
    /// }
    /// # }
    /// ```
    pub async fn finish_task(&self, matches: impl Matcher<Task>) -> Option<serde_json::Value> {
        self.finish_tasks(matches).await.into_iter().next()
    }

    /// Get why the last run ended, or `None` while one is still going.
    ///
    /// Cleared by [`Self::start`], so a re-started Werk does not report the
    /// previous run. A [`Self::finish_tasks`] over a subset can return while the run
    /// carries on, and this reads `None` until it ends.
    pub fn get_finish_reason(&self) -> Option<FinishReason> {
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

    /// Get the result of every finished task, in creation order.
    ///
    /// Read a structured result back with `serde_json::from_value`. A task
    /// still running, or finished without a result, contributes nothing, so
    /// this is shorter than [`Self::get_tasks`] rather than aligned with it.
    pub fn get_results(&self) -> Vec<serde_json::Value> {
        self.find_results(Query::all().task())
    }

    /// Get every result whose task is selected directly or through a matching
    /// event, in source-query order.
    ///
    /// A task query defaults status to `finished` when it names none. Event
    /// queries always project only finished tasks. A task contributes a result
    /// only when it has one, so this is shorter than the set the filter named.
    ///
    /// ```no_run
    /// # use agentwerk::Werk;
    /// let werk = Werk::new();
    /// let scans = werk.find_results("task.label = scan");
    /// ```
    pub fn find_results(&self, matches: impl Matcher<Task>) -> Vec<serde_json::Value> {
        self.result_tasks(matches)
            .into_iter()
            .filter_map(|task| task.result)
            .collect()
    }

    /// Get the first result selected through a matching task or event.
    ///
    /// Status defaults to `finished` as in [`Self::find_results`], and the
    /// order is that method's too.
    pub fn find_result(&self, matches: impl Matcher<Task>) -> Option<serde_json::Value> {
        self.result_tasks(matches)
            .into_iter()
            .next()
            .and_then(|task| task.result)
    }

    fn result_tasks(&self, matches: impl Matcher<Task>) -> Vec<Task> {
        let query = matches.into_query().task_if_originless();
        if query.is_event() {
            return self
                .tasks_referenced_by(&query)
                .into_iter()
                .filter(|task| task.status == Status::Finished && task.result.is_some())
                .collect();
        }
        self.matching_tasks(
            &query
                .default_task_status(Status::Finished)
                .and_task_result(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    #[test]
    fn policy_finish_reason_keeps_its_public_spelling() {
        let reason = FinishReason::PolicyViolated(crate::PolicyViolation::Turns);
        assert_eq!(reason.to_string(), "policy_violated(turns)");
    }

    fn emit_event(werk: &Werk, id: &str, agent: &str, event: Event) -> Event {
        werk.emit_event(event.task_id(id).agent_id(agent))
    }

    fn tool_call_failed(message: &str) -> Event {
        Event::new(Event::TOOL_CALL_FAILED).data(serde_json::json!({
            "tool_name": "grep",
            "call_id": "c1",
            "kind": "execution_failed",
            "message": message,
        }))
    }

    #[test]
    fn werk_handle_is_shared_between_caller_and_added_agent() {
        let (werk, _tmp) = test_werk();
        let alice = werk.add_agent(minimal_agent("alice"));
        alice.add_task("from alice");
        werk.add_task("from Werk");
        let all_ids: Vec<String> = werk
            .find_tasks(|t: &Task| t.status == Status::Todo)
            .iter()
            .map(|t| t.id.clone())
            .collect();
        assert_eq!(all_ids.len(), 2);
    }

    #[test]
    fn repeated_task_calls_route_to_shared_werk_after_rebind() {
        let (werk, _tmp) = test_werk();
        let mut alice = minimal_agent("alice");
        werk.bind_agent(&mut alice);
        alice.add_task("first");
        alice.add_task("second");
        assert_eq!(
            werk.find_tasks(|t: &Task| t.status == Status::Todo).len(),
            2
        );
    }

    #[test]
    fn tasks_returns_all_in_creation_order() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        werk.add_task("c");
        let all = werk.get_tasks();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, "t-1");
        assert_eq!(all[1].id, "t-2");
        assert_eq!(all[2].id, "t-3");
    }

    #[test]
    fn find_tasks_answers_in_creation_order() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        werk.add_task("c");
        let ids: Vec<String> = werk
            .find_tasks("task.status = todo")
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, ["t-1", "t-2", "t-3"]);
    }

    #[test]
    fn cancel_ignores_an_order_by_and_takes_every_task() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        werk.cancel_tasks("ORDER BY task.id DESC");
        assert_eq!(werk.find_tasks("task.cancelled = true").len(), 2);
    }

    #[test]
    fn find_tasks_answers_in_the_order_the_query_names() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        werk.add_task("c");
        let ids: Vec<String> = werk
            .find_tasks("ORDER BY task.id DESC")
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, ["t-3", "t-2", "t-1"]);
    }

    #[test]
    fn find_task_answers_the_first_in_the_order_the_query_names() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        let found = werk.find_task("ORDER BY task.id DESC").expect("a task");
        assert_eq!(found.id, "t-2");
    }

    #[test]
    fn find_results_answers_in_the_order_the_query_names() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        attach_done_result(&werk, "t-1", "first");
        attach_done_result(&werk, "t-2", "second");
        assert_eq!(
            werk.find_results("ORDER BY task.id DESC"),
            vec![serde_json::json!("second"), serde_json::json!("first")]
        );
    }

    #[test]
    fn one_task_query_can_select_tasks_and_their_results() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::labeled("scan", "a"));
        werk.add_task(Task::labeled("scan", "b"));
        werk.add_task(Task::labeled("report", "c"));
        attach_done_result(&werk, "t-1", "clean one");
        attach_done_result(&werk, "t-2", "clean two");
        attach_done_result(&werk, "t-3", "reported");
        let query =
            Query::new("task.label = scan AND task.result ~ clean ORDER BY task.id DESC").unwrap();

        let ids = werk
            .find_tasks(query.clone())
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, ["t-2", "t-1"]);
        assert_eq!(
            werk.find_results(query),
            [
                serde_json::json!("clean two"),
                serde_json::json!("clean one")
            ]
        );
    }

    #[test]
    fn compiled_queries_work_with_singular_and_plural_finders() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::labeled("scan", "a"));
        attach_done_result(&werk, "t-1", "clean");

        let tasks = Query::new("task.label = scan").unwrap();
        assert_eq!(werk.find_task(tasks.clone()).unwrap().id, "t-1");
        assert_eq!(werk.find_tasks(tasks.clone()).len(), 1);
        assert_eq!(
            werk.find_result(tasks.clone()),
            Some(serde_json::json!("clean"))
        );
        assert_eq!(werk.find_results(tasks), [serde_json::json!("clean")]);
        assert_eq!(
            werk.find_result(|task: &Task| task.label.as_deref() == Some("scan")),
            Some(serde_json::json!("clean"))
        );

        let events = Query::new("event.name = task_created").unwrap();
        assert_eq!(werk.find_event(events.clone()).unwrap().task_id, "t-1");
        assert_eq!(werk.find_events(events).len(), 1);
    }

    #[test]
    fn find_result_accepts_a_bare_task_id_and_skips_tasks_without_results() {
        let (werk, _tmp) = test_werk();
        werk.add_task("no result");
        werk.add_task("has result");
        werk.set_finished_by("t-1", "agent").unwrap();
        attach_done_result(&werk, "t-2", "answer");

        assert_eq!(werk.find_result("t-1"), None);
        assert_eq!(werk.find_result("t-2"), Some(serde_json::json!("answer")));
        assert_eq!(
            werk.find_result("task.status = finished"),
            Some(serde_json::json!("answer"))
        );
    }

    #[test]
    fn results_return_done_payloads_in_creation_order() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        werk.add_task("c");
        attach_done_result(&werk, "t-1", "first");
        attach_done_result(&werk, "t-3", "third");
        assert_eq!(
            werk.get_results(),
            vec![serde_json::json!("first"), serde_json::json!("third")]
        );
    }

    #[test]
    fn results_order_by_creation_regardless_of_done_order() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        werk.add_task("c");
        attach_done_result(&werk, "t-3", "third");
        attach_done_result(&werk, "t-1", "first");
        attach_done_result(&werk, "t-2", "second");
        assert_eq!(
            werk.get_results(),
            vec![
                serde_json::json!("first"),
                serde_json::json!("second"),
                serde_json::json!("third")
            ]
        );
    }

    #[test]
    fn results_are_empty_when_nothing_finished() {
        let (werk, _tmp) = test_werk();
        werk.add_task("pending");
        assert!(werk.get_results().is_empty());
    }

    #[test]
    fn find_results_selects_by_task_label() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::labeled("scan", "a"));
        werk.add_task(Task::labeled("report", "b"));
        attach_done_result(&werk, "t-1", "scanned");
        attach_done_result(&werk, "t-2", "reported");
        assert_eq!(
            werk.find_results("task.label = scan"),
            vec![serde_json::json!("scanned")]
        );
    }

    #[test]
    fn find_results_defaults_to_finished_when_the_query_names_no_status() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::labeled("scan", "a"));
        werk.add_task(Task::labeled("scan", "b"));
        attach_done_result(&werk, "t-1", "scanned");
        assert_eq!(
            werk.find_results("task.label = scan"),
            vec![serde_json::json!("scanned")]
        );
    }

    #[test]
    fn find_results_keeps_the_status_the_query_names() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::labeled("scan", "a"));
        werk.set_result("t-1", serde_json::json!("mid-flight"))
            .unwrap();
        assert_eq!(
            werk.find_results("task.label = scan AND task.status = todo"),
            vec![serde_json::json!("mid-flight")]
        );
    }

    #[test]
    fn find_results_takes_a_closure_in_place_of_a_query() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::labeled("scan", "a"));
        werk.add_task(Task::labeled("report", "b"));
        attach_done_result(&werk, "t-1", "scanned");
        attach_done_result(&werk, "t-2", "reported");
        assert_eq!(
            werk.find_results(|task: &Task| task.label.as_deref() == Some("scan")),
            vec![serde_json::json!("scanned")]
        );
    }

    #[test]
    fn find_results_defaults_a_closure_to_finished_tasks() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::labeled("scan", "a"));
        // A result attached without the finish transition, which the default
        // leaves out because a closure names no status of its own.
        werk.set_result("t-1", serde_json::json!("mid-flight"))
            .unwrap();
        assert!(werk
            .find_results(|task: &Task| task.label.as_deref() == Some("scan"))
            .is_empty());
    }

    #[test]
    fn event_queries_project_ordered_unique_referenced_tasks() {
        let (werk, tmp) = test_werk();
        werk.add_task("first");
        werk.add_task("second");
        for (task_id, created_at) in [
            ("t-2", 100),
            ("", 150),
            ("t-404", 175),
            ("t-1", 200),
            ("t-2", 300),
        ] {
            let event = Event {
                created_at,
                ..Event::new("selected").task_id(task_id)
            };
            Stats::append(tmp.path(), &event).unwrap();
        }
        let query = Query::new("event.name = selected ORDER BY event.created").unwrap();

        let ids = werk
            .find_tasks(query.clone())
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, ["t-2", "t-1"]);
        assert_eq!(
            werk.find_task("event.name = selected ORDER BY event.created")
                .unwrap()
                .id,
            "t-2"
        );
    }

    #[test]
    fn task_queries_project_events_grouped_in_task_order() {
        let (werk, tmp) = test_werk();
        werk.add_task(Task::labeled("scan", "first"));
        werk.add_task(Task::labeled("scan", "second"));
        werk.add_task(Task::labeled("report", "third"));
        Stats::append(tmp.path(), &Event::new("noted").task_id("t-2")).unwrap();
        Stats::append(tmp.path(), &Event::new("noted").task_id("t-1")).unwrap();
        let query = Query::new("task.label = scan ORDER BY task.id DESC").unwrap();

        let events = werk.find_events(query.clone());
        let identified = events
            .iter()
            .map(|event| (event.task_id.as_str(), event.name.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            identified,
            [
                ("t-2", Event::TASK_CREATED),
                ("t-2", "noted"),
                ("t-1", Event::TASK_CREATED),
                ("t-1", "noted"),
            ]
        );
        assert_eq!(
            werk.find_event("task.label = scan ORDER BY task.id DESC")
                .unwrap()
                .task_id,
            "t-2"
        );
    }

    #[test]
    fn event_queries_project_finished_results_in_event_order() {
        let (werk, tmp) = test_werk();
        for task in ["first", "mid-flight", "empty", "fourth"] {
            werk.add_task(task);
        }
        attach_done_result(&werk, "t-1", "first");
        werk.set_result("t-2", serde_json::json!("mid-flight"))
            .unwrap();
        werk.set_finished_by("t-3", "agent").unwrap();
        attach_done_result(&werk, "t-4", "fourth");
        for (task_id, created_at) in [
            ("t-4", 100),
            ("t-3", 200),
            ("t-2", 300),
            ("t-1", 400),
            ("t-4", 500),
        ] {
            let event = Event {
                created_at,
                ..Event::new("selected").task_id(task_id)
            };
            Stats::append(tmp.path(), &event).unwrap();
        }
        let query = Query::new("event.name = selected ORDER BY event.created").unwrap();

        assert_eq!(
            werk.find_results(query.clone()),
            [serde_json::json!("fourth"), serde_json::json!("first")]
        );
        assert_eq!(
            werk.find_result("event.name = selected ORDER BY event.created"),
            Some(serde_json::json!("fourth"))
        );
    }

    #[test]
    #[should_panic(expected = "requires a task query")]
    fn lifecycle_operations_still_reject_event_queries() {
        let (werk, _tmp) = test_werk();
        werk.cancel_tasks(Query::new("event.name = task_created").unwrap());
    }

    #[test]
    fn find_tasks_compiles_the_string_as_a_query() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::labeled("scan", "a"));
        werk.add_task(Task::labeled("report", "b"));
        let found = werk.find_tasks("task.label = report AND task.status = todo");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].task, serde_json::json!("b"));
    }

    #[test]
    fn pending_on_a_todo_task() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        assert!(werk.pending(&Query::all()));
    }

    #[test]
    fn pending_only_for_the_matching_tasks() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("a").label("research"));
        assert!(werk.pending(&Query::from("task.label = research")));
        assert!(!werk.pending(&Query::from("task.label = report")));
    }

    #[test]
    fn pending_while_a_claimed_task_awaits_the_model() {
        let (werk, _tmp) = test_werk();
        werk.add_task("x");
        werk.claim(&Query::from("task.status = todo"), "agent")
            .unwrap();
        assert!(werk.pending(&Query::all()));
    }

    #[test]
    fn pending_when_a_text_only_reply_pauses_a_non_interactive_agent() {
        let (werk, _tmp) = test_werk();
        werk.add_task("x");
        let id = werk
            .claim(&Query::from("task.status = todo"), "agent")
            .unwrap();
        werk.append_reply(
            &id,
            Reply::assistant(&[crate::providers::ContentBlock::Text {
                text: "hello".into(),
            }]),
        );
        // Only an interactive agent waits on the caller; the rest are retried.
        assert!(werk.pending(&Query::all()));
    }

    #[test]
    fn not_pending_once_every_task_is_finished_or_failed() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        let id_a = werk.claim(&Query::from("t-1"), "agent").unwrap();
        let id_b = werk.claim(&Query::from("t-2"), "agent").unwrap();
        werk.set_finished_by(&id_a, "agent").unwrap();
        werk.set_task_failed(&id_b).unwrap();
        assert!(!werk.pending(&Query::all()));
    }

    #[test]
    fn not_pending_on_a_cancelled_task() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("a").label("research"));
        werk.cancel_tasks("task.label = research");
        assert!(!werk.pending(&Query::all()));
    }

    #[test]
    fn policy_round_trips_through_get_policy() {
        let (werk, _tmp) = test_werk();
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
        werk.set_policy(policy.clone());

        assert_eq!(werk.get_policy(), policy);
    }

    #[test]
    fn get_policy_returns_the_defaults_before_policy_is_called() {
        let (werk, _tmp) = test_werk();
        assert_eq!(werk.get_policy(), Policy::default());
    }

    #[test]
    fn compaction_threshold_clamps_a_fraction_outside_the_unit_range() {
        let (werk, _tmp) = test_werk();
        for (given, expected) in [(1.5, 1.0), (-0.2, 0.0), (f64::NAN, 1.0)] {
            werk.set_policy(Policy {
                compaction_threshold: Some(given),
                ..Default::default()
            });
            assert_eq!(
                werk.get_policy().compaction_threshold,
                Some(expected),
                "given {given}"
            );
        }
    }

    #[test]
    fn find_events_returns_the_matching_events_oldest_first() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");
        werk.claim(&Query::from("t-1"), "alice");

        let created = werk.find_events(|e: &Event| e.name == Event::TASK_CREATED);
        assert_eq!(created.len(), 2);
        assert_eq!(created[0].task_id, "t-1");
        assert_eq!(created[1].task_id, "t-2");
        assert!(created[0].created_at <= created[1].created_at);
    }

    #[test]
    fn find_events_matching_nothing_is_empty() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        assert!(werk
            .find_events(|e: &Event| e.name == Event::RUN_FINISHED)
            .is_empty());
    }

    #[test]
    fn find_events_without_a_log_is_empty() {
        let (werk, _tmp) = test_werk();
        assert!(werk.find_events(|_: &Event| true).is_empty());
    }

    #[test]
    fn find_event_returns_the_earliest_match() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        werk.add_task("b");

        let first = werk.find_event(|e: &Event| e.name == Event::TASK_CREATED);
        assert_eq!(first.unwrap().task_id, "t-1");
        assert!(werk
            .find_event(|e: &Event| e.name == Event::TASK_FAILED)
            .is_none());
    }

    #[test]
    fn find_events_accepts_namespaced_event_fields() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("scan").label("scout"));
        werk.add_task("b");
        werk.claim(&Query::from("t-1"), "scout-1");

        assert_eq!(werk.find_events("event.name = task_created").len(), 2);
        assert_eq!(werk.find_events("event.task_id = t-1").len(), 2);
        assert_eq!(werk.find_events("event.name = task_started").len(), 1);
        assert_eq!(werk.find_events("event.agent_id = scout-1").len(), 1);
        assert!(werk.find_events("event.name = run_finished").is_empty());
    }

    #[test]
    fn emit_event_accepts_each_optional_context_shape() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task(Task::new("work").label("scan"));

        let global = werk.emit_event(Event::new("global_ready").data(serde_json::json!(null)));
        let agent = werk.emit_event(
            Event::new("agent_ready")
                .data(serde_json::json!(null))
                .agent_id("scout-1"),
        );
        let task = werk.emit_event(
            Event::new("task_ready")
                .data(serde_json::json!(null))
                .task_id(&id),
        );
        let both = werk.emit_event(
            Event::new("work_ready")
                .data(serde_json::json!(null))
                .task_id(&id)
                .agent_id("scout-1"),
        );
        let unknown = werk.emit_event(Event::new("unknown_task_ready").task_id("t-404"));

        assert!(global.created_at > 0);
        assert_eq!(
            (&global.agent_id, &global.task_id, &global.label),
            (&"".into(), &"".into(), &None)
        );
        assert_eq!(
            (&agent.agent_id, &agent.task_id, &agent.label),
            (&"scout-1".into(), &"".into(), &None)
        );
        assert_eq!(
            (&task.agent_id, &task.task_id, &task.label),
            (&"".into(), &id, &Some("scan".into()))
        );
        assert_eq!(
            (&both.agent_id, &both.task_id, &both.label),
            (&"scout-1".into(), &id, &Some("scan".into()))
        );
        assert_eq!(unknown.label, None);
    }

    #[test]
    fn a_named_event_round_trips_through_the_log_and_aql() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        werk.emit_event(
            Event::new("Document Indexed").data(serde_json::json!({ "documents": 42 })),
        );

        let line = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let wire: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(wire["name"], "Document Indexed");
        assert_eq!(wire["data"], serde_json::json!({ "documents": 42 }));
        assert_eq!(wire["agent_id"], "");
        assert_eq!(wire["task_id"], "");
        assert!(wire.get("event").is_none());

        drop(werk);

        let resumed = Werk::load(dir.path()).unwrap();
        let found = resumed
            .find_event(r#"event.name = "Document Indexed""#)
            .unwrap();
        assert_eq!(found.get_name(), "Document Indexed");
        assert_eq!(found.get_data(), &serde_json::json!({ "documents": 42 }));
    }

    #[test]
    fn named_events_do_not_fire_task_result_or_failure_hooks() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("work");
        let event_calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&event_calls);
        werk.on_event(move |_, _| {
            observed.fetch_add(1, Ordering::Relaxed);
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let task_calls = Arc::clone(&calls);
        werk.on_task(move |_, _, _| {
            task_calls.fetch_add(1, Ordering::Relaxed);
        });
        let result_calls = Arc::clone(&calls);
        werk.on_result(move |_, _, _| {
            result_calls.fetch_add(1, Ordering::Relaxed);
        });
        let failure_calls = Arc::clone(&calls);
        werk.on_failure(move |_, _, _| {
            failure_calls.fetch_add(1, Ordering::Relaxed);
        });

        werk.emit_event(Event::new("work_noted").task_id(id));

        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(event_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn an_emitted_builtin_uses_name_based_hooks_without_changing_task_state() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("work");
        werk.set_result(&id, serde_json::json!("reported")).unwrap();
        let task_calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&task_calls);
        werk.on_task(move |_, _, _| {
            seen.fetch_add(1, Ordering::Relaxed);
        });
        let seen = Arc::clone(&task_calls);
        werk.on_result(move |_, _, _| {
            seen.fetch_add(1, Ordering::Relaxed);
        });
        let seen = Arc::clone(&task_calls);
        werk.on_failure(move |_, _, _| {
            seen.fetch_add(1, Ordering::Relaxed);
        });

        werk.emit_event(Event::new(Event::TASK_FINISHED).task_id(&id));

        assert_eq!(werk.get_task(&id).unwrap().status, Status::Todo);
        assert_eq!(werk.find_events("event.name = task_finished").len(), 1);
        assert_eq!(task_calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn find_event_answers_the_first_in_the_order_the_query_names() {
        let (werk, tmp) = test_werk();
        for (task_id, created_at) in [("t-1", 100), ("t-2", 200)] {
            let event = Event {
                created_at,
                ..Event::task_created().task_id(task_id)
            };
            Stats::append(tmp.path(), &event).unwrap();
        }

        let newest = werk.find_event("event.name = task_created ORDER BY event.created DESC");
        assert_eq!(newest.unwrap().task_id, "t-2");
        let oldest = werk.find_event("event.name = task_created");
        assert_eq!(oldest.unwrap().task_id, "t-1");
    }

    #[test]
    fn a_condition_reads_the_label_and_the_agent_that_caused_the_event() {
        // What makes a per-label or per-agent breakdown possible without the
        // crate keeping one.
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("scan").label("scout"));
        werk.claim(&Query::from("task.label = scout"), "scout-1");

        assert_eq!(
            werk.find_events(|e: &Event| e.label.as_deref() == Some("scout"))
                .len(),
            2
        );
        assert_eq!(
            werk.find_events(|e: &Event| e.agent_id == "scout-1").len(),
            1
        );
    }

    #[test]
    fn a_condition_naming_a_streamed_chunk_never_matches() {
        let (werk, _tmp) = test_werk();
        werk.add_task("a");
        emit_event(
            &werk,
            "t-1",
            "alice",
            Event::new(Event::TEXT_CHUNK_RECEIVED)
                .data(serde_json::json!({ "content": "a piece of the reply" })),
        );

        // Chunks are deliberately left out of the log, so nothing finds them.
        assert!(werk
            .find_events(|e: &Event| e.name == Event::TEXT_CHUNK_RECEIVED)
            .is_empty());
    }

    #[test]
    fn the_totals_keep_reporting_once_the_log_is_gone() {
        // The counters are what the run spent; the finders are what it wrote
        // down. Deleting the log separates the two.
        let (werk, dir) = test_werk();
        werk.add_task("a");
        emit_event(
            &werk,
            "t-1",
            "alice",
            Event::new(Event::REQUEST_FINISHED).data(serde_json::json!({
                "model": "m",
                "usage": crate::providers::TokenUsage {
                    input_tokens: 900,
                    output_tokens: 120,
                },
            })),
        );

        std::fs::remove_file(dir.path().join("events.jsonl")).unwrap();

        assert_eq!(werk.get_input_tokens(), 900);
        assert_eq!(werk.get_output_tokens(), 120);
        assert!(werk.find_events(|_: &Event| true).is_empty());
    }

    #[test]
    fn malformed_builtin_data_still_publishes_without_changing_derived_statistics() {
        let (werk, _tmp) = test_werk();

        let emitted = werk.emit_event(
            Event::new(Event::REQUEST_FINISHED).data(serde_json::json!({ "usage": "unknown" })),
        );

        assert_eq!(emitted.get_name(), Event::REQUEST_FINISHED);
        assert_eq!(werk.stats.event_count(Event::REQUEST_FINISHED), 1);
        assert_eq!(werk.get_input_tokens(), 0);
        assert_eq!(werk.get_output_tokens(), 0);
        assert_eq!(werk.find_events("event.name = request_finished").len(), 1);
    }

    #[test]
    fn get_dir_reads_back_the_configured_directory() {
        let (werk, tmp) = test_werk();
        assert_eq!(werk.get_dir(), tmp.path());
    }

    #[test]
    fn cancel_takes_only_the_matching_tasks_off_the_werk() {
        let (werk, _tmp) = test_werk();
        let research = werk.add_task(Task::new("x").label("research"));
        werk.add_task(Task::new("x").label("analysis"));
        werk.add_task(Task::new("x"));
        werk.cancel_tasks("task.label = research");

        let cancelled = werk.find_tasks("task.cancelled = true");
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].id, research);
    }

    #[test]
    fn cancel_applies_to_matching_tasks_inserted_later() {
        let (werk, _tmp) = test_werk();
        werk.cancel_tasks("task.label = research");
        let research = werk.add_task(Task::new("x").label("research"));
        werk.add_task(Task::new("x").label("analysis"));

        assert_eq!(
            werk.find_task("task.cancelled = true").map(|task| task.id),
            Some(research),
        );
        assert_eq!(werk.find_tasks("task.cancelled = false").len(), 1);
    }

    #[tokio::test]
    async fn start_clears_cancellation_flags_and_filters() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("first").label("research"));
        werk.cancel_tasks("task.label = research");
        assert_eq!(werk.find_tasks("task.cancelled = true").len(), 1);

        werk.start();
        werk.add_task(Task::new("second").label("research"));

        assert!(werk.find_tasks("task.cancelled = true").is_empty());
        assert_eq!(werk.find_tasks("task.pending = true").len(), 2);
        werk.cancel_all_tasks();
        werk.finish_all_tasks().await;
    }

    #[tokio::test]
    async fn finish_does_not_restart_a_run_that_ended_by_cancellation() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("work").label("research"));
        werk.start();
        werk.cancel_tasks("task.pending = true AND task.label = research");
        werk.finish_all_tasks().await;

        assert_eq!(werk.find_events("event.name = run_started").len(), 1);
        werk.finish_tasks("task.label = research").await;
        assert_eq!(werk.find_events("event.name = run_started").len(), 1);

        werk.start();
        assert_eq!(werk.find_events("event.name = run_started").len(), 2);
        werk.cancel_all_tasks();
        werk.finish_all_tasks().await;
    }

    #[test]
    fn on_event_appends_handlers_in_installation_order() {
        use std::sync::Mutex;
        let (werk, _tmp) = test_werk();
        let log: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let l1 = Arc::clone(&log);
        let l2 = Arc::clone(&log);
        werk.on_event(move |_, _| l1.lock().unwrap().push(1));
        werk.on_event(move |_, _| l2.lock().unwrap().push(2));
        emit_event(&werk, "ID", "agent", Event::new(Event::TURN_STARTED));
        assert_eq!(*log.lock().unwrap(), vec![1, 2]);
    }

    #[test]
    fn on_event_falls_back_to_default_logger_when_empty() {
        // No assertion target beyond "does not panic": with no installed
        // handlers, emit_event() must run default_logger without crashing.
        let (werk, _tmp) = test_werk();
        emit_event(&werk, "ID", "agent", Event::new(Event::TURN_STARTED));
    }

    #[test]
    fn an_event_names_the_agent_and_the_tasks_label() {
        // What a handler needs to count per agent or per label, which is where
        // those figures live now that `Stats` counts the run as a whole.
        let (werk, _tmp) = test_werk();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&outcomes);
        werk.on_event(move |_, event| {
            if event.name == Event::TASK_FINISHED {
                seen.lock()
                    .unwrap()
                    .push((event.agent_id.clone(), event.label.clone()));
            }
        });
        werk.add_task(Task::new("a").label("scan"));
        let id = werk
            .claim(&Query::from("task.label = scan"), "scout")
            .unwrap();
        werk.set_result(&id, serde_json::json!("done")).unwrap();
        werk.set_finished_by(&id, "scout").unwrap();

        assert_eq!(
            *outcomes.lock().unwrap(),
            vec![("scout".to_string(), Some("scan".to_string()))],
        );
    }

    #[tokio::test]
    async fn finish_returns_once_the_matching_task_resolves() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("work");
        let writer = Arc::clone(&werk);
        let claimed = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            attach_done_result(&writer, &claimed, "done");
        });
        let target = id.clone();
        werk.finish_tasks(move |t: &Task| t.id == target).await;
        assert!(werk.get_task(&id).unwrap().is_finished());
    }

    #[tokio::test]
    async fn finish_returns_without_an_event_when_nothing_matches_yet() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("work");
        attach_done_result(&werk, &id, "done");
        // Nothing emits from here on, so only the check before the wait can
        // resolve this.
        assert_eq!(
            werk.finish_tasks(move |t: &Task| t.id == id).await,
            vec![serde_json::json!("done")]
        );
    }

    #[test]
    fn edit_replies_edits_the_transcript_on_demand() {
        use crate::agents::tasks::ReplyContent;
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("go");
        werk.append_reply(&id, Reply::user_text("keep me"));
        werk.append_reply(&id, Reply::user_text("drop me"));

        werk.edit_replies(&id, |replies| {
            replies.retain(|reply| {
                !matches!(reply.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
            });
        });

        let replies = werk.get_task(&id).unwrap().replies;
        assert!(replies.iter().any(
            |r| matches!(r.content.first(), Some(ReplyContent::Text { text: t }) if t == "keep me")
        ));
        assert!(replies.iter().all(
            |r| !matches!(r.content.first(), Some(ReplyContent::Text { text: t }) if t == "drop me")
        ));
    }

    #[test]
    fn on_result_receives_the_finished_task_and_its_result() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_result(move |_, task, result| {
            record
                .lock()
                .unwrap()
                .push((task.id.clone(), result.clone()))
        });
        werk.add_task(Task::new("x").label("L"));
        let id = werk.claim(&Query::from("task.label = L"), "agent").unwrap();

        attach_done_result(&werk, &id, "lead");

        assert_eq!(*seen.lock().unwrap(), vec![(id, serde_json::json!("lead"))]);
    }

    #[test]
    fn on_failure_fires_for_a_tool_call_failure_not_only_a_failed_task() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_failure(move |_, event, task| {
            record
                .lock()
                .unwrap()
                .push((event.get_name().to_string(), task.id.clone()))
        });
        let id = werk.add_task("work");

        emit_event(&werk, &id, "agent", Event::new(Event::TURN_STARTED));
        emit_event(&werk, &id, "agent", tool_call_failed("no such directory"));
        werk.set_task_failed(&id).unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                ("tool_call_failed".to_string(), id.clone()),
                ("task_failed".to_string(), id.clone()),
            ]
        );
    }

    #[test]
    fn failures_accumulate_on_the_task_in_order() {
        let (werk, dir) = test_werk();
        let id = werk.add_task("work");

        emit_event(&werk, &id, "agent", tool_call_failed("no such directory"));
        emit_event(
            &werk,
            &id,
            "agent",
            Event::new(Event::REQUEST_FAILED).data(serde_json::json!({
                "model": "mock",
                "kind": crate::providers::RequestErrorKind::ConnectionFailed,
                "message": "dns lookup failed",
            })),
        );

        let task = werk.get_task(&id).unwrap();
        let names: Vec<String> = task
            .errors
            .iter()
            .map(|e| e.get_name().to_string())
            .collect();
        assert_eq!(names, ["tool_call_failed", "request_failed"]);

        // Written once, to the session log, as a full event per line.
        let body = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let logged: Vec<String> = body
            .lines()
            .map(|line| serde_json::from_str::<crate::event::Event>(line).unwrap())
            .filter(is_recorded_failure)
            .map(|event| event.get_name().to_string())
            .collect();
        assert_eq!(logged, names);
        assert!(!dir
            .path()
            .join("tasks")
            .join(&id)
            .join("errors.jsonl")
            .exists());
    }

    #[test]
    fn a_recoverable_failure_stays_on_a_finished_task() {
        let (werk, _tmp) = test_werk();
        let id = werk.add_task("work");

        // A failed tool call the model recovered from: the task finishes.
        emit_event(&werk, &id, "agent", tool_call_failed("boom"));
        werk.set_task_finished(&id, "done").unwrap();

        let task = werk.get_task(&id).unwrap();
        assert_eq!(task.status, Status::Finished);
        assert_eq!(task.errors.len(), 1);
        assert_eq!(task.errors[0].get_name(), "tool_call_failed");
    }

    #[test]
    fn the_terminal_task_failed_is_not_recorded_as_an_error() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let werk = Werk::new();
        werk.set_dir(dir.path().to_path_buf());
        let id = werk.add_task("work");
        werk.set_task_failed(&id).unwrap();
        assert!(werk.get_task(&id).unwrap().errors.is_empty());

        // The log carries `task_failed` either way, so a resumed session that
        // read it back as a failure would disagree with the run that wrote it.
        drop(werk);
        let resumed = Werk::load(dir.path()).unwrap();
        assert!(resumed.get_task(&id).unwrap().errors.is_empty());
    }

    #[test]
    fn a_failure_naming_a_task_the_directory_lost_is_skipped() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        let id = original.add_task("work");
        emit_event(&original, &id, "agent", tool_call_failed("boom"));
        drop(original);
        std::fs::remove_dir_all(dir.path().join("tasks").join(&id)).unwrap();

        let resumed = Werk::load(dir.path()).unwrap();
        assert!(resumed.get_task(&id).is_none());
        assert_eq!(resumed.get_input_tokens(), 0);
    }

    #[test]
    fn failures_round_trip_through_load() {
        let dir = crate::test_util::TempDir::new().unwrap();
        let original = Werk::new();
        original.set_dir(dir.path().to_path_buf());
        let id = original.add_task("work");
        emit_event(&original, &id, "agent", tool_call_failed("boom"));
        drop(original);

        let resumed = Werk::load(dir.path()).unwrap();
        let task = resumed.get_task(&id).unwrap();
        assert_eq!(task.errors.len(), 1);
        assert_eq!(task.errors[0].get_name(), "tool_call_failed");
    }

    #[test]
    fn on_failure_files_a_retry_through_the_werk_it_is_handed() {
        let (werk, _tmp) = test_werk();
        werk.on_failure(|werk, _, failed| {
            if failed.parent.is_none() {
                werk.add_task(Task::new(failed.task.clone()).parent(&failed.id));
            }
        });
        let id = werk.add_task("work");

        werk.set_task_failed(&id).unwrap();

        let retry = werk.find_task(format!("task.parent_id = {id}")).unwrap();
        assert_eq!(retry.task, serde_json::json!("work"));
    }

    #[test]
    fn on_event_files_a_follow_up_for_any_kind() {
        let (werk, _tmp) = test_werk();
        werk.on_event(|werk, event| {
            if event.name == Event::TURN_STARTED {
                werk.add_task(Task::new("report").label("report"));
            }
        });
        let id = werk.add_task("work");

        emit_event(&werk, &id, "agent", Event::new(Event::TURN_STARTED));

        assert_eq!(werk.find_tasks("task.label = report").len(), 1);
    }

    #[test]
    fn on_result_links_a_follow_up_to_the_finished_parent() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("scout").label("scout"));
        let id = werk
            .claim(&Query::from("task.label = scout"), "agent")
            .unwrap();
        werk.set_result(&id, serde_json::json!("lead")).unwrap();
        werk.set_finished_by(&id, "agent").unwrap();
        werk.on_result(|werk, done, _| {
            werk.add_task(Task::new("hunt").label("sniper").parent(&done.id));
        });
        emit_event(&werk, &id, "agent", Event::new(Event::TASK_FINISHED));
        let spawned = werk.find_task("task.label = sniper").unwrap();
        assert_eq!(spawned.parent, Some(id));
    }

    #[test]
    fn on_result_ignores_unfinished_events() {
        let (werk, _tmp) = test_werk();
        werk.on_result(|werk, _, _| {
            werk.add_task(Task::new("follow-up").label("next"));
        });
        emit_event(&werk, "ID", "agent", Event::new(Event::TURN_STARTED));
        assert!(werk.get_tasks().is_empty());
    }

    #[test]
    fn on_result_reads_the_results_that_landed_before_it() {
        // A condition across results, which the handler selects for itself.
        let (werk, _tmp) = test_werk();
        let counts = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&counts);
        werk.on_result(move |werk, _, _| {
            record
                .lock()
                .unwrap()
                .push(werk.find_results("task.label = scan").len());
        });
        for id in scans(&werk, 2) {
            werk.set_task_finished(&id, "clean").unwrap();
        }

        assert_eq!(*counts.lock().unwrap(), vec![1, 2]);
    }

    /// File `count` tasks labelled `scan`, all `todo`.
    fn scans(werk: &Werk, count: usize) -> Vec<String> {
        (0..count)
            .map(|i| werk.add_task(Task::new(format!("scan {i}")).label("scan")))
            .collect()
    }

    #[tokio::test]
    async fn on_result_async_handlers_run_before_finish_all_returns() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_result_async(move |_, task, result| {
            let record = Arc::clone(&record);
            async move { record.lock().unwrap().push((task.id, result)) }
        });
        let id = werk.add_task("scan the corpus");
        werk.set_task_finished(&id, "clean").unwrap();

        werk.finish_all_tasks().await;

        assert_eq!(
            *seen.lock().unwrap(),
            vec![(id, serde_json::json!("clean"))]
        );
    }

    #[tokio::test]
    async fn on_result_async_runs_every_handler_once_per_result() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        for name in ["first", "second"] {
            let record = Arc::clone(&seen);
            werk.on_result_async(move |_, _, _| {
                let record = Arc::clone(&record);
                async move { record.lock().unwrap().push(name) }
            });
        }
        let id = werk.add_task("scan the corpus");
        werk.set_task_finished(&id, "clean").unwrap();

        werk.finish_all_tasks().await;

        // Two entries, not four: a second registration does not queue twice.
        assert_eq!(*seen.lock().unwrap(), vec!["first", "second"]);
    }

    #[tokio::test]
    async fn registering_from_two_threads_at_once_queues_each_result_once() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|threads| {
            for name in ["first", "second"] {
                let werk = Arc::clone(&werk);
                let record = Arc::clone(&seen);
                let start = &start;
                threads.spawn(move || {
                    start.wait();
                    werk.on_result_async(move |_, _, _| {
                        let record = Arc::clone(&record);
                        async move { record.lock().unwrap().push(name) }
                    });
                });
            }
        });
        let id = werk.add_task("scan the corpus");
        werk.set_task_finished(&id, "clean").unwrap();

        werk.finish_all_tasks().await;

        // Two entries, not four: neither thread installed a second hook.
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_cancelled_finish_leaves_the_rest_of_the_queue_for_the_next_one() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_result_async(move |_, task, _| {
            let record = Arc::clone(&record);
            async move {
                let first = record.lock().unwrap().is_empty();
                record.lock().unwrap().push(task.id);
                if first {
                    // Outlasts the timeout below, so the finish is dropped here.
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            }
        });
        let first = werk.add_task("scan the first half");
        let second = werk.add_task("scan the second half");
        werk.set_task_finished(&first, "clean").unwrap();
        werk.set_task_finished(&second, "clean").unwrap();

        let cancelled = tokio::time::timeout(Duration::from_millis(50), werk.finish_all_tasks());
        assert!(cancelled.await.is_err());
        werk.finish_all_tasks().await;

        assert_eq!(*seen.lock().unwrap(), vec![first, second]);
    }

    #[tokio::test]
    async fn on_result_async_finishes_one_handler_before_starting_the_next() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_result_async(move |_, task, _| {
            let record = Arc::clone(&record);
            async move {
                record.lock().unwrap().push(format!("start {}", task.id));
                // A spawned handler would let the next one start here.
                tokio::task::yield_now().await;
                record.lock().unwrap().push(format!("end {}", task.id));
            }
        });
        let first = werk.add_task("scan the first half");
        let second = werk.add_task("scan the second half");
        werk.set_task_finished(&first, "clean").unwrap();
        werk.set_task_finished(&second, "clean").unwrap();

        werk.finish_all_tasks().await;

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
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let one = Arc::clone(&seen);
        werk.on_result_async(move |_, _, result| {
            let one = Arc::clone(&one);
            async move { one.lock().unwrap().push(format!("result {result}")) }
        });
        let each = Arc::clone(&seen);
        werk.on_task_async(move |_, event, _| {
            let each = Arc::clone(&each);
            async move {
                each.lock()
                    .unwrap()
                    .push(format!("task {}", event.get_name()))
            }
        });
        let id = werk.add_task("scan the corpus");
        werk.set_task_finished(&id, 1).unwrap();

        werk.finish_all_tasks().await;

        // One entry each: the two kinds share one queueing hook.
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["result 1", "task task_finished"]
        );
    }

    #[tokio::test]
    async fn on_event_async_sees_the_kinds_no_task_hook_accepts() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_event_async(move |_, event| {
            let record = Arc::clone(&record);
            async move { record.lock().unwrap().push(event.get_name().to_string()) }
        });
        emit_event(&werk, "ID", "agent", Event::new(Event::TURN_STARTED));

        werk.finish_all_tasks().await;

        assert!(seen.lock().unwrap().contains(&"turn_started".to_string()));
    }

    #[tokio::test]
    async fn on_event_async_receives_named_events() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_event_async(move |_, event| {
            let record = Arc::clone(&record);
            async move {
                if event.get_name() == "document_indexed" {
                    record.lock().unwrap().push(event.get_name().to_string());
                }
            }
        });
        werk.emit_event(Event::new("document_indexed"));

        werk.finish_all_tasks().await;

        assert_eq!(*seen.lock().unwrap(), vec!["document_indexed"]);
    }

    #[tokio::test]
    async fn on_failure_async_hands_over_the_failed_task() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_failure_async(move |_, event, task| {
            let record = Arc::clone(&record);
            async move {
                record
                    .lock()
                    .unwrap()
                    .push((event.get_name().to_string(), task.id.clone()))
            }
        });
        let id = werk.add_task("scan the corpus");
        werk.set_task_failed(&id).unwrap();

        werk.finish_all_tasks().await;

        assert_eq!(*seen.lock().unwrap(), vec![("task_failed".to_string(), id)]);
    }

    #[tokio::test]
    async fn an_async_handler_files_a_follow_up_through_the_werk_it_is_handed() {
        let (werk, _tmp) = test_werk();
        werk.on_result_async(|werk, done, _| async move {
            werk.add_task(Task::new("hunt").label("sniper").parent(&done.id));
        });
        let id = werk.add_task(Task::new("scout").label("scout"));
        werk.set_task_finished(&id, "lead").unwrap();

        werk.finish_all_tasks().await;

        let spawned = werk.find_task("task.label = sniper").unwrap();
        assert_eq!(spawned.parent, Some(id));
    }

    #[tokio::test]
    async fn an_async_handler_waits_for_a_finish_to_run_it() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_result_async(move |_, task, _| {
            let record = Arc::clone(&record);
            async move { record.lock().unwrap().push(task.id) }
        });
        let id = werk.add_task("scan the corpus");

        werk.set_task_finished(&id, "clean").unwrap();
        assert!(seen.lock().unwrap().is_empty());

        werk.finish_all_tasks().await;
        assert_eq!(*seen.lock().unwrap(), vec![id]);
    }

    #[tokio::test]
    async fn on_result_async_leaves_a_failed_task_alone() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_result_async(move |_, task, _| {
            let record = Arc::clone(&record);
            async move { record.lock().unwrap().push(task.id) }
        });
        let id = werk.add_task("scan the corpus");
        werk.set_task_failed(&id).unwrap();

        werk.finish_all_tasks().await;

        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_hook_waits_for_the_results_it_needs_before_filing_the_next_step() {
        let (werk, _tmp) = test_werk();
        werk.on_result(|werk, _, _| {
            if werk.find_results("task.label = scan").len() == 2 {
                werk.add_task(Task::new("write the report").label("report"));
            }
        });
        let scans = scans(&werk, 2);

        werk.set_task_finished(&scans[0], "clean").unwrap();
        assert!(werk.find_tasks("task.label = report").is_empty());

        werk.set_task_finished(&scans[1], "clean").unwrap();
        assert_eq!(werk.find_tasks("task.label = report").len(), 1);
    }

    #[test]
    fn on_result_inserts_a_follow_up_before_drain_is_observable() {
        let (werk, _tmp) = test_werk();
        werk.on_result(move |werk, done, _| {
            if done.get_label() == Some("scout") {
                werk.add_task(Task::new("hunt").label("sniper"));
            }
        });
        werk.add_task(Task::new("scout").label("scout"));
        let id = werk
            .claim(&Query::from("task.label = scout"), "agent")
            .unwrap();
        werk.set_result(&id, serde_json::json!("lead")).unwrap();
        werk.set_finished_by(&id, "agent").unwrap();
        // The handler ran inside `set_finished_by`, so the Werk is never
        // observably empty between the parent finishing and the follow-up.
        assert!(werk.pending(&Query::all()));
    }

    #[test]
    fn on_task_hands_the_handler_the_finished_task() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_task(move |_, event, task| {
            if event.name == Event::TASK_FINISHED {
                record.lock().unwrap().push((
                    event.agent_id.clone(),
                    task.id.clone(),
                    task.replies.len(),
                    task.result.clone(),
                ));
            }
        });
        werk.add_task(Task::new("scan").label("scan"));
        let id = werk
            .claim(&Query::from("task.label = scan"), "analyst")
            .unwrap();
        werk.append_reply(&id, Reply::user_text("hello"));
        werk.set_result(&id, serde_json::json!("done")).unwrap();
        werk.set_finished_by(&id, "analyst").unwrap();

        // `replies` is `#[serde(skip)]`, so replies here prove the
        // handler holds the in-memory task, not a disk round-trip.
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(
                "analyst".to_string(),
                id,
                1,
                Some(serde_json::json!("done"))
            )]
        );
    }

    #[test]
    fn on_task_skips_events_that_are_not_lifecycle_transitions() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.add_task(Task::new("scan").label("scan"));
        let id = werk
            .claim(&Query::from("task.label = scan"), "analyst")
            .unwrap();
        // Installed after the claim, so only the turn is in the handler's view.
        werk.on_task(move |_, _, task| record.lock().unwrap().push(task.id.clone()));
        emit_event(&werk, &id, "analyst", Event::new(Event::TURN_STARTED));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn on_task_skips_events_that_name_no_task() {
        let (werk, _tmp) = test_werk();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_task(move |_, _, task| record.lock().unwrap().push(task.id.clone()));
        emit_event(&werk, "", "", Event::new(Event::RUN_STARTED));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn on_task_coexists_with_a_user_handler() {
        let (werk, _tmp) = test_werk();
        let logged = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&logged);
        werk.on_event(move |_, e| log.lock().unwrap().push(e.get_name().to_string()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        werk.on_task(move |_, _, task| record.lock().unwrap().push(task.id.clone()));
        werk.add_task(Task::new("scan").label("scan"));
        // The claim is the lifecycle event; no second publication is needed.
        let id = werk
            .claim(&Query::from("task.label = scan"), "analyst")
            .unwrap();

        assert_eq!(*seen.lock().unwrap(), vec![id]);
        assert!(logged.lock().unwrap().contains(&"task_started".to_string()));
    }

    #[test]
    fn on_event_fires_every_handler_per_event() {
        use std::sync::atomic::AtomicU32;
        let (werk, _tmp) = test_werk();
        let count = Arc::new(AtomicU32::new(0));
        let c1 = Arc::clone(&count);
        let c2 = Arc::clone(&count);
        werk.on_event(move |_, _| {
            c1.fetch_add(1, Ordering::Relaxed);
        });
        werk.on_event(move |_, _| {
            c2.fetch_add(10, Ordering::Relaxed);
        });
        emit_event(&werk, "ID", "agent", Event::new(Event::TURN_STARTED));
        emit_event(&werk, "ID", "agent", Event::new(Event::TURN_STARTED));
        assert_eq!(count.load(Ordering::Relaxed), 22);
    }

    #[tokio::test]
    async fn run_finished_reports_drained_on_empty_werk() {
        let (werk, _tmp) = test_werk();
        let reasons = collect_finish_reasons(&werk);
        werk.finish_all_tasks().await;
        assert_eq!(*reasons.lock().unwrap(), vec![FinishReason::Drained]);
    }

    #[tokio::test]
    async fn finish_reason_reports_nothing_until_the_run_ends() {
        let (werk, _tmp) = test_werk();
        werk.start();
        assert_eq!(werk.get_finish_reason(), None);
        werk.finish_all_tasks().await;
        assert_eq!(werk.get_finish_reason(), Some(FinishReason::Drained));
    }

    #[tokio::test]
    async fn the_finish_reason_is_cleared_by_a_restart() {
        let (werk, _tmp) = test_werk();
        werk.start();
        werk.cancel_all_tasks();
        werk.finish_all_tasks().await;
        assert_eq!(werk.get_finish_reason(), Some(FinishReason::Cancelled));
        werk.start();
        assert_eq!(werk.get_finish_reason(), None);
    }

    #[tokio::test]
    async fn a_clean_drain_is_not_reported_as_cancelled() {
        let (werk, _tmp) = test_werk();
        werk.finish_all_tasks().await;
        assert_eq!(werk.get_finish_reason(), Some(FinishReason::Drained));
    }

    #[tokio::test]
    async fn finish_hands_back_only_the_results_its_filter_named() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("a").label("scan"));
        werk.add_task(Task::new("b").label("report"));
        attach_done_result(&werk, "t-1", "scanned");
        attach_done_result(&werk, "t-2", "reported");

        assert_eq!(
            werk.finish_tasks("task.label = scan").await,
            vec![serde_json::json!("scanned")]
        );
    }

    #[tokio::test]
    async fn finish_all_hands_back_the_results_of_every_pool() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("a").label("scan"));
        werk.add_task(Task::new("b").label("report"));
        attach_done_result(&werk, "t-1", "scanned");
        attach_done_result(&werk, "t-2", "reported");

        assert_eq!(
            werk.finish_all_tasks().await,
            vec![serde_json::json!("scanned"), serde_json::json!("reported")]
        );
    }

    #[tokio::test]
    async fn finish_task_hands_back_the_first_result_in_query_order() {
        let (werk, _tmp) = test_werk();
        werk.add_task(Task::new("a").label("scan"));
        werk.add_task(Task::new("b").label("report"));
        // Resolve back to front so the answer distinguishes creation order from completion order.
        attach_done_result(&werk, "t-2", "reported");
        attach_done_result(&werk, "t-1", "scanned");

        assert_eq!(
            werk.finish_task("ORDER BY task.id DESC").await,
            Some(serde_json::json!("reported"))
        );
    }

    #[tokio::test]
    async fn finish_task_is_none_when_nothing_finished() {
        let (werk, _tmp) = test_werk();

        assert_eq!(werk.finish_task("task.status = finished").await, None);
    }

    #[tokio::test]
    async fn run_finished_reports_cancelled_when_cancel_fires_during_run() {
        let (werk, _tmp) = test_werk();
        let reasons = collect_finish_reasons(&werk);
        werk.start();
        werk.cancel_all_tasks();
        werk.finish_all_tasks().await;
        assert_eq!(*reasons.lock().unwrap(), vec![FinishReason::Cancelled]);
        assert_eq!(werk.get_finish_reason(), Some(FinishReason::Cancelled));
    }

    #[tokio::test]
    async fn run_finished_reports_policy_violated_when_max_turns_zero() {
        let (werk, _tmp) = test_werk();
        let reasons = collect_finish_reasons(&werk);
        werk.set_policy(Policy {
            max_turns: Some(0),
            ..Default::default()
        });
        werk.finish_all_tasks().await;
        assert_eq!(
            *reasons.lock().unwrap(),
            vec![FinishReason::PolicyViolated(crate::PolicyViolation::Turns)],
        );
    }

    #[tokio::test]
    async fn run_finished_is_emitted_again_after_a_restart() {
        let (werk, _tmp) = test_werk();
        let reasons = collect_finish_reasons(&werk);
        werk.finish_all_tasks().await;
        werk.start();
        werk.finish_all_tasks().await;
        assert_eq!(
            *reasons.lock().unwrap(),
            vec![FinishReason::Drained, FinishReason::Drained],
        );
    }

    #[tokio::test]
    async fn run_started_emitted_before_run_finished() {
        let (werk, _tmp) = test_werk();
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        werk.on_event(move |_, e| {
            if matches!(e.get_name(), Event::RUN_STARTED | Event::RUN_FINISHED) {
                sink.lock().unwrap().push(e.get_name().to_string());
            }
        });
        werk.finish_all_tasks().await;
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2, "expected RunStarted then RunFinished");
        assert_eq!(entries[0], Event::RUN_STARTED);
        assert_eq!(entries[1], Event::RUN_FINISHED);
    }
}
