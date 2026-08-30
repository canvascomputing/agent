# Style

Naming, comment, and prose rules, plus README structure. Skim the section matching what is being written.

## Crate Root

**A type earns a `pub use` at `lib.rs` only when it names a concept in the one-sentence description of the crate, or when root-level signatures hand it to the caller.**

`Agent`, `Queue`, `Task`, `Policy`, `PolicyViolation`, `Knowledge`, `Directive`, `Text`, `Trajectory`, `Reply`, `Event`, `Status`, `FinishReason`, `Schema`, `SchemaStore`

- Discriminants callers match on earn a root slot: `Status`, `FinishReason`, `PolicyViolation`.
- Builder parameters and run outputs earn one when callers name them: `Schema`, `SchemaStore`, `Policy`, `Directive`, `Text`, `Reply`, `Trajectory`.
- Errors and conversion traits do not. They live in their domain module.
- Free functions at the root are forbidden: convert to an associated function or move to the domain module.
- Name collisions at the root are forbidden.

## Where Non-Root Types Live

**Types live next to the abstraction, owner, or protocol they belong to.**

- Concrete implementations live with their abstraction: `Anthropic` under `providers::`, `CommandTool` under `tools::`.
- Companion types and handles live with their owner: `Task`, `Status`, `TaskError`, `Reply`, and `ReplyContent` under `agents::tasks`.
- Domain errors live with their domain: `ProviderError` under `providers::`, `TaskError` under `agents::tasks`.
- Request and response types live with the protocol: `ModelRequest`, `Message`, `TokenUsage` under `providers::`.
- Free functions live in their module, never at the crate root: the env readers in `providers::environment`, helpers in `tools::util`.

## Name Disambiguation

**Names are disambiguated through content, not through redundant prefixes.**

- Specific compound names stand alone: `Queue`, `SchemaStore`, `PolicyViolation`.
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
- Struct form: `RequestRetried { attempt, max_attempts }`, and also when a single field name carries meaning the type alone does not.
- Two-arm result enums use one word per variant: `Success` and `Error`, with no `is_*` predicates.

## Discriminant Members

**A fieldless discriminant exposes `name()`, and `Display` prints what `name()` returns.**

```rust
event.get_data()["kind"]                 // "execution_failed"
```

- `name()` returns the stable snake_case spelling, which is also what serde reads and writes.
- No `as_str`, no `label`, and no second spelling of the same string.
- Built-in event names are associated constants on `Event`, with the same spelling in Rust and Python.

## Payload Fields

**One vocabulary is used across every error type.**

- Human-readable strings MUST be named `message: String`, never `error`.
- Wrapped underlying errors MUST be named `source`, as in `FooFailed { source: io::Error }`.
- Typed metadata uses descriptive names: `status`, `retryable`, `retry_delay`, `tool_name`, `retries`, `after_ms`, `action`, `slug`.
- A stable machine-readable category is `kind`; a human-readable explanation is `message`.
- An initiating condition is `trigger`, a successful termination value is `outcome`, and a breached policy is `policy`.
- IMPORTANT: do not overload one payload key across these meanings; generic event readers depend on the distinction.

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

- Directories: `results_dir`, `knowledge_dir`, `task_dir`, `working_dir`. Matches `std::fs::read_dir` and `std::env::current_dir`.
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

- `Option`: an average, a rate, and `get_duration()`, which has no answer until execution starts.
- Not `Option`: `get_input_tokens()`, `event_count(event)`. Zero is the honest answer for a sum over nothing.
- A sum is named `total`: the bare noun reads as one subject's value, not the population's. When a sum and its mean travel together they are two fields of one struct, not accessors prefixed `total_` and `avg_`.

## Persistence Verbs

**Two traits cover every read and write in the crate. The trait dictates the verb; the implementer's type name binds the file location.**

- `Persist` (in `persistence`): `save(&self, dir)` and `load(dir, &Self::Key)`. Service bootstrap (`Queue::load`, `Knowledge::load`) uses the same `load` verb by convention.
- `append(dir, ..)` is the inherent verb on a type that owns an append-only log: `Stats` and `Replies`. Each encodes its own filename, so the wrong file cannot be reached through the wrong type.
- No `open` for bootstrap, no `write_X_to_dir`, no `to_json` or `from_json`, and no `checkpoint`, `snapshot`, `persist`, or `counter` in names.
- Function names do not embed the type names of their arguments: `Stats::append(dir, &event)`, not `append_event`.

## Builders

**Builder methods are bare nouns. No `with_` prefix.**

`.name()`, `.model()`, `.tool()`, `.label()`, `.concurrent()`

- The `with_` prefix is reserved for a bare name that would be ambiguous even with an inherent and trait split; no current builder needs it.
- A value the caller owns before execution consumes itself: `Agent` and `ToolBuilder` take `mut self` and return `Self`. `Agent` configures itself rather than through a second type, so a provider or model left unset is caught when it joins a queue.
- A type handed out as `Arc` configures through `&self` and returns `&Self`: `Queue` and `Knowledge`. A third shape, `self: Arc<Self> -> Arc<Self>`, is not used.

## Constructors

**`new()` for the primary path. Named constructors carry semantics.**

- Named constructors: `load()`, `success()`, `error()`, `from_id()`, `from_env()`.

## Getters and Setters

**Mutable accessors use `set_` and `get_` prefixes to distinguish them from builders.**

- Example: `set_extension()`, `get_extension()`. Builder methods remain unprefixed.
- A public method returning `bool` is `is_<state>` or `has_<thing>`. A bare past participle such as `label_cancelled` reads as a field, not a question.
- `get_<name>` reads back a value a builder set where the bare noun would collide with the builder method on the same type: `Queue::get_dir`, `Model::get_context_window`, `Agent::get_provider`. A reader with no setter to collide with keeps the bare noun: `Tool::name`, `Agent::id`. A lookup by ID keeps `get_` for the `HashMap::get` sense, which is why `get_task(id)` stands apart from `find_task(matches)`.

## Lifecycle

**Queue action names state their target: `finish_result(matches)`, `finish_results(matches)`, `finish_all_tasks()`, `cancel_tasks(matches)`, and `cancel_all_tasks()`. A filter is a `Matcher<Task>`, so the same call names one task or one pool.**

```rust
tasks.start();
tasks.finish_results("label = scan").await;                 // one pool
tasks.finish_all_tasks().await;                             // the whole run
tasks.finish_result("ORDER BY created DESC").await;         // one result
tasks.cancel_tasks("label = scan");                        // one pool
tasks.cancel_all_tasks();                                   // the whole run
```

- A verb takes a filter when it can mean part of the queue, and none when it cannot: `run` starts everything or nothing.
- IMPORTANT: the filter says WHICH tasks, never WHAT to wait for. `finish_results("status = Finished")` returns at once because the filter selects tasks and "no work left" is the fixed wait condition.
- The whole-run case has exactly one spelling: `finish_all_tasks()` and `cancel_all_tasks()`.
- `finish_result(matches)` follows the same wait and query order as `finish_results(matches)`, then returns the first available result.
- Do not grow back label-, status-, or predicate-specific queue methods; fixed selections are AQL.

## Hooks

**A hook's name says when it runs. Every hook observes: `on_<trigger>`, and `on_<trigger>_async` for the same trigger in a handler `finish` awaits.**

```rust
on_event(handler)                    // observe
on_result_async(handler)             // observe, in a handler `finish` awaits
edit_replies(id, editor)             // act once, now
```

- `on_<trigger>(handler)` observes: the handler sees every `<trigger>` and returns nothing.
- `on_<trigger>_async(handler)` is the same trigger in a handler whichever `finish` is waiting awaits. Every observer has one, and only observers do.
- A bare `<action>(..)` acts once, now: `cancel`, `edit_replies`.
- IMPORTANT: the trigger fixes the handler's parameters. `_on_event` hands over `&Event`, `_on_result` a `&Task` and its validated `&Value`, `_on_failure` the `&Event` and the `&Task` it happened in. Observing returns `()`.
- IMPORTANT: an observer takes the queue first, `&Arc<Queue>` before the trigger's own parameters, owned in the `_async` twin. That is what a hook acts through, and why neither a `create_task_on_*` nor an `edit_replies_on_*` family exists: `queue.add_task(..)` inside `on_result`, and `queue.edit_replies(&event.task_id, ..)` inside `on_event`, are the whole of them.
- A hook reacts to something agentwerk produces. Anything the caller already holds needs no hook: to stop a pool on a verdict, `finish` for it and `cancel`.
- `on_task` sits outside the trigger grid, keying on a task rather than naming a trigger.
- No hook registers something agentwerk calls in place of its own work. Compaction summarizes and says so through the four `Compaction*` events, and what agentwerk writes to correct the model is set once by `Agent::directives`, which takes one function over every directive.

## Event Publication

**Publishing is always `tasks.emit_event(event)`, from both host code and crate internals.**

- Keep `event` in the verb. Bare `emit` is ambiguous beside provider streams and is not an event-publication API.
- Construct events with `Event::new(name)`, then add `.data(value)`, `.task_id(id)`, or `.agent_id(id)` when those values apply. Builder names match the attributes they set. Do not use a struct literal: the queue owns the timestamp and derived task label.
- Order Event members by relevance: `name`, `data`, `task_id`, `agent_id`, `label`, `created_at`; builders follow the constructor and readers follow in that same order.
- Contextual helpers, when they remove repeated agent or task plumbing, are also named `emit_event` and delegate immediately to `Queue::emit_event`.
- Do not add parallel names such as `emit`, `emit_custom`, or `publish_event`; every built-in and caller-defined event takes the same pipeline.
- `Event.name` is the sole semantic discriminator. Internal code constructs the same `Event::new(name).data(value)` record as host code and branches defensively on its name and JSON data; do not introduce a parallel typed event model or provenance marker.

## Editors

**An editor is `edit_<noun>`. Its last parameter is the `&mut` value it rewrites; anything before it is read-only context.**

- `edit_replies(id, FnOnce(&mut Vec<Reply>))`: the ID is the read-only context, the replies are the value.
- The value arrives holding what agentwerk would otherwise have used, so an editor that writes nothing keeps the default. No editor returns `Option<T>`: there is nothing left to signal.
- IMPORTANT: an editor acts once, on the value as it stands. Nothing is registered, so a second caller reads what the first left rather than replacing it.

## Python Bindings

**Every public Rust item has a Python counterpart of the same name. The transforms below are permitted; nothing else.**

- Type-state collapses: `ToolBuilder<D, H>` folds into the class it builds and takes its name. The collapsed class validates at `build()`.
- `Duration` becomes a float named `seconds`, with the unit repeated in the docstring: `Policy::request_retry_delay` binds as a float in seconds. Every other parameter keeps its Rust name.
- A fieldless enum becomes its snake_case `Display` string. That `Display` impl is the single source, so the binding never formats a variant with `{:?}`.
- An enum whose variants carry fields becomes a class with a `kind` string, a `data` dict, and one static constructor per variant. `ReplyContent` does this; `Event` is instead a generic record whose Python API mirrors Rust.
- A builder method whose name collides with a reader on the same Python class becomes a constructor keyword argument, because a Python class cannot carry both. `Task` needs this for `label`, `schema`, and `parent`.
- A `&mut` editor becomes a callable that returns the replacement, or `None` to keep the current value, since Python cannot take a Rust `&mut`.
- A conversion type a setter takes collapses into the Python types it converts from: `Text` is a `str` for the text itself and an `os.PathLike` for the file holding it.
- A reader taking no argument becomes an attribute: `Agent::id()` is `agent.id`, `Task::get_id()` is `task.get_id()`.
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

Naming is `snake_case`. Tool structs keep the `{Name}Tool` suffix. The name the model calls is a separate namespace and takes no suffix: `read_file`, `grep`, `tasks`. It is written once, in the tool's `From<XTool> for Tool` conversion, and never at a call site: a host registers `ReadFileTool`, not the string.

## Doc Comments (`///`)

**State the purpose in one sentence, with the same verb the item's README row uses.**

```rust
/// Set the LLM provider.            // builder: imperative configuration verb
/// Get the elapsed duration.        // accessor: Get
/// Begin processing tasks.        // action: imperative effect
/// A task finished successfully.  // event: past-tense state sentence
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
//! Runs many agents in parallel, each in its own tokio task, over one shared task store.
// BAD: lists contents
//! Agent loop.
//! - `run_main_loop`: entry point.

// GOOD: purpose and invariant
/// A task. Caller-settable fields: `task`, `label`, `schema`, `parent`. System-managed fields are set at insertion time.
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
GOOD: A `Schema` constrains the result an agent produces for a task.
BAD:  A `Schema` is a JSON Schema document that gets attached to a task and,
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
A `Schema` constrains the result an agent produces for a task.
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
- "stamp", "trip", "walk off", and "mint" are internal metaphors for writing a timestamp, breaching a limit, releasing a task, and creating one. Use the plain verb.
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
- "header" and "task header" for the on-disk file holding a `Task` without its `replies`: say "the task" or "the task without its messages". The internal helper `task_header_path` and `architecture.md` may keep the term.

Also:

- Bare "provider" is spelled "LLM provider". Identifier names stay unqualified.
- "execution" is the word for a run, in prose and in identifiers: `Queue::get_duration()`. `run` survives only in built-in names such as `Event::RUN_STARTED` and `Event::RUN_FINISHED`.
- The Knowledge store is described as durable memory the agent shares across tasks and other agents; the sharing is the headline, not a footnote.

## README Structure

**Terse, example-driven, scannable.**

- Fixed section order: Why use agentwerk?, Installation, Quick Start, Agent Swarms, Demo, the API sections, Use Cases, Security, Development.
- The opening section is one bullet per reason to reach for the crate: `**Reason:** one short sentence`. A reason with nothing behind it is marketing and is cut. It runs before the reader has met a single agentwerk concept, so it carries no identifier, no type or function name, and none of the domain vocabulary the API sections introduce. "Task" and "agent" are the only nouns assumed.
- API sections run in the order a new reader needs them: Agents, Tasks, Tools, Events, Knowledge. Sessions is a subsection of Tasks.
- Prompting has no section of its own. `role`, `task`, and the template bindings configure an agent, so they live in Agents next to `name` and `tool`.
- Every section leads with one minimal example, then at most three sentences. Facts live in one place; other sections cross-link rather than repeat.

## README Folds

**Above the fold is what a reader needs. The exhaustive reference goes inside a `<details>` block at the end of the section.**

- Nothing is deleted, only folded. A method that exists is documented somewhere, or the fold is not doing its job.
- Folds are the last thing in a section. Every `<summary>` reads `All <what the fold holds>`: `All event names`, `All hooks`, `All session files`. One section holds one fold; a second catalogue earns its own `h3` with its own lead example.
- IMPORTANT: a blank line after `</summary>` and before `</details>`. `details` opens a raw-HTML block, so without the blank line every table inside renders as literal pipe characters on GitHub, crates.io, and PyPI alike.
- Snippet budgets: eight lines for a section lead, five for a subsection lead. Quick Start gets sixteen.
- Agent Swarms is the one exception and runs long, because it is the only place a whole system is shown at once: a pool working in parallel, a second pool the first hands tasks to, and one knowledge store between them.

## README Mechanics

**Formatting choices that are not about either language.**

- One `h1` per file, the title. Every section is `h2`, every subsection `h3`. No wrapper heading above a group of sections.
- `h2` is Title Case, `h3` is Sentence case.
- A method placeholder is spelled as what the caller passes, never a single letter: `add_reply(id, content)`, `cancel_tasks(matches)`. In a bullet list the bare method name carries no parentheses at all.
- Centered blocks use `<div align="center">`. `align` is not allowed on `<p>` by the crates.io sanitizer, so `<p align="center">` renders left-aligned there.

## README Tables

**No table above a fold. Every enumeration inside a fold is a table.**

- Above the fold there is prose and one example, so a table there is a sign the section is doing reference work it should have folded.
- Inside a fold the content is the reference, and a two-column grid is what a reader scans. Bullets there only hide the second column.
- A catalogue with categories takes a third column on the left holding the bold group label: the built-in tools, the event names, the execution methods, and the hooks.
- Prose still belongs in a fold when it is a caveat rather than an entry, as does a snippet showing how one of the entries is called.

## README Table Shape

**Each table picks one cell shape and holds it. Mixing shapes inside one table is a defect.**

- Builder rows lead with an imperative configuration verb: `Set the LLM provider.`
- Action rows lead with the imperative effect: `Begin processing tasks.`
- Accessor rows lead with `Get`: `Get every finished task's result.`
- Tool rows lead with the imperative action the agent takes: `Fetch a URL and read its body.`
- Event rows are past-tense state sentences: `A task finished successfully.`
- Every row is a description, not a constraint fragment. A cell that does not lead with a verb is a defect.
- A tool does not act; the agent does. The table intro carries that framing once so individual rows stay terse.
- The prose verb "Return" stays for instructions to the caller, as in "Return a `tool_call_failed` event for a failure the model should work around".

## README Table Order

**Rows are grouped by subject, and one axis order is repeated in every group.**

- Groups do not interleave. The result rows run before the task rows in Results; the observers run before the cancels in Hooks.
- One axis order is chosen and held across every group. Hooks is the model: `event`, `result`, `failure`.
- Queue tables follow caller workflow: configure, submit/interact, observe, run, cancel, inspect tasks, inspect results/events, then inspect run metadata. Setters stay beside getters, synchronous hooks beside async twins, and singular selectors before plural selectors.
- One table holds one receiver. A method on another type goes in the fold's trailing prose.
- The Execution fold holds everything that acts once over a run. The hooks fold holds only what registers a handler the queue calls back into on every matching event.

## README Examples

**Show the smallest snippet that demonstrates the feature.**

- Example models are `claude-haiku-4-5-20251001` or `claude-sonnet-4-20250514`.
- Update triggers: a new builder method, a new tool, a new event name, a new environment variable, or a changed default.
- A chain of more than two calls breaks one call per line, even where it would fit the formatter's width.
- A code change edits only the doc sentences it made wrong. Surrounding prose is not rewritten unprompted.

## Rust and Python READMEs

**The Python README mirrors the Rust one section for section, carrying the same examples.**

- The heading lists of the two files match. A section in one is a section in the other.
- A snippet is a translation of its twin: same variable names, same string literals, same order of operations.
- A difference that is real belongs in the root `INVENTORY.md`, and the README shows it in the same place in both files.
- Only the Installation cross-links and the Development section differ in substance.
