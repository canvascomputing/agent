# Style

Naming, comment, and prose rules, plus README structure. Skim the section matching what is being written.

## Crate Root

**A type earns a `pub use` at `lib.rs` only when it names a concept in the one-sentence description of the crate, or when root-level signatures hand it to the caller.**

`Agent`, `AgentBuilder`, `TicketQueue`, `Ticket`, `Knowledge`, `Stats`, `Trajectory`, `Reply`, `Event`, `Status`, `EventKind`, `FinishReason`, `Schema`

- Discriminants callers match on in their own code earn a root slot: `Status` (on `Ticket.status`), `EventKind` (on `Event.kind`), `FinishReason` (on `EventKind::RunFinished`).
- Errors and conversion traits do not earn a root slot. They live in their domain module.
- Builder parameters and run outputs do earn one when callers name them in their own code: `Schema` (on `Ticket::schema`), `AgentBuilder` (from `Agent::new`), `Reply` (on `Ticket.replies`), `Trajectory` (built from a ticket an `on_ticket` handler receives).
- Free functions at the root are forbidden: convert to an associated function or move to the domain module.
- Name collisions at the root are forbidden; `ToolResult` next to `Result` is not acceptable.

## Where Non-Root Types Live

**Types live next to the abstraction, owner, or protocol they belong to.**

- Concrete implementations live with their abstraction: `AnthropicProvider` under `providers::`, `BashTool` under `tools::`.
- Companion types and handles live with their owner: `Ticket`, `Status`, `TicketError`, `Reply`, and `ReplyContent` under `agents::tickets`; `Stats` and `ToolStat` under `agents::stats`.
- Domain errors live with their domain: `ProviderError`, `ToolError`.
- Request and response types live with the protocol: `ModelRequest`, `Message`, `TokenUsage` under `providers::`.
- Free functions live in their module, never at the crate root: `from_env()` in `providers::environment`, helpers in `tools::util`.

## Name Disambiguation

**Names are disambiguated through content, not through redundant prefixes.**

- Specific compound names stand alone: `TicketQueue`, `ToolStat`, `PolicyKind`.
- Vendor prefixes are used only to distinguish concrete LLM providers or tools: `AnthropicProvider`, `OpenAiProvider`, `LiteLlmProvider`.
- Acronyms follow Rust API guidelines: `OpenAi`, not `OpenAI`.
- Two structs may not share a bare name within one module; both stay qualified.

## Failure Variants

**Failure variants use passive-voice past-participle: `<Subject><Verb-ed>`.**

```rust
RequestFailed, TodoItemNotFound, ContextWindowExceeded, PolicyViolated   // accepted
InvalidRequest, UnexpectedStatus, MissingKey, RequestError               // rejected
```

- Rejected: adjective-first forms such as `InvalidX`, `UnexpectedX`, or `MissingX`.
- Rejected: noun-suffix forms such as `XError`; the `Error` suffix is reserved for the top-level `Error` and its domain sub-enums.
- State-transition events use the same form: `AgentStarted`, `RequestRetried`, `ContextCompacted`.
- Whether a failure is terminal is documented on the variant, not encoded in the name.

## Variant Shape

**Tuple for one payload. Struct for multiple fields or a meaningful field name.**

- Tuple form: `Provider(ProviderError)`, `TodoItemNotFound(String)`, `IoFailed(io::Error)`.
- Struct form: `AgentError::PolicyViolated { kind, limit }`.
- Struct form is also used when a single field name carries meaning the type alone does not.
- Two-arm result enums use one word per variant: `Success` and `Error`, with no `is_*` predicates.

## Payload Fields

**One vocabulary is used across every error type.**

- Human-readable strings MUST be named `message: String`, never `error`.
- Wrapped underlying errors MUST be named `source`, as in `FooFailed { source: io::Error }`.
- Typed metadata uses descriptive names: `status`, `retryable`, `retry_delay`, `tool_name`, `retries`, `after_ms`.
- A discriminant explaining why something happened is `reason`: `RunFinished`, `RequestFailed`, `RequestRetried`, `ToolCallFailed`, and the `Compaction*` variants all use it. `PolicyViolated` names its field `policy` instead, because `reason` next to `limit` reads as the limit's justification.
- IMPORTANT: never name such a field `kind`. `Event` already carries `kind`, so `event.data["kind"]` and `event.kind` would be unrelated values one word apart. The type may still be named `PolicyKind` or `ToolFailureKind`; only the field is constrained.

## RAII Guard Fields

**Fields held only for their `Drop` behavior use a plain name and `#[allow(dead_code)]`.**

- Name the field for its purpose: `guard`, `lock`, `permit`. The `_guard` form is not used.
- `rustc`'s `dead_code` lint flags such fields because neither `Clone` nor drop glue count as reads; the attribute acknowledges this on the one field that needs it.
- `#[expect(dead_code)]` is preferred on Rust 1.81+: it self-removes if the field later does get read.

## Time-Typed Fields

**Public API fields MUST use `std::time::Duration`. The type is the unit.**

- No `_ms`, `_MS`, or `_seconds` suffix on public API names.
- Internal helpers and on-the-wire JSON may use raw integers where the protocol requires it.
- Example: `timeout_ms` is acceptable inside a tool input schema, because the schema is the JSON shape sent to the model.

## Path Identifiers

**A directory path uses `_dir`. A file path uses `_file`. The bare suffix `_path` is used only when the value can be either.**

- Directories: `results_dir`, `knowledge_dir`, `ticket_dir`, `working_dir`. Matches `std::fs::read_dir`, `std::env::current_dir`, `std::fs::DirEntry`.
- Files: `page_file`, `tool_file`. The value is always a concrete file on disk.
- `_path` is for genuinely ambiguous cases: input that could name either, or a value passed through as opaque.
- IMPORTANT: `folder` is never used; it has no std analog.
- Doc comments and environment labels may still say "working directory" in English prose.

## Count Identifiers

**A field or method that returns a count uses a bare plural noun. No `_count` suffix.**

`requests`, `tool_calls`, `turns`, `input_tokens`, `output_tokens`

- `Stats` sets the vocabulary; event payloads follow suit: `usage` on `RequestFinished` carries token counts, not a `token_count`.
- Accessor methods mirror the field form: `Stats::input_tokens()` returns how many input tokens were recorded.
- The `_count` suffix is reserved for the rare case where the plural would clash with a sibling collection field on the same type.
- The ban is on scalars: a map keyed by subject is named for what its values are, `<subject>_counts()` for a bare count (`event_counts()`) and `<subject>_stats()` for a struct (`tool_stats()`). Bare `<subject>s()` stays reserved for a collection of the subject itself, so the map is not `Stats::events()`.

## Optional Returns

**A value undefined over an empty population returns `Option`. A sum returns its zero.**

- `Option`: `TimeStat::average`, `ToolStat::error_rate()`, and `execution_duration()`, which has no answer until execution starts.
- Not `Option`: `TimeStat::total`, `input_tokens()`, `event_count(event)`. Zero is the honest answer for a sum over nothing.
- One sentence covers averages, rates, and not-yet-started values, so per-accessor exceptions are not added.
- A sum is named `total`: the bare noun reads as one subject's value, not the population's. When a sum and its mean travel together they are the two fields of one struct, `TimeStat { total, average }`, rather than two accessors prefixed `total_` and `avg_`.

## Persistence Verbs

**Two traits cover every read and write in the crate. The trait dictates the verb; the implementer's type name binds the file location.**

- `Persist` (in `persistence`): `save(&self, dir)` and `load(dir, &Self::Key)`. Implemented by `Stats`, `Ticket`, `Page`. Service bootstrap (`TicketQueue::load`, `Knowledge::load`) uses the same `load` verb by convention.
- `Append` (in `persistence`): `append(dir, &Self::Record)`. Implemented by `Results` (`results.jsonl`) and `TicketEvents` (`tickets.jsonl`). Each implementer encodes its own filename, so the wrong file cannot be reached through the wrong type.
- No `open` for bootstrap, no `write_X_to_dir`, no `to_json` or `from_json`, and no `checkpoint`, `snapshot`, `persist`, or `counter` in names. The jargon these replaced is what the convention exists to keep out.
- Function names do not embed the type names of their arguments: `Stats::derive(&tickets)`, not `derive_from_tickets`. The argument type carries the meaning.

## Builders

**Builder methods are bare nouns. No `with_` prefix.**

`.name()`, `.model()`, `.tool()`, `.label()`, `.read_only()`

- The `with_` prefix is reserved for a bare name that would be ambiguous even with an inherent and trait split; no current builder needs it.
- Two chaining shapes exist, picked by whether the type is shared. A value the caller owns before execution consumes itself: `AgentBuilder` takes `mut self` and returns `Self`, which is also what lets its type-state track the filled provider and model slots.
- A type handed out as `Arc` configures through `&self` and returns `&Self`: `TicketQueue` and `Knowledge`. A third shape, `self: Arc<Self> -> Arc<Self>`, is not used.

## Constructors

**`new()` for the primary path. Named constructors carry semantics.**

- `new()` is the primary constructor.
- Named constructors: `load()`, `unrestricted()`, `success()`, `error()`, `empty()`, `from_id()`, `from_env()`.

## Getters and Setters

**Mutable accessors use `set_` and `get_` prefixes to distinguish them from builders.**

- Example: `set_extension()`, `get_extension()`.
- Builder methods remain unprefixed.
- A public method returning `bool` is `is_<state>` or `has_<thing>`: `is_finished`, `is_cancelled`, `is_label_cancelled`, `has_label`. A bare past participle such as `label_cancelled` reads as a field, not a question.
- `get_<name>` is reserved for reading back a value a builder set. A lookup by key keeps it for the `HashMap::get` sense, which is why `get_ticket(key)` stands apart from `tickets_for_label(label)`.

## Hooks

**A hook's name says when it runs and how much it stops. Trigger and scope are separate axes, and both are spelled out.**

```rust
on_event(handler)                    // observe
cancel_on_event(condition)           // react whenever the trigger matches
cancel()                             // act once, now
cancel_label_on_event(label, cond)   // react, scoped to one label
```

- `on_<trigger>(handler)` observes: the handler sees every `<trigger>` and returns nothing, as in `on_event`, `on_result`, and `on_failure`.
- `<action>_on_<trigger>(..)` reacts whenever `<trigger>` matches. The action may be more than one word, so `create_ticket_on_result` reads as `create_ticket` plus `on_result`.
- A bare `<action>(..)` acts once, now: `cancel`, `cancel_label`, `edit_replies`. The `_on_` infix names the trigger, and the return type says whether the call installs a standing rule or resolves once: `&Self` registers a handler, a value awaits a single match. `cancel_on_event` is a standing rule, `finish_on_event` awaits.
- `cancel_on_<trigger>` and `finish_on_<trigger>` are not synonyms. `cancel*` ends execution; `finish_on_*` hands back the first match and leaves execution running. Every `finish_on_*` row and doc comment closes on "and execution carries on", because the name alone suggests otherwise. The contrast is carried by the cells themselves, not by prose under the table.
- Scope crosses all three forms and lives in the prefix: `cancel*` ends execution, `cancel_label*` ends one label's pool and leaves the others running.
- IMPORTANT: the trigger fixes the handler's parameters, the action fixes its return type. A caller who knows one hook in a column knows them all: `_on_event` hands over `&Event`, `_on_result` a `&Ticket` and its validated `&Value`, `_on_failure` the `&Event` and the `&Ticket` it happened in. Observing returns `()`, `cancel*` returns `bool`, `create_ticket*` returns `Option<Ticket>`.
- Every trigger carries a reaction for every action, so the grid has no holes to explain. The three triggers cross `cancel`, `cancel_label`, `create_ticket`, and `finish_on`; `on_ticket` and `finish_on_ticket` sit outside it, keying on a ticket rather than naming a trigger.
- `finish_on_ticket` takes only the `&Ticket`, where `on_ticket` takes the `&Event` too. It also answers before any event arrives, by reading the tickets already in the store, and there is no event to hand over at that moment. That check is what lets it resolve on a state no transition announces.
- `<action>_on(value)` names no trigger, because the caller supplies the trigger whole instead of a condition over something agentwerk produces. `cancel_on` is the only one, and it is not renamed after a cancellation signal: `signal` already names the `AtomicBool` pair on `TicketQueue`.
- The editor row is the one exception, and it holds three members: `edit_replies_on_event`, `edit_replies_on_compaction`, and `edit_directive_on_retry`. Compaction and the retry earn the last two because each is a moment agentwerk writes on the host's behalf, so without a hook there the built-in summarizer and the built-in directive are the only ones anyone can have. Both still hand over the `&Event` their trigger names, so the parameter rule above holds: `_on_retry` is the `SchemaRetried` that says which of the two retry paths ran. No `_on_result` or `_on_failure` sibling follows: an editor runs once per request over the batch of events since the previous one, so an `_on_result` sibling would have no next request to act on and an `_on_failure` sibling would be a second rewriter of one ticket's replies, which the singular-editor rule below forbids. A failure is already reachable by matching `EventKind::ToolCallFailed` inside the batch.

## Editors

**An editor is `edit_<noun>`. Its last parameter is the `&mut` value it rewrites; anything before it is read-only context.**

- `edit_replies(key, FnOnce(&mut Vec<Reply>))`, `edit_replies_on_event(Fn(&[Event], &mut Vec<Reply>))`, `edit_directive_on_retry(Fn(&Event, &mut String))`.
- The value arrives holding what agentwerk would otherwise have used, so an editor that writes nothing keeps the default. No editor returns `Option<T>`: there is nothing left to signal.
- An async editor takes the value by move and returns the replacement instead: `edit_replies_on_compaction(Fn(Compaction, Vec<Reply>) -> Future<Output = ProviderResult<Vec<Reply>>>)`. Handing back what it was given is how it says it changed nothing, and the `Result` is there because an editor that calls the model can fail. A `&mut` would compile, the way `Tool::call` borrows across its await, but it forces a higher-ranked bound and a `Box::pin` at every call site, where by value a plain `async move` closure works. The Python transform is unchanged: return-the-replacement was already the sixth transform, so this needs no seventh.
- A hook that rewrites a value is named for that value, not for its trigger alone. Naming it `on_<trigger>` alone reads as an observer and hides what it changes.
- IMPORTANT: an observer composes, an editor is singular. Every handler on the `on_event` chain runs, and agentwerk stacks `cancel_on_event`, `on_ticket` and the rest there; installing a second editor replaces the first, like `dir` or `max_turns`. Two rewriters of one value would each see the other's output, so stack edits inside a single editor.

## Python Bindings

**Every public Rust item has a Python counterpart of the same name. Six transforms are permitted; nothing else.**

- Type-state collapses: `AgentBuilder<P, M>` and `ToolBuilder<H>` fold into the class they build and take its name, so the builder type has no Python counterpart. The collapsed class validates at `build()`.
- `Duration` becomes float seconds: the parameter keeps its name and the unit moves into the docstring.
- A fieldless enum becomes its snake_case `Display` string. That `Display` impl is the single source, so the binding never formats a variant with `{:?}`.
- An enum whose variants carry fields becomes a class with a `kind` string, a `data` dict, and one static constructor per variant. `Event` and `ReplyContent` are the two; a bare dict would make callers hand-build a tagged shape.
- A builder method whose name collides with a reader on the same Python class becomes a constructor keyword argument, because a Python class cannot carry both. `Ticket` needs this for `labels`, `schema`, and `parent`; nothing else does.
- A `&mut` editor becomes a callable that returns the replacement, or `None` to keep the current value, since Python cannot take a Rust `&mut`.
- IMPORTANT: no `with_` prefix in either language, and no seventh transform.

## Free Functions

**A free function is used only for one of five reasons. Otherwise the function lives on a type.**

Permitted:

- **Ambient state** has no receiver: timestamp helpers and similar utilities in `tools::util` or a sibling helper module.
- **Foreign-type constructors** cannot use an inherent `impl`: `build_client()` returns a `reqwest::Client`.
- **Module entry points** drive multiple types: `run_main_loop()` in `agents::r#loop`, `from_env()` in `providers::environment`.
- **Higher-order utilities** take a function and wrap it: `with_file_lock(path, || ...)`.
- **Shared algorithm helpers** are called by two or more sibling types in the same module: helpers in `tools::util` shared across filesystem tools, and provider-side helpers shared across concrete providers.

Forbidden:

- A free function that delegates to a single method on one type. Inline it as a method instead.
- A free constructor for a local type that already has an inherent `impl`. Constructors for `Foo` live on `Foo`.
- A free helper called from exactly one private method. Make it a private method or a nested `fn`.
- An associated function that takes no `self` and does not return `Self` or `Result<Self>`. Move it to the module as a free function. Exception: a per-variant static lookup where the `Type::` prefix partitions otherwise-colliding names, such as `AnthropicProvider::lookup_context_window_size` next to `OpenAiProvider::lookup_context_window_size`.

Naming is `snake_case`. Tool structs keep the `{Name}Tool` suffix: `ReadFileTool`, `BashTool`, `ManageTicketsTool`.

The name the model calls is a separate namespace and takes no suffix: `read_file`, `bash`, `manage_tickets`. It lives in the tool's `.tool.md` frontmatter, never as a Rust literal at a call site. A `_tool` suffix there restates what the tools array already says.

## Doc Comments (`///`)

**State the purpose in one sentence, with the same verb the item's README row uses.**

```rust
/// Set the LLM provider.            // builder: imperative configuration verb
/// Get the elapsed duration.        // accessor: Get
/// Begin processing tickets.        // action: imperative effect
/// A ticket finished successfully.  // event: past-tense state sentence
```

- Noun phrase for types and fields; verb for functions. No "This function…" or "Returns…".
- The full set of verbs is under [README Table Shape](#readme-table-shape). A method and its README row say the same thing in the same words.
- Additional paragraphs are added only for a constraint, invariant, or non-obvious semantic, and only when the caller can act on it.
- Trivial getters, `Default::default`, `From` impls, and self-explanatory variants are left undocumented.
- Within one type, coverage is all-or-none: every member has a real doc comment, or none does.

## Module Docs (`//!`)

**Every file begins with a `//!` that states what the file contributes to the crate.**

- One sentence; two only when the second adds context the first cannot carry.
- State the problem the file solves, not the types it defines.
- Do not list the contents of the file.
- The `//!` stays even when the filename is already descriptive.

## Hiding Internal Types

**A type a public trait or extension point hands to callers is documented; a genuinely internal type is `pub(crate)`.**

- The request and response types under `providers::` (`Message`, `ContentBlock`, `ModelRequest`, `ProviderToolDefinition`, `ToolChoice`, `StreamEvent`, `ModelResponse`, `ResponseStatus`) are documented: implementing `Provider` is supported, and implementors name them.
- A type that is genuinely internal becomes `pub(crate)` instead. `tools::ToolFile` is the example: callers go through `Tool::from_tool_file(definition: &str)` and never name the struct.
- `#[doc(hidden)]` is reserved for items a macro or trait forces `pub` that are useless even to implementors; there are currently none.

## Line Comments (`//`)

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

## Comment Examples

**Good and bad variants of each comment type.**

Module `//!`:

```rust
// GOOD: states what the file contributes
//! Runs many agents in parallel, each in its own tokio task, over one shared ticket store.

// BAD: lists contents
//! Agent loop.
//! - `Runnable`: trait.
//! - `run_main_loop`: entry point.
```

Doc comment `///`:

```rust
// GOOD: purpose and invariant
/// A ticket. Caller-settable fields: `task`, `labels`, `schema`, `parent`. System-managed fields are set at insertion time.
pub struct Ticket { ... }

// BAD: restates the name
/// The task field.
pub task: serde_json::Value,
```

Function `///`:

```rust
// GOOD: verb, one line
/// Build the environment metadata block for the first message.

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

## Sentence Shape

**One idea per sentence. A section lead is one sentence, and so is a table cell.**

```markdown
GOOD: A `Schema` constrains the result an agent produces for a ticket.
BAD:  A `Schema` is a JSON Schema document that gets attached to a ticket and,
      when the result comes back, is used to validate it; failures retry.
```

- Budgets: 10 to 25 words for a lead, 4 to 15 for a table cell. The cell figure is a hard cap.
- Present tense, declarative, no hedging. No semicolons stapling two facts together.
- A second short sentence is allowed only when it carries information the first cannot.
- A description that needs more than two sentences moves to prose under the table.

## Voice and Address

**Second person for the reader. Imperative for the agent. No marketing language.**

- "you" and "your" address the reader; "we" is never used.
- In agent-facing text the agent is the actor and the tool exposes the action: `Fetch a URL and read its body.`
- "Give the agent access to filesystem tools", not "empower the application".
- Avoid adjectives that carry no information ("powerful", "seamless", "sensible").

## Component Descriptions

**Introduce a type as `` `Type` `` plus one clause saying what it is for.**

```markdown
An `Agent` is the core entity of agentwerk.
A `Schema` constrains the result an agent produces for a ticket.
`Knowledge` allows agents to share insights or learnings.
```

- A second sentence is added only for a constraint: "A violation triggers a retry until `max_schema_retries` is exhausted."
- The type's `///` and its README lead use the same shape, so the two read alike.
- Name the type's job, not its implementation: not "the shared work queue holding an `Arc<Mutex<..>>`", but "the core data structure of agentwerk allowing to coordinate complex interactions".

## Abstraction Level

**In caller-facing text, describe what the caller gets, not how it works inside.**

- The reader may be new to agent concepts: write for them.
- Internal type names, private field names, and enum variant names do not belong in the README. The reference lives in the API docs.
- Internal mechanics do not appear in caller-facing rustdoc either: no `Weak<Self>` or `Arc<Self>` references, no "stamps", no "recorder protocol", no `record_*`, no lock ordering, no drain counts. They live in `agentdocs/architecture.md`.
- Accepted: `// run the task once and return the result`, "Transient provider error triggered a retry".
- Rejected: `// drive the loop`, `// one-shot`, "(carries typed `kind: RequestErrorKind`)", "`InBand` is model-fixable, `Infrastructure` is harness-level".
- Jargon and internal terms are cut even when they are shorter.

## Punctuation

**No em dashes anywhere. A colon or a second sentence replaces one.**

- Applies to the README, rustdoc, agentdocs, and agent-facing prompt files alike.
- Numbers are spelled out with a space: "33 000 tokens", never "33K".
- No emoji, and no decorative banners.
- Prose is not hard-wrapped in Markdown files. Wrapping reflows a whole paragraph when one word changes.

## Terminology

**Word-level rules for caller-facing prose: rustdoc, README, and agentdocs (except where called out).**

- "worker" is not used as a role noun. The type is `Agent`; the noun is "agent".
- "user" is not a domain concept. It names one thing only, the `Message::User` role in the exchange with the model.
- "routed" and "routing" are replaced with "assigned" and "assignment".
- "replies" or "messages" replace "transcript". The field is `replies`, so name it.
- "execution" is the word for a run, in prose and in identifiers: `Stats::execution_duration()`. `run` survives only where it names the event itself, `EventKind::RunStarted` and `RunFinished`.
- Bare "provider" in caller-facing prose is spelled "LLM provider". Identifier names (`Provider`, `AnthropicProvider`, the `providers::` module) stay unqualified.
- "finisher" is banned outright, in agent-facing prompts (role files, `*.tool.md`, directives) as much as in caller-facing prose. It names nothing an agent can call: name the tool, `finish` (rustdoc names the type `FinishTool`).
- "caps" is replaced with "limits" everywhere it is used as a noun. Imperative cells say "Limit X", not "Cap X".
- "snapshot" does not appear in caller-facing prose. Say what the value is, not that it is a snapshot.
- "counters" is replaced with "statistics" in caller-facing prose. `Stats` is statistics, not counters, on docs.rs.
- "live" as an adjective for statistics is rejected ("live counters", "readable live"). Say *when* the value is available in plain English.
- "wall-clock" is replaced with "elapsed duration", "max time", or "time cap".
- "stamp", "trip", "walk off", and "mint" are internal metaphors for writing a timestamp, breaching a limit, releasing a ticket, and creating one. Use the plain verb.
- "settle" and "settled" are replaced with "finish", "mark done", or "done".
- "park" and other vehicle metaphors are replaced by what actually happens: "stays `InProgress`", "is not re-claimed".
- "smoke test" is replaced with "high-signal set", "starting point", or "core checks".
- "drift" is replaced with the specific verb, or with "safety margin" or "stays anchored".
- "upsert" is replaced with "creates or replaces".
- "wire-protocol" and "wire-shaped" are not used. Describe the types by what they carry.
- The Knowledge store is described as durable memory the agent shares across tickets and other agents; the sharing is the headline, not a footnote.
- "drives the provider/tool loop" is slang. An agent calls the LLM provider and runs the tools it requests.
- "stream" and "streaming" are too technical for caller-facing prose. Say "print as it arrives", "forward", or "show live", or name the SSE layer when describing the implementation.
- "the loop" and "the agent loop" are project-internal jargon. In caller-facing prose say "agentwerk", "the agent", or name the subject directly. The phrase is fine in `agentdocs/architecture.md` and `agentdocs/layout.md`, where the audience already knows what it refers to.
- "ships" and "ships with" are empty filler; so are "sensible defaults", "tuning", and "various options". State one concrete fact, list the identifiers and point at docs.rs, or do both. Do not dump every default value into prose either; those numbers belong on docs.rs.
- Rust async primitive nouns ("future", "closure", "predicate", "callback") are jargon in caller-facing prose. Say "another task that finishes", "a condition you supply", "your function". The Rust identifiers stay as identifiers; only the prose changes.
- Abstract pronouns and fractions ("one half", "the other", "either side") leave the reader guessing. Name the subject directly: not "detect one half from the environment and override the other", but "read only the provider from the environment, or only the model".
- "header" and "ticket header" are project-internal jargon for the on-disk file holding a `Ticket` without its `replies`. In caller-facing prose say "the ticket" or "the ticket without its messages"; the internal helper `ticket_header_path` and `architecture.md` may keep the term.

## README Structure

**Terse, example-driven, scannable.**

- Fixed section order: Why use agentwerk?, Installation, Quick Start, Agent Swarms, Demo, Use Cases, the API sections, Development.
- The opening section is one bullet per reason to reach for the crate: `**Reason:** one short sentence` saying what agentwerk does to deliver it. A reason with nothing behind it is marketing and is cut.
- That section runs before the reader has met a single agentwerk concept, so it carries no identifier, no type or function name, no counted-surface claim, and none of the domain vocabulary the API sections introduce. "Task" and "agent" are the only nouns assumed.
- API sections run in the order a new reader needs them: Agents, Tickets, Tools, Events, Stats, Knowledge, Sessions.
- Prompting has no section of its own. `role`, `task`, and the template bindings configure an agent, so they live in Agents next to `name` and `tool`; a separate section only forced a forward reference out of the Agents lead.
- Every section leads with one minimal example, then at most three sentences.
- Facts live in one place; other sections cross-link rather than repeat.

## README Folds

**Above the fold is what a reader needs. The exhaustive reference goes inside a `<details>` block at the end of the section.**

- Nothing is deleted, only folded. A method that exists is documented somewhere, or the fold is not doing its job.
- Folds are the last thing in a section. Every `<summary>` reads `All <what the fold holds>`: `All event kinds`, `All hooks`, `All session files`. One section holds one fold; a second catalogue earns its own `h3` with its own lead example, as the hooks do under Events.
- IMPORTANT: a blank line after `</summary>` and before `</details>`. `details` opens a raw-HTML block, so without the blank line every table inside renders as literal pipe characters on GitHub, crates.io, and PyPI alike.
- Snippet budgets: eight lines for a section lead, five for a subsection lead. Quick Start gets sixteen.
- Agent Swarms is the one exception and runs long, because it is the only place a whole system is shown at once: a pool working in parallel, a second pool the first hands tickets to, and one knowledge store between them. Every line there earns its place by carrying one of those three, and anything that does not belongs in a section below.

## README Mechanics

**Formatting choices that are not about either language.**

- One `h1` per file, the title. Every section is `h2`, every subsection `h3`. No wrapper heading above a group of sections.
- `h2` is Title Case, `h3` is Sentence case.
- A method placeholder is spelled as what the caller passes, never a single letter: `max_turns(count)`, `cancel_on_event(condition)`, `results_for_label(label)`. In a bullet list the bare method name carries no parentheses at all, since the description says what it takes.
- Centered blocks use `<div align="center">`. `align` is not allowed on `<p>` by the crates.io sanitizer, so `<p align="center">` renders left-aligned there.

## README Tables

**No table above a fold. Every enumeration inside a fold is a table.**

- Above the fold there is prose and one example, so a table there is a sign the section is doing reference work it should have folded.
- Inside a fold the content is the reference, and a two-column grid is what a reader scans. Bullets there only hide the second column.
- A catalogue with categories takes a third column on the left holding the bold group label: the built-in tools and the event kinds.
- Prose still belongs in a fold when it is a caveat rather than an entry, as does a snippet showing how one of the entries is called.

## README Table Shape

**Each table picks one cell shape and holds it. Mixing shapes inside one table is a defect.**

- Builder rows lead with an imperative configuration verb: `Set the LLM provider.`
- Action rows lead with the imperative effect: `Begin processing tickets.`
- Accessor rows lead with `Get`: `Get every finished ticket's result.`
- Tool rows lead with the imperative action the agent takes: `Fetch a URL and read its body.`
- Event rows are past-tense state sentences: `A ticket finished successfully.`
- Every row is a description, not a constraint fragment. A cell that does not lead with a verb is a defect.
- A tool does not act; the agent does. The table intro carries that framing once so individual rows stay terse.
- The prose verb "Return" stays for instructions to the caller, as in "Return `ToolResult::error(message)` for a failure the model should work around".

## README Table Order

**Rows are grouped by subject, and one axis order is repeated in every group.**

- Groups do not interleave. The result rows run before the ticket rows in Results; the observers run before the cancels in Hooks.
- One axis order is chosen and held across every group. Hooks is the model: `event`, `result`, `failure`, in that order, in all of its groups.
- Within a group, selectors run widest to narrowest: everything, then by label or agent, then by condition, then by key.
- Singular leads plural where both exist, as `label(label)` and `labels(labels)` already do.
- An action is followed by the query that reads it back: `cancel()` then `is_cancelled()`, `cancel_label(label)` then `is_label_cancelled(label)`.
- One table holds one receiver. A method on another type goes in the fold's trailing prose, which is why `TicketQueue::model_for_agent` is prose under the Providers fold rather than a fourth `AgentBuilder` row.

## README Examples

**Show the smallest snippet that demonstrates the feature.**

- Example models are `claude-haiku-4-5-20251001` or `claude-sonnet-4-20250514`.
- Update triggers: a new builder method, a new tool, a new event kind, a new environment variable, or a changed default.
- A chain of more than two calls breaks one call per line, even where it would fit the formatter's width. Packed onto one line the calls stop being scannable.
- A code change edits only the doc sentences it made wrong. Surrounding prose is not rewritten unprompted.

## Rust and Python READMEs

**The Python README mirrors the Rust one section for section, carrying the same examples.**

- The heading lists of the two files match. A section in one is a section in the other.
- A snippet is a translation of its twin: same variable names, same string literals, same order of operations.
- A difference that is real belongs in `crates/agentwerk-py/DIFFS.md`, and the README shows it in the same place in both files.
- Only the Installation cross-links and the Development section differ in substance.
