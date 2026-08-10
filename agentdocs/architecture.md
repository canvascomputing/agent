# Architecture

The invariants that shape how code fits together. Layout says where code lives; this file says why the boundaries are where they are.

## Builder, Queue, Loop

**A run has three stages: build the `Agent`, bind it to a `TicketQueue`, drive the queue with `start` (long-lived) or `finish` (process a fixed batch and return).**

```rust
let agent = Agent::from_env().build();
tickets.agent(agent);
tickets.finish_all().await;
```

- The `Agent` builder carries identity, prompt parts, provider and model, tools, working directory, event handler, and a `Weak<TicketQueue>` (dangling by default).
- `TicketQueue::new` captures its own `Weak<Self>` through `Arc::new_cyclic`, so binding can hand every agent the back-reference it needs at run time.
- `TicketQueue::agent(a)` sets that `Weak<Self>` on the agent, drains any tickets the agent had queued in its private default queue into the shared one, and pushes a clone of the agent onto the queue's agents list.
- `TicketQueue::start` and `finish` spawn one tokio task per registered agent. Each task upgrades its `Weak` once at the start and reads the shared store, policies, stats, and ending from the resulting `Arc<TicketQueue>`.
- `tickets.task(value)` creates a new ticket and returns its key as `String`. `tickets.reply(&key, content)` appends a text reply to an existing ticket, and the loop's wait-for-input branch picks it up and drives the next turn on the same replies.
- Use `task` to start a conversation and `reply` to continue it. That is how multi-turn chat is built on top of one ticket.

## Shared Queue, Per-Agent Task

**Agents read shared state through one `Arc<TicketQueue>`. Locks are held only around queue and metric operations, never across `provider.respond().await`.**

- The ticket store, policies, stats, ending, cancel filters, and registered-agent list live on `TicketQueue`.
- The per-agent loop in `agents/loop.rs` claims one ticket, drives it through one or more provider and tool turns, and releases locks before each await.
- Multiple agents share one queue; a ticket is claimed exactly once.
- Nested queues are not supported: a single `TicketQueue` is the unit of orchestration.

## Assignment Is Labels, and a Name Is a Label

**Labels are the only assignment mechanism. An agent's own name counts as one of its labels, which is what makes direct assignment a special case of label scope rather than a second path.**

```rust
tickets.ticket(Ticket::new("Audit src/db.").label("scout_0"));  // direct
tickets.ticket(Ticket::new("Audit src/db.").label("scan"));     // by scope
```

- `Agent::handles_labels` answers three ways, in order: a ticket label equal to the agent's name matches; otherwise a labelled agent matches when its labels intersect the ticket's; otherwise an agent with no labels matches only tickets with no labels, which is the default scope.
- Direct assignment is therefore `Ticket::new(...).label(agent_name)`. The ticket is born `Status::Todo` like any other; nothing is born `InProgress`.
- `TicketQueue::claim` pushes the claiming agent's name onto the ticket's labels. That is what pins a resumed ticket: the `resumable` check in `loop/agent.rs` requires a label equal to the agent's name, so no other agent picks up work already started.
- The queue never auto-resolves a name against the registered-agent set. A label naming an agent that was never registered simply never matches.

## A Tool Call Resolves by Exact Name, Then by Folded Key

**`ToolRegistry::get` matches the name the model sent exactly. Failing that, it folds both sides onto a lookup key (lowercase, hyphens to underscores, one trailing `_tool` removed) and resolves to the tool that key matches. A key two registered tools share resolves to nothing.**

- The fold only removes information, so it cannot reach a tool the model did not name. That is what separates it from repairing a malformed argument, which agentwerk never does: the argument payload still reaches the tool untouched and is still rejected by the tool's own schema when wrong.
- Refusing on ambiguity is the load-bearing half. A host registering `grep_tool` beside the built-in `grep` keeps both reachable under their own names, and a third spelling resolves to neither rather than to an arbitrary winner.
- `get` is the only entry point: dispatch, the read-only batching decision in `partition_tool_calls`, and the `opened_paths` lookup all go through it, so they cannot disagree about which tool a call names.
- The loop rewrites each call to the registered name before emitting `ToolCallStarted`, so `Event` and `Stats` never split one tool across spellings. No event reports the fold; it is a lookup detail, not a state the run reached.
- A name that resolves to nothing returns `ToolNotFound`, whose model-visible message names every registered tool. Without that list the model has nothing to correct against, and each retry spends `max_schema_retries` budget until the ticket fails.
- `ToolChoice::Specific` is not folded: it travels outbound and is written by agentwerk or the host, never by the model.

## Finishing Is a Tool Call

**Agents finish tickets through one tool, `finish`. An optional `handover` argument additionally creates a child ticket; its presence is the only discriminator, so there is no second tool and no mode field.**

`finish` records the result through `TicketQueue::set_result`, which owns the result-validation-and-logging contract, then transitions the ticket to `Finished`. The loop enforces the rule: a turn that ends without a `finish` call is rejected and retried.

- Without `handover`, `finish` writes a `result`, attaches it to the current ticket, and transitions to `Finished` through the `write_result` helper. This is terminal work.
- With `handover`, it does the same and then inserts a child ticket pinned to that agent or label, with the current ticket recorded as its `parent`.
- The child is inserted BEFORE the parent finishes, so it is already `Todo` when the parent leaves the queue. A concurrent `work_left` check can never see an empty queue between them, and `finish` cannot end the chain early.
- `TicketFinished` and `TicketFailed` are emitted synchronously from the status transition itself, and a count of in-flight transitions keeps `work_left` true until every handler returns, so a `create_ticket_on_result` follow-up is inserted before `finish` can observe an empty queue.
- The alternative, a plain `finish` followed by `manage_tickets::create`, is order-sensitive the other way and leaves the current ticket re-claimed when the order is wrong.
- An agent that must always chain can no longer be forced to by its tool registry, since every `finish` accepts an optional `handover`. Its role prompt carries that requirement instead, which makes those prompt lines load-bearing.
- When a turn ends without a `finish` call and no result attached, the loop pushes a corrective directive and retries. This is the same retry path used for schema-validation failures, bounded by `max_schema_retries`; exhaustion emits `PolicyViolated { MaxSchemaRetries, .. }` and `TicketFailed`. Both paths emit `SchemaRetried` first and hand that event to `TicketQueue::edit_directive_on_retry`, so the hook, its budget, and the event a host reads all sit on the queue.
- `Status` transitions go through tickets-side helpers; the agent never writes status directly. `Failed` is reserved for system-driven outcomes: exhausted schema retries, exhausted missing-`finish` retries, and policy violations.

Schemas and results:

- `Ticket::schema(...)` attaches a `Schema` to the ticket; `finish` validates the result and the loop applies `max_schema_retries` on mismatch.
- `TicketQueue::schemas(&store)` binds a shared `SchemaStore` to the queue, which holds one schema per label. `SchemaStore::label(label, document)` parses the document itself, so registering a contract never builds a `Schema`; `Schema::new` stays public for `Ticket::schema`, which takes the compiled value. `claim` reads the store once and writes the first match onto `Ticket::schema`, so `Ticket::schema` stays the single source every reader already uses: the prompt directive, the finish tool's advertised arguments, result validation, and the retry directive. A ticket that already carries a schema is left alone.
- Resolution happens in `claim` rather than `insert` because only there is the label set final, with the claiming agent's name on it. The ticket's own label order decides, so a label it was filed under beats a registration under the agent's name. `claim` ends in `save_ticket`, so the bound schema is persisted and survives `load`. A resumed ticket never re-enters `claim` and keeps what its first claim bound.
- This is what gives a ticket nobody could build a contract: a handover child, or one the model filed through `manage_tickets`. No tool takes a schema document, since a small model does not write nested schemas reliably.
- A handover validates its `result` against the parent ticket's own schema, exactly as a plain finish does. It carries no schema for the child, which takes one from its handover label when it is claimed. A schema mismatch aborts before the child is inserted, so neither the parent's finish nor the child happens and the operation stays atomic.
- `handover` and `task` are reserved argument names for `finish`. A ticket whose schema is an object has its fields passed as `finish`'s top-level arguments, so such a schema must not declare a `handover` or `task` property: those names are stripped as control keys before the result is recovered.
- A successful finish appends one NDJSON record `{ticket, result}` to `<dir>/results.jsonl` (configured through `TicketQueue::dir(d)`, default `./.agentwerk`) and attaches the same `result` value to the ticket. The value is surfaced through `Ticket::result()`.
- The queue also appends one JSON line to `<dir>/tickets.jsonl` per lifecycle event (`created`, `started`, `done`, `failed`) and writes the full ticket state to `<dir>/tickets/<key>/ticket.json`. The `created` event carries the optional `parent` key when set, giving the log a complete handover audit trail. The log is observational: errors are swallowed. The result payload stays in `results.jsonl`; `tickets.jsonl` carries only the transition.

## Knowledge Is Opt-In and Shareable Across Agents

**An agent can carry durable facts across every ticket it handles through `Agent::knowledge(&store)`, including across separate `start` and `finish` calls and across process restarts. Off by default; each ticket starts without a knowledge section.**

Two layers of state exist. The per-ticket replies live on `Ticket::replies`: every message the loop sends to the provider is appended as a `Reply`, and the loop derives the request's `Vec<Message>` from those replies through `Ticket::to_messages` each turn. `Agent::knowledge(&store)` adds a separate cross-ticket layer: a `Knowledge` store rooted at a caller-supplied directory, surfaced to the model through `ManageKnowledgeTool` and rendered into the system prompt.

- The store is constructed through `Knowledge::load(store_dir)` and passed to one or more agents through `Agent::knowledge(&store)`. Two agents bound to the same `Arc<Knowledge>` share the same `index.md` and `pages/` directory; two agents bound to different stores see independent knowledge.
- Pointing `Knowledge::load` at the same directory as `TicketQueue::dir` co-locates the `knowledge/` bundle with `results.jsonl` and `tickets.jsonl`.
- The store is an Open Knowledge Format (OKF) v0.1 bundle held in `<dir>/knowledge/` (`BUNDLE_DIR`), which keeps it out of a co-located `TicketQueue`'s files and keeps the recursive page walk inside the bundle.
- `<dir>/knowledge/pages/<slug>.md` holds each concept with `type`, `description`, and `timestamp` frontmatter, and pages cross-link with standard markdown links (`[text](/pages/slug.md)`). `<dir>/knowledge/index.md` is a derived progressive-disclosure view with a clickable link per page.
- Only the compact index is injected into the system prompt; the agent reads full pages on demand through the `read` action. `index.md` is written but never parsed back: on load the in-memory index is rebuilt by walking the page frontmatter (`rebuild_index_from_pages`), so an OKF bundle placed in `<dir>/knowledge/` seeds the store from it.
- The loop reads `Knowledge::index()` once at the top of `process_ticket` and feeds the result to `Agent::system_prompt(knowledge: Option<&str>)`. The system prompt stays byte-stable across every turn of the ticket so the provider's prefix cache survives mid-ticket knowledge writes; cross-ticket and cross-agent writes become visible at the top of the next ticket.
- Knowledge is purely model-driven. The model calls `manage_knowledge` with `write`, `read`, `remove`, or `list`; the tool description carries the policy (durable facts only, do NOT save task progress or TODOs). A page's `type` and `tags` are host-side concerns set through the `Page` API, not tool parameters.
- A character limit on the rendered index caps how much of it the prompt lists, never what may be written. Past the limit `Knowledge::index()` lists the pages that fit and closes with a directive naming the absolute path to `index.md`, which the agent reads itself; `manage_knowledge`'s `list` still returns the whole index. It defaults to 12 000 and is configurable through `Knowledge::index_char_limit(n)` on the loaded store.

## Observer Chain, One Error Path

**`Event` reports state. `ProviderError` and `ToolError` report failed contracts. The two channels carry independent information.**

- State transitions exist only as `Event`: `TicketClaimed`, `TicketFinished`, `RequestStarted`, `RequestFinished`, `TextChunkReceived`.
- An observable failure fires both the typed error (`ProviderError`, `ToolError`) and a matching `Event` (`RequestFailed`, `ToolCallFailed`, `PolicyViolated`).
- A model-fixable failure (wrong arguments, schema mismatch, missing file) goes back to the model as a `ToolResult::Error` content block. It still fires `ToolCallFailed` but does not stop the run.
- Handlers MUST be cheap and non-blocking; the loop does not await them.

`TicketQueue::on_event(h)` pushes a handler onto an ordered chain. Every installed handler fires on every event, in installation order, and each is handed the same `&Event` rather than its own copy. When no handler is installed, `default_logger` runs in its place.

- That composition is what every hook is built on: `create_ticket_on_event(make)` pushes a handler that files a follow-up when `make` returns one, so a host's logger and the hook coexist.
- `on_result` filters to `TicketFinished` and unwraps the stored result, `on_failure` filters on `EventKind::is_failure`, and `on_ticket` filters to the three lifecycle kinds.
- Each of those three resolves `event.ticket_key` to a cloned `Ticket` first, which is why none of them fires on every kind: resolving on `TextChunkReceived` would copy a ticket's whole replies once per chunk.
- The `create_ticket_on_result` and `create_ticket_on_failure` hooks are in turn built on `on_result` and `on_failure`, so the resolve happens in one place.
- `EventKind::RunStarted` and `EventKind::RunFinished { reason }` ride the same chain. They are emitted by the `TicketQueue` itself and arrive with an empty `agent_name`, as does `TicketFailed` from a host-driven `set_failed`.

## New Observables Pick a Channel

**Each new signal goes on `Event`, on a typed error, or on both. Pick by what the signal describes.**

- Reached a state: `Event` only.
- Could not fulfil a contract: typed error in the matching domain.
- Both at once (terminal request failure, policy violation): define both. Share the payload type when observer-friendly (`PolicyKind`); introduce a stripped `Kind` enum when the error carries observer-hostile detail (`RequestErrorKind`, `ToolFailureKind`).
- Model-fixable failure: `ToolResult::Error(String)`; still fires `ToolCallFailed` but is recoverable.
- A public error enum carries `#[non_exhaustive]`, so a later variant is not a breaking change for callers that match on it. `ProviderError` and `ToolError` both do. The attribute covers new variants only: adding a field to an existing struct variant still breaks a caller that matches it without `..`, so prefer a new variant to widening an old one.

## Providers Own Their Client

**Each concrete provider owns a `reqwest::Client` directly. There is no transport abstraction.**

- The `ProviderLike` trait fulfils one contract: `respond` (drive one turn) plus per-vendor metadata. Callers hold it as a `Provider`, a cloneable handle any implementer converts into.
- `ModelRequest`, `Message`, `ContentBlock`, and `TokenUsage` are the request and response types every provider converts to and from.
- Those types, plus `ModelResponse`, `StreamEvent`, `ResponseStatus`, `ToolChoice`, and `ProviderToolDefinition`, are `pub` and documented: the `ProviderLike` trait is a supported extension point, and its implementors name them.
- HTTP error mapping is shared through `providers::map_http_errors` plus a provider-specific `classify_error`; SSE parsing lives in `providers::stream`.
- Retry happens at the request level using `Policies::max_request_retries` and `request_retry_delay`; vendor code does not retry.

## The Lifecycle Is Three Verbs Over One Filter

**`start` starts, `finish(matches)` waits, `cancel(matches)` stops. Both filters are `Fn(&Ticket) -> bool`, so waiting for one pool or one ticket is the same call with a different filter, and `finish_all()` and `cancel_all()` pass the filter that names every ticket.**

- `TicketQueue::work_left(matches)` is the one definition of "not done yet", and both the main loop and `finish` ask it. A ticket has work left while it is pending, uncancelled, and not paused for a caller reply.
- `TicketQueue::cancel_filters` holds what `cancel` took off the queue. The claim and resume path reads it through `is_cancelled(ticket)`, so a cancelled ticket is neither claimed nor resumed and an agent already holding one is taken off it. The ticket stays `InProgress`.
- A filter runs while the ticket store lock is held, so it MUST NOT call back into the queue: the same rule `find_ticket` and `find_tickets` carry.

## The Run Names Its Own Ending

**`run_main_loop` decides when a run is over and announces it once, rather than whichever caller happens to await. A limit breached while the host is busy elsewhere still ends the run.**

- `TicketQueue::run` is one `Arc<Run>` over a `watch` channel of three phases: `Working`, `Draining(reason)` once the reason is known and the agents are stopping, and `Finished(reason)` once they have stopped and `RunFinished` has been announced. The channel is both the value and the wake, and its sender takes `&self`, so the cell needs neither a lock nor a flag, and a run complete without a reason cannot be written.
- `TicketQueue::ending_reason()` names a `FinishReason::PolicyViolated(kind)` for a breached limit and `FinishReason::Cancelled` once a cancel leaves nothing claimable. An empty queue is not an ending: a host that called `start()` may still be filing work, and a paused ticket revives on the next reply.
- `FinishReason::Drained` is named by the `finish` that waited for it, and only when no ticket at all is still open. That is what keeps an interactive chat alive between turns.
- The main loop joins its agents and emits `EventKind::RunFinished { reason }` before `Run::set_finished`, so a caller that starts another run never overlaps the previous one.
- Tools observe the ending through `ToolContext::cancelled`; pair it with `tokio::select!` so it drops the losing branch promptly.
- Dropping the `TicketQueue` while agents still reference it through `Weak` is the public way to abort: the upgrade fails and each task panics out cleanly.

## Stats Are Event-Derived, One Writer

**`Stats::record_event` is the single writer for event-derived statistics: every `EventKind` is counted automatically by its name; only the ticket lifecycle writes directly.**

- `TicketQueue::emit` forwards every event to `Stats::record_event(kind, key, labels)` before firing observers. The event's `EventKind::event_name()` keys a per-kind count map, so a new variant is counted the moment it names itself in that exhaustive match, with no statistics code to add.
- Every kind is read the same way: `event_count(EventName::TurnStarted)` for one, `event_counts()` for the whole map. No kind has an accessor of its own, and `stats.json` carries the counts under `events` alone, never repeated at the top. `EventName` is the payload-free half of `EventKind`; the snake_case spelling lives in `EventName::name()`, and serde's `rename_all` reproduces it for `stats.json`.
- Payload-bearing measures are declared by the event, not by `Stats`: `EventKind::measures()` sits next to `name()` in `event.rs` and returns the counters the event adds, each naming a `Subject` (the `stats.json` category plus what keys it) and the counter within it. `record_event` walks that list; there is no per-kind arm to write. The per-ticket token usage from `RequestFinished` is the one exception, since it stays run-wide.
- One table in `stats.rs` names the four categories (`tools`, `files`, `models`, `knowledge`), the counters each stores, and which of those count attempts and which count failures. It drives both `Serialize` and `load`, so `errors` and `error_rate` are derived once for every category, written for readers, and skipped when reading back. `input_tokens()` and `output_tokens()` sum the `models` category rather than holding their own totals.
- No category carries a bare `failed`. Every failing event names a reason, and the category counts one column per reason: `ToolFailureKind` for `tools` and for `files`, since a path fails with the call that named it; `RequestErrorKind` for `models`; `KnowledgeFailureKind` for `knowledge`. The reason's `as_str()` is the column name, so the string an observer reads off `Event.data["reason"]` is the one `stats.json` counts under. The `*Stat` structs surface the split as `failures`, a map keyed by that category's reason enum, with a kind nothing failed under left out.
- The ticket lifecycle is counted like everything else, through `EventKind::TicketCreated`, `TicketStarted`, `TicketFinished`, and `TicketFailed`. What the store still writes directly is the timing: `record_started` and `record_durations`, since a transition carries a duration no event reports.
- Reads happen on `Stats` directly through inherent accessors such as `event_count(event)`, `input_tokens()`, `execution_duration()`, and `ticket_duration()`. A count is always an `event_count`; the rest are sums, spans, or scopes, and a ratio like the ticket success rate is the caller's division over two counts.
- `Stats::stats_for_label(label)` returns a nested `Stats` slice scoped to one label, and `stats_for_agent(agent_name)` does the same per agent. The slice names are the host's own labels and agent names, so no accessor lists them. Reading a slice does not create one: `record_scoped` and the store use the `pub(crate)` `slice_for_label` and `slice_for_agent` for that, so a misspelled lookup leaves nothing behind in `stats.json`.
- `record_event` mirrors every measure onto each slice the ticket carries, in one walk of the labels: the counts and the subject categories behind `tool_stats()`, `file_stats()`, `knowledge_stats()`, and `model_stats()`. An accessor therefore answers the same question whichever `Stats` holds it.
- `execution_duration()` is the one measure that stays run-wide: it is `None` on a slice, since the elapsed duration is global.
- The per-ticket token series (`usage_for_ticket`) is `pub(crate)`. Compaction clears it on every compaction, so a caller would read a silently truncated series; a host that wants the figures reads `EventKind::RequestFinished`, which reports every one as it happens.

## Persistence Routes Through Two Traits

**Every read and write in the crate goes through `Persist` (state files) or `Append` (jsonl logs) in `persistence`. No domain module hand-rolls file IO; no module knows its file's name except the implementer.**

- `Persist` defines `save(&self, dir) -> io::Result<()>` and `load(dir, &Self::Key) -> io::Result<Self>`. `Stats`, `Ticket`, `Replies`, `Page`, and `Trajectory` implement it; each owns its own path layout (`stats.json`, `tickets/<key>/ticket.json`, `tickets/<key>/replies.jsonl`, `pages/<slug>.md`, `trajectories/<key>.json`).
- A value type the caller stores itself reaches its file through an inherent method that delegates to its own impl, never by publishing the trait: `Trajectory::save(dir)` and `Knowledge::pages().save(page)` are the two. Service bootstrap (`TicketQueue::load`, `Knowledge::load`) uses the same `load` verb for its directory-to-`Arc<Self>` entry by convention.
- `Append` defines `append(dir, &Self::Record) -> io::Result<()>`. `Results` writes `results.jsonl`; `TicketEvents` writes `tickets.jsonl`. The wrong type cannot reach the wrong file: each implementer's `append` body hardcodes the filename.
- The per-ticket replies are the one shape that does not fit either trait. `Replies` (in `agents::tickets`) is a free type with `append(dir, key, &Reply)` and `load(dir, key) -> Vec<Reply>`, writing one JSON line per `Reply` to `tickets/<key>/replies.jsonl`.
- `Replies` also implements `Persist`, whose `save` overwrites the file wholesale so a dropped or redacted reply leaves nothing behind. The `append` half is per-key, so the single-fixed-filename `Append` trait does not generalize cleanly; promote it to a trait only when a second per-key log appears.
- `TICKET-<N>` keys are handed out in order. `load()` seeds the next key from the tickets it just read off disk; a queue built with `new()` scans for the highest existing key at the first insert instead, since `new()` never reads the directory itself.
- One agent processes one ticket at a time (claim is atomic), so `add_reply` and the rewrite for one key are sequential within a single loop task. No per-key lock is needed for either path.
- Crate-internal helpers `write_atomic` (tmp plus rename) and `append_line` (`O_APPEND` plus newline) are the only places that touch the filesystem. They are `pub(crate)` so trait impls colocated with their types can call them; by convention nothing outside a `Persist` or `Append` impl reaches for them.
- One documented exception: `TicketQueue::write_tool_output` writes single-shot flat files that fit neither trait.
- Vocabulary is fixed: `save`, `load`, `append`. Bootstrap verbs other than `load` (such as `open`) are not used. Domain words (`checkpoint`, `snapshot`, `counter`, `persist`) do not appear in identifiers or test names.

## Policies Are Per-Queue, Checked at Turn Boundaries

**A run stops cleanly when any limit on `Policies` is breached. The check fires `EventKind::PolicyViolated` and exits the per-agent task.**

- The loop calls `policy_violated_kind` at each iteration; a non-`None` return takes the agent off the queue.
- Token budgets read from `Stats`; `max_time` reads from `Policies` and from `Stats::execution_duration()`. All limits, including `max_time`, route through `policy_violated_kind` and emit `PolicyViolated`; `get_finish_reason` reports the matching `FinishReason::PolicyViolated(kind)` once the run has ended.
- The schema-retry budget is applied per-ticket inside the result-writing path, not at the top of the loop.
- `compact_at` rides on `Policies` for the same per-queue snapshot every limit gets, but it is a trigger rather than a limit: `policy_violated_kind` ignores it, and reaching it costs a compaction, not the run.
