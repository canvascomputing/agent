# Style

Naming, comment, and prose rules, plus README structure. Skim the section matching what is being written.

## Crate Root

**A type earns a `pub use` at `lib.rs` only when it names a concept in the one-sentence description of the crate, or when root-level signatures hand it to the caller.**

`Agent`, `AgentBuilder`, `TicketQueue`, `Ticket`, `Knowledge`, `Trajectory`, `Reply`, `Event`, `Status`, `EventKind`, `FinishReason`, `Schema`, `SchemaStore`

- Discriminants callers match on earn a root slot: `Status`, `EventKind`, `FinishReason`.
- Builder parameters and run outputs earn one when callers name them: `Schema`, `SchemaStore`, `AgentBuilder`, `Reply`, `Trajectory`.
- Errors and conversion traits do not. They live in their domain module.
- Free functions at the root are forbidden: convert to an associated function or move to the domain module.
- Name collisions at the root are forbidden; `ToolResult` next to `Result` is not acceptable.

## Where Non-Root Types Live

**Types live next to the abstraction, owner, or protocol they belong to.**

- Concrete implementations live with their abstraction: `Anthropic` under `providers::`, `CommandTool` under `tools::`.
- Companion types and handles live with their owner: `Ticket`, `Status`, `TicketError`, `Reply`, and `ReplyContent` under `agents::tickets`.
- Domain errors live with their domain: `ProviderError` under `providers::`, `TicketError` under `agents::tickets`.
- Request and response types live with the protocol: `ModelRequest`, `Message`, `TokenUsage` under `providers::`.
- Free functions live in their module, never at the crate root: the env readers in `providers::environment`, helpers in `tools::util`.

## Name Disambiguation

**Names are disambiguated through content, not through redundant prefixes.**

- Specific compound names stand alone: `TicketQueue`, `SchemaStore`, `PolicyKind`.
- A concrete LLM provider is named for its vendor alone. Acronyms follow Rust API guidelines, so `OpenAi`, not `OpenAI`.
- Two structs may not share a bare name within one module; both stay qualified.
- When a trait and the concrete type callers hold want the same name, the bare noun goes to the type and the trait takes a `Like` suffix: `Provider` / `ProviderLike`.

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
- Struct form: `AgentError::PolicyViolated { kind, limit }`, and also when a single field name carries meaning the type alone does not.
- Two-arm result enums use one word per variant: `Success` and `Error`, with no `is_*` predicates.

## Discriminant Members

**A fieldless discriminant exposes `name()`, and `Display` prints what `name()` returns.**

```rust
ToolFailureKind::ExecutionFailed.name()   // "execution_failed"
```

- `name()` returns the stable snake_case spelling, which is also what serde reads and writes.
- No `as_str`, no `label`, and no second spelling of the same string.
- A `pub const ALL` is added only where the crate itself enumerates the variants: `EventName`, which the Python `EventName` class is built from.
- A variant carrying fields has no `name()`; `EventKind` reaches its through `event_name()`.

## Payload Fields

**One vocabulary is used across every error type.**

- Human-readable strings MUST be named `message: String`, never `error`.
- Wrapped underlying errors MUST be named `source`, as in `FooFailed { source: io::Error }`.
- Typed metadata uses descriptive names: `status`, `retryable`, `retry_delay`, `tool_name`, `retries`, `after_ms`, `action`, `slug`.
- A discriminant explaining why something happened is `reason`. `PolicyViolated` names its field `policy` instead, because `reason` next to `limit` reads as the limit's justification.
- IMPORTANT: never name such a field `kind`. `Event` already carries `kind`, so `event.data["kind"]` and `event.kind` would be unrelated values one word apart. The type may still be named `PolicyKind` or `ToolFailureKind`; only the field is constrained.

## RAII Guard Fields

**Fields held only for their `Drop` behavior use a plain name and `#[allow(dead_code)]`.**

- Name the field for its purpose: `guard`, `lock`, `permit`. The `_guard` form is not used.
- `rustc`'s `dead_code` lint flags such fields because neither `Clone` nor drop glue count as reads.
- `#[expect(dead_code)]` is preferred on Rust 1.81+: it self-removes if the field later does get read.

## Time-Typed Fields

**Public API fields MUST use `std::time::Duration`. The type is the unit.**

- No `_ms`, `_MS`, or `_seconds` suffix on public Rust API names; the Python binding takes a float `seconds` instead.
- Internal helpers and on-the-wire JSON may use raw integers where the protocol requires it: `timeout_ms` is acceptable inside a tool input schema.

## Path Identifiers

**A directory path uses `_dir`. A file path uses `_file`. The bare suffix `_path` is used only when the value can be either.**

- Directories: `results_dir`, `knowledge_dir`, `ticket_dir`, `working_dir`. Matches `std::fs::read_dir` and `std::env::current_dir`.
- Files: `page_file`. The value is always a concrete file on disk.
- `_path` is for genuinely ambiguous cases: input that could name either, or a value passed through as opaque.
- IMPORTANT: `folder` is never used; it has no std analog. Doc comments and environment labels may still say "working directory" in English prose.

## Count Identifiers

**A field or method that returns a count uses a bare plural noun. No `_count` suffix.**

`requests`, `tool_calls`, `turns`, `input_tokens`, `output_tokens`

- Event payloads follow suit: `usage` on `RequestFinished` carries token counts, not a `token_count`. Accessor methods mirror the field form.
- The `_count` suffix is reserved for the rare case where the plural would clash with a sibling collection field on the same type.
- The ban is on scalars: a map keyed by subject is `<subject>_counts()` for a bare count and `<subject>_stats()` for a struct.
- Bare `<subject>s()` stays reserved for a collection of the subject itself, which is why `find_events(condition)` hands back the events themselves.

## Optional Returns

**A value undefined over an empty population returns `Option`. A sum returns its zero.**

- `Option`: an average, a rate, and `execution_duration()`, which has no answer until execution starts.
- Not `Option`: `input_tokens()`, `event_count(event)`. Zero is the honest answer for a sum over nothing.
- A sum is named `total`: the bare noun reads as one subject's value, not the population's. When a sum and its mean travel together they are two fields of one struct, not accessors prefixed `total_` and `avg_`.

## Persistence Verbs

**Two traits cover every read and write in the crate. The trait dictates the verb; the implementer's type name binds the file location.**

- `Persist` (in `persistence`): `save(&self, dir)` and `load(dir, &Self::Key)`. Service bootstrap (`TicketQueue::load`, `Knowledge::load`) uses the same `load` verb by convention.
- `append(dir, ..)` is the inherent verb on a type that owns an append-only log: `Stats` and `Replies`. Each encodes its own filename, so the wrong file cannot be reached through the wrong type.
- No `open` for bootstrap, no `write_X_to_dir`, no `to_json` or `from_json`, and no `checkpoint`, `snapshot`, `persist`, or `counter` in names.
- Function names do not embed the type names of their arguments: `Stats::append(dir, &event)`, not `append_event`.

## Builders

**Builder methods are bare nouns. No `with_` prefix.**

`.name()`, `.model()`, `.tool()`, `.label()`, `.concurrent()`

- The `with_` prefix is reserved for a bare name that would be ambiguous even with an inherent and trait split; no current builder needs it.
- A value the caller owns before execution consumes itself: `AgentBuilder` takes `mut self` and returns `Self`, which is also what lets its type-state track the filled provider and model slots.
- A type handed out as `Arc` configures through `&self` and returns `&Self`: `TicketQueue` and `Knowledge`. A third shape, `self: Arc<Self> -> Arc<Self>`, is not used.

## Constructors

**`new()` for the primary path. Named constructors carry semantics.**

- Named constructors: `load()`, `success()`, `error()`, `from_id()`, `from_env()`.

## Getters and Setters

**Mutable accessors use `set_` and `get_` prefixes to distinguish them from builders.**

- Example: `set_extension()`, `get_extension()`. Builder methods remain unprefixed.
- A public method returning `bool` is `is_<state>` or `has_<thing>`. A bare past participle such as `label_cancelled` reads as a field, not a question.
- `get_<name>` reads back a value a builder set where the bare noun would collide with the builder method on the same type: `TicketQueue::get_dir`, `Model::get_context_window`. A type whose builder is a separate type keeps the bare noun: `Tool::name`, `Agent::id`. A lookup by key keeps `get_` for the `HashMap::get` sense, which is why `get_ticket(key)` stands apart from `find_ticket(matches)`.

## Lifecycle

**Three verbs over one filter, each scoped verb paired with a whole-run form: `start` starts, `finish(matches)` and `finish_all()` wait, `cancel(matches)` and `cancel_all()` stop. A filter is `Fn(&Ticket) -> bool`, so the same call names one ticket or one pool.**

```rust
tickets.start();
tickets.finish(|t| t.has_label("scan")).await;   // one pool
tickets.finish_all().await;                      // the whole run
tickets.finish_last().await;                     // the whole run, one result
tickets.cancel(|t| t.has_label("scan"));         // one pool
tickets.cancel_all();                            // the whole run
```

- A verb takes a filter when it can mean part of the queue, and none when it cannot: `run` starts everything or nothing.
- IMPORTANT: the filter says WHICH tickets, never WHAT to wait for. `finish(|t| t.is_finished())` is a mistake that returns at once: the filter selects the tickets, and "no work left" is the condition, fixed.
- The whole-run case has exactly one spelling. `finish_all()` and `cancel_all()` are it; `|_| true` is never written at a call site.
- A filterless form earns its place only by naming how many results the caller wants back, never by re-spelling a filter. `finish_last()` waits exactly as `finish_all()` does and gives the last result in creation order.
- The two `_all` forms and `finish_last()` are the only filterless additions the family takes. Do not grow back the `cancel_label`, `cancel_on`, `cancel_*_on_*`, and `wait_for_*` methods a filter replaced.

## Hooks

**A hook's name says when it runs. `on_<trigger>` observes, `<action>_on_<trigger>` reacts.**

```rust
on_event(handler)                    // observe
create_ticket_on_result(make)        // react whenever the trigger matches
edit_replies(key, editor)            // act once, now
```

- `on_<trigger>(handler)` observes: the handler sees every `<trigger>` and returns nothing.
- `<action>_on_<trigger>(..)` reacts whenever `<trigger>` matches. The action may be more than one word, so `create_ticket_on_result` reads as `create_ticket` plus `on_result`.
- A bare `<action>(..)` acts once, now: `cancel`, `edit_replies`. Every method carrying the `_on_` infix returns `&Self`.
- IMPORTANT: the trigger fixes the handler's parameters, the action fixes its return type. `_on_event` hands over `&Event`, `_on_result` a `&Ticket` and its validated `&Value`, `_on_failure` the `&Event` and the `&Ticket` it happened in. Observing returns `()`, `create_ticket*` returns `Option<Ticket>`.
- A hook reacts to something agentwerk produces. Anything the caller already holds needs no hook: to stop a pool on a verdict, `finish` for it and `cancel`.
- `on_ticket` sits outside the trigger grid, keying on a ticket rather than naming a trigger.
- The editor row is the one exception and holds three members: `edit_replies_on_event`, `edit_replies_on_compaction`, and `edit_directive_on_retry`. Compaction and the retry earn the last two because each is a moment agentwerk writes on the host's behalf. No `_on_result` or `_on_failure` sibling follows: an editor runs once per request over the batch of events since the previous one, and a failure is already reachable by matching `EventKind::ToolCallFailed` inside the batch.

## Editors

**An editor is `edit_<noun>`. Its last parameter is the `&mut` value it rewrites; anything before it is read-only context.**

- `edit_replies(key, FnOnce(&mut Vec<Reply>))`, `edit_replies_on_event(Fn(&[Event], &mut Vec<Reply>))`, `edit_directive_on_retry(Fn(&Event, &mut String))`.
- The value arrives holding what agentwerk would otherwise have used, so an editor that writes nothing keeps the default. No editor returns `Option<T>`: there is nothing left to signal.
- An async editor takes the value by move and returns the replacement: `edit_replies_on_compaction(Fn(Compaction, Vec<Reply>) -> Future<Output = ProviderResult<Vec<Reply>>>)`. Handing back what it was given is how it says it changed nothing, and the `Result` is there because an editor that calls the model can fail.
- A hook that rewrites a value is named for that value, not for its trigger alone. Naming it `on_<trigger>` alone reads as an observer and hides what it changes.
- IMPORTANT: an observer composes, an editor is singular. Installing a second editor replaces the first, like `dir` or `max_turns`, so stack edits inside a single editor.

## Python Bindings

**Every public Rust item has a Python counterpart of the same name. The transforms below are permitted; nothing else.**

- Type-state collapses: `AgentBuilder<P, M>` and `ToolBuilder<D, H>` fold into the class they build and take its name. The collapsed class validates at `build()`.
- `Duration` becomes a float named `seconds`, with the unit repeated in the docstring: `max_time(duration)` binds as `max_time(seconds)`. Every other parameter keeps its Rust name.
- A fieldless enum becomes its snake_case `Display` string. That `Display` impl is the single source, so the binding never formats a variant with `{:?}`.
- An enum whose variants carry fields becomes a class with a `kind` string, a `data` dict, and one static constructor per variant. `Event` and `ReplyContent` are the two.
- A builder method whose name collides with a reader on the same Python class becomes a constructor keyword argument, because a Python class cannot carry both. `Ticket` needs this for `label`, `schema`, and `parent`.
- A `&mut` editor becomes a callable that returns the replacement, or `None` to keep the current value, since Python cannot take a Rust `&mut`.
- A reader taking no argument becomes an attribute: `Agent::id()` is `agent.id`, `Ticket::key()` is `ticket.key`.
- IMPORTANT: no `with_` prefix in either language, and no transform beyond this list.

## Free Functions

**A free function is used only for one of five reasons. Otherwise the function lives on a type.**

Permitted:

- **Ambient state** has no receiver: timestamp helpers and similar utilities in `tools::util`.
- **Foreign-type constructors** cannot use an inherent `impl`: `build_client()` returns a `reqwest::Client`.
- **Module entry points** drive multiple types: `run_main_loop()` in `agents::r#loop`.
- **Higher-order utilities** take a function and wrap it: `with_file_lock(path, || ...)`.
- **Shared algorithm helpers** are called by two or more sibling types in the same module.

Forbidden:

- A free function that delegates to a single method on one type. Inline it as a method.
- A free constructor for a local type that already has an inherent `impl`.
- A free helper called from exactly one private method. Make it a private method or a nested `fn`.
- An associated function that takes no `self` and does not return `Self` or `Result<Self>`. Move it to the module.

Naming is `snake_case`. Tool structs keep the `{Name}Tool` suffix. The name the model calls is a separate namespace and takes no suffix: `read_file`, `grep`, `tickets`. It is written once, in the tool's `From<XTool> for Tool` conversion, and never at a call site: a host registers `ReadFileTool`, not the string.

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
- Additional paragraphs are added only for a constraint, invariant, or non-obvious semantic the caller can act on.
- Trivial getters, `Default::default`, `From` impls, and self-explanatory variants are left undocumented.
- Within one type, coverage is all-or-none: every member has a real doc comment, or none does.

## Module Docs (`//!`)

**Every file begins with a `//!` that states what the file contributes to the crate.**

- One sentence; two only when the second adds context the first cannot carry.
- State the problem the file solves, not the types it defines. Do not list the contents of the file.
- The `//!` stays even when the filename is already descriptive.

## Hiding Internal Types

**A type a public trait or extension point hands to callers is documented; a genuinely internal type is `pub(crate)`.**

- The request and response types under `providers::` are documented: implementing `ProviderLike` is supported, and implementors name them.
- `tools::ToolRegistry` is the example of the other case: callers reach it through `Agent::tool(..)` and never name the struct.
- `#[doc(hidden)]` is reserved for items a macro or trait forces `pub` that are useless even to implementors; there are currently none.

## Line Comments (`//`)

**Four reasons are allowed. Everything else is deleted.**

Allowed:

- Order-dependency or crash-safety, such as `Write mark BEFORE task file: crash-safe.`
- API quirk or workaround, such as `serde_json::Map is sorted alphabetically, so we format manually.`
- Non-obvious constraint, such as `Newest first so 'gpt-4' does not shadow 'gpt-4.1'.`
- Plain section label in a long function, on its own line above the block it introduces.

Not allowed:

- Restating what the code does on the same line.
- Task, PR, issue, or changelog references, and commented-out code.
- Stub or aspirational markers; use `unimplemented!(...)` or return `Ok(())`.
- IMPORTANT: no `TODO`, `FIXME`, or `NOTE`. Fix it or file an issue.
- Decorative banners of any kind: `// ── Title`, `// ==== Title ====`, `// ----- Title -----`.

## Tests

**Test names carry intent. Setup is not narrated.**

- A comment is justified only to pin an architectural invariant the test guards.
- A module-level `//!` describing the test file's scope is acceptable.

## Comment Examples

**The failure mode in each case is restating what the reader can already see.**

```rust
// GOOD: states what the file contributes
//! Runs many agents in parallel, each in its own tokio task, over one shared ticket store.
// BAD: lists contents
//! Agent loop.
//! - `run_main_loop`: entry point.

// GOOD: purpose and invariant
/// A ticket. Caller-settable fields: `task`, `label`, `schema`, `parent`. System-managed fields are set at insertion time.
// BAD: restates the name
/// The task field.

// GOOD: flags an order constraint
// Write mark BEFORE task file: crash-safe.
// BAD: restates the code, then decorates it
// Increment the counter.
// ── Parse the reply, append the assistant message
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
- Name the type's job, not its implementation: not "the shared work queue holding an `Arc<Mutex<..>>`", but "the core data structure of agentwerk for coordinating complex interactions".

## Abstraction Level

**In caller-facing text, describe what the caller gets, not how it works inside.**

- The reader may be new to agent concepts: write for them.
- Internal type names, private field names, and enum variant names do not belong in the README. The reference lives in the API docs.
- Internal mechanics do not appear in caller-facing rustdoc either: no `Weak<Self>` or `Arc<Self>`, no "stamps", no `record_*`, no lock ordering, no drain counts. They live in `agentdocs/architecture.md`.
- Accepted: `// run the task once and return the result`, "Transient provider error triggered a retry". Rejected: `// drive the loop`, `// one-shot`, "(carries typed `kind: RequestErrorKind`)".
- Jargon and internal terms are cut even when they are shorter.

## Punctuation

**No em dashes anywhere. A colon or a second sentence replaces one.**

- Applies to the README, rustdoc, agentdocs, and agent-facing prompt files alike.
- Numbers are spelled out with a space: "33 000 tokens", never "33K".
- No emoji, and no decorative banners.
- Prose is not hard-wrapped in Markdown files. Wrapping reflows a whole paragraph when one word changes.

## Terminology

**Word-level rules for caller-facing prose: rustdoc, README, and agentdocs (except where called out).**

Replaced:

- "worker" as a role noun becomes "agent"; "routed" and "routing" become "assigned" and "assignment".
- "transcript" becomes "replies" or "messages". The field is `replies`, so name it.
- "caps" as a noun becomes "limits"; imperative cells say "Limit X", not "Cap X".
- "counters" becomes "statistics"; "wall-clock" becomes "elapsed duration", "max time", or "time cap".
- "settle" and "settled" become "finish", "mark done", or "done"; "upsert" becomes "creates or replaces".
- "smoke test" becomes "high-signal set", "starting point", or "core checks"; "drift" becomes the specific verb, "safety margin", or "stays anchored".
- "park" and other vehicle metaphors become what actually happens: "stays `InProgress`", "is not re-claimed".
- "stamp", "trip", "walk off", and "mint" are internal metaphors for writing a timestamp, breaching a limit, releasing a ticket, and creating one. Use the plain verb.
- Rust async primitive nouns ("future", "closure", "predicate", "callback") become "another task that finishes", "a condition you supply", "your function". The Rust identifiers stay as identifiers.
- Abstract pronouns and fractions ("one half", "the other", "either side") leave the reader guessing. Name the subject: not "detect one half from the environment and override the other", but "read only the provider from the environment, or only the model".

Banned:

- "user" is not a domain concept. It names one thing only, the `Message::User` role in the exchange with the model.
- "finisher", in agent-facing prompts (role files, `*.tool.md`, directives) as much as in caller-facing prose. Name the tool, `finish` (rustdoc names the type `FinishTool`).
- "snapshot": say what the value is. "live" as an adjective for statistics: say *when* the value is available in plain English.
- "wire-protocol" and "wire-shaped": describe the types by what they carry.
- "ships", "ships with", "sensible defaults", "tuning", "various options": state one concrete fact, or list the identifiers and point at docs.rs. Do not dump every default value into prose either.
- "stream" and "streaming": say "print as it arrives", "forward", or "show live", or name the SSE layer when describing the implementation.
- "drives the provider/tool loop" is slang. An agent calls the LLM provider and runs the tools it requests.
- "the loop" and "the agent loop": say "agentwerk", "the agent", or name the subject. The phrase is fine in `agentdocs/architecture.md` and `agentdocs/layout.md`.
- "header" and "ticket header" for the on-disk file holding a `Ticket` without its `replies`: say "the ticket" or "the ticket without its messages". The internal helper `ticket_header_path` and `architecture.md` may keep the term.

Also:

- Bare "provider" is spelled "LLM provider". Identifier names stay unqualified.
- "execution" is the word for a run, in prose and in identifiers: `TicketQueue::execution_duration()`. `run` survives only where it names the event itself, `EventKind::RunStarted` and `RunFinished`.
- The Knowledge store is described as durable memory the agent shares across tickets and other agents; the sharing is the headline, not a footnote.

## README Structure

**Terse, example-driven, scannable.**

- Fixed section order: Why use agentwerk?, Installation, Quick Start, Agent Swarms, Demo, the API sections, Use Cases, Development.
- The opening section is one bullet per reason to reach for the crate: `**Reason:** one short sentence`. A reason with nothing behind it is marketing and is cut. It runs before the reader has met a single agentwerk concept, so it carries no identifier, no type or function name, and none of the domain vocabulary the API sections introduce. "Task" and "agent" are the only nouns assumed.
- API sections run in the order a new reader needs them: Agents, Tickets, Tools, Events, Knowledge. Sessions is a subsection of Tickets.
- Prompting has no section of its own. `role`, `task`, and the template bindings configure an agent, so they live in Agents next to `name` and `tool`.
- Every section leads with one minimal example, then at most three sentences. Facts live in one place; other sections cross-link rather than repeat.

## README Folds

**Above the fold is what a reader needs. The exhaustive reference goes inside a `<details>` block at the end of the section.**

- Nothing is deleted, only folded. A method that exists is documented somewhere, or the fold is not doing its job.
- Folds are the last thing in a section. Every `<summary>` reads `All <what the fold holds>`: `All event kinds`, `All hooks`, `All session files`. One section holds one fold; a second catalogue earns its own `h3` with its own lead example.
- IMPORTANT: a blank line after `</summary>` and before `</details>`. `details` opens a raw-HTML block, so without the blank line every table inside renders as literal pipe characters on GitHub, crates.io, and PyPI alike.
- Snippet budgets: eight lines for a section lead, five for a subsection lead. Quick Start gets sixteen.
- Agent Swarms is the one exception and runs long, because it is the only place a whole system is shown at once: a pool working in parallel, a second pool the first hands tickets to, and one knowledge store between them.

## README Mechanics

**Formatting choices that are not about either language.**

- One `h1` per file, the title. Every section is `h2`, every subsection `h3`. No wrapper heading above a group of sections.
- `h2` is Title Case, `h3` is Sentence case.
- A method placeholder is spelled as what the caller passes, never a single letter: `max_turns(count)`, `cancel(matches)`. In a bullet list the bare method name carries no parentheses at all.
- Centered blocks use `<div align="center">`. `align` is not allowed on `<p>` by the crates.io sanitizer, so `<p align="center">` renders left-aligned there.

## README Tables

**No table above a fold. Every enumeration inside a fold is a table.**

- Above the fold there is prose and one example, so a table there is a sign the section is doing reference work it should have folded.
- Inside a fold the content is the reference, and a two-column grid is what a reader scans. Bullets there only hide the second column.
- A catalogue with categories takes a third column on the left holding the bold group label: the built-in tools, the event kinds, the execution methods, and the hooks.
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
- One axis order is chosen and held across every group. Hooks is the model: `event`, `result`, `failure`.
- Within a group, selectors run widest to narrowest: everything, then by label or agent, then by condition, then by key. Singular leads plural, and an action is followed by the query that reads it back: `cancel(matches)` then `is_cancelled(ticket)`.
- One table holds one receiver. A method on another type goes in the fold's trailing prose, which is why `TicketQueue::model_for_agent` is prose under the Providers fold rather than a fourth `AgentBuilder` row.
- The Execution fold holds everything that acts once over a run. The hooks fold holds only what registers a handler the queue calls back into on every matching event.

## README Examples

**Show the smallest snippet that demonstrates the feature.**

- Example models are `claude-haiku-4-5-20251001` or `claude-sonnet-4-20250514`.
- Update triggers: a new builder method, a new tool, a new event kind, a new environment variable, or a changed default.
- A chain of more than two calls breaks one call per line, even where it would fit the formatter's width.
- A code change edits only the doc sentences it made wrong. Surrounding prose is not rewritten unprompted.

## Rust and Python READMEs

**The Python README mirrors the Rust one section for section, carrying the same examples.**

- The heading lists of the two files match. A section in one is a section in the other.
- A snippet is a translation of its twin: same variable names, same string literals, same order of operations.
- A difference that is real belongs in the root `INVENTORY.md`, and the README shows it in the same place in both files.
- Only the Installation cross-links and the Development section differ in substance.
