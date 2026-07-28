# Layout

Where code lives and the rules that govern placement.

## Crates

**Three crates: one library, one binding layer, one example set.**

- `crates/agentwerk/` is the library.
- `crates/agentwerk-py/` is the Python binding layer, described below.
- `crates/use-cases/` holds runnable example binaries that depend on the library.
- Nothing in `use-cases` is re-exported by the library.

## The `agentwerk-py` crate

**One file per bound concept, mirroring the library. Naming rules live in [style.md](style.md).**

- `src/lib.rs` is the `#[pymodule]` and registers every class and function; `agent.rs`, `ticket.rs`, `ticket_system.rs`, `trajectory.rs`, `knowledge.rs`, `stats.rs`, `schema.rs`, `event.rs`, `providers.rs`, and `tools.rs` each bind the library module of the same name.
- `src/convert.rs` holds the only JSON boundary: `py_to_value` and `value_to_py` over `pythonize`, plus `runtime_error`.
- The compiled extension is `_agentwerk`; `python/agentwerk/__init__.py` re-exports it and holds the `@tool` decorator, the one piece of pure-Python logic. `__init__.pyi` declares the surface and MUST match the module, which `tests/test_parity.py` enforces.
- `examples/` holds runnable Python scripts, the counterpart of `crates/use-cases/` on the Rust side.
- `DIFFS.md` lists the whole public surface of both languages in one table, Rust next to Python. A public item missing from it, or a divergence its cells do not state, is a bug in one of the two.
- The crate is a workspace member but not a default member: `cargo build` and `cargo test` skip it because it links against a Python interpreter. Its commands live in [workflow.md](workflow.md).

## Top-level files

**Each top-level source file is one concern the caller observes directly.**

- `lib.rs` holds public re-exports only. The crate root lands the orchestration surface plus the types its own signatures hand to callers: `Agent`, `AgentBuilder`, `TicketSystem`, `Ticket`, `Status`, `Reply`, `Trajectory`, `Knowledge`, `Stats`, `Schema`, `Event`, `EventKind`, `FinishReason`. Extension types live in `tools::`; `default_logger` lives in `event::`. Callers reach into a sub-module when they need anything below the orchestration level.
- `event.rs` defines `Event`, `EventKind`, `PolicyKind`, `FinishReason`, `ToolFailureKind`, `CompactReason`, and `default_logger`.
- `persistence.rs` holds the `Persist` and `Append` traits, the log types (`Results`, `TicketEvents`), and the shared `write_atomic` / `append_line` / `latest_path` / `parse_filename_ts` / `output_path` helpers. Every persistable type and the results-log writer (in `tools/tickets`) route through it. Internal (`pub(crate)`); not re-exported from `lib.rs`.
- The `agents/`, `prompts/`, `providers/`, `schemas/`, and `tools/` modules each own their domain. The `agents/` and `tools/` modules also re-export their headline types so `use agentwerk::agents::{Agent, TicketSystem}` and `use agentwerk::tools::BashTool` work without descending into leaf files.

## The `agents/` module

**Holds the per-agent builder, the ticket system, and the multi-agent loop.**

- `agent.rs` holds the `Agent` builder and ticket-dispatch helpers; an `Agent` carries a `Weak<TicketSystem>` bound at `bind_agent` time.
- `tickets/` holds the ticket value types and the orchestrator. `Reply` is the per-ticket transcript entry; `ReplyContent` mirrors `providers::ContentBlock` so the ticket surface stays free of provider types. Split by concern:
  - `tickets/mod.rs`: re-exports `Author`, `Reply`, `ReplyContent`, `Status`, `Ticket`, `TicketError`, `TicketSystem`, `Trajectory`; hosts free helpers `policy_violated`, `policy_violated_kind`, `now_millis`, `numeric_id`.
  - `tickets/ticket.rs`: `Ticket`, `Status`, the `Replies` transcript-log helper, and the `tickets/<key>/...` path helpers.
  - `tickets/reply.rs`: `Author`, `Reply`, `ReplyContent`, and their conversions to and from `providers::Message` / `ContentBlock`.
  - `tickets/error.rs`: `TicketError`.
  - `tickets/ticket_system.rs`: the `TicketSystem` struct, constructors, configuration, policy builders, ticket-creation API, agent binding, run lifecycle, results, and queries.
  - `tickets/store.rs`: the `impl TicketSystem` block for store mutations (`insert`, `claim`, `set_finished`, `summarize`, transition recording, etc.).
  - `tickets/trajectory.rs`: `Trajectory`, a ticket's messages captured as a training example, its `trajectories/<key>.json` write, and the `.html` rendering written beside it.
- `loop/` holds the multi-agent loop, split by state:
  - `loop/mod.rs`: module wiring and the `Step` enum naming each state of the per-ticket state machine.
  - `loop/main.rs`: `run_main_loop`, which spawns one tokio task per registered agent and joins them on shutdown.
  - `loop/agent.rs`: `run_agent` (outer claim loop plus the inner `Step` match), `TicketContext`, the ticket check, and the silence retry.
  - `loop/compact.rs`: proactive and reactive transcript compaction.
  - `loop/request.rs`: the provider round-trip with retry and backoff.
  - `loop/tool_call.rs`: tool dispatch, output offloading, and the tool-failure budget.
- `knowledge.rs` holds `Knowledge`: the cross-ticket store, an OKF v0.1 bundle in `<dir>/knowledge/` backed by a `pages/` directory of concept files and a derived `index.md`. Pages are curated through the `pages()` handle (`save` / `load` / `remove`) plus `clear`; failures are typed as `KnowledgeError`.
- `policy.rs` holds `Policies` and the limit checks the loop applies on each turn.
- `stats.rs` holds `Stats` and the run-wide counters and timings.

## The `providers/` module

**Holds every concrete provider plus the shared transport types.**

- `provider.rs` defines `Provider`, `ModelRequest`, `ProviderToolDefinition`, and `ToolChoice`.
- `types.rs` defines `Message`, `ContentBlock`, `TokenUsage`, `AsUserMessage`, `ResponseStatus`, and `StreamEvent`.
- `anthropic.rs`, `openai.rs`, `mistral.rs`, and `litellm.rs` are concrete providers.
- `environment.rs` implements `from_env()` and `model_from_env()`.
- `stream.rs` holds the SSE parser; `error.rs` holds `ProviderError`, `ProviderResult`, and `RequestErrorKind`.

## The `tools/` module

**`tool.rs` holds the trait and registry; every other file is one built-in tool or a helper.**

- `tool.rs` defines `ToolLike`, `Tool`, `ToolRegistry`, `ToolContext`, and `ToolCall`.
- `read_file.rs`, `write_file.rs`, `edit_file.rs`, `glob.rs`, `grep.rs`, and `list_directory.rs` are filesystem tools.
- `code.rs` backs `grep`'s `syntax: "code"` shape matching, delegating to the `codegrep` engine.
- `bash.rs` is the shell tool (restricted via `new()`, unrestricted via `unrestricted()`).
- `tickets/` holds `ManageTicketsTool` and `ReadTicketsTool`.
- `manage_knowledge.rs` is the model-facing wrapper around `Knowledge` (the store lives in `agents::knowledge`).
- `find_tools.rs` is the discovery surface for deferred tools.
- `fetch_url.rs` is the web fetch tool.
- Each built-in tool pairs with a `<tool>.tool.md` definition: `---` frontmatter (`name`, `read_only`), a prose body shown to the model, and a `## Schema` section whose ` ```json ` fence holds the input schema. `tool_file.rs` parses it; `util.rs` is a shared helper; `error.rs` holds `ToolError`.

## The `prompts/` and `schemas/` modules

**Composable prompt assembly and JSON-Schema validation.**

- `prompts/builder.rs` and `prompts/section.rs` hold `PromptBuilder` and `Section`, which assemble role/context blocks.
- `schemas/mod.rs` holds `Schema`, `SchemaParseError`, and `SchemaViolation`.

## Tests

**Integration tests live in their own directory; everything else is inline.**

- `crates/agentwerk/tests/integration/` holds real-provider tests, bundled by `tests/integration.rs`.
- `tests/integration/common.rs` holds shared integration helpers.
- Every module also carries its own `#[cfg(test)] mod tests` for mock-free unit coverage.
