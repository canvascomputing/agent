# Architecture

The invariants that shape how code fits together. Layout says where code lives; this file says why the boundaries are where they are.

## Builder, Queue, Loop

**A run has three stages: build the `Agent`, bind it to a `TicketQueue`, drive the queue with `start` (long-lived) or `finish` (process a fixed batch and return).**

```rust
let agent = Agent::from_env().build();
tickets.agent(agent);
tickets.finish_all().await;
```

- An `Agent` carries a `Weak<TicketQueue>` that dangles until `agent(a)` binds it. `TicketQueue::new` captures its own `Weak<Self>` through `Arc::new_cyclic` to have one to hand out.
- `agent(a)` also drains tickets the agent queued in its private default queue into the shared one.
- `start` and `finish` spawn one tokio task per registered agent. Each upgrades its `Weak` once and reads the shared store, policies, budget, and ending from the resulting `Arc`.
- `tickets.ticket(value)` creates a ticket and returns its key; `tickets.reply(&key, content)` appends a text reply and the wait-for-input branch drives the next turn on the same replies. That is how multi-turn chat is built on one ticket.

## Shared Queue, Per-Agent Task

**Agents read shared state through one `Arc<TicketQueue>`. Locks are held only around queue and metric operations, never across `provider.respond().await`.**

- The ticket store, policies, budget, ending, cancel filters, and registered-agent list live on `TicketQueue`.
- The per-agent loop in `loop/agent.rs` claims one ticket, drives it through one or more provider and tool turns, and releases locks before each await.
- Multiple agents share one queue; a ticket is claimed exactly once. Nested queues are not supported: one `TicketQueue` is the unit of orchestration.

## Assignment Is a Label, Identity Is an Id

**One label per agent, one label per ticket: the label is the only assignment mechanism, and the id `build` derives from it is the only identity. Neither does the other's job.**

```rust
tickets.ticket(Ticket::new("Audit src/db.").label("scan")); // one scope
tickets.ticket(Ticket::new("Audit src/db."));               // the default scope
```

- `Agent::handles` is equality in both directions: an agent with no label matches only tickets with no label, the default scope. A label no agent serves never matches, since the queue never resolves one against the registered-agent set.
- Addressing one agent alone is giving it a label no other agent serves. The ticket is born `Status::Todo` like any other; nothing is born `InProgress`.
- `AgentBuilder::build` assigns `<label>-<n>`, numbering per label from 1; an agent with no label gets `agent-<n>`. `Agent::clone` keeps the id, since `bind_agent` holds a clone and the two must agree about which tickets are theirs.
- `claim` writes the claiming agent's id to `Ticket::assignee`, and the `resumable` check requires the two to match, so agents sharing a label never take over each other's started work. A host that wants resumption builds the same agents, in the same order, after a restart.
- A session directory written before the label became singular stores `"labels": [..]`, which no longer deserializes. There is no shim: start a fresh session directory.

## A Tool Call Resolves by Exact Name, Then by Folded Key

**`ToolRegistry::get` matches the name the model sent exactly. Failing that, it folds both sides onto a lookup key (lowercase, hyphens to underscores, one trailing `_tool` removed) and resolves to the tool that key matches. A key two registered tools share resolves to nothing.**

- The fold only removes information, so it cannot reach a tool the model did not name. A host's `grep_tool` and the built-in `grep` both stay reachable under their own names, and a third spelling resolves to neither.
- `get` is the only entry point: dispatch, `partition_tool_calls`, and the `opened_paths` lookup all go through it.
- Every registered tool carries a compiled `Schema`, so a tool without one is unrepresentable. `ToolBuilder::schema` is the one place a document compiles, and it panics on one the compiler refuses, so a broken definition fails the build rather than a request.
- `Schema::validate` is the only thing that rejects a call's arguments. It retypes what the schema names a type for, then checks what that produced, so the model reads back everything still wrong in one report per turn.
- No tool checks its own arguments. A requirement that holds only for some values of a discriminator is stated with `allOf`/`if`/`then` in the tool's `.schema.json`; a rejection answers as a `ToolResult::Error` tagged `SchemaValidationFailed`.
- The loop rewrites each call to the registered name before emitting `ToolCallStarted`, so `Event` and `Stats` never split one tool across spellings. Both repairs reach the host as `EventKind::ResponseRepaired` naming the tool, with `RepairKind::CallMalformed` for a folded name and `RepairKind::ValueMistyped` for a retyped value.
- A name that resolves to nothing fails as `ToolFailureKind::ToolNotFound`, with a message naming every registered tool. Without that list each retry spends `max_schema_retries` until the ticket fails. A call the model wrote as text takes the same path.

## Every Directive Is a Catalogue Entry

**Text agentwerk sends the model to report a failure or correct its behavior is one entry in `prompts/directives/*.md`, named by a `pub const` beside it, and the function an agent takes through `AgentBuilder::directives` decides what it renders as. No call site writes one inline.**

```rust
ToolResult::error(ctx.directives.render(EDIT_FILE_OLD_STRING_NOT_FOUND, &[("path", &path)]))
```

- `DirectiveStore::render(key, values)` hands that function the key, then binds `{name}` into what comes back, or into the catalogue text when it answers `None`. A store therefore varies a directive by which one it is, and the values reach the model through the template rather than through the function. It takes a `&str`, so the constants rather than the compiler are what keep the 94 call sites honest.
- The `directives!` macro declares each key once and emits the crate-private constant the render sites write, the `Directive` constant a host matches on, and the `ALL` entry, so the three cannot disagree. A test pairs `ALL` against the `## ` headings, which the macro cannot reach.
- One function decides every directive, rather than a table of per-key replacements. A host matches the key against the constants `Directive` carries, so a misspelled arm does not compile, and answers `None` to leave a key as it is.
- `AgentBuilder::directives` wraps the function in the crate-private `DirectiveStore`, which then travels the way `Knowledge` does: `ToolContext` carries it and the loop reads `context.agent.directives()`. Two agents in one process therefore word a failure differently, and no test needs a lock. A host sharing one function across agents passes the same `fn` or a cloned handle.
- Twenty-one keys render through `built_in` instead, composed where no agent is in reach: the 19 schema violations inside `Node::check`, `knowledge_index_truncated` inside `Knowledge::index`, and `result_schema_required` inside `Ticket::as_user_message`. Threading a store into any of the three would re-type public API.
- Binding is one pass, so a value carrying `{` is never read as a placeholder of its own. A `{name}` with no value renders as written, the rule `AgentBuilder::template` already states.
- Two categories stay out: a `SchemaParseError` and the "could not read its arguments" answer both name a mistake in the host's code, and no model reads them. The tags around an offloaded result stay in Rust too, since `cap_aggregate_outputs` reads the opening one back.
- The retry site binds `attempt`, `max_attempts`, `ticket`, and `agent` beside `detail`, so a replacement naming `{attempt}` or `{agent}` says how far into the budget a retry is, or which agent it addresses, without an event in reach.

## Finishing Is a Tool Call

**Agents finish tickets through one tool, `finish`. An optional `handover` argument additionally creates a child ticket; its presence is the only discriminator, so there is no second tool and no mode field.**

`finish` records the result through `TicketQueue::set_result`, which owns the result-validation-and-logging contract, then transitions the ticket to `Finished`. The loop enforces the rule on every agent but an interactive one: a turn that ends without a `finish` call is rejected and retried.

- An interactive agent gets no `finish` at all, since ending the ticket would end the conversation. It pauses on a reply that calls no tool, and the host closes the ticket with `TicketQueue::set_finished`. `AgentBuilder::build` is where the tool is registered, because only there is `interactive` known; a host that registers `FinishTool` itself keeps it either way.
- With `handover`, `finish` also inserts a child ticket pinned to that agent or label, with the current ticket recorded as its `parent`. The child's body is the result or the caller's `task` (with `{parent_key}`, `{parent_result_path}`, and `{parent_result}` substituted, the result last so nothing it carries is expanded again), and always ends with the parent key and its result file.
- The child is inserted BEFORE the parent finishes, so a concurrent `work_left` check can never see an empty queue between them. `TicketFinished` and `TicketFailed` are emitted synchronously from the transition, and a count of in-flight transitions keeps `work_left` true until every handler returns, so an `on_result` follow-up lands first as well.
- A turn that ends without a `finish` call pushes a corrective directive and retries, the same path a schema failure takes, bounded by `max_schema_retries`; exhaustion emits `PolicyViolated { MaxSchemaRetries, .. }` and `TicketFailed`. Both paths emit `SchemaRetried` first, and its `attempt` and `max_attempts` are bound into the directive that follows.
- `finish` holds one schema, not two: `FinishTool::from_schema` makes the ticket's document the tool's own `result` argument at claim time, so a result that misses it is rejected before the handler runs and one written as JSON text is decoded there. Its one own check: a handover needs a real result.
- An agent that must always chain cannot be forced to by its tool registry, since every `finish` accepts an optional `handover`. Its role prompt carries that requirement instead.
- `Status` transitions go through tickets-side helpers; the agent never writes status directly. `Failed` is reserved for system-driven outcomes: exhausted schema retries, exhausted missing-`finish` retries, and policy violations.

Schemas and results:

- `Ticket::schema(...)` attaches a `Schema` to one ticket. `TicketQueue::schemas(&store)` binds a shared `SchemaStore` holding one schema per label, and `SchemaStore::label(label, document)` parses the document itself.
- `claim` reads the store once and writes the first match onto `Ticket::schema`, leaving a ticket that already carries one alone. Resolution happens there rather than in `insert` because a ticket the model filed gets its label there; `claim` ends in `save_ticket`, so the binding survives `load` and a resumed ticket keeps it.
- That is what gives a ticket nobody could build a contract: a handover child, or one the model filed through `tickets`. No tool takes a schema document, since a small model does not write nested schemas reliably.
- A handover validates its `result` against the parent's schema and carries none for the child, which takes one from its handover label at claim. A mismatch aborts before the child is inserted, so the operation stays atomic.
- `handover` and `task` are `finish`'s own arguments; the result is always `result`. A ticket schema declaring a property named `handover` needs no special case, because it sits inside `result`.
- A successful finish writes `<dir>/tickets/<key>/result.json` (`TicketQueue::dir(d)`, default `./.agentwerk`) and attaches the same value, read back through `Ticket::result()`. The full ticket state goes to `ticket.json` on every transition and the transition itself to `<dir>/events.jsonl`. Both writes are observational: errors are swallowed.

## Knowledge Is Opt-In and Shareable Across Agents

**`Agent::knowledge(&store)` carries durable facts across every ticket an agent handles, across separate `start` and `finish` calls, and across process restarts. Off by default.**

- Per-ticket state is `Ticket::replies`, which the loop turns into each request's `Vec<Message>` through `Ticket::to_messages`. `Knowledge` is the separate cross-ticket layer, surfaced through `KnowledgeTool` and rendered into the system prompt.
- Two agents bound to the same `Arc<Knowledge>` share one bundle; two agents bound to different stores see independent knowledge.
- The store is an Open Knowledge Format (OKF) v0.1 bundle in `<dir>/knowledge/` (`BUNDLE_DIR`), which keeps it out of a co-located `TicketQueue`'s files and keeps the recursive page walk inside the bundle.
- `index.md` is derived and never parsed back: on load the in-memory index is rebuilt by walking the page frontmatter (`rebuild_index_from_pages`), so a bundle dropped into `<dir>/knowledge/` seeds the store.
- Only the index is injected into the system prompt; the agent reads full pages on demand through `read`. The loop reads `Knowledge::index()` once at the top of `process_ticket`, so the prompt stays byte-stable across every turn and the provider's prefix cache survives mid-ticket writes. Writes become visible at the top of the next ticket.
- Knowledge is purely model-driven, and the tool description carries the policy (durable facts only, do NOT save task progress or TODOs). A page's `type` and `tags` are host-side concerns set through the `Page` API, not tool parameters.
- A character limit caps how much of the index the prompt lists, never what may be written. Past it the index names the absolute path to `index.md` instead, while `list` still returns the whole thing. It defaults to 12 000 and is configurable through `Knowledge::index_char_limit(count)`.

## Observer Chain, One Error Path

**`Event` reports state. `ProviderError` reports a failed provider contract. The two channels carry independent information.**

- State transitions exist only as `Event`. A failed request fires both `ProviderError` and a matching `Event` (`RequestFailed`, `PolicyViolated`).
- A failed tool call is an `Event` alone: `ToolCallFailed` carries a `ToolFailureKind` and the model-visible message, which is all a tool failure is.
- A model-fixable failure (wrong arguments, schema mismatch, missing file) goes back to the model as a `ToolResult::Error` content block. It still fires `ToolCallFailed` but does not stop the run.
- Handlers MUST be cheap and non-blocking; the loop does not await them. The four `_async` twins are the exception, and the loop still never awaits: registering one only queues the event, and whichever `finish` is waiting drains it and awaits each handler on its own task. A handler that never returns therefore stalls the caller rather than an agent, and a `start()`-only host uses the blocking form.

`TicketQueue::on_event(h)` pushes a handler onto an ordered chain. Every installed handler fires on every event, in installation order, and each is handed the queue and the same `&Event` rather than its own copy. When no handler is installed, `default_logger` runs in its place.

- Every other hook is built on that chain, so a host's logger and a hook coexist.
- The queue is the first parameter of every handler. That is what let the `create_ticket_on_*` and `edit_replies_on_event` hooks go: a handler files its own follow-up work through `queue.ticket(..)`, rewrites what the model reads next through `queue.edit_replies(&event.ticket_key, ..)`, and selects what it needs through `queue.find_*`, without an `Arc` into the queue that holds it.
- `on_result` filters to `TicketFinished` and unwraps the stored result, `on_failure` filters on `EventKind::is_failure`, and `on_ticket` filters to the three lifecycle kinds. Each resolves `event.ticket_key` to a cloned `Ticket` first, which is why none fires on every kind: resolving on `TextChunkReceived` would copy a ticket's whole replies once per chunk. The queuing hook the `_async` twins share resolves a ticket on those same kinds only, for the same reason.
- An event that announces a reply is emitted after that reply has landed in the store: `RequestFinished` after the assistant reply, `ToolCallFinished` and `ToolCallFailed` after the tool results of their turn. A handler therefore finds the message the event names, and what it rewrites through `queue.edit_replies` reaches the next request.
- `RunStarted`, `RunFinished { reason }`, and a host-driven `set_failed` are emitted by the queue itself and arrive with an empty `agent_id`.

## New Observables Pick a Channel

**Each new signal goes on `Event`, on a typed error, or on both. Pick by what the signal describes.**

- Reached a state: `Event` only.
- Could not fulfil a contract: typed error in the matching domain.
- Both at once (terminal request failure, policy violation): define both. Share the payload type when observer-friendly (`PolicyKind`); introduce a stripped `Kind` enum when the error carries observer-hostile detail (`RequestErrorKind`, `ToolFailureKind`).
- Model-fixable failure: `ToolResult::Error(String)`; still fires `ToolCallFailed` but is recoverable.
- A public error enum carries `#[non_exhaustive]`, which covers new variants only: adding a field to an existing struct variant still breaks a caller that matches it without `..`, so prefer a new variant to widening an old one.

## Providers Own Their Client

**Each concrete provider owns an `Endpoint`, which owns a `reqwest::Client` directly. There is no transport abstraction beyond it.**

- The `ProviderLike` trait fulfils one contract: `respond`, drive one turn. Callers hold it as a `Provider`, a cloneable handle any implementer converts into.
- The request and response types are `pub` and documented, because implementing `ProviderLike` is supported and implementors name them.
- `ModelRequest.tools` carries the `Tool` values themselves rather than a second description of them, and the registry they come from is the ticket's own: `claim` clones the agent's registry and rebinds a `finish` already in it to `FinishTool::from_schema(ticket.schema)`. A provider never calls a tool's handler.
- Where a request goes, how long it may take, and what a non-2xx answer means are decided in `providers::endpoint`. Vendor code adds its own authentication headers and its own `classify_error`, and never retries: retry is request-level, using `Policies::max_request_retries` and `request_retry_delay`.
- Two protocols cover four configurations, and the `Protocol` trait is where that split lives: `AnthropicMessages` and `OpenAiChat` each supply a path, authentication headers, a request shape, a 400-body classifier, and a decoder. `mistral` and `litellm` name `OpenAiChat` against their own `Endpoint`, and every `respond` is one call to `provider::respond::<P>`.
- A provider decodes its own payloads and names which `ResponseBuilder` call each one is; `providers::stream` decides which block a fragment continues and when a `StreamEvent` fires. The number an endpoint attaches to a fragment routes tool calls only, and never sizes anything.
- A context window is looked up by model name in `providers::model`, not per vendor.

## The Lifecycle Is Three Verbs Over One Filter

**`start` starts, `finish(matches)` waits, `cancel(matches)` stops. Both filters are `Fn(&Ticket) -> bool`, so waiting for one pool or one ticket is the same call with a different filter, and `finish_all()` and `cancel_all()` pass the filter that names every ticket.**

- `TicketQueue::work_left(matches)` is the one definition of "not done yet", and both the main loop and `finish` ask it. A ticket has work left while it is pending, uncancelled, and not paused for a caller reply.
- `TicketQueue::cancel_filters` holds what `cancel` took off the queue. The claim and resume path reads it through `is_cancelled(ticket)`, so a cancelled ticket is neither claimed nor resumed and an agent already holding one is taken off it. The ticket stays `InProgress`.
- A filter runs while the ticket store lock is held, so it MUST NOT call back into the queue: the same rule `find_ticket` and `find_tickets` carry.

## A String That Selects Tickets Is AQL

**Every method taking a filter accepts a string, and the string is AQL: a query compiled by `Query::new`. There is no second string meaning, and no method takes a bare label.**

```rust
tickets.find_tickets("scan");                    // label = scan
tickets.find_results("TICKET-3");                // key = TICKET-3
tickets.find_tickets("label IN (scan, report) AND status != Failed");
```

- `Query` holds a private condition tree of `All`, `Any`, `Not`, and one term per field. `Query::new` is the only way to build one, so AQL is the one grammar a selection is written in and no field gains a second spelling as a method.
- A lone bare word is `key = <word>` when it is spelled `TICKET-<digits>` and `label = <word>` otherwise, which is what keeps a label the shortest thing you can write. A label named like a key needs `label = TICKET-3`.
- `From<&str>` and `TicketMatcher for &str` are infallible by signature, so a string that does not compile panics, the way `ToolBuilder::schema` panics on a document the compiler refuses. `Query::new` returns a `Result` for a string built at run time, and the Python bindings raise `ValueError` rather than panicking across the binding.
- `TicketMatcher for &str` compiles the string once per ticket. A host filtering a large queue passes a `Query`.
- `ORDER BY` rides on the query rather than on the call, so the one string says both which tickets and in what order, and the `tickets` tool needs no second argument. A query that is nothing but an `ORDER BY` selects every ticket.
- `TicketMatcher::sort` is how the order reaches the queue, and its default is creation order, so a closure and a query without `ORDER BY` answer as they always did. `find_tickets` and `find_results` share `matching_tickets(keep, order)`, which takes the filter and the order apart because `find_results` wraps the matcher in a closure of its own and a closure names no order.
- `cancel`, `finish`, and `pending` read `matches` alone, so an `ORDER BY` handed to them does nothing. Nothing there is ordered.
- `TicketMatcher::names_status` is what lets `find_results` and `find_result` take any filter and still default the status to `Finished`. `Query` answers it by walking its tree, the string impls by parsing, and a closure takes the `false` default, so a closure always gets the `Finished` filter. It is the one thing the trait knows beyond `matches`, and it exists because that default cannot be read off a closure.
- Two equalities on one single-valued field are a parse error rather than a query no ticket satisfies, and the message names `IN` as the fix. An absent field fails every comparison, so `label != scan` never reaches an unlabelled ticket and `IS EMPTY` is what does.
- The `tickets` tool takes the same syntax as its one `aql` argument, and answers a `QueryError` as a `ToolResult::Error` through `ticket_query_invalid`. Nothing on that path panics.

## The Run Names Its Own Ending

**`run_main_loop` decides when a run is over and announces it once, rather than whichever caller happens to await. A limit breached while the host is busy elsewhere still ends the run.**

- `TicketQueue::run` is one `Arc<Run>` over a `watch` channel of three phases: `Working`, `Draining(reason)` while the agents stop, and `Finished(reason)` once `RunFinished` has been announced. The channel is both the value and the wake, and a run complete without a reason cannot be written.
- `ending_reason()` names `FinishReason::PolicyViolated(kind)` for a breached limit and `FinishReason::Cancelled` once a cancel leaves nothing claimable. An empty queue is not an ending: a host that called `start()` may still be filing work, and a paused ticket revives on the next reply.
- `FinishReason::Drained` is named by the `finish` that waited for it, and only when no ticket at all is still open. That is what keeps an interactive chat alive between turns.
- The main loop joins its agents and emits `RunFinished { reason }` before `Run::set_finished`, so a caller that starts another run never overlaps the previous one.
- Tools observe the ending through `ToolContext::cancelled`; pair it with `tokio::select!` so it drops the losing branch promptly. Dropping the `TicketQueue` while agents still hold a `Weak` is the public way to abort: the upgrade fails and each task panics out cleanly.

## Every Event Is Logged, Statistics Are Folded From It

**`TicketQueue::emit` folds every event into the crate-private `Stats` and appends it to `events.jsonl`, `TextChunkReceived` aside. A host reads the log back through `TicketQueue::find_events`; the crate counts only what a policy needs.**

- `emit` writes the line before firing observers, so the log holds what every handler saw. One line per streamed token would outweigh every other line and repeats what `replies.jsonl` already carries, which is why the chunk kinds are excluded.
- The write is best-effort, and a line this build cannot parse is skipped rather than costing every line after it, so a log written by another version still reports.
- `find_events(condition)` reads the log; a count is `.len()` and any breakdown is a fold. `input_tokens()`, `output_tokens()`, and `execution_duration()` stay off the counters, because the policy check reads the same ones every 50ms.
- The two sources can disagree, by design: delete the log mid-run and the three totals keep reporting while the finders find nothing.
- `RequestFinished` is the one kind whose payload the statistics read: its `usage` adds to the two token totals. Every other kind contributes its count and nothing else.
- `execution_duration` spans the first `TicketStarted` to the `RunFinished`, or to now while the run is going. `TicketStarted` is emitted from `claim`, so a host claiming a ticket without running the loop still starts the clock. `TicketQueue::load` restarts it: `max_time` bounds the run resuming the session, not the one that wrote the log.
- Breakdowns are the host's, not the crate's. Per tool, per model, per file, per agent, or per label is a fold, on the `on_event` chain or over `find_events` afterwards, which is why an `Event` carries `agent_id`, `ticket_key`, and the ticket's `label` alongside the kind.
- `Stats::record` is the single writer and takes the whole `Event`, so a live queue and `Stats::load` arrive at the same figures and a run never keeps a second set of counters. The per-ticket token series (`usage_for_ticket`) stays crate-internal, because compaction clears it and a caller would read a silently truncated series.

## Persistence Routes Through One Trait

**Every read and write in the crate goes through `Persist` in `persistence`, or through an inherent `append` on the type that owns its log. No domain module hand-rolls file IO; no module knows its file's name except the implementer.**

- `Persist` defines `save(&self, dir) -> io::Result<()>` and `load(dir, &Self::Key) -> io::Result<Self>`. `Ticket`, `Replies`, `TicketResult`, `Page`, and `Trajectory` implement it, each owning its own path layout under `tickets/<key>/`, `pages/`, or `trajectories/`.
- A ticket's result and its replies are both `#[serde(skip)]` on `Ticket` and spliced back in by `Ticket::load`, so each fact has one file.
- A value type the caller stores itself reaches its file through an inherent method that delegates to its own impl, never by publishing the trait: `Trajectory::save(dir)` and `Knowledge::pages().save(page)` are the two. Service bootstrap (`TicketQueue::load`, `Knowledge::load`) uses the same `load` verb by convention.
- The two append-only logs own their filename on the type that reads them back: `Stats::append(dir, &Event)` writes `events.jsonl`, `Replies::append(dir, key, &Reply)` writes `tickets/<key>/replies.jsonl`. Neither earns a trait, since the second takes a key the first does not.
- `Replies` also implements `Persist`, whose `save` overwrites the file wholesale so a dropped or redacted reply leaves nothing behind.
- `TICKET-<N>` keys are handed out in order. `load()` seeds the next key from the tickets it just read off disk; a queue built with `new()` scans for the highest existing key at the first insert instead.
- One agent processes one ticket at a time, so `add_reply` and the rewrite for one key are sequential within a single loop task. No per-key lock is needed.
- `write_atomic` (tmp plus rename) and `append_line` (`O_APPEND` plus newline) are the only places that touch the filesystem. They are `pub(crate)`, and by convention nothing outside a `Persist` impl or an `append` reaches for them. One documented exception: `TicketQueue::write_tool_output` writes single-shot flat files that fit neither trait.
- Vocabulary is fixed: `save`, `load`, `append`. Bootstrap verbs other than `load` (such as `open`) are not used, and `checkpoint`, `snapshot`, `counter`, and `persist` do not appear in identifiers or test names.

## Policies Are Per-Queue, Checked at Turn Boundaries

**A run stops cleanly when any limit on `Policies` is breached. The check fires `EventKind::PolicyViolated` and exits the per-agent task.**

- The loop calls `policy_violated_kind` at each iteration; a non-`None` return takes the agent off the queue.
- Token budgets read from the queue's live `Stats`; `max_time` reads from `Policies` and from `Stats::execution_duration()`. `finish_reason` reports the matching `FinishReason::PolicyViolated(kind)` once the run has ended.
- The schema-retry budget is applied per-ticket inside the result-writing path, not at the top of the loop.
- `compact_at` rides on `Policies` for the same per-queue snapshot every limit gets, but it is a trigger rather than a limit: `policy_violated_kind` ignores it, and reaching it costs a compaction, not the run.
