# Layout

Where code lives and the rules that govern placement.

## Crates

**Three crates: one library, one binding layer, one example set.**

```
crates/
├── agentwerk/      the library
├── agentwerk-py/   the Python bindings
└── use-cases/      runnable example binaries
```

- `use-cases` depends on the library, never the other way round, and nothing in it is re-exported.

## The `agentwerk-py` Crate

**One file per bound concept, mirroring the library. Naming rules live in [style.md](style.md).**

- `src/lib.rs` is the `#[pymodule]` and registers every class and function. `agent.rs`, `task.rs`, `queue.rs`, `reply.rs`, `trajectory.rs`, `knowledge.rs`, `schema.rs`, `event.rs`, `providers.rs`, and `tools.rs` each bind the library module of the same name.
- `src/query.rs` binds `Query`, which covers both field sets: Python carries no type parameter, so the class compiles its string over tasks and over events at once and each call reads the compilation it needs.
- `src/convert.rs` holds the only JSON boundary: `py_to_value` and `value_to_py` over `pythonize`, plus `py_to_text` for a prompt argument and `runtime_error`.
- The compiled extension is `_agentwerk`. `python/agentwerk/__init__.py` re-exports it and holds the `@tool` decorator, the one piece of pure-Python logic. `__init__.pyi` declares the surface and MUST match the module, which `tests/test_parity.py` enforces.
- The root `INVENTORY.md` lists every declaration of both crates, Rust rows next to Python rows. A binding missing from it, or a divergence its cells do not state, is a bug in one of the two.
- The crate is a workspace member but not a default member: `cargo build` and `cargo test` skip it because it links against a Python interpreter. Its commands live in [workflow.md](workflow.md).

## Top-Level Files

**Each top-level source file is one concern the caller observes directly.**

- `lib.rs` holds public re-exports only. Extension types live in `tools::` and `default_logger` in `event::`.
- `event.rs` defines `Event`, `EventKind`, `EventName`, `PolicyViolation`, `FinishReason`, `ToolFailureKind`, `CompactReason`, and `default_logger`, plus the crate-internal `Subject` and `Measure` that `EventKind::measures` returns.
- `persistence.rs` holds the `Persist` trait and the shared `write_atomic`, `append_line`, and `output_path` helpers. It is `pub(crate)` and not re-exported.
- The root `INVENTORY.md` lists every declaration of both crates, one section per source file, public rows before internal ones. It changes in the same commit that adds, renames, removes, or re-types an item.
- The `agents/`, `prompts/`, `providers/`, `schemas/`, and `tools/` modules each own their domain. `agents/` and `tools/` re-export their headline types, so `use agentwerk::agents::{Agent, Queue}` works without descending into leaf files.

## The `agents/` Module

**Holds the agent, the task queue, and the multi-agent loop.**

- `agent.rs`: `Agent`, its configuration methods, and task-dispatch helpers.
- `compaction.rs`: the summarizer that compaction runs, and the threshold and chunking arithmetic behind it.
- `policy.rs`: the public `Policy`, what a run may spend, how it retries, and when it compacts.
- `knowledge.rs`: `Knowledge`, the cross-task store, an OKF v0.1 bundle in `<dir>/knowledge/`. Pages are curated through the `pages()` handle (`save`, `load`, `remove`) plus `clear`; failures are typed as `KnowledgeError`.
- `stats.rs`: the crate-private `Stats`, the counters a limit check reads and the one reader over `events.jsonl`.
- `query.rs`: AQL. The tokenizer, the parser, the private `Queryable`, `QueryField`, and `Compiled<F>`, and the two field sets: `TaskField` behind `Query<Task>`, `EventField` behind `Query<Event>`. `Matcher<R>` and `QueryError` live here too.

`tasks/` holds the task value types and the orchestrator:

- `mod.rs` re-exports them and hosts the free helpers `policy_violated`, `now_millis`, `numeric_id`.
- `task.rs`: `Task`, `Status`, the `Replies` log helper, and the `tasks/<key>/...` path helpers. `reply.rs`: `Author`, `Reply`, `ReplyContent`, and their conversions to and from `providers::Message` and `ContentBlock`. `error.rs`: `TaskError`.
- `queue.rs`: constructors, configuration, task creation, agent binding, run lifecycle, results, and queries. `store.rs`: the store mutations (`insert`, `claim`, `set_task_finished`, `edit_replies`, transition recording).
- `trajectory.rs`: `Trajectory`, a task's replies captured as a training example, its `trajectories/<key>.json` write, and the `.html` rendering written beside it.

`loop/` holds the multi-agent loop, split by state:

- `main.rs`: `run_main_loop`, which spawns one tokio task per registered agent, decides when the run is over, joins them, and emits `RunFinished`.
- `agent.rs`: `run_agent` (outer claim loop plus the inner `Step` match), `TaskContext`, the task check, and the silence retry. `mod.rs` names the `Step` enum itself.
- `compact.rs`, `request.rs`, `tool_call.rs`: compaction dispatched to the summarizer; the provider round-trip with retry and backoff; tool dispatch, output offloading, and the tool-failure budget.

## The `providers/` Module

**Holds every concrete LLM provider plus the shared request and response types.**

- `provider.rs` defines the behavior: `ProviderLike`, the `Provider` handle over it, the crate-internal `Protocol` trait, and the generic `respond` every provider answers through.
- `types.rs` defines every value the two sides exchange, in the order a turn happens: `ModelRequest`, `ReasoningEffort`, `Message`, `AsUserMessage`, `ContentBlock`, `ModelResponse`, `TokenUsage`, `ResponseStatus`, `ToolDeclineKind`, `StreamEvent`.
- `anthropic.rs`, `openai.rs`, `mistral.rs`, and `litellm.rs` are concrete providers, each a newtype over one `Endpoint` whose `respond` names a `Protocol`. `mistral.rs` and `litellm.rs` name `OpenAiChat` too, so the OpenAI request shape is written once.
- `endpoint.rs` holds `Endpoint`, the one HTTP call every provider makes. `environment.rs` reads the variables behind `Provider::from_env()` and `Model::from_env()`. `model.rs` holds `Model` and the one table of context window sizes.
- `error.rs` holds `ProviderError`, `ProviderResult`, `RequestErrorKind`, and the bank of upstream wordings a proxy wraps.
- `stream.rs` takes an HTTP response and gives back a `ModelResponse`: `read_reply`, the SSE reader, and `ResponseBuilder`, the one place a `StreamEvent` is emitted from. `frames.rs` recovers the calls a model wrote as prose rather than emitting through the tool channel.

## The `tools/` Module

**`tool.rs` holds the trait and registry; every other file is one built-in tool or a helper.**

- `tool.rs` defines `Tool`, `ToolRegistry`, `ToolContext`, and `ToolCall`.
- `read_file.rs`, `write_file.rs`, `edit_file.rs`, `glob.rs`, `grep.rs`, and `list_directory.rs` are filesystem tools; `code.rs` backs `grep`'s `syntax: "code"` shape matching, delegating to the `codegrep` engine. `fetch_url.rs` is the web fetch tool.
- `command/tool.rs` is the command tool, restricted through `new()` and widened through `allow()`; it runs one program per call and never a shell. `command/parse.rs` splits a line into one command and classifies its arguments, which is how the tool refuses anything that is not one command.
- `tasks/` holds `TasksTool` and `FinishTool`; `knowledge.rs` is the model-facing wrapper around `Knowledge`. `util.rs` is a shared helper.
- Each built-in tool pairs with a `<tool>.tool.md` definition (the prose shown to the model) and a `<tool>.schema.json` (the input schema). Both reach the tool through `include_str!` in its `From<XTool> for Tool` conversion, which is also where the name and concurrency are stated.

## The `prompts/` and `schemas/` Modules

**Composable prompt assembly and JSON-Schema validation.**

- `prompts/builder.rs` and `prompts/section.rs` hold `PromptBuilder` and `Section`, which assemble role and knowledge blocks.
- `prompts/directives.rs` holds `Directive`, the key namespace, the crate-private `DirectiveStore` carrying the function an agent decides its text with, and one `directives!` block declaring every key as a constant and an `ALL` entry. The text lives in `prompts/directives/*.md`, one file per area, each entry under a `## key` heading; a test pairs every key with its heading. It reaches the caller as the root re-export `agentwerk::Directive`.
- `prompts/text.rs` holds `Text`, the text a role, a description, or a task is set from, reading a file where the caller names a path. It reaches the caller as the root re-export `agentwerk::Text`.
- `schemas/mod.rs` holds `Schema`, `SchemaParseError`, and `SchemaViolation`.
- `schemas/store.rs` holds `SchemaStore`, the label-keyed store a `Queue` reads on each claim. It sits beside the compiler rather than inside it: binding a contract to a label is a separate concern from validating one.

## Tests

**Integration tests live in their own directory; everything else is inline.**

- `crates/agentwerk/tests/integration/` holds real-provider tests, bundled by `tests/integration.rs`, with shared helpers in `common.rs`.
- Every module also carries its own `#[cfg(test)] mod tests` for mock-free unit coverage.
