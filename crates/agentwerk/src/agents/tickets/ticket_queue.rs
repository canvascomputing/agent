//! The shared queue agents claim tickets from, and the lifecycle that drives them.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;

use crate::event::{default_logger, Event, EventKind, FinishReason};
use crate::persistence::Persist;
use crate::providers::ProviderResult;
use crate::schemas::Schema;

use super::super::agent::{Agent, TicketQueueRef};
use super::super::compaction::Compaction;
use super::super::policy::Policies;
use super::super::r#loop::run_main_loop;
use super::super::stats::Stats;
use super::ticket::{Status, Ticket};
use super::{now_millis, numeric_id, policy_violated_kind, Reply};

type EventHandler = dyn Fn(&Event) + Send + Sync;

/// How many events a `finish_on_*` waiter may fall behind before it starts
/// missing them. `TextChunkReceived` fires once per streaming delta and sets
/// the volume this has to absorb.
const EVENT_STREAM_CAPACITY: usize = 1024;

type ReplyEditor = dyn Fn(&[Event], &mut Vec<Reply>) + Send + Sync;

type CompactionEditor = dyn Fn(Compaction, Vec<Reply>) -> EditedReplies + Send + Sync;

type DirectiveEditor = dyn Fn(&Event, &mut String) + Send + Sync;

/// The replies an editor hands back, once its work finishes.
type EditedReplies = Pin<Box<dyn Future<Output = ProviderResult<Vec<Reply>>> + Send>>;

/// The registered reply editor and the per-ticket events buffered for it since
/// each ticket's previous request. The two are always touched together.
#[derive(Default)]
pub(super) struct ReplyEditing {
    pub(super) editor: Option<Arc<ReplyEditor>>,
    pub(super) pending: HashMap<String, Vec<Event>>,
}

/// The core data structure of agentwerk, coordinating complex work across
/// agents. Many agents share one `TicketQueue` and pick up tickets
/// concurrently; labels and names assign work to the right agent.
///
/// ```no_run
/// use agentwerk::{Agent, Ticket, TicketQueue};
/// use agentwerk::tools::FetchUrlTool;
///
/// # async fn run() {
/// let tickets = TicketQueue::new();
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
/// ├── stats.json                            execution statistics
/// ├── tickets.jsonl                         lifecycle events (one per line)
/// ├── results.jsonl                         finished results (one per line)
/// ├── tickets/
/// │   └── TICKET-1/
/// │       ├── ticket.json                   the ticket without its messages
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
    /// Set when `finish()` should stop polling: external cancel, policy
    /// violation, or clean drain. The agent tasks and the tools poll it too,
    /// and flipping it takes them off the queue.
    pub(crate) stop_signal: Mutex<Arc<AtomicBool>>,
    /// Set only by `cancel()`, `cancel_on`, and `cancel_on_event`. Read
    /// by `is_cancelled()` so observers can tell external cancel apart
    /// from policy stops and clean drains.
    pub(crate) cancel_signal: Mutex<Arc<AtomicBool>>,
    /// Labels whose pool `cancel_label` has stopped. A ticket carrying one is
    /// neither claimed nor resumed, and an agent already holding one is taken
    /// off it, while the rest of the run continues.
    pub(crate) cancelled_labels: Mutex<HashSet<String>>,
    /// Result schema applied at insert time to every ticket that carries a
    /// registered label and no schema of its own, so a result contract follows
    /// its label no matter how the ticket was created.
    pub(crate) label_schemas: Mutex<HashMap<String, Schema>>,
    /// How many terminal status transitions are between their status change and
    /// the return of their event handlers. `finish()` counts a non-zero value as
    /// pending work, so a handler creating a follow-up ticket always beats the drain.
    pub(crate) terminal_transitions_in_flight: AtomicUsize,
    pub(crate) stats: Stats,
    pub(super) event_handlers: Mutex<Vec<Arc<EventHandler>>>,
    /// Every emitted event, for the `finish_on_*` family to await. A separate
    /// channel rather than one more `on_event` entry: a handler stays on the
    /// chain for the life of the queue, so one registered per call would grow
    /// without bound in a host that awaits in a loop.
    pub(super) event_stream: broadcast::Sender<Event>,
    /// Fires when `stop_signal` is set: the signal is the half you read, this
    /// is the half you await. `cancel()` emits no event, so without it a
    /// `finish_on_*` waiter would never learn the run had stopped.
    pub(super) stop_notice: Notify,
    /// The editor that rewrites a ticket's replies before each request, plus the
    /// per-ticket events buffered for it. Buffering runs whether or not an
    /// editor is installed.
    pub(super) reply_editing: Mutex<ReplyEditing>,
    /// What compaction does with a ticket's replies. `None` summarizes them.
    pub(super) compaction_editor: Mutex<Option<Arc<CompactionEditor>>>,
    /// What corrects an agent asked to try again. `None` keeps the built-in
    /// directive.
    pub(super) directive_editor: Mutex<Option<Arc<DirectiveEditor>>>,
    pub(super) dir: Mutex<PathBuf>,
    pub(super) tickets_log_lock: Mutex<()>,
    pub(super) results_log_lock: Mutex<()>,
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
            stop_signal: Mutex::new(Arc::new(AtomicBool::new(false))),
            cancel_signal: Mutex::new(Arc::new(AtomicBool::new(false))),
            cancelled_labels: Mutex::new(HashSet::new()),
            label_schemas: Mutex::new(HashMap::new()),
            terminal_transitions_in_flight: AtomicUsize::new(0),
            stats: Stats::new(),
            event_handlers: Mutex::new(Vec::new()),
            event_stream: broadcast::Sender::new(EVENT_STREAM_CAPACITY),
            stop_notice: Notify::new(),
            reply_editing: Mutex::new(ReplyEditing::default()),
            compaction_editor: Mutex::new(None),
            directive_editor: Mutex::new(None),
            dir: Mutex::new(PathBuf::from(".agentwerk")),
            tickets_log_lock: Mutex::new(()),
            results_log_lock: Mutex::new(()),
            join_handle: Mutex::new(None),
            next_ticket_id: Mutex::new(None),
        })
    }

    /// Continue a session from `tickets_dir`, or start one there when it is empty.
    ///
    /// Every ticket is read back with its status, result, and messages, and the
    /// statistics resume from `stats.json`, so success rate and totals stay
    /// continuous across restarts. Pointing this and `Knowledge::load` at the
    /// same directory keeps the knowledge pages beside the session.
    ///
    /// An unfinished ticket is picked up again by the agent whose name it
    /// carries, so agent names must stay the same across restarts.
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
                // and timestamps with it, and `Stats::derive` would then
                // recompute the run's counters from whatever did load.
                let ticket = Ticket::load(&tickets_dir, &key).map_err(|source| {
                    io::Error::new(
                        source.kind(),
                        format!("ticket {key} could not be read: {source}"),
                    )
                })?;
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
            stats,
            event_handlers: Mutex::new(Vec::new()),
            event_stream: broadcast::Sender::new(EVENT_STREAM_CAPACITY),
            stop_notice: Notify::new(),
            reply_editing: Mutex::new(ReplyEditing::default()),
            compaction_editor: Mutex::new(None),
            directive_editor: Mutex::new(None),
            dir: Mutex::new(tickets_dir),
            tickets_log_lock: Mutex::new(()),
            results_log_lock: Mutex::new(()),
            join_handle: Mutex::new(None),
            next_ticket_id: Mutex::new(Some(next_id)),
        }))
    }

    /// Get the execution statistics, available during execution and after it finishes.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Push an event observer onto the handler chain. Every installed
    /// handler fires on every event, in installation order. Handlers
    /// must be cheap and non-blocking. When no handler has been
    /// installed, [`default_logger`] runs in its place.
    pub fn on_event(&self, h: impl Fn(&Event) + Send + Sync + 'static) -> &Self {
        self.event_handlers.lock().unwrap().push(Arc::new(h));
        self
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
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            if !matches!(event.kind, EventKind::TicketFinished) {
                return;
            }
            let Some(queue) = supervisor.upgrade() else {
                return;
            };
            let Some(finished) = queue.get_ticket(&event.ticket_key) else {
                return;
            };
            let Some(result) = finished.result.clone() else {
                return;
            };
            handler(&finished, &result);
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
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            if !event.kind.is_failure() {
                return;
            }
            let Some(queue) = supervisor.upgrade() else {
                return;
            };
            let Some(ticket) = queue.get_ticket(&event.ticket_key) else {
                return;
            };
            handler(event, &ticket);
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
        // handler (like `cancel_on_event`), installed once with the first editor.
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

    /// Rewrite the prompt that corrects an agent's behavior.
    ///
    /// agentwerk asks again when a turn ends without an accepted result: the
    /// model called no tool, or its result missed the ticket's schema. Your
    /// function receives the `SchemaRetried` event that says which of the two
    /// happened, in whose ticket, and for which agent, together with the prompt
    /// itself. The prompt arrives holding the built-in text, so writing nothing
    /// keeps the default. It runs once per retry, so keep it cheap.
    ///
    /// One editor is held at a time, and installing a second replaces the
    /// first, the way [`Self::edit_replies_on_compaction`] does.
    ///
    /// ```no_run
    /// use agentwerk::TicketQueue;
    /// use agentwerk::event::EventKind;
    ///
    /// let tickets = TicketQueue::new();
    /// tickets.edit_directive_on_retry(|event, directive| {
    ///     let EventKind::SchemaRetried { message, .. } = &event.kind else {
    ///         return;
    ///     };
    ///     *directive = format!("{message}\nCall `finish` with a result now.");
    /// });
    /// ```
    pub fn edit_directive_on_retry(
        &self,
        editor: impl Fn(&Event, &mut String) + Send + Sync + 'static,
    ) -> &Self {
        *self.directive_editor.lock().unwrap() = Some(Arc::new(editor));
        self
    }

    /// Apply the registered editor to the directive `event` calls for. Does
    /// nothing until an editor is registered.
    pub(crate) fn edit_directive(&self, event: &Event, directive: &mut String) {
        let editor = self.directive_editor.lock().unwrap().clone();
        if let Some(editor) = editor {
            editor(event, directive);
        }
    }

    /// Publish `kind` and hand back the event it became, so a caller that also
    /// acts on it works from what every observer saw.
    pub(crate) fn emit(&self, key: &str, agent: &str, kind: EventKind) -> Event {
        self.stats
            .record_event(&kind, key, &self.labels_for(key), agent);
        let event = Event::new(agent, key, kind);
        // Published before the handlers run: a `finish_on_*` waiter competes
        // with them for nothing, and no handler can swallow the event. The
        // count is checked first so a run with no waiter never pays the clone,
        // which `TextChunkReceived` would otherwise charge per streamed token.
        if self.event_stream.receiver_count() > 0 {
            let _ = self.event_stream.send(event.clone());
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

    fn labels_for(&self, key: &str) -> Vec<String> {
        self.tickets
            .lock()
            .unwrap()
            .get(key)
            .map(|t| t.labels.clone())
            .unwrap_or_default()
    }

    /// Get the model that agent runs, or `None` when no agent of that name is bound.
    ///
    /// Pairs with [`Self::on_ticket`]: the event names the agent, this names
    /// its model, and [`Trajectory::from_ticket`] needs both.
    ///
    /// [`Trajectory::from_ticket`]: super::Trajectory::from_ticket
    pub fn model_for_agent(&self, agent_name: &str) -> Option<String> {
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

    /// Limit the total number of turns.
    pub fn max_turns(&self, n: u32) -> &Self {
        self.policies.lock().unwrap().max_turns = Some(n);
        self
    }

    /// Limit the total input tokens.
    pub fn max_input_tokens(&self, n: u64) -> &Self {
        self.policies.lock().unwrap().max_input_tokens = Some(n);
        self
    }

    /// Limit the total output tokens.
    pub fn max_output_tokens(&self, n: u64) -> &Self {
        self.policies.lock().unwrap().max_output_tokens = Some(n);
        self
    }

    /// Limit the output tokens of a single request.
    pub fn max_request_tokens(&self, n: u32) -> &Self {
        self.policies.lock().unwrap().max_request_tokens = Some(n);
        self
    }

    /// Limit how often a result may fail its schema before the ticket fails.
    pub fn max_schema_retries(&self, n: u32) -> &Self {
        self.policies.lock().unwrap().max_schema_retries = Some(n);
        self
    }

    /// Limit how often a failing request is retried.
    pub fn max_request_retries(&self, n: u32) -> &Self {
        self.policies.lock().unwrap().max_request_retries = n;
        self
    }

    /// Wait this long between retries.
    pub fn request_retry_delay(&self, d: Duration) -> &Self {
        self.policies.lock().unwrap().request_retry_delay = d;
        self
    }

    /// Limit the total elapsed duration.
    ///
    /// On reaching it, `finish` stops with
    /// `FinishReason::PolicyViolated(PolicyKind::Time)` and emits the matching
    /// `PolicyViolated` event.
    pub fn max_time(&self, d: Duration) -> &Self {
        self.policies.lock().unwrap().max_time = Some(d);
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

    /// Stop execution when another task you supply finishes.
    ///
    /// Its output is discarded; only finishing matters. Any source works:
    /// ctrl-c, a deadline, a channel receive, an external signal.
    pub fn cancel_on<F>(&self, trigger: F) -> &Self
    where
        F: Future + Send + 'static,
        F::Output: Send,
    {
        let supervisor = self
            .weak_self
            .upgrade()
            .expect("TicketQueue dropped during cancel_on");
        tokio::spawn(async move {
            let _ = trigger.await;
            supervisor.cancel();
        });
        self
    }

    /// Stop execution when an event matches.
    ///
    /// This is one more entry on the [`Self::on_event`] chain, so it works
    /// alongside any logger you installed.
    pub fn cancel_on_event<F>(&self, predicate: F) -> &Self
    where
        F: Fn(&Event) -> bool + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            if !predicate(event) {
                return;
            }
            if let Some(s) = supervisor.upgrade() {
                s.cancel();
            }
        })
    }

    /// Stop one label's agents when an event matches, while the rest keep working.
    ///
    /// The label-scoped counterpart of [`Self::cancel_on_event`]: it calls
    /// [`Self::cancel_label`] rather than ending execution.
    pub fn cancel_label_on_event<F>(&self, label: impl Into<String>, predicate: F) -> &Self
    where
        F: Fn(&Event) -> bool + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        let label = label.into();
        self.on_event(move |event| {
            if !predicate(event) {
                return;
            }
            if let Some(s) = supervisor.upgrade() {
                s.cancel_label(label.clone());
            }
        })
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

    /// Stop execution when a finished result matches.
    ///
    /// Built on [`Self::on_result`], so the value passed is the stored,
    /// schema-validated result.
    pub fn cancel_on_result<F>(&self, predicate: F) -> &Self
    where
        F: Fn(&Ticket, &serde_json::Value) -> bool + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_result(move |ticket, result| {
            if !predicate(ticket, result) {
                return;
            }
            if let Some(s) = supervisor.upgrade() {
                s.cancel();
            }
        })
    }

    /// Stop one label's agents when a finished result matches.
    ///
    /// The label-scoped counterpart of [`Self::cancel_on_result`]: it calls
    /// [`Self::cancel_label`] rather than ending execution. Any matching result
    /// stops `label`, whether or not the finished ticket carried it, which is
    /// how [`Self::cancel_label_on_event`] behaves too.
    ///
    /// ```no_run
    /// # use agentwerk::TicketQueue;
    /// let tickets = TicketQueue::new();
    /// tickets.cancel_label_on_result("scan", |_, result| result["verdict"] == "malicious");
    /// ```
    pub fn cancel_label_on_result<F>(&self, label: impl Into<String>, predicate: F) -> &Self
    where
        F: Fn(&Ticket, &serde_json::Value) -> bool + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        let label = label.into();
        self.on_result(move |ticket, result| {
            if !predicate(ticket, result) {
                return;
            }
            if let Some(s) = supervisor.upgrade() {
                s.cancel_label(label.clone());
            }
        })
    }

    /// Enqueue a follow-up ticket from a finished ticket.
    ///
    /// Your function reads the finished ticket's key, labels, and task
    /// alongside its result, and can chain the follow-up through
    /// `Ticket::parent`. Guard against a follow-up that triggers itself, or
    /// execution never ends.
    pub fn create_ticket_on_result<F>(&self, make: F) -> &Self
    where
        F: Fn(&Ticket, &serde_json::Value) -> Option<Ticket> + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_result(move |finished, result| {
            let Some(ticket) = make(finished, result) else {
                return;
            };
            if let Some(queue) = supervisor.upgrade() {
                queue.ticket(ticket);
            }
        })
    }

    /// Stop execution when a failure matches.
    ///
    /// Built on [`Self::on_failure`], so your condition sees all six failure
    /// kinds. Narrow it on `event.kind` to stop on a failed ticket only, or
    /// leave it broad to stop on the first tool or LLM provider error.
    pub fn cancel_on_failure<F>(&self, predicate: F) -> &Self
    where
        F: Fn(&Event, &Ticket) -> bool + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_failure(move |event, ticket| {
            if !predicate(event, ticket) {
                return;
            }
            if let Some(s) = supervisor.upgrade() {
                s.cancel();
            }
        })
    }

    /// Stop one label's agents when a failure matches.
    ///
    /// The label-scoped counterpart of [`Self::cancel_on_failure`]: it calls
    /// [`Self::cancel_label`] rather than ending execution.
    pub fn cancel_label_on_failure<F>(&self, label: impl Into<String>, predicate: F) -> &Self
    where
        F: Fn(&Event, &Ticket) -> bool + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        let label = label.into();
        self.on_failure(move |event, ticket| {
            if !predicate(event, ticket) {
                return;
            }
            if let Some(s) = supervisor.upgrade() {
                s.cancel_label(label.clone());
            }
        })
    }

    /// Enqueue a retry for a ticket that failed.
    ///
    /// Your function reads the failed ticket's task and labels and hands back a
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
        let supervisor = self.weak_self.clone();
        self.on_failure(move |event, failed| {
            let Some(ticket) = make(event, failed) else {
                return;
            };
            if let Some(queue) = supervisor.upgrade() {
                queue.ticket(ticket);
            }
        })
    }

    /// Read a ticket as it starts, finishes, or fails.
    ///
    /// The handler receives the event plus the ticket it names, already
    /// resolved, so it reads the result, labels, and replies without a second
    /// lookup. No other kind reaches the handler: resolving a ticket copies its
    /// replies, which on `TextChunkReceived` would cost once per piece of the
    /// reply.
    pub fn on_ticket<F>(&self, handler: F) -> &Self
    where
        F: Fn(&Event, &Ticket) + Send + Sync + 'static,
    {
        let supervisor = self.weak_self.clone();
        self.on_event(move |event| {
            if !matches!(
                event.kind,
                EventKind::TicketStarted | EventKind::TicketFinished | EventKind::TicketFailed
            ) {
                return;
            }
            let Some(queue) = supervisor.upgrade() else {
                return;
            };
            let Some(ticket) = queue.get_ticket(&event.ticket_key) else {
                return;
            };
            handler(event, &ticket);
        })
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

    /// Register a schema every ticket of that label validates against.
    ///
    /// A ticket created with a schema of its own keeps it. Otherwise the schema
    /// is applied at creation, so the contract follows the label whether the
    /// ticket came from `task`, from `ticket`, or from a `finish` handover.
    pub fn schema_for_label(&self, label: impl Into<String>, schema: Schema) -> &Self {
        self.label_schemas
            .lock()
            .unwrap()
            .insert(label.into(), schema);
        self
    }

    /// Submit a task and return its ticket key.
    pub fn task<T: Serialize>(&self, task: T) -> String {
        self.dispatch(Ticket::new(task))
    }

    /// Submit a `Ticket` with custom labels or schema, and return its key.
    ///
    /// Key, reporter, creation time, status, and result are set at insertion
    /// and overwrite whatever the ticket carried. To pin the ticket to one
    /// agent, label it with that agent's name.
    pub fn ticket(&self, ticket: Ticket) -> String {
        self.dispatch(ticket)
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
    pub fn find_tickets<F>(&self, predicate: F) -> Vec<Ticket>
    where
        F: Fn(&Ticket) -> bool,
    {
        let store = self.tickets.lock().unwrap();
        let mut out: Vec<Ticket> = store.values().filter(|t| predicate(t)).cloned().collect();
        out.sort_by_key(|t| (t.created_at, numeric_id(&t.key)));
        out
    }

    /// Get the earliest ticket matching a condition.
    ///
    /// Your condition MUST NOT call another `TicketQueue` method that reads
    /// the ticket store, or the call deadlocks.
    pub fn find_ticket<F>(&self, predicate: F) -> Option<Ticket>
    where
        F: Fn(&Ticket) -> bool,
    {
        let store = self.tickets.lock().unwrap();
        let mut matching: Vec<&Ticket> = store.values().filter(|t| predicate(t)).collect();
        matching.sort_by_key(|t| (t.created_at, numeric_id(&t.key)));
        matching.into_iter().next().cloned()
    }

    /// Stop one label's agents while the rest keep working.
    ///
    /// Its tickets are neither claimed nor resumed, and an agent already
    /// holding one is taken off it. That ticket stays `InProgress`, exactly as
    /// [`Self::cancel`] leaves the tickets in flight.
    pub fn cancel_label(&self, label: impl Into<String>) -> &Self {
        self.cancelled_labels.lock().unwrap().insert(label.into());
        self
    }

    /// Check whether one label's agents have been stopped.
    ///
    /// Ask before creating follow-up work: a ticket carrying a stopped label is
    /// never claimed. It reads no ticket state, so it is safe to call from a
    /// condition passed to [`Self::find_ticket`] or [`Self::find_tickets`].
    ///
    /// ```no_run
    /// # use agentwerk::{Ticket, TicketQueue};
    /// let tickets = TicketQueue::new();
    /// let queue = std::sync::Arc::downgrade(&tickets);
    /// tickets.create_ticket_on_result(move |done, _| {
    ///     if !done.has_label("research") || queue.upgrade()?.is_label_cancelled("research") {
    ///         return None;
    ///     }
    ///     Some(Ticket::new("search again").label("research"))
    /// });
    /// ```
    pub fn is_label_cancelled(&self, label: &str) -> bool {
        self.cancelled_labels.lock().unwrap().contains(label)
    }

    /// True when any of `labels` has been stopped by [`Self::cancel_label`]. The
    /// claim and resume path reads this to keep a stopped pool's tickets off the
    /// queue. It reads no ticket state, so a claim check may call it.
    pub(crate) fn is_any_label_cancelled(&self, labels: &[String]) -> bool {
        let cancelled = self.cancelled_labels.lock().unwrap();
        labels.iter().any(|l| cancelled.contains(l))
    }

    /// How many tickets are still `Todo` or `InProgress`.
    pub(crate) fn pending_count(&self) -> usize {
        self.tickets
            .lock()
            .unwrap()
            .values()
            .filter(|t| matches!(t.status, Status::Todo | Status::InProgress))
            .count()
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
                let reporter = agent.name.clone();
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
    /// Any tickets the agent queued on its own move into this queue. To keep a
    /// handle on the agent itself, use [`Agent::ticket_queue`] instead. An
    /// agent added while execution is under way picks up its first ticket
    /// within about 100 ms.
    pub fn agent(&self, mut agent: Agent) -> &Self {
        self.bind_agent(&mut agent);
        self
    }

    /// Begin processing tickets, on a background task.
    ///
    /// A ticket queued afterwards is picked up within about 100 ms. Pair this
    /// with [`Self::finish`] to wait for the queue to empty, or with
    /// [`Self::cancel`] to stop early.
    pub fn start(&self) -> &Self {
        // Reset both signals so a queue can be re-started after a
        // previous finish left flags set.
        self.stop_signal
            .lock()
            .unwrap()
            .store(false, Ordering::Relaxed);
        self.cancel_signal
            .lock()
            .unwrap()
            .store(false, Ordering::Relaxed);
        self.cancelled_labels.lock().unwrap().clear();
        self.reply_editing.lock().unwrap().pending.clear();
        let supervisor = self
            .weak_self
            .upgrade()
            .expect("TicketQueue dropped during start");
        self.emit("", "", EventKind::RunStarted);
        let join = tokio::spawn(async move {
            run_main_loop(&supervisor).await;
            supervisor.stats.record_execution_finished(now_millis());
        });
        *self.join_handle.lock().unwrap() = Some(join);
        self
    }

    /// Process every queued ticket, then return.
    ///
    /// Execution begins here when it has not already, and otherwise this waits
    /// on what is already running. It ends on cancel, on a policy violation, or
    /// once the queue is empty, in that precedence. The reason is announced as
    /// [`EventKind::RunFinished`].
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
        self.stop_notice.notify_waiters();
        self.take_join_handle().await;
        self.stats.record_execution_finished(now_millis());
        self.emit("", "", EventKind::RunFinished { reason });
        self
    }

    /// Cancel the execution.
    ///
    /// This is not async, so it can be called from a ctrl-c handler, a drop
    /// guard, or anywhere else. Every agent and tool stops shortly after, and
    /// [`Self::finish`] returns with `FinishReason::Cancelled`.
    pub fn cancel(&self) -> &Self {
        self.cancel_signal
            .lock()
            .unwrap()
            .store(true, Ordering::Relaxed);
        self.stop_signal
            .lock()
            .unwrap()
            .store(true, Ordering::Relaxed);
        self.stop_notice.notify_waiters();
        self
    }

    async fn take_join_handle(&self) {
        let handle = self.join_handle.lock().unwrap().take();
        if let Some(h) = handle {
            let _ = h.await;
        }
    }

    /// Check whether the execution was cancelled.
    ///
    /// True only after [`Self::cancel`], [`Self::cancel_on`], or
    /// [`Self::cancel_on_event`]. An empty queue or a policy violation does not
    /// count as cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_signal.lock().unwrap().load(Ordering::Relaxed)
    }

    /// Get the result of every finished ticket, in creation order.
    ///
    /// Read a structured result back with `serde_json::from_value`. A ticket
    /// still running, or finished without a result, contributes nothing, so
    /// this is shorter than [`Self::tickets`] rather than aligned with it.
    pub fn results(&self) -> Vec<serde_json::Value> {
        self.find_tickets(|t| t.status == Status::Finished && t.result.is_some())
            .into_iter()
            .filter_map(|t| t.result)
            .collect()
    }

    /// Get the result of every finished ticket carrying a label.
    pub fn results_for_label(&self, label: &str) -> Vec<serde_json::Value> {
        self.find_tickets(|t| t.is_finished() && t.has_label(label))
            .into_iter()
            .filter_map(|t| t.result)
            .collect()
    }

    /// Get the result of every finished ticket claimed by an agent.
    pub fn results_for_agent(&self, agent_name: &str) -> Vec<serde_json::Value> {
        self.find_tickets(|t| t.is_finished() && t.assignee.as_deref() == Some(agent_name))
            .into_iter()
            .filter_map(|t| t.result)
            .collect()
    }

    /// Get one ticket's result by key.
    ///
    /// `None` while the ticket is unfinished, so a handover child can read its
    /// parent's result through `ticket.parent` without checking the status.
    pub fn result_for_ticket(&self, key: &str) -> Option<serde_json::Value> {
        self.get_ticket(key).filter(Ticket::is_finished)?.result
    }

    /// Get every ticket carrying a label, in any status.
    pub fn tickets_for_label(&self, label: &str) -> Vec<Ticket> {
        self.find_tickets(|t| t.has_label(label))
    }

    /// Get every ticket claimed by an agent, in any status.
    ///
    /// A ticket only labelled with the agent's name is not counted: a label
    /// makes a ticket eligible, `assignee` records the agent that claimed it.
    pub fn tickets_for_agent(&self, agent_name: &str) -> Vec<Ticket> {
        self.find_tickets(|t| t.assignee.as_deref() == Some(agent_name))
    }

    fn is_stopped(&self) -> bool {
        self.stop_signal.lock().unwrap().load(Ordering::Relaxed)
    }

    /// Resolves once execution has stopped, whether through `cancel()`, a
    /// policy violation, or a clean drain.
    async fn stopped(&self) {
        let notified = self.stop_notice.notified();
        tokio::pin!(notified);
        // Registered before the flag is read: a stop landing between the two
        // would otherwise wake nobody and leave the caller waiting for good.
        notified.as_mut().enable();
        if self.is_stopped() {
            return;
        }
        notified.await;
    }

    /// Get the first event that matches, and execution carries on.
    ///
    /// Call it after [`Self::start`]. It gives back `None` when execution stops
    /// before anything matches. To stop the run on a match instead, use
    /// [`Self::cancel_on_event`].
    ///
    /// A waiter that falls far enough behind loses the events it skipped, so a
    /// match can go unseen under heavy streaming. [`Self::finish_on_ticket`]
    /// and [`Self::finish_on_result`] re-read the store instead and cannot.
    pub async fn finish_on_event<F>(&self, condition: F) -> Option<Event>
    where
        F: Fn(&Event) -> bool,
    {
        self.finish_on(|event| condition(event).then(|| event.clone()))
            .await
    }

    /// Await the first event `pick` accepts and hand back what it produced.
    ///
    /// One subscription spans the whole wait. Taking a fresh one per rejected
    /// event would start at the tail and lose whatever was published in
    /// between, which no store re-read could recover.
    async fn finish_on<T, F>(&self, pick: F) -> Option<T>
    where
        F: Fn(&Event) -> Option<T>,
    {
        let mut stream = self.event_stream.subscribe();
        loop {
            tokio::select! {
                received = stream.recv() => match received {
                    Ok(event) => {
                        if let Some(found) = pick(&event) {
                            return Some(found);
                        }
                    }
                    // A lagging reader misses events rather than the run
                    // stalling to wait for it, so keep reading the rest.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
                _ = self.stopped() => {
                    while let Ok(event) = stream.try_recv() {
                        if let Some(found) = pick(&event) {
                            return Some(found);
                        }
                    }
                    return None;
                }
            }
        }
    }

    /// Waits for the next event, giving back `false` once execution has stopped
    /// or nothing can emit again. One subscription spans the whole wait, so an
    /// event arriving between two reads of the store is never missed.
    async fn next_event(&self, stream: &mut broadcast::Receiver<Event>) -> bool {
        tokio::select! {
            // A lagging reader misses events rather than the run stalling to
            // wait for it, so only a closed channel ends the wait.
            received = stream.recv() => !matches!(received, Err(broadcast::error::RecvError::Closed)),
            _ = self.stopped() => false,
        }
    }

    /// Clones only the winner, the way [`Self::find_ticket`] does. Going
    /// through `find_tickets` would deep-copy every finished ticket, replies
    /// included, on each event a waiter wakes for.
    fn matching_result<F>(&self, condition: &F) -> Option<(Ticket, serde_json::Value)>
    where
        F: Fn(&Ticket, &serde_json::Value) -> bool,
    {
        let store = self.tickets.lock().unwrap();
        let mut matching: Vec<&Ticket> = store
            .values()
            .filter(|t| t.is_finished() && t.result.is_some())
            .collect();
        matching.sort_by_key(|t| (t.created_at, numeric_id(&t.key)));
        let found = matching
            .into_iter()
            .find(|t| condition(t, t.result.as_ref().expect("filtered to Some")))?;
        let result = found.result.clone().expect("filtered to Some");
        Some((found.clone(), result))
    }

    /// Get the first finished result that matches, and execution carries on.
    ///
    /// The value handed over is the stored, schema-validated result, as with
    /// [`Self::on_result`]. A ticket that finished before the call counts. To
    /// stop the run on a match instead, use [`Self::cancel_on_result`].
    ///
    /// Your condition MUST NOT call another `TicketQueue` method that reads the
    /// ticket store, or the call deadlocks.
    pub async fn finish_on_result<F>(&self, condition: F) -> Option<(Ticket, serde_json::Value)>
    where
        F: Fn(&Ticket, &serde_json::Value) -> bool,
    {
        let mut stream = self.event_stream.subscribe();
        loop {
            if let Some(found) = self.matching_result(&condition) {
                return Some(found);
            }
            if !self.next_event(&mut stream).await {
                return self.matching_result(&condition);
            }
        }
    }

    /// Get the first failure that matches, and execution carries on.
    ///
    /// Covers every failure [`Self::on_failure`] reports, so match on
    /// `event.kind` to tell one that ends the ticket from one the agent works
    /// around. To stop the run on a match instead, use
    /// [`Self::cancel_on_failure`].
    pub async fn finish_on_failure<F>(&self, condition: F) -> Option<(Event, Ticket)>
    where
        F: Fn(&Event, &Ticket) -> bool,
    {
        self.finish_on(|event| {
            if !event.kind.is_failure() {
                return None;
            }
            let ticket = self.get_ticket(&event.ticket_key)?;
            condition(event, &ticket).then(|| (event.clone(), ticket))
        })
        .await
    }

    /// Get the first ticket that matches, and execution carries on.
    ///
    /// A ticket already matching when you call counts, and the condition is
    /// retried on every event, so it sees states no lifecycle transition
    /// announces, such as an agent waiting on the next reply. Your condition
    /// MUST NOT call another `TicketQueue` method that reads the ticket store,
    /// or the call deadlocks.
    pub async fn finish_on_ticket<F>(&self, condition: F) -> Option<Ticket>
    where
        F: Fn(&Ticket) -> bool,
    {
        let mut stream = self.event_stream.subscribe();
        loop {
            if let Some(ticket) = self.find_ticket(&condition) {
                return Some(ticket);
            }
            if !self.next_event(&mut stream).await {
                return self.find_ticket(&condition);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;
    use crate::event::{EventName, ToolFailureKind};

    #[test]
    fn ticket_queue_handle_is_shared_between_caller_and_added_agent() {
        let (queue, _tmp) = test_queue();
        let alice = queue.agent(minimal_agent("alice"));
        // Alice's task lands in the same queue.
        alice.task("from alice");
        queue.task("from queue");
        let all_keys: Vec<String> = queue
            .find_tickets(|t| t.status == Status::Todo)
            .iter()
            .map(|t| t.key.clone())
            .collect();
        assert_eq!(all_keys.len(), 2);
    }

    #[test]
    fn repeated_task_calls_route_to_shared_queue_after_rebind() {
        let (queue, _tmp) = test_queue();
        let alice = minimal_agent("alice").ticket_queue(&queue);
        alice.task("first");
        alice.task("second");
        assert_eq!(queue.find_tickets(|t| t.status == Status::Todo).len(), 2);
    }

    #[test]
    fn tickets_returns_all_in_creation_order() {
        let (queue, _tmp) = test_queue();
        queue.task("a");
        queue.task("b");
        queue.task("c");
        let all = queue.tickets();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].key, "TICKET-1");
        assert_eq!(all[1].key, "TICKET-2");
        assert_eq!(all[2].key, "TICKET-3");
    }

    #[test]
    fn results_return_done_payloads_in_creation_order() {
        let (queue, _tmp) = test_queue();
        queue.task("a");
        queue.task("b");
        queue.task("c");
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
        queue.task("a");
        queue.task("b");
        queue.task("c");
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
        queue.task("pending");
        assert!(queue.results().is_empty());
    }

    #[test]
    fn pending_count_counts_todo() {
        let (queue, _tmp) = test_queue();
        queue.task("a");
        queue.task("b");
        assert_eq!(queue.pending_count(), 2);
    }

    #[test]
    fn pending_count_counts_inprogress_waiting_for_response() {
        let (queue, _tmp) = test_queue();
        queue.task("x");
        queue.claim(|t| t.status == Status::Todo, "agent").unwrap();
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn pending_count_counts_inprogress_with_text_only_last_reply() {
        let (queue, _tmp) = test_queue();
        queue.task("x");
        let key = queue.claim(|t| t.status == Status::Todo, "agent").unwrap();
        queue.add_reply(
            &key,
            Reply::assistant(&[crate::providers::ContentBlock::Text {
                text: "hello".into(),
            }]),
        );
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn pending_count_counts_inprogress_with_empty_content_last_reply() {
        let (queue, _tmp) = test_queue();
        queue.task("x");
        let key = queue.claim(|t| t.status == Status::Todo, "agent").unwrap();
        queue.add_reply(&key, Reply::assistant(&[]));
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn pending_count_excludes_finished_and_failed() {
        let (queue, _tmp) = test_queue();
        queue.task("a");
        queue.task("b");
        let key_a = queue.claim(|t| t.key == "TICKET-1", "agent").unwrap();
        let key_b = queue.claim(|t| t.key == "TICKET-2", "agent").unwrap();
        queue.set_finished_by(&key_a, "agent").unwrap();
        queue.set_failed(&key_b).unwrap();
        assert_eq!(queue.pending_count(), 0);
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
    fn get_dir_reads_back_the_configured_directory() {
        let (queue, tmp) = test_queue();
        assert_eq!(queue.get_dir(), tmp.path());
    }

    #[test]
    fn cancel_label_flags_only_that_label() {
        let (queue, _tmp) = test_queue();
        queue.cancel_label("research");

        assert!(queue.is_any_label_cancelled(&["research".into()]));
        // A ticket carries its agent name too once claimed; the pool label still hits.
        assert!(queue.is_any_label_cancelled(&["research".into(), "Threat Researcher 1".into()]));
        assert!(
            !queue.is_any_label_cancelled(&["analysis".into()]),
            "other pools are untouched",
        );
        assert!(!queue.is_any_label_cancelled(&[]));
    }

    #[test]
    fn label_cancelled_reads_back_the_pool_cancel_label_called_off() {
        let (queue, _tmp) = test_queue();
        assert!(!queue.is_label_cancelled("research"));
        queue.cancel_label("research");
        assert!(queue.is_label_cancelled("research"));
        assert!(
            !queue.is_label_cancelled("analysis"),
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
    fn results_for_label_returns_only_matching_label() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("analysis"));
        queue.ticket(Ticket::new("b").label("other"));
        let key_a = queue
            .claim(|t| t.task == serde_json::json!("a"), "agent")
            .unwrap();
        let key_b = queue
            .claim(|t| t.task == serde_json::json!("b"), "agent")
            .unwrap();
        queue
            .set_result(&key_a, serde_json::json!({"score": 7}))
            .unwrap();
        queue.set_finished_by(&key_a, "agent").unwrap();
        queue
            .set_result(&key_b, serde_json::json!({"score": 99}))
            .unwrap();
        queue.set_finished_by(&key_b, "agent").unwrap();
        assert_eq!(
            queue.results_for_label("analysis"),
            vec![serde_json::json!({"score": 7})]
        );
    }

    #[test]
    fn results_for_agent_returns_only_the_claiming_agents_results() {
        let (queue, _tmp) = test_queue();
        queue.task("a");
        queue.task("b");
        let key_a = queue
            .claim(|t| t.task == serde_json::json!("a"), "scout")
            .unwrap();
        let key_b = queue
            .claim(|t| t.task == serde_json::json!("b"), "writer")
            .unwrap();
        attach_done_result(&queue, &key_a, "from scout");
        attach_done_result(&queue, &key_b, "from writer");
        assert_eq!(
            queue.results_for_agent("scout"),
            vec![serde_json::json!("from scout")]
        );
    }

    #[test]
    fn result_for_ticket_is_none_until_the_ticket_finishes() {
        let (queue, _tmp) = test_queue();
        let key = queue.task("work");
        queue.set_result(&key, serde_json::json!("early")).unwrap();
        assert_eq!(queue.result_for_ticket(&key), None);
        queue.set_finished_by(&key, "agent").unwrap();
        assert_eq!(
            queue.result_for_ticket(&key),
            Some(serde_json::json!("early"))
        );
        assert_eq!(queue.result_for_ticket("TICKET-404"), None);
    }

    #[test]
    fn tickets_for_agent_ignores_a_ticket_only_labelled_with_the_name() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("targeted").label("scout"));
        queue.task("claimed");
        queue
            .claim(|t| t.task == serde_json::json!("claimed"), "scout")
            .unwrap();
        // The label makes a ticket eligible; only a claim assigns it.
        assert_eq!(queue.tickets_for_label("scout").len(), 2);
        let worked = queue.tickets_for_agent("scout");
        assert_eq!(worked.len(), 1);
        assert_eq!(worked[0].task, serde_json::json!("claimed"));
    }

    #[test]
    fn tickets_for_label_returns_every_status_not_just_finished() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("done").label("analysis"));
        queue.ticket(Ticket::new("todo").label("analysis"));
        let key = queue
            .claim(|t| t.task == serde_json::json!("done"), "agent")
            .unwrap();
        attach_done_result(&queue, &key, "answer");

        let tasks: Vec<serde_json::Value> = queue
            .tickets_for_label("analysis")
            .into_iter()
            .map(|t| t.task)
            .collect();
        assert_eq!(
            tasks,
            vec![serde_json::json!("done"), serde_json::json!("todo")]
        );
        assert_eq!(queue.results_for_label("analysis").len(), 1);
    }

    #[test]
    fn stats_for_agent_counts_only_the_tickets_that_agent_worked() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("a").label("scan"));
        queue.ticket(Ticket::new("b").label("scan"));
        let key_a = queue
            .claim(|t| t.task == serde_json::json!("a"), "scout")
            .unwrap();
        let key_b = queue
            .claim(|t| t.task == serde_json::json!("b"), "writer")
            .unwrap();
        queue.set_result(&key_a, serde_json::json!("done")).unwrap();
        queue.set_finished_by(&key_a, "scout").unwrap();
        queue.set_failed_by(&key_b, "writer").unwrap();

        let scout = queue.stats().stats_for_agent("scout");
        assert_eq!(scout.event_count(EventName::TicketFinished), 1);
        assert_eq!(scout.event_count(EventName::TicketFailed), 0);
        let writer = queue.stats().stats_for_agent("writer");
        assert_eq!(writer.event_count(EventName::TicketFinished), 0);
        assert_eq!(writer.event_count(EventName::TicketFailed), 1);
    }

    #[test]
    fn stats_for_agent_counts_the_tickets_that_agent_reported() {
        let (queue, _tmp) = test_queue();
        queue.insert(Ticket::new("filed by scout"), "scout".into());
        queue.task("filed by the host");

        assert_eq!(
            queue
                .stats()
                .stats_for_agent("scout")
                .event_count(EventName::TicketCreated),
            1
        );
        assert_eq!(queue.stats().event_count(EventName::TicketCreated), 2);
    }

    #[test]
    fn cancel_label_on_result_calls_off_one_pool_without_cancelling_the_run() {
        let (queue, _tmp) = test_queue();
        queue.cancel_label_on_result("scan", |_, result| result.as_str() == Some("stop"));
        queue.ticket(Ticket::new("a").label("scan"));
        let key = queue.claim(|t| t.has_label("scan"), "agent").unwrap();
        attach_done_result(&queue, &key, "stop");

        assert!(queue.is_label_cancelled("scan"));
        assert!(!queue.is_cancelled(), "the other pools keep working");
    }

    #[test]
    fn results_for_label_empty_when_no_label_match() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("x").label("other"));
        let key = queue.claim(|t| t.has_label("other"), "agent").unwrap();
        queue.set_result(&key, serde_json::json!({"n": 1})).unwrap();
        queue.set_finished_by(&key, "agent").unwrap();
        assert!(queue.results_for_label("missing").is_empty());
    }

    #[tokio::test]
    async fn finish_on_ticket_resolves_when_ticket_matches() {
        let (queue, _tmp) = test_queue();
        let key = queue.task("work");
        let writer = Arc::clone(&queue);
        let claimed = key.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            attach_done_result(&writer, &claimed, "done");
        });
        let found = queue.finish_on_ticket(|t| t.is_finished()).await;
        assert_eq!(found.map(|t| t.key), Some(key));
    }

    #[tokio::test]
    async fn finish_on_ticket_none_after_cancel() {
        let (queue, _tmp) = test_queue();
        queue.task("never matches");
        queue.cancel();
        let found = queue.finish_on_ticket(|t| t.is_finished()).await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn finish_on_ticket_resolves_without_an_event_when_already_matching() {
        let (queue, _tmp) = test_queue();
        let key = queue.task("work");
        attach_done_result(&queue, &key, "done");
        // Nothing emits from here on, so only the check before the wait can
        // resolve this.
        let found = queue.finish_on_ticket(|t| t.is_finished()).await;
        assert_eq!(found.map(|t| t.key), Some(key));
    }

    #[tokio::test]
    async fn finish_on_ticket_leaves_execution_running() {
        let (queue, _tmp) = test_queue();
        let reasons = collect_finish_reasons(&queue);
        let key = queue.task("work");
        attach_done_result(&queue, &key, "done");
        queue.finish_on_ticket(|t| t.is_finished()).await;
        assert!(!queue.is_cancelled());
        assert!(
            reasons.lock().unwrap().is_empty(),
            "the run must not have finished",
        );
    }

    #[tokio::test]
    async fn finish_on_event_resolves_when_an_event_matches() {
        let (queue, _tmp) = test_queue();
        let writer = Arc::clone(&queue);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let key = writer.task("work");
            attach_done_result(&writer, &key, "done");
        });
        let found = queue
            .finish_on_event(|e| matches!(e.kind, EventKind::TicketFinished))
            .await;
        assert!(found.is_some());
    }

    #[tokio::test]
    async fn finish_on_event_none_after_cancel() {
        let (queue, _tmp) = test_queue();
        queue.cancel();
        let found = queue
            .finish_on_event(|e| matches!(e.kind, EventKind::TicketFinished))
            .await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn finish_on_result_hands_back_the_ticket_and_its_result() {
        let (queue, _tmp) = test_queue();
        let key = queue.task("work");
        attach_done_result(&queue, &key, "done");
        let found = queue.finish_on_result(|_, r| r == "done").await;
        let (ticket, result) = found.expect("the finished result matches");
        assert_eq!(ticket.key, key);
        assert_eq!(result, serde_json::json!("done"));
    }

    #[tokio::test]
    async fn finish_on_result_none_after_cancel() {
        let (queue, _tmp) = test_queue();
        queue.task("never finishes");
        queue.cancel();
        assert!(queue.finish_on_result(|_, _| true).await.is_none());
    }

    #[tokio::test]
    async fn finish_on_failure_hands_back_the_event_and_its_ticket() {
        let (queue, _tmp) = test_queue();
        let key = queue.task("work");
        let writer = Arc::clone(&queue);
        let failed = key.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            writer.set_failed(&failed).unwrap();
        });
        let found = queue.finish_on_failure(|_, _| true).await;
        let (event, ticket) = found.expect("the failure matches");
        assert!(event.kind.is_failure());
        assert_eq!(ticket.key, key);
    }

    #[tokio::test]
    async fn finish_on_failure_sees_a_match_that_follows_a_rejected_failure() {
        let (queue, _tmp) = test_queue();
        let rejected = queue.task("first");
        let wanted = queue.task("second");
        let writer = Arc::clone(&queue);
        let expected = wanted.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            // Emitted back to back: a waiter that re-subscribed after the
            // rejected one would start past this and never see it.
            writer.set_failed(&rejected).unwrap();
            writer.set_failed(&expected).unwrap();
        });
        let target = wanted.clone();
        let found = tokio::time::timeout(
            Duration::from_secs(5),
            queue.finish_on_failure(move |_, ticket| ticket.key == target),
        )
        .await
        .expect("finish_on_failure did not resolve within 5s");
        assert_eq!(found.map(|(_, ticket)| ticket.key), Some(wanted));
    }

    #[tokio::test]
    async fn finish_on_failure_none_after_cancel() {
        let (queue, _tmp) = test_queue();
        queue.task("never fails");
        queue.cancel();
        assert!(queue.finish_on_failure(|_, _| true).await.is_none());
    }

    #[test]
    fn cancel_on_event_trips_signal_when_predicate_matches() {
        let (queue, _tmp) = test_queue();
        assert!(!queue.is_cancelled());
        queue.cancel_on_event(|e| matches!(e.kind, EventKind::TicketFailed));
        queue.emit("KEY", "agent", EventKind::TurnStarted);
        assert!(!queue.is_cancelled());
        queue.emit("KEY", "agent", EventKind::TicketFailed);
        assert!(queue.is_cancelled());
    }

    #[test]
    fn message_editor_receives_buffered_events_excluding_text_chunks() {
        use crate::event::ToolFailureKind;
        let (queue, _tmp) = test_queue();
        let key = queue.task("go");

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
        let key = queue.task("go");

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
        let key = queue.task("go");
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

    /// One editor at a time, like the reply editors above.
    #[test]
    fn a_second_directive_editor_replaces_the_first() {
        let (queue, _tmp) = test_queue();
        queue.edit_directive_on_retry(|_event, directive| *directive = "first".into());
        queue.edit_directive_on_retry(|_event, directive| *directive = "second".into());

        let retried = queue.emit(
            "KEY",
            "agent",
            EventKind::SchemaRetried {
                attempt: 1,
                max_attempts: 3,
                message: "no tool call".into(),
            },
        );
        let mut directive = String::from("built-in");
        queue.edit_directive(&retried, &mut directive);

        assert_eq!(directive, "second");
    }

    #[test]
    fn message_editor_sees_only_the_events_of_the_ticket_it_edits() {
        let (queue, _tmp) = test_queue();
        let a = queue.task("a");
        let b = queue.task("b");

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
        let key = queue.task("go");
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
    fn cancel_on_event_coexists_with_user_handler() {
        use std::sync::atomic::AtomicU32;
        let (queue, _tmp) = test_queue();
        let count = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&count);
        queue.on_event(move |_| {
            c.fetch_add(1, Ordering::Relaxed);
        });
        queue.cancel_on_event(|e| matches!(e.kind, EventKind::TurnStarted));
        queue.emit("KEY", "agent", EventKind::TurnStarted);
        assert_eq!(count.load(Ordering::Relaxed), 1, "user handler should fire");
        assert!(queue.is_cancelled(), "predicate should trip cancel");
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
        let key = queue.task("work");

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
    fn cancel_on_failure_trips_signal_when_predicate_matches() {
        let (queue, _tmp) = test_queue();
        queue.cancel_on_failure(|event, _| matches!(event.kind, EventKind::TicketFailed));
        let key = queue.task("work");
        assert!(!queue.is_cancelled());

        queue.set_failed(&key).unwrap();

        assert!(queue.is_cancelled());
    }

    #[test]
    fn cancel_label_on_failure_calls_off_one_pool_without_cancelling_the_run() {
        let (queue, _tmp) = test_queue();
        queue.cancel_label_on_failure("scan", |_, _| true);
        queue.ticket(Ticket::new("a").label("scan"));
        let key = queue.claim(|t| t.has_label("scan"), "agent").unwrap();

        queue.set_failed_by(&key, "agent").unwrap();

        assert!(queue.is_label_cancelled("scan"));
        assert!(!queue.is_cancelled(), "the other pools keep working");
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
        let key = queue.task("work");

        queue.set_failed(&key).unwrap();

        let retry = queue
            .find_ticket(|t| t.parent.as_deref() == Some(&key))
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
        let key = queue.task("work");

        queue.emit(&key, "agent", EventKind::TurnStarted);

        assert_eq!(queue.find_tickets(|t| t.has_label("report")).len(), 1);
    }

    #[test]
    fn cancel_on_result_trips_when_finished_result_matches() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("x").label("L"));
        let key = queue.claim(|t| t.has_label("L"), "agent").unwrap();
        queue
            .set_result(&key, serde_json::json!({"status": "malicious"}))
            .unwrap();
        queue
            .cancel_on_result(|_, r| r.get("status").and_then(|v| v.as_str()) == Some("malicious"));
        assert!(!queue.is_cancelled());
        queue.emit(&key, "agent", EventKind::TicketFinished);
        assert!(queue.is_cancelled());
    }

    #[test]
    fn cancel_on_result_ignores_nonmatching_result() {
        let (queue, _tmp) = test_queue();
        queue.ticket(Ticket::new("x").label("L"));
        let key = queue.claim(|t| t.has_label("L"), "agent").unwrap();
        queue
            .set_result(&key, serde_json::json!({"status": "benign"}))
            .unwrap();
        queue
            .cancel_on_result(|_, r| r.get("status").and_then(|v| v.as_str()) == Some("malicious"));
        queue.emit(&key, "agent", EventKind::TicketFinished);
        assert!(!queue.is_cancelled());
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
        assert_eq!(queue.find_tickets(|t| t.has_label("sniper")).len(), 1);
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
        let spawned = queue.find_ticket(|t| t.has_label("sniper")).unwrap();
        assert_eq!(spawned.parent, Some(key));
    }

    #[test]
    fn create_ticket_on_result_ignores_unfinished_events() {
        let (queue, _tmp) = test_queue();
        queue.create_ticket_on_result(|_, _| Some(Ticket::new("follow-up").label("next")));
        queue.emit("KEY", "agent", EventKind::TurnStarted);
        assert!(queue.tickets().is_empty());
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
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn on_ticket_hands_the_handler_the_finished_ticket() {
        let (queue, _tmp) = test_queue();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        queue.on_ticket(move |event, ticket| {
            if matches!(event.kind, EventKind::TicketFinished) {
                record.lock().unwrap().push((
                    event.agent_name.clone(),
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
        queue.on_ticket(move |_, ticket| record.lock().unwrap().push(ticket.key.clone()));
        queue.ticket(Ticket::new("scan").label("scan"));
        let key = queue.claim(|t| t.has_label("scan"), "analyst").unwrap();
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
        let key = queue.claim(|t| t.has_label("scan"), "analyst").unwrap();
        queue.emit(&key, "analyst", EventKind::TicketStarted);

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
        queue.finish().await;
        assert_eq!(*reasons.lock().unwrap(), vec![FinishReason::Drained]);
    }

    #[tokio::test]
    async fn is_cancelled_stays_false_after_clean_drain() {
        let (queue, _tmp) = test_queue();
        queue.finish().await;
        assert!(
            !queue.is_cancelled(),
            "clean drain must not flip the cancel signal",
        );
    }

    #[tokio::test]
    async fn run_finished_reports_cancelled_when_cancel_fires_during_run() {
        let (queue, _tmp) = test_queue();
        let reasons = collect_finish_reasons(&queue);
        queue.start();
        queue.cancel();
        queue.finish().await;
        assert_eq!(*reasons.lock().unwrap(), vec![FinishReason::Cancelled]);
        assert!(queue.is_cancelled());
    }

    #[tokio::test]
    async fn run_finished_reports_policy_violated_when_max_turns_zero() {
        let (queue, _tmp) = test_queue();
        let reasons = collect_finish_reasons(&queue);
        queue.max_turns(0);
        queue.finish().await;
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
        queue.finish().await;
        queue.start();
        queue.finish().await;
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
        queue.finish().await;
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2, "expected RunStarted then RunFinished");
        assert!(entries[0].starts_with("RunStarted"));
        assert!(entries[1].starts_with("RunFinished"));
    }
}
