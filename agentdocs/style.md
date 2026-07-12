# Style

Naming and comment rules, plus README structure. Skim the section matching what is being written.

## Crate root

**A type earns a `pub use` at `lib.rs` only when it names a concept in the one-sentence description of the crate, or when root-level signatures hand it to the caller.**

- The current root: `Agent`, `TicketSystem`, `Ticket`, `Knowledge`, `Policies`, `Stats`, `Event`, `Status`, `EventKind`, `FinishReason`, `Schema`.
- Discriminants callers match on in their own code earn a root slot: `Status` (on `Ticket.status`), `EventKind` (on `Event.kind`), `FinishReason` (from `finish_reason()`).
- Errors and conversion traits do not earn a root slot. They live in their domain module.
- Builder parameters and run outputs do earn one when callers name them in their own code: `Schema` (on `Ticket::schema`).
- Free functions at the root are forbidden: convert to an associated function or move to the domain module.
- Name collisions at the root are forbidden; `ToolResult` next to `Result` is not acceptable.

## Where non-root types live

**Types live next to the abstraction, owner, or protocol they belong to.**

- Concrete implementations live with their abstraction: `AnthropicProvider` under `providers::`, `BashTool` under `tools::`.
- Companion types and handles live with their owner: `Ticket`, `Status`, `TicketError`, `Reply`, and `ReplyContent` under `agents::tickets`; `Stats` and `LoopStats` under `agents::stats`.
- Domain errors live with their domain: `ProviderError`, `ToolError`.
- Provider request and response types live with the protocol: `ModelRequest`, `Message`, `TokenUsage` under `providers::`.
- Free functions live in their module, never at the crate root: `from_env()` in `providers::environment`, helpers in `tools::util`.

## Name disambiguation

**Names are disambiguated through content, not through redundant prefixes.**

- Specific compound names stand alone: `TicketSystem`, `LoopStats`, `PolicyKind`.
- Vendor prefixes are used only to distinguish concrete providers or tools: `AnthropicProvider`, `OpenAiProvider`, `LiteLlmProvider`.
- Acronyms follow Rust API guidelines: `OpenAi`, not `OpenAI`.
- Two structs may not share a bare name within one module; both stay qualified.

## Failure variants

**Failure variants use passive-voice past-participle: `<Subject><Verb-ed>`.**

- Accepted: `RequestFailed`, `TodoItemNotFound`, `ContextWindowExceeded`, `PolicyViolated`.
- Rejected: adjective-first forms such as `InvalidX`, `UnexpectedX`, or `MissingX`.
- Rejected: noun-suffix forms such as `XError`; the `Error` suffix is reserved for the top-level `Error` and its domain sub-enums.
- State-transition events use the same form: `AgentStarted`, `RequestRetried`, `ContextCompacted`.
- Whether a failure is terminal is documented on the variant, not encoded in the name.

## Variant shape

**Tuple for one payload. Struct for multiple fields or a meaningful field name.**

- Tuple form: `Provider(ProviderError)`, `TodoItemNotFound(String)`, `IoFailed(io::Error)`.
- Struct form: `AgentError::PolicyViolated { kind, limit }`.
- Struct form is also used when a single field name carries meaning the type alone does not.
- Two-arm result enums use one word per variant: `Success` / `Error`, with no `is_*` predicates.

## Payload fields

**One vocabulary is used across every error type.**

- Human-readable strings MUST be named `message: String`, never `error`.
- Wrapped underlying errors MUST be named `source`, as in `FooFailed { source: io::Error }`.
- Typed metadata uses descriptive names: `status`, `retryable`, `retry_delay`, `tool_name`, `retries`, `after_ms`.

## RAII guard fields

**Fields held only for their `Drop` behavior use a plain name and `#[allow(dead_code)]`.**

- Name the field for its purpose: `guard`, `lock`, `permit`. The `_guard` form is not used.
- `rustc`'s `dead_code` lint flags such fields because neither `Clone` nor drop glue count as reads; the attribute acknowledges this on the one field that needs it.
- `#[expect(dead_code)]` is preferred on Rust 1.81+: it self-removes if the field later does get read.

## Time-typed fields

**Public API fields MUST use `std::time::Duration`. The type is the unit.**

- No `_ms`, `_MS`, or `_seconds` suffix on public API names.
- Internal helpers and on-the-wire JSON may use raw integers where the protocol requires it.
- Example: `timeout_ms` is acceptable inside a tool input schema because the schema is the on-the-wire JSON shape.

## Path identifiers

**A directory path uses `_dir`. A file path uses `_file`. The bare suffix `_path` is used only when the value can be either.**

- Directories: `scan_dir`, `session_dir`, `base_dir`, `data_dir`. Matches `std::fs::read_dir`, `std::env::current_dir`, `std::fs::DirEntry`.
- Files: `transcript_file`, `task_file`, `lock_file`. The value is always a concrete file on disk.
- `_path` is for genuinely ambiguous cases: input that could name either, or a value passed through as opaque.
- IMPORTANT: `folder` is never used; it has no std analog.
- Doc comments and environment labels may still say "working directory" in English prose.

## Counter identifiers

**Counters use a bare plural noun. No `_count` suffix on fields or on methods that return a count.**

- `Stats` sets the vocabulary: `requests`, `tool_calls`, `turns`, `input_tokens`, `output_tokens`.
- Event payloads follow suit: `usage` on `RequestFinished` carries token counts, not a `token_count`.
- Accessor methods mirror the field form: `Stats::requests()` returns the count of recorded requests.
- The `_count` suffix is reserved for the rare case where the plural would clash with a sibling collection field on the same type.

## Persistence verbs

**Two traits cover every read/write in the crate. The trait dictates the verb; the implementer's type name binds the file location.**

- `Persist` (in `persistence`) — `save(&self, dir)` / `load(dir, &Self::Key)`. Implemented by `Stats`, `Ticket`, `Page`. Service bootstrap (`TicketSystem::load`, `Knowledge::load`) uses the same `load` verb by convention.
- `Append` (in `persistence`) — `append(dir, &Self::Record)`. Implemented by `Results` (`results.jsonl`) and `TicketEvents` (`tickets.jsonl`). Each implementer encodes its own filename, so the wrong file cannot be reached through the wrong type.
- No `open` for bootstrap, no `write_X_to_dir`, no `to_json` / `from_json`, no `checkpoint`, `snapshot`, `persist`, or `counter` in names. The jargon these replaced is what the convention exists to keep out.
- Function names do not embed the type names of their arguments: `Stats::derive(&tickets)`, not `derive_from_tickets`. The argument type carries the meaning.

## Builders

**Builder methods are bare nouns. No `with_` prefix.**

- Examples: `.name()`, `.model()`, `.tool()`, `.label()`, `.read_only()`.
- The `with_` prefix is used only when a bare name clashes with a trait method, such as `with_description` on `BashTool`.

## Constructors

**`new()` for the primary path. Named constructors carry semantics.**

- `new()` is the primary constructor.
- Named constructors: `load()`, `unrestricted()`, `success()`, `error()`, `empty()`, `from_id()`, `from_env()`.

## Getters and setters

**Mutable accessors use `set_` and `get_` prefixes to distinguish them from builders.**

- Example: `set_extension()`, `get_extension()`.
- Builder methods remain unprefixed.

## Free functions

**A free function is used only for one of six reasons. Otherwise the function lives on a type.**

Permitted:

- **Ambient state** has no receiver: timestamp helpers and similar utilities in `tools::util` or a sibling helper module.
- **Foreign-type constructors** cannot use an inherent `impl`: `build_client()` returns a `reqwest::Client`.
- **Module entry points** drive multiple types: `run_main_loop()` in `agents::r#loop`, `from_env()` in `providers::environment`.
- **Higher-order utilities** take a closure and wrap it: `with_file_lock(path, || ...)`.
- **Shared algorithm helpers** are called by two or more sibling types in the same module: helpers in `tools::util` shared across filesystem tools; provider-side helpers shared across concrete providers.

Forbidden:

- A free function that delegates to a single method on one type. Inline it as a method instead.
- A free constructor for a local type that already has an inherent `impl`. Constructors for `Foo` live on `Foo`.
- A free helper called from exactly one private method. Make it a private method or a nested `fn`.
- An associated function that takes no `self` and does not return `Self` or `Result<Self>`. Move it to the module as a free function. Exception: a per-variant static lookup where the `Type::` prefix partitions otherwise-colliding names, such as `AnthropicProvider::lookup_context_window_size` vs `OpenAiProvider::lookup_context_window_size`.

Naming: `snake_case`. Tool structs keep the `{Name}Tool` suffix: `ReadFileTool`, `BashTool`, `ManageTicketsTool`.

## Doc comments (`///`)

**State the purpose in one sentence. No "This function…" or "Returns…".**

- Noun phrase for types and fields; verb for functions.
- Additional paragraphs are added only for a constraint, invariant, or non-obvious semantic.
- Trivial getters, `Default::default`, `From` impls, and self-explanatory variants are left undocumented.
- Within one type, coverage is all-or-none: every member has a real doc comment, or none does.

## Module docs (`//!`)

**Every file begins with a `//!` that states what the file contributes to the crate.**

- One sentence; two only when the second adds context the first cannot carry.
- State the problem the file solves, not the types it defines.
- Do not list the contents of the file.
- The `//!` stays even when the filename is already descriptive.

## Hiding implementor-only types

**Types that are `pub` only because they appear in a public trait signature get `#[doc(hidden)]`.**

- Examples: the request and response types under `providers::` (`Message`, `ContentBlock`, `ModelRequest`, `ProviderToolDefinition`, `ToolChoice`, `StreamEvent`, `ModelResponse`, `ResponseStatus`). Forced public by `Provider::respond`; irrelevant to anyone who is not implementing a `Provider`.
- `#[doc(hidden)]` keeps them reachable for implementors (`use agentwerk::providers::Message;` still works) while removing them from the rustdoc index.
- The standard Rust idiom: `tokio`, `serde`, and `tracing` do the same for items that exist only to satisfy a trait or a macro.
- A type that is genuinely internal (no public trait forces it `pub`) becomes `pub(crate)` instead. `tools::ToolFile` is the example: callers go through `Tool::from_tool_file(definition: &str)` and never name the struct.

## Line comments (`//`)

**Four reasons are allowed. Everything else is deleted.**

Allowed:

- Order-dependency or crash-safety, such as `Write mark BEFORE task file: crash-safe.`
- API quirk or workaround, such as `serde_json::Map is sorted alphabetically, so we format manually.`
- Non-obvious constraint, such as `Newest first so 'gpt-4' does not shadow 'gpt-4.1'.`
- Plain section label in a long function, on its own line above the block it introduces, such as `// Parse the reply, append the assistant message`.

Not allowed:

- Restating what the code does on the same line.
- Task, PR, issue, or changelog references.
- Commented-out code.
- Stub or aspirational markers; use `unimplemented!(...)` or return `Ok(())`.
- IMPORTANT: no `TODO`, `FIXME`, or `NOTE`. Fix it or file an issue.
- Decorative banners of any kind: `// ── Title`, `// ==== Title ====`, `// ----- Title -----`.

## Tests

**Test names carry intent. Setup is not narrated.**

- A comment is justified only to pin an architectural invariant the test guards.
- A module-level `//!` describing the test file's scope is acceptable.

## Comment examples

**Good and bad variants of each comment type.**

Module `//!`:

```rust
// GOOD: states what the file contributes
//! Multi-agent loop driver. Each agent runs in its own tokio task, reading the shared ticket store.

// BAD: lists contents
//! Agent loop.
//! - `Runnable`: trait.
//! - `run_main_loop`: entry point.
```

Doc comment `///`:

```rust
// GOOD: purpose and invariant
/// A ticket. Caller-settable fields: `task`, `labels`, `schema`, `assignee`. System-managed fields are set at insertion time.
pub struct Ticket { ... }

// BAD: restates the name
/// The task field.
pub task: serde_json::Value,
```

Function `///`:

```rust
// GOOD: verb, one line
/// Build the environment metadata block for the first user message.

// BAD: signature already says this
/// This function returns a String containing the environment metadata.
```

Line comment `//`:

```rust
// GOOD: flags an order constraint
// Write mark BEFORE task file: crash-safe.
fs::write(&mark_path, b"")?;

// GOOD: plain section header in a long function
// Parse the reply, append the assistant message

// BAD: restates the code
// Increment the counter.
counter += 1;

// BAD: decorative banner
// ── Parse the reply, append the assistant message
// ----- Core types -----
```

## README structure

**Terse, example-driven, scannable.**

- Fixed section order: Installation, Quick Start, Use Cases, API, Development.
- Every subsection leads with a minimal example, then explains.
- Enumerations use bullets or grouped bullets; tables are not used.
- Facts live in one place; other sections cross-link rather than repeat.

## README voice

**Direct and neutral. No marketing language.**

- "Give the agent access to filesystem tools", not "empower the application".
- Examples stay minimal; show the smallest snippet that demonstrates the feature.
- Example models are `claude-haiku-4-5-20251001` or `claude-sonnet-4-20250514`.
- Update triggers: a new builder method, a new tool, a new event kind, a new environment variable, or a changed default.

## Terminology

**Word-level rules for caller-facing prose: rustdoc, README, and agentdocs (except where called out).**

- "worker" is not used as a role noun. The type is `Agent`; the noun is "agent".
- "routed" / "routing" is replaced with "assigned" / "assignment".
- Bare "provider" in caller-facing prose is spelled "LLM provider". Identifier names (`Provider`, `AnthropicProvider`, the `providers::` module) stay unqualified.
- "finisher" is banned outright, in agent-facing prompts (role files, `*.tool.md`, directives) as much as in caller-facing prose. It names nothing an agent can call: name the concrete tool, `finish_ticket` or `handover_ticket` (rustdoc names the types `FinishTicketTool` / `HandoverTicketTool`). For the pair, say "the finish tools" or list both. Identifiers may carry "finish" (`TICKET_FINISH_TOOLS`), never "finisher".
- Internal mechanics do not appear in caller-facing rustdoc: no `Weak<Self>` / `Arc<Self>` references, no "stamps", no "recorder protocol", no `record_*` / `mark_finished`. They live in `agentdocs/architecture.md`.
- "caps" is replaced with "limits" everywhere it is used as a noun. Imperative cells say "Limit X" not "Cap X".
- "snapshot" does not appear in caller-facing prose. Say what the value is, not that it is a snapshot.
- "counters" is replaced with "statistics" in caller-facing prose. `Stats` is statistics, not counters, on docs.rs.
- "live" as an adjective for stats is rejected ("live counters", "readable live"). Say *when* the value is available in plain English.
- The Knowledge store is described as durable memory the agent shares across tickets and other agents; the sharing is the headline, not a footnote.
- "drives the provider/tool loop" is slang. An agent calls the LLM provider and runs the tools it requests.
- "stream" / "streaming" is too technical for caller-facing prose. Say "print as it arrives", "forward", "show live", or name the SSE layer when describing the implementation.
- "the loop" / "the agent loop" is project-internal jargon. In caller-facing prose say "agentwerk", "the agent", or name the subject directly. The phrase is fine in `agentdocs/architecture.md` and `agentdocs/layout.md`, where the audience already knows what "the loop" refers to.
- "ships" / "ships with" is empty filler; so are "sensible defaults", "tuning", "various options". State one concrete fact, list the identifiers and point at docs.rs, or do both. Do not dump every default value into prose either; those numbers belong on docs.rs.
- Rust async primitive nouns ("future", "closure", "predicate", "callback") are jargon in caller-facing prose. Say "another task that finishes", "a condition you supply", "your function". The Rust identifiers stay as identifiers (parameter names, type names); only the prose changes.
- Abstract pronouns and fractions ("one half", "the other", "either side") leave the reader guessing. Name the subject directly: not "detect one half from the environment and override the other", but "read only the provider from the environment, or only the model".
- "header" / "ticket header" is project-internal jargon for the on-disk file holding a `Ticket` without its `replies`. In caller-facing prose say "the ticket" or "the ticket without its transcript"; the internal helpers `ticket_header_path` and architecture.md may keep the term since the codebase audience knows what it means.

## README table shape

**Each table picks one cell shape and holds it. Mixing shapes inside one table is a defect.**

- Builder rows lead with an imperative verb describing the configuration effect: `Set the LLM provider.`, `Register a tool.`, `Restrict the agent to matching labels.`
- Action rows lead with an imperative verb describing the effect: `Create a task.`, `Run until interrupted.`
- Accessor rows lead with `Return ...`: `Return every finished ticket's result.`
- Tool rows lead with an imperative verb describing the action the tool exposes: `Read a file with line numbers.`, `Fetch a URL and read its body.` The tool itself does not act; the agent does. The table intro carries that framing once so individual rows stay terse.
- Event rows are past-tense state sentences: `A ticket finished successfully.`

## README cell length

**One sentence per cell. No semicolons stapling two facts together.**

- Hard cap: roughly fifteen words per cell.
- A second short sentence is allowed only when it carries information the first cannot.
- A description that needs more than two sentences moves to prose under the table.
- Em dashes are not used; colons or two sentences replace them.

## README descriptions

**In README table cells, bullet descriptions, and inline `//` comments, describe what the caller gets, not how it works inside.**

- The reader may be new to agent concepts: write for them.
- The README is an abstraction: internal type names, private field names, and enum variant names do not belong there. The reference lives in the API docs.
- Accepted: `// run the task once and return the result`, "Transient provider error triggered a retry".
- Rejected: `// drive the loop`, `// one-shot`, "(carries typed `kind: RequestErrorKind`)", "`InBand` is model-fixable, `Infrastructure` is harness-level".
- Jargon and internal terms are cut even when they are shorter.
