# Architecture

The invariants that govern orchestration, tools, providers, events, and durable state.

## Ownership and Concurrency

**A `Werk` owns shared execution state; each `Agent` processes one claimed task at a time.**

- Configure an `Agent`, bind it through `Werk::add_agent`, then drive the Werk with `start` or a `finish_*` method.
- Let `Werk::bind_agent` move tasks queued on an agent's private Werk into the shared Werk.
- Claim each task once and release Werk locks before `ProviderLike::respond` or a tool handler is awaited.
- Keep one `Werk` as the orchestration boundary; nested Werks are not supported.

## Assignment and Identity

**Use labels for assignment and generated IDs for ownership.**

- Match an unlabelled agent only to unlabelled tasks; match a labelled agent only to the same task label.
- Use a unique label when a task must target one agent; agents sharing a label form a pool.
- Let `Agent::get_id` assign `<label>-<n>` or `agent-<n>` and store that ID in `Task::assignee` when claimed.
- Recreate agents in the same label order when resuming a session, because started tasks resume only on the same generated ID.

## Queries and Lifecycle

**Use AQL for every string selection over tasks or events.**

- Accept `Matcher<Task>` in task selectors; reserve `Query<Event>` for recorded events.
- Treat a bare `t-<n>` as an ID and another bare word as a label; require `label = t-3` for an ID-shaped label.
- Use `Query::new` for runtime input so invalid AQL returns `QueryError`; infallible string conversions may panic.
- Define pending work as unfinished, uncancelled work selected by the query; a task paused for caller input does not keep a `finish_*` wait open.
- Keep cancellation scoped to the current run: `start()` clears cancellation without changing `Status`.

## Completion and Handovers

**Finish tasks through the completion engine owned by `EventTool` and wrapped by `FinishTool`.**

- Register `FinishTool` automatically for non-interactive agents; interactive agents pause and the host ends them with `Werk::set_task_finished`.
- Bind an object `Schema` directly as the finish arguments; keep scalar and unbound results in the legacy `result` envelope.
- Treat `EventTool`'s `task_finished` event as completion; every other published event remains observational.
- Create a configured `Agent::handover` child before marking its parent finished so the Werk never appears empty between them.
- Move `Status` only through task-store transitions; reserve `Status::Failed` for system-driven terminal outcomes.

## Tools and Corrections

**Validate and dispatch every tool call through the registered `Tool`.**

- Resolve the model's exact tool name first, then its lowercase hyphen-to-underscore form with one trailing `_tool` removed.
- Reject an ambiguous folded name instead of choosing one registered tool.
- Compile input rules through `Tool::schema` and validate arguments through `Schema::validate`; do not repeat schema checks inside each tool.
- Keep model-facing recovery text in `prompts/directives/*.md`; `DirectiveStore` applies exact per-agent overrides before rendering it.
- Emit `tool_call_repaired` when a name or value is corrected and `tool_call_failed` when the model must recover.

## Events and Hooks

**Route every observation through `Werk::emit_event`.**

- Use `Event.name` as the semantic discriminator for built-in and application events.
- Do not make publication mutate task state; completion through `EventTool` is the explicit exception.
- Append events to `events.jsonl` before handlers run, excluding `text_chunk_received`, and fold policy statistics from the same records.
- Keep synchronous handlers cheap; async hook variants are queued and drained by the waiting `finish_*` call.
- Build `on_result`, `on_failure`, and `on_task` on the ordered `on_event` chain so handlers coexist.
- Let an explicit directive keyed by a non-terminal `EventTool` event name replace its model-facing acknowledgement, binding the event's JSON data.

## Providers and Retries

**Keep vendor protocols behind `ProviderLike` and centralize HTTP behavior in `Endpoint`.**

- Let each concrete provider own an `Endpoint`; do not add another transport abstraction.
- Keep Anthropic and OpenAI message shapes behind the internal `Protocol` trait; reuse the OpenAI shape for Mistral and LiteLLM.
- Decode vendor payloads in each provider and assemble `ModelResponse` through the shared `ResponseBuilder`.
- Apply request retries in the agent request path through `Policy::max_request_retries`, never inside a provider.

## Persistence

**Let each persisted type own its path and encoding.**

- Implement `Persist` for values saved and loaded as a whole; use inherent `append` only for append-only logs such as `Stats` and `Replies`.
- Route whole-file writes through `write_atomic` and log writes through `append_line`.
- Store task metadata, replies, results, tool outputs, events, trajectories, and knowledge in separate files under the Werk directory.
- Keep automatic session writes best-effort so an I/O failure does not replace an in-memory task outcome.
- Return I/O failures from caller-driven operations such as `Trajectory::save` and `Knowledge::get_pages` mutations.

## Knowledge

**Keep `Knowledge` optional, durable, and shared only by explicit handle.**

- Treat the directory passed to `Knowledge::load` as the OKF v0.1 bundle root and rebuild `index.md` from page frontmatter on load.
- Inject only the index into the prompt; let `KnowledgeTool` read full pages on demand.
- Read the index once per task so writes become visible on the next task without changing an active prompt prefix.
- Cap injected index characters through `Knowledge::set_index_char_limit` without limiting stored page content.

## Policy and Run Ending

**Let `run_main_loop` announce one terminal `FinishReason` after agents stop.**

- Check `Policy` limits at turn boundaries and emit `policy_violated` before ending the run.
- Keep schema retries per task; treat `compaction_threshold` as a trigger, not a terminal limit.
- Use `FinishReason::Drained` only when no open task remains, so an interactive task can continue across replies.
- Emit `run_finished` before allowing another run to begin on the same Werk.
