# Architecture

The invariants that shape how code fits together. Layout says where code lives; this file says why the seams are where they are.

## Builder, system, loop

**A run has three stages: build the `Agent`, bind it to a `TicketSystem`, drive the system with `start` (long-lived) or `finish` (process a fixed batch and return).**

- The `Agent` builder carries identity, prompt parts, provider/model, tools, working dir, event handler, and a `Weak<TicketSystem>` (dangling by default).
- `TicketSystem::add(agent)` (or `agent.ticket_system(&shared)`) sets the system's `Weak<Self>` on the agent, drains any tickets the agent had queued in its private default system into the shared one, and pushes a clone of the agent onto the system's agents list.
- `TicketSystem::start` / `finish` spawn one tokio task per registered agent; each task upgrades its `Weak` once at the start and reads the shared store, policies, stats, and stop signal from the resulting `Arc<TicketSystem>`.
- `tickets.task(value)` and `tickets.task_labeled(value, label)` create a new ticket and return its key as `String`. `tickets.reply(&key, content)` appends a user-side text reply to an existing ticket: the agent loop's wait-for-input branch picks the reply up and drives the next turn on the same transcript. Use `task` to start a conversation, `reply` to continue it; this is how multi-turn chat is built on top of one ticket.

## Shared system, per-agent task

**Agents read shared state through one `Arc<TicketSystem>`. Locks are held only around queue and metric operations, never across `provider.respond().await`.**

- The ticket store, policies, stats, stop and cancel signals, and registered-agent list live on `TicketSystem`.
- The per-agent loop in `agents/loop.rs` claims one ticket, drives it through one or more provider/tool turns, and releases locks before each await.
- Multiple agents share one queue; a ticket is claimed exactly once.
- Sub-systems are not nested: a single `TicketSystem` is the unit of orchestration.

## Path A and Path B assignment

**Tickets reach agents either by direct assignment (Path A) or by label scope (Path B).**

- A ticket built with `Ticket::new(...).assign_to(name)` is born `Status::InProgress` and pinned to the named agent; only that agent can pick it up.
- A ticket built with `.label(...)` (or via `task_labeled(value, label)`) is `Status::Todo` and picked up by any agent whose `label` scope intersects.
- An agent with empty labels handles only tickets with no labels; that is the "default scope".
- The system never auto-resolves a name against the registered-agent set: callers know which assignment path they want.

## Finishing is a tool call

**Agents finish tickets through one of two finisher tools: `finish_ticket` (terminal) or `handover_ticket` (terminal + spawn child). Both record the result through `TicketSystem::set_result`, which owns the result-validation-and-logging contract, then transition the ticket to `Finished` (`finish_ticket` via the `write_result` helper; `handover_ticket` directly, around the child insert). The loop enforces the rule: a turn that ends without a finisher tool call is rejected and retried.**

- `finish_ticket` writes a `result`, attaches it to the current ticket, and transitions to `Finished`. Use it for terminal work.
- `handover_ticket` does the same and then inserts a child ticket pinned to a target agent or label with the current ticket recorded as its `parent`. It inserts the child BEFORE finishing the parent, so the child is already `Todo` when the parent leaves the queue: a concurrent `pending_count()` poll can never see an empty queue between them and `finish()` cannot drain the chain early. Use it to finish-and-chain in one call; the alternative — `finish_ticket` followed by `manage_tickets_tool::create` — is order-sensitive the other way and leaves the current ticket re-picked when the order is wrong.
- When a turn ends without a finisher tool call (no `finish_ticket`, no `handover_ticket`, no result attached), the loop pushes a corrective directive and retries. This is the same retry path used for schema-validation failures, bounded by `max_schema_retries`; exhaustion emits `PolicyViolated { MaxSchemaRetries, .. }` and `TicketFailed`.
- `Status` transitions go through tickets-side helpers; the agent never writes status directly. `Failed` is reserved for system-driven outcomes (schema-retry trip, missing-finisher exhaustion, policy violations).
- `Ticket::schema(...)` attaches a `Schema` to the ticket; the finisher tool validates the result and the loop applies `max_schema_retries` on mismatch. A schema can also be registered as a per-label default via `TicketSystem::schema_for_label`, stamped onto a schemaless ticket at creation, so a result contract follows its label however the ticket was created (direct, `task_labeled`, or a `handover_ticket` child). `handover_ticket` accepts any JSON `result` and validates it against the parent ticket's own schema, like `finish_ticket`; it does not carry a schema for the child, which inherits one only through its label. A schema mismatch in `handover_ticket` aborts before the child is inserted, so neither the parent's finish nor the child happens: the operation stays atomic.
- A successful finish appends one NDJSON record `{ticket, result}` to `<dir>/results.jsonl` (configured via `TicketSystem::dir(d)`; defaults to `./.agentwerk`) and attaches the same `result` value to the ticket. The value is surfaced through `Ticket::result()`; `last_result()` returns its serialized form for the most recent `Finished` ticket.
- The system also appends one JSON line to `<dir>/tickets.jsonl` per lifecycle event (`created`, `started`, `done`, `failed`) and writes the full ticket state to `<dir>/tickets/<key>/ticket.<ts>.json`. The `created` event carries the optional `parent` key when set, giving the log a complete handover audit trail. The log is observational: errors are swallowed. The result payload stays in `results.jsonl`; `tickets.jsonl` carries only the transition.

## Knowledge is opt-in and shareable across agents

**An agent can carry durable facts across every ticket it handles via `Agent::knowledge(&store)`, including across separate `start` / `finish` calls and across process restarts. Off by default; each ticket starts without a knowledge section.**

Two layers of state exist. The per-ticket transcript lives on `Ticket::replies`: every message the loop sends to the provider is appended as a `Reply`, and the loop derives the request's `Vec<Message>` from those replies via `Ticket::to_messages` each turn. `Agent::knowledge(&store)` adds a separate cross-ticket layer: a `Knowledge` store rooted at a caller-supplied directory, surfaced to the model through `ManageKnowledgeTool` and rendered into the system prompt under `## Knowledge`.

- The store is constructed via `Knowledge::load(knowledge_dir)` and passed to one or more agents through `Agent::knowledge(&store)`. Two agents bound to the same `Arc<Knowledge>` share the same `index.md` and `pages/` directory; two agents bound to different stores see independent knowledge. The pattern mirrors `Agent::ticket_system(&Arc<TicketSystem>)`. Pointing `Knowledge::load` at the same directory as `TicketSystem::dir` co-locates knowledge pages with `results.jsonl` and `tickets.jsonl`.
- The store is an Open Knowledge Format (OKF) v0.1 bundle: `<dir>/pages/<slug>.md` holds each concept with `type` / `description` / `timestamp` frontmatter, and pages cross-link with standard markdown links (`[text](/pages/slug.md)`). `<dir>/index.md` is a derived progressive-disclosure view with a clickable link per page. Only the compact index is injected into the system prompt; the agent reads full pages on demand via the `read` action. `index.md` is written but never parsed back: on load the in-memory index is rebuilt by walking the page frontmatter (`rebuild_index_from_pages`), so pointing `Knowledge::load` at an existing OKF bundle seeds the store from it.
- The loop reads `Knowledge::index()` once at the top of `process_ticket` and feeds the result to `Agent::system_prompt(knowledge: Option<&str>)`. The system prompt stays byte-stable across every turn of the ticket so the provider's prefix cache survives mid-ticket knowledge writes; cross-ticket and cross-agent writes become visible at the top of the next ticket.
- Knowledge is purely model-driven. The model calls `manage_knowledge` with `write` / `read` / `remove` / `list`; the tool description carries the policy (durable facts only, do NOT save task progress / TODOs). The write action also accepts an optional `type` (default `Knowledge`). A hard char limit on the rendered index rejects writes that would push the prompt section past the cap and tells the model to consolidate first. The limit defaults to 4000 and is configurable via `Knowledge::load(dir)?.index_char_limit(n)`.

## Observer chain, one error path

**`Event` reports state. `ProviderError` and `ToolError` report failed contracts. The two channels carry independent information.**

- State transitions exist only as `Event` (`TicketClaimed`, `TicketFinished`, `RequestStarted`, `RequestFinished`, `TextChunkReceived`).
- An observable failure fires both the typed error (`ProviderError`, `ToolError`) and a matching `Event` (`RequestFailed`, `ToolCallFailed`, `PolicyViolated`).
- A model-fixable failure (wrong arguments, schema mismatch, missing file) goes back to the model as a `ToolResult::Error` content block; it still fires `ToolCallFailed` but does not stop the run.
- Handlers MUST be cheap, non-blocking closures; the loop does not await them.
- `TicketSystem::on_event(h)` pushes a handler onto an ordered chain — every installed handler fires on every event, in installation order. When no handler is installed, `default_logger` runs in its place. This composition is what `cancel_on_event(predicate)` is built on: it pushes a handler that calls `cancel()` when the predicate matches, so the user's logger and the cancel trigger coexist. `EventKind::RunStarted` and `EventKind::RunFinished { reason }` ride the same chain; they are emitted by the `TicketSystem` itself and arrive with an empty `agent_name`.

## New observables pick a channel

**Each new signal goes on `Event`, on a typed error, or on both. Pick by what the signal describes.**

- Reached a state: `Event` only.
- Could not fulfil a contract: typed error in the matching domain.
- Both at once (terminal request failure, policy trip): define both. Share the payload type when observer-friendly (`PolicyKind`); introduce a stripped `Kind` enum when the error carries observer-hostile detail (`RequestErrorKind`, `ToolFailureKind`).
- Model-fixable failure: `ToolResult::Error(String)`; still fires `ToolCallFailed` but is recoverable.

## Providers own their client

**Each concrete provider owns a `reqwest::Client` directly. There is no transport abstraction.**

- The `Provider` trait fulfils one contract: `respond` (drive one turn) plus per-vendor metadata.
- `ModelRequest`, `Message`, `ContentBlock`, and `TokenUsage` are the request and response types every provider converts to and from.
- Those types (plus `ModelResponse`, `StreamEvent`, `ResponseStatus`, `ToolChoice`, `ProviderToolDefinition`) are `pub` but `#[doc(hidden)]`: forced public by the `Provider` trait, hidden from rustdoc because only implementors name them.
- HTTP error mapping is shared through `providers::map_http_errors` plus a provider-specific `classify` closure; SSE parsing lives in `providers::stream`.
- Retry happens at the request level using `Policies::max_request_retries` and `request_retry_delay`; vendor code does not retry.

## Cancellation is cooperative, split into two signals

**Two `Arc<AtomicBool>` signals separate "stop the workers" from "external cancel was requested." Both flip on cancel; only the stop signal flips on policy or drain.**

- `TicketSystem::stop_signal` is what workers and tools poll. `finish()` flips it on cancel, on policy violation, and on clean drain so the worker loop, in-flight tools, and the join handle all wind down.
- `TicketSystem::cancel_signal` is flipped only by `cancel()`, `cancel_on(trigger)`, `cancel_on_event(predicate)`, and `cancel_on_result(predicate)`. `is_cancelled()` reads it; a clean drain leaves it untouched so observers can tell the three exit paths apart.
- `cancel()` flips both atomics in sync. `cancel_on*` route through `cancel()` so cancellation triggers compose with the rest of the run's lifecycle.
- Tools observe the stop signal through `ToolContext::interrupt_signal` and `wait_for_cancel`; pair with `tokio::select!` so cancel drops the losing branch promptly.
- Dropping the `TicketSystem` while agents still reference it via `Weak` is the public way to abort: the upgrade fails and each task panics out cleanly.
- `finish()` announces its exit reason as `FinishReason::Drained`, `FinishReason::PolicyViolated(kind)`, or `FinishReason::Cancelled`, in that precedence. The reason is stashed for `TicketSystem::finish_reason()` and emitted as `EventKind::RunFinished { reason }`.

## Stats are per-system, write-only-by-domain

**`Stats` is one struct of atomic counters; each domain interacts through its own write-only protocol.**

- `LoopStats` is what the per-agent loop sees: `record_turn`, `record_request`, `record_tool_call`, `record_error`.
- `TicketStats` is what the ticket lifecycle sees: `record_created`, `record_started`, `record_done`, `record_failed`.
- Reads happen on `Stats` directly through inherent accessors (`turns()`, `tickets_finished()`, `run_duration()`, `success_rate()`, ...), never through the recorder traits.
- Lock-free for increments; readers do one atomic load per call.
- `Stats::stats_for_label(label)` returns a nested `Stats` slice scoped to one label. The loop and ticket lifecycle bump each slice alongside the global counters; `run_duration()` is `None` on a slice (elapsed run duration stays global).
- `Stats::tool_stats()` returns per-tool call and failure counts keyed by tool name, broken down by failure kind. It is recorded global-only (no per-label slices, like `usage_history`): `emit()` consumes `ToolCallStarted` for the call count and `ToolCallFailed` for the kind-split failures. The aggregate `tool_calls`/`errors` counters are unaffected; `errors()` stays provider-only.

## Persistence routes through two traits

**Every read and write in the crate goes through `Persist` (state files) or `Append` (jsonl logs) in `persistence`. No domain module hand-rolls file IO; no module knows its file's name except the implementer.**

- `Persist` defines `save(&self, dir) -> io::Result<()>` and `load(dir, &Self::Key) -> io::Result<Self>`. `Stats`, `Ticket`, and `Page` implement it; each owns its own path layout (`stats.json`, `tickets/<key>/ticket.json`, `pages/<slug>.md`). Service bootstrap (`TicketSystem::load`, `Knowledge::load`) uses the same `load` verb for its dir-to-`Arc<Self>` entry by convention.
- `Append` defines `append(dir, &Self::Record) -> io::Result<()>`. `Results` writes `results.jsonl`; `TicketEvents` writes `tickets.jsonl`. The wrong type cannot reach the wrong file: each implementer's `append` body hardcodes the filename.
- The per-ticket transcript is the one shape that does not fit either trait. `Replies` (in `agents::tickets`) is a free type with `append(dir, key, &Reply)` and `load(dir, key) -> Vec<Reply>`. It writes one JSON line per `Reply` to `tickets/<key>/replies.jsonl`; `load` reconstructs the in-memory transcript by picking the newest `(ticket.<ts>.json, replies.<ts>.jsonl)` compaction-pair (paired check: `ticket.<ts>.json` is the commit marker) as the base and merging running-log entries with strictly greater `created_at` on top. The pattern is per-key, so the single-fixed-filename `Append` trait does not generalize cleanly; promote to a trait only when a second per-key transcript appears.
- One agent processes one ticket at a time (claim is atomic), so `add_reply` and the compaction-pair write for one key are sequential within a single loop task. No per-key lock is needed for either path.
- Crate-internal helpers `write_atomic` (tmp+rename) and `append_line` (`O_APPEND` + newline) are the only places that touch the filesystem. They are `pub(crate)` so trait impls colocated with their types can call them; by convention nothing outside a `Persist` or `Append` impl reaches for them. Two documented exceptions: `TicketSystem::write_tool_output` writes single-shot flat files that don't fit either trait; `TicketSystem::summarize` (called from `agents::compaction::run`) invokes `write_atomic` twice via its `save_compaction` helper (replies file first, then the header file as commit marker) because the pair is a two-file atomic-ish operation that no single in-memory value owns.
- Vocabulary is fixed: `save`, `load`, `append`. Bootstrap verbs other than `load` (e.g. `open`) are not used. Domain words (`checkpoint`, `snapshot`, `counter`, `persist`) do not appear in identifiers or test names.

## Policies are per-system, checked at turn boundaries

**A run stops cleanly when any limit on `Policies` trips. The check fires `EventKind::PolicyViolated` and exits the per-agent task.**

- The loop calls `policy_violated_kind` at each iteration; a non-`None` return walks the agent off the queue.
- Token budgets read from `Stats`; `max_time` reads from `Policies` and from `Stats::run_duration()`. All limits, including `max_time`, route through `policy_violated_kind` and emit `PolicyViolated`; `finish()` carries the matching `FinishReason::PolicyViolated(kind)` back to the caller.
- Schema-retry budget is applied per-ticket inside the result-writing path, not at the top of the loop.
