# Layout

Where code lives and the rules that govern placement.

## Crates

**Two crates: one library, one example set.**

- `crates/agentwerk/` is the library.
- `crates/use-cases/` holds runnable example binaries that depend on the library.
- Nothing in `use-cases` is re-exported by the library.

## Top-level files

**Each top-level source file is one concern the caller observes directly.**

- `lib.rs` holds public re-exports only. The crate root lands the orchestration surface plus the types its own signatures hand to callers: `Agent`, `TicketSystem`, `Ticket`, `Status`, `Knowledge`, `Policies`, `Stats`, `Schema`, `Event`, `EventKind`, `FinishReason`. Extension types live in `tools::`; `default_logger` lives in `event::`. Callers reach into a sub-module when they need anything below the orchestration level.
- `event.rs` defines `Event`, `EventKind`, `PolicyKind`, `FinishReason`, `ToolFailureKind`, `CompactReason`, and `default_logger`.
- `persistence.rs` holds the `Persist` and `Append` traits, the log types (`Results`, `TicketEvents`), and the shared `write_atomic` / `append_line` / `latest_path` / `parse_filename_ts` / `output_path` helpers. Every persistable type and the results-log writer (in `tools/tickets`) route through it. Internal (`pub(crate)`); not re-exported from `lib.rs`.
- The `agents/`, `prompts/`, `providers/`, `schemas/`, and `tools/` modules each own their domain. The `agents/` and `tools/` modules also re-export their headline types so `use agentwerk::agents::{Agent, TicketSystem}` and `use agentwerk::tools::BashTool` work without descending into leaf files.

## The `agents/` module

**Holds the per-agent builder, the ticket system, and the multi-agent loop.**

- `agent.rs` holds the `Agent` builder and ticket-dispatch helpers; an `Agent` carries a `Weak<TicketSystem>` bound at `bind_agent` time.
- `tickets/` holds the ticket value types and the orchestrator. `Reply` is the per-ticket transcript entry; `ReplyContent` mirrors `providers::ContentBlock` so the ticket surface stays free of provider types. Split by concern:
  - `tickets/mod.rs`: re-exports `Author`, `Reply`, `ReplyContent`, `Status`, `Ticket`, `TicketError`, `TicketSystem`; hosts free helpers `policy_violated`, `policy_violated_kind`, `now_millis`, `numeric_id`.
  - `tickets/ticket.rs`: `Ticket`, `Status`, the `Replies` transcript-log helper, and the `tickets/<key>/...` path helpers.
  - `tickets/reply.rs`: `Author`, `Reply`, `ReplyContent`, and their conversions to and from `providers::Message` / `ContentBlock`.
  - `tickets/error.rs`: `TicketError`.
  - `tickets/ticket_system.rs`: the `TicketSystem` struct, constructors, configuration, policy builders, ticket-creation API, agent binding, run lifecycle, results, and queries.
  - `tickets/store.rs`: the `impl TicketSystem` block for store mutations (`insert`, `claim`, `set_finished`, `summarize`, transition recording, etc.).
- `loop.rs` holds the `Runnable` trait (implemented by `TicketSystem`) and the per-agent loop driver.
- `knowledge.rs` holds `Knowledge`: the cross-ticket store, an OKF v0.1 bundle backed by a `pages/` directory of concept files and a derived `index.md`. Pages are curated through the `pages()` handle (`save` / `load` / `remove`) plus `clear`; failures are typed as `KnowledgeError`.
- `policy.rs` holds `Policies` and the limit checks the loop applies on each turn.
- `stats.rs` holds `Stats`, `LoopStats`, and the run-wide counters and timings.

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
- `bash.rs` is the shell tool (restricted via `new()`, unrestricted via `unrestricted()`).
- `tickets/` holds `ManageTicketsTool` and `ReadTicketsTool`.
- `knowledge.rs` is the model-facing wrapper around `Knowledge` (the store lives in `agents::knowledge`).
- `tool_search.rs` is the discovery surface for deferred tools.
- `web_fetch.rs` is the web fetch tool.
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
