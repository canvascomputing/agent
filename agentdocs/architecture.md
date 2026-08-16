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
- `TicketQueue::start` and `finish` spawn one tokio task per registered agent. Each task upgrades its `Weak` once at the start and reads the shared store, policies, budget, and ending from the resulting `Arc<TicketQueue>`.
- `tickets.task(value)` creates a new ticket and returns its key as `String`. `tickets.reply(&key, content)` appends a text reply to an existing ticket, and the loop's wait-for-input branch picks it up and drives the next turn on the same replies.
- Use `task` to start a conversation and `reply` to continue it. That is how multi-turn chat is built on top of one ticket.

## Shared Queue, Per-Agent Task

**Agents read shared state through one `Arc<TicketQueue>`. Locks are held only around queue and metric operations, never across `provider.respond().await`.**

- The ticket store, policies, budget, ending, cancel filters, and registered-agent list live on `TicketQueue`.
- The per-agent loop in `agents/loop.rs` claims one ticket, drives it through one or more provider and tool turns, and releases locks before each await.
- Multiple agents share one queue; a ticket is claimed exactly once.
- Nested queues are not supported: a single `TicketQueue` is the unit of orchestration.

## Assignment Is a Label, Identity Is an Id

**One label per agent, one label per ticket: the label is the only assignment mechanism, and the id `build` derives from it is the only identity. Neither does the other's job.**

```rust
tickets.ticket(Ticket::new("Audit src/db.").label("scan")); // one scope
tickets.ticket(Ticket::new("Audit src/db."));               // the default scope
```

- `Agent::handles` is equality in both directions: an agent matches a ticket whose label equals its own, and an agent with no label matches only tickets with no label, which is the default scope.
- A session directory written before the label became singular stores `"labels": [..]`, which no longer deserializes. Those tickets reload with no label and fall to the default scope rather than to their pool. There is no shim: start a fresh session directory.
- Addressing one agent alone is therefore giving it a label no other agent serves. The ticket is born `Status::Todo` like any other; nothing is born `InProgress`.
- `AgentBuilder::build` assigns `<label>-<n>`, numbering per label from 1 through the counters in `agent.rs`; an agent with no label gets `agent-<n>`. `Agent::clone` keeps the id, since `bind_agent` holds a clone and the two must agree about which tickets are theirs.
- `TicketQueue::claim` writes the claiming agent's id to `Ticket::assignee` and leaves the label as the filer set it. That is what pins a resumed ticket: the `resumable` check in `loop/agent.rs` requires `assignee` to equal the agent's id, so agents sharing a label never take over each other's started work.
- Ids reproduce only as far as build order does. `TicketQueue::load` resumes an unfinished ticket by assignee, so a host that wants resumption builds the same agents, in the same order, after a restart.
- The queue never auto-resolves a label against the registered-agent set. A label no agent serves simply never matches.

## A Tool Call Resolves by Exact Name, Then by Folded Key

**`ToolRegistry::get` matches the name the model sent exactly. Failing that, it folds both sides onto a lookup key (lowercase, hyphens to underscores, one trailing `_tool` removed) and resolves to the tool that key matches. A key two registered tools share resolves to nothing.**

- The fold only removes information, so it cannot reach a tool the model did not name.
- `Schema::validate` is the only thing that rejects a call's arguments. It retypes what the schema names a type for, then checks what that produced, so the model reads back everything still wrong with the value the tool would have received: one report per turn rather than one problem per turn. Each rewrite is reported as `EventKind::ResponseRepaired` carrying `RepairKind::ValueMistyped`, its message led by the tool name, so a tool description that keeps causing one is discoverable.
- Every registered tool carries a schema, so no call reaches a tool unchecked. `ToolLike::input_schema` returns a compiled `Schema`, which makes a tool without one unrepresentable rather than something the registry has to cover for: a `.schema.json` compiles in `ToolFile::parse` and `Tool::schema` compiles where the document is written, each panicking on one the compiler refuses. A definition is a compile-time asset, so a broken one fails the build rather than a request.
- No tool checks its own arguments. A requirement that holds only for some values of a discriminator is stated with `allOf`/`if`/`then` in the tool's `.schema.json`, so the shape the model is shown, the shape it is held to, and the shape a rejection reads back are one document. `ToolResult::schema_error` stays for a host with its own validator; inside the crate its one caller forwards `SchemaViolations`.
- Refusing on ambiguity is the load-bearing half. A host registering `grep_tool` beside the built-in `grep` keeps both reachable under their own names, and a third spelling resolves to neither rather than to an arbitrary winner.
- `get` is the only entry point: dispatch, the concurrent batching decision in `partition_tool_calls`, and the `opened_paths` lookup all go through it, so they cannot disagree about which tool a call names.
- The loop rewrites each call to the registered name before emitting `ToolCallStarted`, so `Event` and `Stats` never split one tool across spellings. The fold itself is reported as `EventKind::ResponseRepaired` carrying `RepairKind::CallMalformed`, so a host folding the event chain sees a model that keeps misspelling one tool.
- A name that resolves to nothing fails as `ToolFailureKind::ToolNotFound`, with a message naming every registered tool. Without that list the model has nothing to correct against, and each retry spends `max_schema_retries` budget until the ticket fails. A call the model wrote as text rather than emitting takes the same path: it is promoted whatever it names, so one reason covers an unknown tool however the call arrived.
- `ToolChoice::Specific` is not folded: it travels outbound and is written by agentwerk or the host, never by the model.

## Finishing Is a Tool Call

**Agents finish tickets through one tool, `finish`. An optional `handover` argument additionally creates a child ticket; its presence is the only discriminator, so there is no second tool and no mode field.**

`finish` records the result through `TicketQueue::set_result`, which owns the result-validation-and-logging contract, then transitions the ticket to `Finished`. The loop enforces the rule: a turn that ends without a `finish` call is rejected and retried.

- Without `handover`, `finish` writes a `result`, attaches it to the current ticket, and transitions to `Finished` through the `write_result` helper. This is terminal work.
- With `handover`, it does the same and then inserts a child ticket pinned to that agent or label, with the current ticket recorded as its `parent`. The child's body is the result or the caller's `task` (with `{parent_key}`, `{parent_result_path}`, and `{parent_result}` substituted, the result last so nothing it carries is expanded again), and always ends with the parent key and its result file, so the receiving agent can read the whole result rather than only what the body carries.
- The child is inserted BEFORE the parent finishes, so it is already `Todo` when the parent leaves the queue. A concurrent `work_left` check can never see an empty queue between them, and `finish` cannot end the chain early.
- `TicketFinished` and `TicketFailed` are emitted synchronously from the status transition itself, and a count of in-flight transitions keeps `work_left` true until every handler returns, so a `create_ticket_on_result` follow-up is inserted before `finish` can observe an empty queue.
- The alternative, a plain `finish` followed by `tickets::create`, is order-sensitive the other way and leaves the current ticket re-claimed when the order is wrong.
- An agent that must always chain can no longer be forced to by its tool registry, since every `finish` accepts an optional `handover`. Its role prompt carries that requirement instead, which makes those prompt lines load-bearing.
- When a turn ends without a `finish` call and no result attached, the loop pushes a corrective directive and retries. This is the same retry path used for schema-validation failures, bounded by `max_schema_retries`; exhaustion emits `PolicyViolated { MaxSchemaRetries, .. }` and `TicketFailed`. Both paths emit `SchemaRetried` first and hand that event to `TicketQueue::edit_directive_on_retry`, so the hook, its budget, and the event a host reads all sit on the queue. A directive renders the arguments the failing tool advertised for this ticket, whichever tool failed: `ToolRegistry::advertised_schema` answers the retry with the same document `definitions` put in front of the model, so a `finish` violation reads back the ticket's own fields and every other tool reads back its own. What a call is checked against is what `ToolRegistry::resolve` returns, which differs from the advertised document for `finish` alone: it accepts a `result` envelope its advertised shape does not name, unwraps it, and validates the result against the ticket itself. That is also why `finish` keeps one check of its own, answered with `ToolResult::error`: a handover needs a real result, and for an inlined ticket there is no `result` property its static schema could require.
- `Status` transitions go through tickets-side helpers; the agent never writes status directly. `Failed` is reserved for system-driven outcomes: exhausted schema retries, exhausted missing-`finish` retries, and policy violations.

Schemas and results:

- `Ticket::schema(...)` attaches a `Schema` to the ticket; `finish` validates the result and the loop applies `max_schema_retries` on mismatch.
- `TicketQueue::schemas(&store)` binds a shared `SchemaStore` to the queue, which holds one schema per label. `SchemaStore::label(label, document)` parses the document itself, so registering a contract never builds a `Schema`; `Schema::new` stays public for `Ticket::schema`, which takes the compiled value. `claim` reads the store once and writes the first match onto `Ticket::schema`, so `Ticket::schema` stays the single source every reader already uses: the prompt directive, the finish tool's advertised arguments, result validation, and the retry directive. A ticket that already carries a schema is left alone.
- Resolution happens in `claim` rather than `insert` because a ticket the model filed gets its label there. `claim` ends in `save_ticket`, so the bound schema is persisted and survives `load`. A resumed ticket never re-enters `claim` and keeps what its first claim bound.
- This is what gives a ticket nobody could build a contract: a handover child, or one the model filed through `tickets`. No tool takes a schema document, since a small model does not write nested schemas reliably.
- A handover validates its `result` against the parent ticket's own schema, exactly as a plain finish does. It carries no schema for the child, which takes one from its handover label when it is claimed. A schema mismatch aborts before the child is inserted, so neither the parent's finish nor the child happens and the operation stays atomic.
- `handover` and `task` are reserved argument names for `finish`. A ticket whose schema is an object has its fields passed as `finish`'s top-level arguments; one that itself declares a control-key-named property keeps the `result` envelope instead, so no field of the result is ever stripped as a control key.
- A successful finish writes the result to `<dir>/tickets/<key>/result.json` (the directory is configured through `TicketQueue::dir(d)`, default `./.agentwerk`) and attaches the same value to the ticket. The value is surfaced through `Ticket::result()`.
- The queue writes the full ticket state to `<dir>/tickets/<key>/ticket.json` on every transition, and the transition itself reaches `<dir>/events.jsonl` as a `TicketCreated`, `TicketStarted`, `TicketFinished`, or `TicketFailed` event. The result payload stays in `result.json`; the log carries only the transition. Both writes are observational: errors are swallowed.

## Knowledge Is Opt-In and Shareable Across Agents

**An agent can carry durable facts across every ticket it handles through `Agent::knowledge(&store)`, including across separate `start` and `finish` calls and across process restarts. Off by default; each ticket starts without a knowledge section.**

Two layers of state exist. The per-ticket replies live on `Ticket::replies`: every message the loop sends to the provider is appended as a `Reply`, and the loop derives the request's `Vec<Message>` from those replies through `Ticket::to_messages` each turn. `Agent::knowledge(&store)` adds a separate cross-ticket layer: a `Knowledge` store rooted at a caller-supplied directory, surfaced to the model through `KnowledgeTool` and rendered into the system prompt.

- The store is constructed through `Knowledge::load(store_dir)` and passed to one or more agents through `Agent::knowledge(&store)`. Two agents bound to the same `Arc<Knowledge>` share the same `index.md` and `pages/` directory; two agents bound to different stores see independent knowledge.
- Pointing `Knowledge::load` at the same directory as `TicketQueue::dir` co-locates the `knowledge/` bundle with `events.jsonl` and the ticket files.
- The store is an Open Knowledge Format (OKF) v0.1 bundle held in `<dir>/knowledge/` (`BUNDLE_DIR`), which keeps it out of a co-located `TicketQueue`'s files and keeps the recursive page walk inside the bundle.
- `<dir>/knowledge/pages/<slug>.md` holds each concept with `type`, `description`, and `timestamp` frontmatter, and pages cross-link with standard markdown links (`[text](/pages/slug.md)`). `<dir>/knowledge/index.md` is a derived progressive-disclosure view with a clickable link per page.
- Only the compact index is injected into the system prompt; the agent reads full pages on demand through the `read` action. `index.md` is written but never parsed back: on load the in-memory index is rebuilt by walking the page frontmatter (`rebuild_index_from_pages`), so an OKF bundle placed in `<dir>/knowledge/` seeds the store from it.
- The loop reads `Knowledge::index()` once at the top of `process_ticket` and feeds the result to `Agent::system_prompt(knowledge: Option<&str>)`. The system prompt stays byte-stable across every turn of the ticket so the provider's prefix cache survives mid-ticket knowledge writes; cross-ticket and cross-agent writes become visible at the top of the next ticket.
- Knowledge is purely model-driven. The model calls `knowledge` with `write`, `read`, `remove`, or `list`; the tool description carries the policy (durable facts only, do NOT save task progress or TODOs). A page's `type` and `tags` are host-side concerns set through the `Page` API, not tool parameters.
- A character limit on the rendered index caps how much of it the prompt lists, never what may be written. Past the limit `Knowledge::index()` lists the pages that fit and closes with a directive naming the absolute path to `index.md`, which the agent reads itself; `knowledge`'s `list` still returns the whole index. It defaults to 12 000 and is configurable through `Knowledge::index_char_limit(n)` on the loaded store.

## Observer Chain, One Error Path

**`Event` reports state. `ProviderError` reports a failed provider contract. The two channels carry independent information.**

- State transitions exist only as `Event`: `TicketClaimed`, `TicketFinished`, `RequestStarted`, `RequestFinished`, `TextChunkReceived`.
- A failed request fires both `ProviderError` and a matching `Event` (`RequestFailed`, `PolicyViolated`). A failed tool call is an `Event` alone: `ToolCallFailed` carries a `ToolFailureKind` and the model-visible message, which is all a tool failure is.
- A model-fixable failure (wrong arguments, schema mismatch, missing file) goes back to the model as a `ToolResult::Error` content block. It still fires `ToolCallFailed` but does not stop the run.
- Handlers MUST be cheap and non-blocking; the loop does not await them.
- `on_result_async` is the exception that proves it: the loop still never awaits, so registering one only queues the finished ticket, and whichever `finish` is waiting drains the queue and awaits each handler on its own task. That is what puts the handler on the caller's event loop in the bindings, and why one that never returns stalls the caller rather than an agent. Handlers run only while a `finish` is awaited; a `start()`-only host uses `on_result`.

`TicketQueue::on_event(h)` pushes a handler onto an ordered chain. Every installed handler fires on every event, in installation order, and each is handed the same `&Event` rather than its own copy. When no handler is installed, `default_logger` runs in its place.

- That composition is what every hook is built on: `create_ticket_on_event(make)` pushes a handler that files a follow-up when `make` returns one, so a host's logger and the hook coexist.
- `on_result` filters to `TicketFinished` and unwraps the stored result, `on_failure` filters on `EventKind::is_failure`, and `on_ticket` filters to the three lifecycle kinds.
- Each of those three resolves `event.ticket_key` to a cloned `Ticket` first, which is why none of them fires on every kind: resolving on `TextChunkReceived` would copy a ticket's whole replies once per chunk.
- The `create_ticket_on_result` and `create_ticket_on_failure` hooks are in turn built on `on_result` and `on_failure`, so the resolve happens in one place.
- `EventKind::RunStarted` and `EventKind::RunFinished { reason }` ride the same chain. They are emitted by the `TicketQueue` itself and arrive with an empty `agent_id`, as does `TicketFailed` from a host-driven `set_failed`.

## New Observables Pick a Channel

**Each new signal goes on `Event`, on a typed error, or on both. Pick by what the signal describes.**

- Reached a state: `Event` only.
- Could not fulfil a contract: typed error in the matching domain.
- Both at once (terminal request failure, policy violation): define both. Share the payload type when observer-friendly (`PolicyKind`); introduce a stripped `Kind` enum when the error carries observer-hostile detail (`RequestErrorKind`, `ToolFailureKind`).
- Model-fixable failure: `ToolResult::Error(String)`; still fires `ToolCallFailed` but is recoverable.
- A public error enum carries `#[non_exhaustive]`, so a later variant is not a breaking change for callers that match on it. `ProviderError` does. The attribute covers new variants only: adding a field to an existing struct variant still breaks a caller that matches it without `..`, so prefer a new variant to widening an old one.

## Providers Own Their Client

**Each concrete provider owns an `Endpoint`, which owns a `reqwest::Client` directly. There is no transport abstraction beyond it.**

- The `ProviderLike` trait fulfils one contract: `respond`, drive one turn. Callers hold it as a `Provider`, a cloneable handle any implementer converts into. `Provider::verify` is an inherent convenience over `respond`, not a second thing to implement.
- `ModelRequest`, `Message`, `ContentBlock`, and `TokenUsage` are the request and response types every provider converts to and from.
- Those types, plus `ModelResponse`, `StreamEvent`, `ResponseStatus`, `ToolChoice`, and `ToolDefinition`, are `pub` and documented: the `ProviderLike` trait is a supported extension point, and its implementors name them.
- Where a request goes, how long it may take, and what a non-2xx answer means are decided in `providers::endpoint`. Vendor code adds its own authentication headers and its own `classify_error` for the bodies only that vendor words its own way. SSE reading lives in `providers::stream`.
- There are two protocols and four configurations, and the `Protocol` trait is where that split lives: `AnthropicMessages` and `OpenAiChat` each supply a path, their authentication headers, a request shape, a 400-body classifier, and a decoder. `mistral` and `litellm` name `OpenAiChat` against their own `Endpoint` rather than owning an `OpenAi` of their own, and every provider's `respond` is one call to `provider::respond::<P>`, so no two can disagree about the order a turn happens in.
- A provider decodes its own payloads and names which `ResponseBuilder` call each one is. `providers::stream` decides which block a fragment continues and when a `StreamEvent` fires, so no two vendors can disagree about either. Blocks are dense and in arrival order; the number an endpoint attaches to a fragment routes tool calls only, and never sizes anything.
- A context window is looked up by model name in `providers::model`, not per vendor: the same name reaches agentwerk through whichever endpoint serves it.
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

## Every Event Is Logged, Statistics Are Folded From It

**`TicketQueue::emit` folds every event into the crate-private `Stats` and appends it to `events.jsonl`, `TextChunkReceived` aside. A host reads the log back through `TicketQueue::find_events`; the crate counts only what a policy needs.**

- `emit` writes the line before firing observers, so the log holds what every handler saw. The chunk kinds are the exception: one line per streamed token would outweigh every other line and repeats what `replies.jsonl` already carries.
- The write is best-effort, like every observational write. A failed line costs an entry in the log, never the run.
- `TicketQueue::find_events(condition)` and `find_event(condition)` read the log; a count is `.len()` and any breakdown is a fold. `input_tokens()`, `output_tokens()`, and `execution_duration()` stay live off the counters, because the policy check reads the same ones every 50ms and a token total should not cost a file read.
- The two sources can disagree, by design: delete the log mid-run and the three totals keep reporting while the finders find nothing. The counters are what the run spent, the log is what it wrote down.
- A line this build cannot parse is skipped rather than costing every line after it, which is what lets a log written by another version still report.
- `EventKind` is internally tagged under `event`, so a line names itself the way `EventName` spells it. `EventName` is the payload-free half of `EventKind`, and serde's `rename_all` reproduces the same snake_case on both.
- `RequestFinished` is the one kind whose payload the statistics read: its `usage` adds to the two token totals. Every other kind contributes its count and nothing else.
- `execution_duration` spans the first `TicketStarted` to the `RunFinished`, or to now while the run is going. `TicketStarted` is emitted from `claim`, where the transition happens, so a host claiming a ticket without running the loop still starts the clock. `TicketQueue::load` restarts it: `max_time` bounds the run resuming the session, not the one that wrote the log.
- Breakdowns are the host's, not the crate's. Per tool, per model, per file, per agent, or per label is a fold — on the `on_event` chain while the run works, or over `find_events` afterwards — which is why an `Event` carries `agent_id`, `ticket_key`, and the ticket's `label` alongside the kind. The scanner use case shows both shapes.
- `Stats::record` is the single writer, and it takes the whole `Event`: a live queue folds each one in as `emit` publishes it, and `Stats::load` folds the same events back out of the file. Both arrive at the same figures, so a run never keeps a second set of counters. The figures are counts, not the events, which is what keeps a long run's tool output out of memory once its line is written.
- The per-ticket token series (`usage_for_ticket`) stays crate-internal. Compaction clears it on every compaction, so a caller would read a silently truncated series; a host that wants the figures reads `EventKind::RequestFinished`, which reports every one as it happens.

## Persistence Routes Through One Trait

**Every read and write in the crate goes through `Persist` in `persistence`, or through an inherent `append` on the type that owns its log. No domain module hand-rolls file IO; no module knows its file's name except the implementer.**

- `Persist` defines `save(&self, dir) -> io::Result<()>` and `load(dir, &Self::Key) -> io::Result<Self>`. `Ticket`, `Replies`, `TicketResult`, `Page`, and `Trajectory` implement it; each owns its own path layout (`tickets/<key>/ticket.json`, `tickets/<key>/replies.jsonl`, `tickets/<key>/result.json`, `pages/<slug>.md`, `trajectories/<key>.json`).
- A ticket's result and its replies are both `#[serde(skip)]` on `Ticket` and spliced back in by `Ticket::load`, so each fact has one file. An agent reads another ticket's result through the ticket tools' `result` action or straight from `result.json`.
- A value type the caller stores itself reaches its file through an inherent method that delegates to its own impl, never by publishing the trait: `Trajectory::save(dir)` and `Knowledge::pages().save(page)` are the two. Service bootstrap (`TicketQueue::load`, `Knowledge::load`) uses the same `load` verb for its directory-to-`Arc<Self>` entry by convention.
- The two append-only logs each own their filename on the type that reads them back: `Stats::append(dir, &Event)` writes `events.jsonl`, and `Replies::append(dir, key, &Reply)` writes `tickets/<key>/replies.jsonl`. Neither earns a trait, since the second takes a key the first does not; give them one when a third log matches either shape.
- `Replies` also implements `Persist`, whose `save` overwrites the file wholesale so a dropped or redacted reply leaves nothing behind.
- `TICKET-<N>` keys are handed out in order. `load()` seeds the next key from the tickets it just read off disk; a queue built with `new()` scans for the highest existing key at the first insert instead, since `new()` never reads the directory itself.
- One agent processes one ticket at a time (claim is atomic), so `add_reply` and the rewrite for one key are sequential within a single loop task. No per-key lock is needed for either path.
- Crate-internal helpers `write_atomic` (tmp plus rename) and `append_line` (`O_APPEND` plus newline) are the only places that touch the filesystem. They are `pub(crate)` so impls colocated with their types can call them; by convention nothing outside a `Persist` impl or an `append` reaches for them.
- One documented exception: `TicketQueue::write_tool_output` writes single-shot flat files that fit neither trait.
- Vocabulary is fixed: `save`, `load`, `append`. Bootstrap verbs other than `load` (such as `open`) are not used. Domain words (`checkpoint`, `snapshot`, `counter`, `persist`) do not appear in identifiers or test names.

## Policies Are Per-Queue, Checked at Turn Boundaries

**A run stops cleanly when any limit on `Policies` is breached. The check fires `EventKind::PolicyViolated` and exits the per-agent task.**

- The loop calls `policy_violated_kind` at each iteration; a non-`None` return takes the agent off the queue.
- Token budgets read from the queue's live `Stats`; `max_time` reads from `Policies` and from `Stats::execution_duration()`. All limits, including `max_time`, route through `policy_violated_kind` and emit `PolicyViolated`; `finish_reason` reports the matching `FinishReason::PolicyViolated(kind)` once the run has ended.
- The schema-retry budget is applied per-ticket inside the result-writing path, not at the top of the loop.
- `compact_at` rides on `Policies` for the same per-queue snapshot every limit gets, but it is a trigger rather than a limit: `policy_violated_kind` ignores it, and reaching it costs a compaction, not the run.
