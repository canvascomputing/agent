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

- Nothing in `use-cases` is re-exported by the library.
- `use-cases` depends on the library, never the other way round.

## The `agentwerk-py` Crate

**One file per bound concept, mirroring the library. Naming rules live in [style.md](style.md).**

- `src/lib.rs` is the `#[pymodule]` and registers every class and function.
- `agent.rs`, `ticket.rs`, `ticket_queue.rs`, `reply.rs`, `trajectory.rs`, `knowledge.rs`, `schema.rs`, `event.rs`, `providers.rs`, and `tools.rs` each bind the library module of the same name. `reply.rs` also owns the two reply converters the editors on `TicketQueue` use.
- `src/convert.rs` holds the only JSON boundary: `py_to_value` and `value_to_py` over `pythonize`, plus `runtime_error`.
- The compiled extension is `_agentwerk`. `python/agentwerk/__init__.py` re-exports it and holds the `@tool` decorator, the one piece of pure-Python logic.
- `__init__.pyi` declares the surface and MUST match the module, which `tests/test_parity.py` enforces.
- `examples/` holds runnable Python scripts, the counterpart of `crates/use-cases/` on the Rust side.
- `DIFFS.md` lists the whole public surface of both languages in one table, Rust next to Python. A public item missing from it, or a divergence its cells do not state, is a bug in one of the two.
- The crate is a workspace member but not a default member: `cargo build` and `cargo test` skip it because it links against a Python interpreter. Its commands live in [workflow.md](workflow.md).

## Top-Level Files

**Each top-level source file is one concern the caller observes directly.**

- `lib.rs` holds public re-exports only: `Agent`, `AgentBuilder`, `TicketQueue`, `Ticket`, `Status`, `Reply`, `Trajectory`, `Knowledge`, `Schema`, `SchemaStore`, `Event`, `EventKind`, `FinishReason`.
- Extension types live in `tools::` and `default_logger` in `event::`. Callers reach into a sub-module when they need anything below the orchestration level.
- `event.rs` defines `Event`, `EventKind`, `EventName`, `PolicyKind`, `FinishReason`, `ToolFailureKind`, `CompactReason`, and `default_logger`, plus the crate-internal `Subject` and `Measure` that `EventKind::measures` returns.
- `persistence.rs` holds the `Persist` trait and the shared `write_atomic`, `append_line`, and `output_path` helpers. It is `pub(crate)` and not re-exported from `lib.rs`.
- The `agents/`, `prompts/`, `providers/`, `schemas/`, and `tools/` modules each own their domain. `agents/` and `tools/` also re-export their headline types, so `use agentwerk::agents::{Agent, TicketQueue}` and `use agentwerk::tools::CommandTool` work without descending into leaf files.

## The `agents/` Module

**Holds the per-agent builder, the ticket queue, and the multi-agent loop.**

- `agent.rs` holds the `Agent` builder and ticket-dispatch helpers; an `Agent` carries a `Weak<TicketQueue>` bound at `bind_agent` time.
- `knowledge.rs` holds `Knowledge`: the cross-ticket store, an OKF v0.1 bundle in `<dir>/knowledge/` backed by a `pages/` directory of concept files and a derived `index.md`. Pages are curated through the `pages()` handle (`save`, `load`, `remove`) plus `clear`; failures are typed as `KnowledgeError`.
- `policy.rs` holds `Policies` and the limit checks the loop applies on each turn, plus `compact_at`, the one entry that moves a trigger rather than limiting anything.
- `compaction.rs` holds the public `Compaction` handed to a compaction editor, the built-in summarizer that runs without one, and the threshold and chunking arithmetic behind both.
- `stats.rs` holds the crate-private `Stats`: the live counters a policy check reads, and the one reader over `events.jsonl`.

`tickets/` holds the ticket value types and the orchestrator. `Reply` is one entry in a ticket's replies; `ReplyContent` mirrors `providers::ContentBlock` and carries the same serde tags, so both serialize alike and the ticket surface stays free of provider types.

- `tickets/mod.rs`: re-exports `Author`, `Reply`, `ReplyContent`, `Status`, `Ticket`, `TicketError`, `TicketQueue`, `Trajectory`; hosts the free helpers `policy_violated`, `policy_violated_kind`, `now_millis`, `numeric_id`.
- `tickets/ticket.rs`: `Ticket`, `Status`, the `Replies` log helper, and the `tickets/<key>/...` path helpers.
- `tickets/reply.rs`: `Author`, `Reply`, `ReplyContent`, and their conversions to and from `providers::Message` and `ContentBlock`.
- `tickets/error.rs`: `TicketError`.
- `tickets/ticket_queue.rs`: the `TicketQueue` struct, constructors, configuration, policy builders, ticket-creation API, agent binding, run lifecycle, results, and queries.
- `tickets/store.rs`: the `impl TicketQueue` block for store mutations (`insert`, `claim`, `set_finished`, `edit_replies`, transition recording).
- `tickets/trajectory.rs`: `Trajectory`, a ticket's replies captured as a training example, its `trajectories/<key>.json` write, and the `.html` rendering written beside it.

`loop/` holds the multi-agent loop, split by state:

- `loop/mod.rs`: module wiring and the `Step` enum naming each action of the per-ticket state machine.
- `loop/main.rs`: `run_main_loop`, which spawns one tokio task per registered agent, decides when the run is over, joins them, and emits `RunFinished`.
- `loop/agent.rs`: `run_agent` (outer claim loop plus the inner `Step` match), `TicketContext`, the ticket check, and the silence retry.
- `loop/compact.rs`: proactive and reactive compaction of a ticket's replies, dispatched to the installed editor or the built-in summarizer.
- `loop/request.rs`: the provider round-trip with retry and backoff.
- `loop/tool_call.rs`: tool dispatch, output offloading, and the tool-failure budget.

## The `providers/` Module

**Holds every concrete LLM provider plus the shared request and response types.**

- `provider.rs` defines `ProviderLike`, the `Provider` handle over it, `ModelRequest`, `ProviderToolDefinition`, and `ToolChoice`.
- `types.rs` defines `Message`, `ContentBlock`, `TokenUsage`, `AsUserMessage`, `ResponseStatus`, and `StreamEvent`.
- `anthropic.rs`, `openai.rs`, `mistral.rs`, and `litellm.rs` are concrete providers.
- `environment.rs` reads the variables behind `Provider::from_env()` and `Model::from_env()`, and holds the `.env` parser and loader behind their `from_dot_env()` counterparts; its readers are crate-internal.
- `stream.rs` holds the SSE parser; `error.rs` holds `ProviderError`, `ProviderResult`, and `RequestErrorKind`.

## The `tools/` Module

**`tool.rs` holds the trait and registry; every other file is one built-in tool or a helper.**

- `tool.rs` defines `ToolLike`, `Tool`, `ToolRegistry`, `ToolContext`, and `ToolCall`.
- `read_file.rs`, `write_file.rs`, `edit_file.rs`, `glob.rs`, `grep.rs`, and `list_directory.rs` are filesystem tools.
- `code.rs` backs `grep`'s `syntax: "code"` shape matching, delegating to the `codegrep` engine.
- `command/` holds the command tool and the parsing behind it. `tool.rs` is the tool, restricted through `new()` and widened through `allow()`; it runs one program per call and never a shell. `parse.rs` splits a line into one command and classifies its arguments, which is how the tool refuses anything that is not one command and how a rule about a flag means what the program will mean.
- `tickets/` holds `TicketsTool` and `FinishTool`; `knowledge.rs` is the model-facing wrapper around `Knowledge`, whose store lives in `agents::knowledge`.
- `fetch_url.rs` is the web fetch tool.
- Each built-in tool pairs with a `<tool>.tool.md` definition: `---` frontmatter (`name`, `concurrent`), a prose body shown to the model, and a `## Schema` section whose ` ```json ` fence holds the input schema. `tool_file.rs` parses it; `util.rs` is a shared helper; `error.rs` holds `ToolError`.

## The `prompts/` and `schemas/` Modules

**Composable prompt assembly and JSON-Schema validation.**

- `prompts/builder.rs` and `prompts/section.rs` hold `PromptBuilder` and `Section`, which assemble role and knowledge blocks.
- `schemas/mod.rs` holds `Schema`, `SchemaParseError`, and `SchemaViolation`.
- `schemas/store.rs` holds `SchemaStore`, the label-keyed store a `TicketQueue` reads on each claim. It sits beside the compiler rather than inside it: binding a contract to a label is a separate concern from validating one.

## Tests

**Integration tests live in their own directory; everything else is inline.**

- `crates/agentwerk/tests/integration/` holds real-provider tests, bundled by `tests/integration.rs`.
- `tests/integration/common.rs` holds shared integration helpers.
- Every module also carries its own `#[cfg(test)] mod tests` for mock-free unit coverage.
