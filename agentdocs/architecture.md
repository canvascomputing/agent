# Architecture

The invariants that shape how code fits together. Layout says where code lives; this file says why the boundaries are where they are.

## Agent, Queue, Loop

**A run has three stages: configure the `Agent`, bind it to a `Queue`, drive the queue with `start` or a `finish_*` wait.**

```rust
let agent = Agent::from_env();
tasks.add_agent(agent);
tasks.finish_all_tasks().await;
```

- An `Agent` carries a `Weak<Queue>` that dangles until `add_agent(a)` binds it. `Queue::new` captures its own `Weak<Self>` through `Arc::new_cyclic` to have one to hand out.
- `add_agent(a)` also drains tasks the agent queued in its private default queue into the shared one.
- `start` and the `finish_*` waits spawn one tokio task per registered agent. Each upgrades its `Weak` once and reads the shared store, policy, budget, and ending from the resulting `Arc`.
- `tasks.add_task(value)` creates a task and returns its ID; `tasks.add_reply(&id, content)` appends a text reply and the wait-for-input branch drives the next turn on the same replies. That is how multi-turn chat is built on one task.

## Shared Queue, Per-Agent Task

**Agents read shared state through one `Arc<Queue>`. Locks are held only around queue and metric operations, never across `provider.respond().await`.**

- The task store, policy, budget, ending, cancel filters, and registered-agent list live on `Queue`.
- The per-agent loop in `loop/agent.rs` claims one task, drives it through one or more provider and tool turns, and releases locks before each await.
- Multiple agents share one queue; a task is claimed exactly once. Nested queues are not supported: one `Queue` is the unit of orchestration.

## Assignment Is a Label, Identity Is an Id

**One label per agent, one label per task: the label is the only assignment mechanism, and the id `build` derives from it is the only identity. Neither does the other's job.**

```rust
tasks.add_task(Task::new("Audit src/db.").label("scan")); // one scope
tasks.add_task(Task::new("Audit src/db."));               // the default scope
```

- `Agent::handles` is equality in both directions: an agent with no label matches only tasks with no label, the default scope. A label no agent serves never matches, since the queue never resolves one against the registered-agent set. It takes both labels rather than a receiver, because the loop's claim filter holds the label and the queue keeps that filter.
- Addressing one agent alone is giving it a label no other agent serves. The task is born `Status::Todo` like any other; nothing is born `InProgress`.
- `Agent::id` assigns `<label>-<n>` the first time anything asks for it, numbering per label from 1; an agent with no label gets `agent-<n>`. It is that late because the label is set after construction, so a label bound afterwards would otherwise rename an agent the queue already knows. `Agent::clone` reads the id before copying it, since `bind_agent` holds a clone and the two must agree about which tasks are theirs.
- `claim` writes the claiming agent's id to `Task::assignee`, and the `resumable` check requires the two to match, so agents sharing a label never take over each other's started work. A host that wants resumption builds the same agents, in the same order, after a restart.
- A session directory written before the label became singular stores `"labels": [..]`, which no longer deserializes. There is no shim: start a fresh session directory.

## A Tool Call Resolves by Exact Name, Then by Folded Key

**`ToolRegistry::get` matches the name the model sent exactly. Failing that, it folds both sides onto a lookup key (lowercase, hyphens to underscores, one trailing `_tool` removed) and resolves to the tool that key matches. A key two registered tools share resolves to nothing.**

- The fold only removes information, so it cannot reach a tool the model did not name. A host's `grep_tool` and the built-in `grep` both stay reachable under their own names, and a third spelling resolves to neither.
- `get` is the only entry point: dispatch, `partition_tool_calls`, and the `opened_paths` lookup all go through it.
- Every registered tool carries a compiled `Schema`, so a tool without one is unrepresentable. `ToolBuilder::schema` is the one place a document compiles, and it panics on one the compiler refuses, so a broken definition fails the build rather than a request.
- `Schema::validate` is the only thing that rejects a call's arguments. It retypes what the schema names a type for, then checks what that produced, so the model reads back everything still wrong in one report per turn.
- No tool checks its own arguments. A requirement that holds only for some values of a discriminator is stated with `allOf`/`if`/`then` in the tool's `.schema.json`; a rejection answers as `tool_call_failed` with `kind: schema_failed`.
- The loop rewrites each call to the registered name before emitting `tool_call_started`, so `Event` and `Stats` never split one tool across spellings. Both repairs reach the host as `tool_call_repaired` events carrying `tool_name`, `call_id`, and either `call_malformed` or `value_mistyped` as their `kind`.
- A name that resolves to nothing returns `tool_call_failed` with `kind: not_found` and a message naming every registered tool. Without that list each retry spends `max_schema_retries` until the task fails. A call the model wrote as text takes the same path.

## Every Directive Is a Catalogue Entry

**Text agentwerk sends the model to report a failure or correct its behavior is one entry in `prompts/directives/*.md`, named by a `pub const` beside it, and the function an agent takes through `Agent::directives` decides what it renders as. No call site writes one inline.**

```rust
Event::new(Event::TOOL_CALL_FAILED)
    .directive(EDIT_FILE_OLD_STRING_NOT_FOUND)
    .data(json!({ "kind": "execution_failed", "message": message }))
```

- `DirectiveStore::render(key, values)` hands that function the key, then binds `{name}` into what comes back, or into the catalogue text when it answers `None`. A store therefore varies a directive by which one it is, and the values reach the model through the template rather than through the function. It takes a `&str`, so the constants rather than the compiler are what keep the 94 call sites honest.
- The `directives!` macro declares each key once and emits the crate-private constant the render sites write, the `Directive` constant a host matches on, and the `ALL` entry, so the three cannot disagree. A test pairs `ALL` against the `## ` headings, which the macro cannot reach.
- One function decides every directive, rather than a table of per-key replacements. A host matches the key against the constants `Directive` carries, so a misspelled arm does not compile, and answers `None` to leave a key as it is.
- `Agent::directives` wraps the function in the crate-private `DirectiveStore`, which then travels the way `Knowledge` does: `ToolContext` carries it and the loop reads `context.agent.get_directives()`. Two agents in one process therefore word a failure differently, and no test needs a lock. A host sharing one function across agents passes the same `fn` or a cloned handle.
- Twenty-one keys render through `built_in` instead, composed where no agent is in reach: the 19 schema violations inside `Node::check`, `knowledge_index_truncated` inside `Knowledge::index`, and `result_schema_required` inside `Task::as_user_message`. Threading a store into any of the three would re-type public API.
- Binding is one pass, so a value carrying `{` is never read as a placeholder of its own. A `{name}` with no value renders as written, the rule `Agent::template` already states.
- Two categories stay out: a `SchemaParseError` and the "could not read its arguments" answer both name a mistake in the host's code, and no model reads them. The tags around an offloaded result stay in Rust too, since `cap_aggregate_outputs` reads the opening one back.
- The retry site binds `attempt`, `max_attempts`, `task`, and `agent` beside `detail`, so a replacement naming `{attempt}` or `{agent}` says how far into the budget a retry is, or which agent it addresses, without an event in reach.

## Finishing Is a Tool Call

**Agents finish tasks through one tool, `finish`. An optional `handover` argument additionally creates a child task; its presence is the only discriminator, so there is no second tool and no mode field.**

`finish` records the result through `Queue::set_result`, which owns the result-validation-and-logging contract, then transitions the task to `Finished`. The loop enforces the rule on every agent but an interactive one: a turn that ends without a `finish` call is rejected and retried.

- An interactive agent gets no `finish` at all, since ending the task would end the conversation. It pauses on a reply that calls no tool, and the host closes the task with `Queue::set_task_finished`. `bind_agent` is where the tool is registered, because only once the agent joins a queue is `interactive` final; registration only ever adds, so a host that registers `FinishTool` itself keeps it either way.
- With `handover`, `finish` also inserts a child task pinned to that agent or label, with the current task recorded as its `parent`. The child's body is the result or the caller's `task` (with `{parent_id}`, `{parent_result_path}`, and `{parent_result}` substituted, the result last so nothing it carries is expanded again), and always ends with the parent ID and its result file.
- The child is inserted BEFORE the parent finishes, so a concurrent `work_left` check can never see an empty queue between them. `TaskFinished` and `TaskFailed` are emitted synchronously from the transition, and a count of in-flight transitions keeps `work_left` true until every handler returns, so an `on_result` follow-up lands first as well.
- A turn that ends without a `finish` call pushes a corrective directive and retries, the same path a schema failure takes, bounded by `max_schema_retries`; exhaustion emits `PolicyViolated { MaxSchemaRetries, .. }` and `TaskFailed`. Both paths emit `SchemaRetried` first, and its `attempt` and `max_attempts` are bound into the directive that follows.
- `finish` holds one schema, not two: `FinishTool::from_schema` makes the task's document the tool's own `result` argument at claim time, so a result that misses it is rejected before the handler runs and one written as JSON text is decoded there. Its one own check: a handover needs a real result.
- An agent that must always chain cannot be forced to by its tool registry, since every `finish` accepts an optional `handover`. Its role prompt carries that requirement instead.
- `Status` transitions go through tasks-side helpers; the agent never writes status directly. `Failed` is reserved for system-driven outcomes: exhausted schema retries, exhausted missing-`finish` retries, and breached limits.

Schemas and results:

- `Task::schema(...)` attaches a `Schema` to one task. `Queue::set_schemas(&store)` binds a shared `SchemaStore` holding one schema per label, and `SchemaStore::label(label, document)` parses the document itself.
- `claim` reads the store once and writes the first match onto `Task::schema`, leaving a task that already carries one alone. Resolution happens there rather than in `insert` because a task the model filed gets its label there; `claim` ends in `save_task`, so the binding survives `load` and a resumed task keeps it.
- That is what gives a task nobody could build a contract: a handover child, or one the model filed through `tasks`. No tool takes a schema document, since a small model does not write nested schemas reliably.
- A handover validates its `result` against the parent's schema and carries none for the child, which takes one from its handover label at claim. A mismatch aborts before the child is inserted, so the operation stays atomic.
- `handover` and `task` are `finish`'s own arguments; the result is always `result`. A task schema declaring a property named `handover` needs no special case, because it sits inside `result`.
- A successful finish writes `<dir>/tasks/<id>/result.json` (`Queue::dir(d)`, default `./.agentwerk`) and attaches the same value, read back through `Task::result()`. The full task state goes to `task.json` on every transition, while the `task_finished` entry in `<dir>/events.jsonl` carries a copy under `data.result`. A finish without a result omits that key, keeping it distinct from a JSON `null` result. These writes are observational: errors are swallowed.
- Failures are the plural mirror of the single result. `Queue::emit_event` pushes events with a failure name onto `Task::errors`, so a task accumulates the failed requests and tool calls it saw. A failure is not a transition: an entry lands whether or not the task goes on to fail, so a `Finished` task can carry some. Nothing is written for it: the line is already in `<dir>/events.jsonl`, and `Queue::load` folds the failures back onto each task in the one pass it makes over the log for `Stats`. There is no per-task errors file and no second writer to keep in step.

## Knowledge Is Opt-In and Shareable Across Agents

**`Agent::knowledge(&store)` carries durable facts across every task an agent handles, across separate `start` and `finish` calls, and across process restarts. Off by default.**

- Per-task state is `Task::replies`, which the loop turns into each request's `Vec<Message>` through `Task::to_messages`. `Knowledge` is the separate cross-task layer, surfaced through `KnowledgeTool` and rendered into the system prompt.
- Two agents bound to the same `Arc<Knowledge>` share one bundle; two agents bound to different stores see independent knowledge.
- The store is an Open Knowledge Format (OKF) v0.1 bundle in `<dir>/knowledge/` (`BUNDLE_DIR`), which keeps it out of a co-located `Queue`'s files and keeps the recursive page walk inside the bundle.
- `index.md` is derived and never parsed back: on load the in-memory index is rebuilt by walking the page frontmatter (`rebuild_index_from_pages`), so a bundle dropped into `<dir>/knowledge/` seeds the store.
- Only the index is injected into the system prompt; the agent reads full pages on demand through `read`. The loop reads `Knowledge::index()` once at the top of `process_task`, so the prompt stays byte-stable across every turn and the provider's prefix cache survives mid-task writes. Writes become visible at the top of the next task.
- Knowledge is purely model-driven, and the tool description carries the policy (durable facts only, do NOT save task progress or TODOs). A page's `type` and `tags` are host-side concerns set through the `Page` API, not tool parameters.
- A character limit caps how much of the index the prompt lists, never what may be written. Past it the index names the absolute path to `index.md` instead, while `list` still returns the whole thing. It defaults to 12 000 and is configurable through `Knowledge::index_char_limit(count)`.

## Observer Chain, One Error Path

**`Event` reports state. `ProviderError` reports a failed provider contract. The two channels carry independent information.**

- Every state transition publishes an `Event`. A failed request fires both `ProviderError` and a matching `Event` (`RequestFailed`, `PolicyViolated`). `Event.name` is the sole semantic discriminator: caller-published built-in names receive the same hooks, statistics, and persistence behavior, but publication does not perform the associated transition.
- A tool returns one terminal `Event`. `tool_call_finished` carries `output` plus optional `output_path` and `repairs` in its data; `tool_call_failed` carries the stable `kind`, model-visible `message`, and optional top-level `directive`.
- A model-fixable failure (wrong arguments, schema mismatch, missing file) becomes a failed tool-result content block for the provider. It still fires `ToolCallFailed` but does not stop the run.
- Handlers MUST be cheap and non-blocking; the loop does not await them. The four `_async` twins are the exception, and the loop still never awaits: registering one only queues the event, and whichever `finish` is waiting drains it and awaits each handler on its own task. A handler that never returns therefore stalls the caller rather than an agent, and a `start()`-only host uses the blocking form.

`Queue::on_event(h)` pushes a handler onto an ordered chain. Every installed handler fires on every event, in installation order, and each is handed the queue and the same `&Event` rather than its own copy. When no handler is installed, `default_logger` runs in its place.

- Every other hook is built on that chain, so a host's logger and a hook coexist.
- The queue is the first parameter of every handler. That is what let the `create_task_on_*` and `edit_replies_on_event` hooks go: a handler files its own follow-up work through `queue.add_task(..)`, rewrites what the model reads next through `queue.edit_replies(&event.task_id, ..)`, and selects what it needs through `queue.find_*`, without an `Arc` into the queue that holds it.
- `on_result` filters to `task_finished` and unwraps the stored result, `on_failure` filters to failure names, and `on_task` filters to the three lifecycle names. The event name activates these semantic hooks regardless of who published it. Each hook resolves the task only when needed, avoiding a full reply copy for every text chunk.
- An event that announces a reply is emitted after that reply has landed in the store: `RequestFinished` after the assistant reply, `ToolCallFinished` and `ToolCallFailed` after the tool results of their turn. A handler therefore finds the message the event names, and what it rewrites through `queue.edit_replies` reaches the next request.
- `RunStarted`, `RunFinished { outcome }`, and a host-driven `set_failed` are emitted by the queue itself and arrive with an empty `agent_id`.

## New Observables Pick a Channel

**Each new signal goes on `Event`, on a typed error, or on both. Pick by what the signal describes.**

- Reached a state: `Event` only.
- Could not fulfil a contract: typed error in the matching domain.
- Both at once (terminal request failure, breached limit): define both. Share the payload type when observer-friendly (`PolicyViolation`); keep observer-hostile request detail in `RequestErrorKind`.
- Model-fixable failure: a `tool_call_failed` event; still recoverable.
- A public error enum carries `#[non_exhaustive]`, which covers new variants only: adding a field to an existing struct variant still breaks a caller that matches it without `..`, so prefer a new variant to widening an old one.

## Providers Own Their Client

**Each concrete provider owns an `Endpoint`, which owns a `reqwest::Client` directly. There is no transport abstraction beyond it.**

- The `ProviderLike` trait fulfils one contract: `respond`, drive one turn. Callers hold it as a `Provider`, a cloneable handle any implementer converts into.
- The request and response types are `pub` and documented, because implementing `ProviderLike` is supported and implementors name them.
- `ModelRequest.tools` carries the `Tool` values themselves rather than a second description of them, and the registry they come from is the task's own: `claim` clones the agent's registry and rebinds a `finish` already in it to `FinishTool::from_schema(task.schema)`. A provider never calls a tool's handler.
- Where a request goes, how long it may take, and what a non-2xx answer means are decided in `providers::endpoint`. Vendor code adds its own authentication headers and its own `classify_error`, and never retries: retry is request-level, using `Policy::max_request_retries` and `Policy::request_retry_delay`.
- Two protocols cover four configurations, and the `Protocol` trait is where that split lives: `AnthropicMessages` and `OpenAiChat` each supply a path, authentication headers, a request shape, a 400-body classifier, and a decoder. `mistral` and `litellm` name `OpenAiChat` against their own `Endpoint`, and every `respond` is one call to `provider::respond::<P>`.
- A provider decodes its own payloads and names which `ResponseBuilder` call each one is; `providers::stream` decides which block a fragment continues and when a `StreamEvent` fires. The number an endpoint attaches to a fragment routes tool calls only, and never sizes anything.
- A context window is looked up by model name in `providers::model`, not per vendor.

## The Lifecycle Is Three Verbs Over One Filter

**`start` starts, `finish_results(matches)` waits, and `cancel_tasks(matches)` stops. `finish_all_tasks()` and `cancel_all_tasks()` name the whole queue; `finish_result(matches)` keeps the query but returns one value.**

- `Queue::pending(matches)` is the scheduling definition of "not done yet", and both the main loop and `finish_results` ask it. AQL's `pending = true` means `Todo` or `InProgress` and not cancelled; the queue additionally excludes a task paused for a caller reply from a wait.
- `Queue::cancel_filters` marks current matches through `Task::cancelled` and marks later insertions while the run remains active. Claim and resume both reject that private flag. `start()` clears filters and flags, so unfinished tasks resume without persisting cancellation.
- A filter runs while the task store lock is held, so it MUST NOT call back into the queue: the same rule `find_task` and `find_tasks` carry.

## A String That Selects Anything Is AQL

**Every method taking a filter accepts a string, and the string is AQL: a query compiled by `Query::new` over tasks or `Query::<Event>::new` over recorded events. There is no second string meaning, and no method takes a bare label.**

```rust
tasks.find_tasks("scan");                    // label = scan
tasks.find_results("t-3");                // id = t-3
tasks.find_tasks("label IN (scan, report) AND status != Failed");
tasks.find_events("tool_call_failed AND created > -1h");
```

- One grammar, two field sets. The private `QueryField` trait names what a set must answer (`of`, `kind`, `is_optional`, `shorthand`, `label`, `tie_break`, and the `sort_unordered`, `canonical`, and `compare` it may override); `TaskField` and `EventField` implement it, and the tokenizer, `Parser<F>`, `Condition<F>`, `Sort<F>`, and `Compiled<F>` are shared. A third record type would be a third impl, not a second parser.
- `Query<R>` is a newtype over `Compiled<R::Field>`, which holds the condition tree and the sort key and does everything a parsed query does. `Queryable` maps a record to its field set and stays private, which is what keeps `QueryField` and the tree private; the three declarations that name it carry `#[allow(private_bounds)]`. `R` defaults to `Task`, so a task query is `Query` and an event query is `Query<Event>`, and only `default_status`, `and_status`, and `and_result` sit in a `Query<Task>` block.
- `Condition<F>` is a tree of `All`, `Any`, `Not`, one term per field, and `Test`, which holds a caller's closure. AQL is the only grammar a selection is written in, and a closure is a leaf of the same tree rather than a second kind of filter, so the queue holds one thing however the caller wrote it. Nothing outside the module builds a term: the crate reaches the tree through `and`, `default_status`, `and_status`, and `and_result`, all crate-private.
- A lone bare word is `id = <word>` when it is spelled `t-<digits>` and `label = <word>` otherwise, which is what keeps a label the shortest thing you can write. A label named like an ID needs `label = t-3`.
- `From<&str>` and `Matcher<R> for &str` are infallible by signature, so a string that does not compile panics, the way `ToolBuilder::schema` panics on a document the compiler refuses. `Query::new` returns a `Result` for a string built at run time, and the Python bindings raise `ValueError` rather than panicking across the binding.
- `Matcher<R>` holds one method, `into_query`. Every call compiles its filter once and reads the query from then on, so a string costs one parse per call rather than one per record. Taking `self` is what makes that possible, and it is why the trait is not object-safe.
- `ORDER BY` rides on the query rather than on the call, so the one string says both which tasks and in what order, and the `tasks` tool needs no second argument. A query that is nothing but an `ORDER BY` selects every task.
- `Compiled::sort` is how the order reaches the queue, and `QueryField::sort_unordered` is what a query without `ORDER BY` falls back to, so a closure answers in creation order the way it always did. `find_tasks` and `find_results` both pass one `Query` to `matching_tasks`, filter and order together.
- `cancel`, `finish`, and `pending` read `matches` alone, so an `ORDER BY` handed to them does nothing. Nothing there is ordered.
- `find_results` and `find_result` share `results_of`, which is `default_status(Finished)` plus `and_result`. `default_status` adds its term only when the tree mentions no status, and `Condition::Test` mentions no field, so a closure always takes the default. `and_result` is why `find_result` answers with the first match carrying a result rather than the first match. `finish` uses `and_status` instead, which adds `status = Finished` whatever the filter said.
- Two equalities on one single-valued field are a parse error rather than a query no task satisfies, and the message names `IN` as the fix. An absent field fails every comparison, so `label != scan` never reaches an unlabelled task and `IS EMPTY` is what does.
- The `tasks` tool takes the same syntax as its one `aql` argument, and answers a `QueryError` as `tool_call_failed` through `task_query_invalid`. Nothing on that path panics. The tool reads tasks only: the event set is a host API.
- A selection an agent's id or label goes into stays a closure. AQL has no way to bind a value, and both derive from a host-supplied label, so `agent = {id}` would let a label carrying `=`, a quote, or a space change the query. `resolve_current_id` and the loop's own claim filter are the two.
- `EventField::sort_unordered` leaves the log order alone, where `TaskField`'s sorts by creation, because `find_events` reads a file that is already in order and the task store is a map. That is also what lets `find_event` stop at the first match when the query names no order, instead of copying the whole log to sort it.
- The four time fields take `>`, `>=`, `<`, `<=` against a date, an offset back from now, or milliseconds. An offset resolves in `Query::new`, not in `matches`, so one compiled query answers one set however long it is held.

## The Run Names Its Own Ending

**`run_main_loop` decides when a run is over and announces it once, rather than whichever caller happens to await. A limit breached while the host is busy elsewhere still ends the run.**

- `Queue::run` is one `Arc<Run>` over a `watch` channel of three phases: `Working`, `Draining(reason)` while the agents stop, and `Finished(reason)` once `RunFinished` has been announced. The channel is both the value and the wake, and a run complete without a reason cannot be written.
- `ending_reason()` names `FinishReason::PolicyViolated(kind)` for a breached limit and `FinishReason::Cancelled` once a cancel leaves nothing claimable. An empty queue is not an ending: a host that called `start()` may still be filing work, and a paused task revives on the next reply.
- `FinishReason::Drained` is named by the `finish` that waited for it, and only when no task at all is still open. That is what keeps an interactive chat alive between turns.
- The main loop joins its agents and emits `RunFinished { outcome }` before `Run::set_finished`, so a caller that starts another run never overlaps the previous one.
- Tools observe the ending through `ToolContext::cancelled`; pair it with `tokio::select!` so it drops the losing branch promptly. Dropping the `Queue` while agents still hold a `Weak` is the public way to abort: the upgrade fails and each task panics out cleanly.

## Every Event Is Logged, Statistics Are Folded From It

**`Queue::emit_event` is the single public and internal publication path. It stamps the time, resolves a known task's label, folds the event into crate-private `Stats`, and appends it to `events.jsonl`, `TextChunkReceived` aside. A host reads the log back through `Queue::find_events`; the crate counts only what a limit check needs.**

- Publication preserves one order: statistics, live streams, persistence, failure attachment, then handlers. The line is written before observers fire, so the log holds what every handler saw. One line per streamed token would outweigh every other line and repeats what `replies.jsonl` already carries, which is why `text_chunk_received` is excluded.
- `Event::<built_in>(...)` creates a built-in record with its canonical data shape. `Event::new(name)` remains the generic application-event constructor. `.data(value)`, `.task_id(id)`, and `.agent_id(id)` replace those values; task and agent context are optional and independent. Built-in names use lowercase snake case; application names are stored unchanged and matched exactly.
- New log lines store `name` and `data`; the loader also accepts legacy lines that used `event` with flattened built-in data.
- The write is best-effort, and a line this build cannot parse is skipped rather than costing every line after it, so a log written by another version still reports.
- `find_events(condition)` reads the log; a count is `.len()` and any breakdown is a fold. `get_input_tokens()`, `get_output_tokens()`, and `get_duration()` stay off the counters, because the limit check reads the same ones every 50ms.
- The two sources can disagree, by design: delete the log mid-run and the three totals keep reporting while the finders find nothing.
- `RequestFinished` is the one kind whose payload the statistics read: its `usage` adds to the two token totals. Every other kind contributes its count and nothing else.
- `execution_duration` spans the first `TaskStarted` to the `RunFinished`, or to now while the run is going. `TaskStarted` is emitted from `claim`, so a host claiming a task without running the loop still starts the clock. `Queue::load` restarts it: `max_time` bounds the run resuming the session, not the one that wrote the log.
- Breakdowns are the host's, not the crate's. Per tool, per model, per file, per agent, or per label is a fold, on the `on_event` chain or over `find_events` afterwards, which is why an `Event` carries `agent_id`, `task_id`, and the task's `label` alongside its name and data.
- `Stats::record` is the single writer and takes the whole `Event`, so a live queue and `Stats::load` arrive at the same figures and a run never keeps a second set of counters. The per-task token series (`usage_for_task`) stays crate-internal, because compaction clears it and a caller would read a silently truncated series.

## Persistence Routes Through One Trait

**Every read and write in the crate goes through `Persist` in `persistence`, or through an inherent `append` on the type that owns its log. No domain module hand-rolls file IO; no module knows its file's name except the implementer.**

- `Persist` defines `save(&self, dir) -> io::Result<()>` and `load(dir, &Self::Key) -> io::Result<Self>`. `Task`, `Replies`, `TaskResult`, `Page`, and `Trajectory` implement it, each owning its own path layout under `tasks/<id>/`, `pages/`, or `trajectories/`.
- A task's result and its replies are both `#[serde(skip)]` on `Task` and spliced back in by `Task::load`, so each fact has one file.
- A value type the caller stores itself reaches its file through an inherent method that delegates to its own impl, never by publishing the trait: `Trajectory::save(dir)` and `Knowledge::pages().save(page)` are the two. Service bootstrap (`Queue::load`, `Knowledge::load`) uses the same `load` verb by convention.
- The two append-only logs own their filename on the type that reads them back: `Stats::append(dir, &Event)` writes `events.jsonl`, `Replies::append(dir, id, &Reply)` writes `tasks/<id>/replies.jsonl`. Neither earns a trait, since the second takes an ID the first does not.
- `Replies` also implements `Persist`, whose `save` overwrites the file wholesale so a dropped or redacted reply leaves nothing behind.
- `t-<N>` IDs are handed out in order. `load()` seeds the next ID from the tasks it just read off disk; a queue built with `new()` scans for the highest existing ID at the first insert instead.
- One agent processes one task at a time, so `add_reply` and the rewrite for one ID are sequential within a single loop task. No per-ID lock is needed.
- `write_atomic` (tmp plus rename) and `append_line` (`O_APPEND` plus newline) are the only places that touch the filesystem. They are `pub(crate)`, and by convention nothing outside a `Persist` impl or an `append` reaches for them. One documented exception: `Queue::write_tool_output` writes single-shot flat files that fit neither trait.
- Vocabulary is fixed: `save`, `load`, `append`. Bootstrap verbs other than `load` (such as `open`) are not used, and `checkpoint`, `snapshot`, `counter`, and `persist` do not appear in identifiers or test names.

## Policy Is Per-Queue, Checked at Turn Boundaries

**A run stops cleanly when any limit on `Policy` is breached. The check emits `Event::POLICY_VIOLATED` and exits the per-agent task.**

- The loop calls `policy_violated` at each iteration; a non-`None` return takes the agent off the queue.
- Token budgets read from the queue's live `Stats`; `max_time` reads from `Policy` and from `Stats::execution_duration()`. `get_finish_reason` reports the matching `FinishReason::PolicyViolated(violation)` once the run has ended.
- `Queue::set_policy` replaces the whole value, so a host builds one from `Policy::default()`, and `get_policy` reads back what it stored, including the clamped `compaction_threshold`.
- The schema-retry budget is applied per-task inside the result-writing path, not at the top of the loop.
- `compaction_threshold` rides on `Policy` for the same per-queue snapshot every limit gets, but it is a trigger rather than a limit: `policy_violated` ignores it, and reaching it costs a compaction, not the run.
