# Inventory

Every declaration in `crates/agentwerk/src` and `crates/agentwerk-py/src`, one section per source file, public rows before internal ones. `python/agentwerk/__init__.py` adds only `@tool`, which sits with the Rust items it wraps.

> A commit that adds, renames, removes, or re-types an item changes this file in the same commit.

## Notation

Signatures use one language-independent notation, so a Rust row and a Python row read alike.

- Receivers, borrows, and lifetimes drop: `fn record(&self, event: &Event)` is `Stats.record(event: Event): void`.
- A type names itself once, on its first member: `Stats.event_count(name: string): number`. Every member below it starts at the dot, `.input_tokens(): number`, until another type or a free function takes over. A free function carries no owner.
- A method returning its own type returns `this`: `Agent.provider(provider: Provider): this`.
- `u8`, `u32`, `u64`, `usize`, `f32`, `f64`, and `Duration` are `number`, and a `Duration` constant shows milliseconds.
- `&str`, `String`, `impl Into<String>`, `&Path`, and `PathBuf` are `string`.
- `impl Into<Text>` is `Text`: a string is the text itself, a `&Path` or `PathBuf` names the file the crate reads and trims.
- `bool` is `boolean`, `()` is `void`, `serde_json::Value` is `json`.
- `Vec<T>` and `&[T]` are `T[]`, `HashMap<K, V>` and `BTreeMap<K, V>` are `Record<K, V>`.
- `Option<T>` is `T?`, written that way rather than as a union because a `|` splits a table cell.
- `Result<T, E>` is `T throws E`, `io::Result<T>` is `T throws io::Error`.
- An `async fn` returns `Promise<T>`.
- `Arc<T>`, `Box<T>`, `Rc<T>`, `Mutex<T>`, `RwLock<T>`, and `&T` unwrap to `T`. Every other wrapper keeps its name: `Weak<Queue>`, `Sender<Event>`, `JoinHandle<void>`.
- A callback becomes an arrow: `Arc<dyn Fn(&Event) + Send + Sync>` is `(event: Event) => void`.
- A generic bound erases to what the caller passes: `T: Serialize` is `json`. A type's own parameters stay, as in `ProviderResult<T>`.
- Domain type names stay as the code writes them: `Task`, `Event`, `Status`, `PolicyViolation`.
- A constant shows `= value` when the value is one scalar or short string, and nothing when it is longer or computed.
- Names are never re-cased: `get_tasks`, never `getTasks`. Every name here is greppable in `src/`.

## Rows

- Each section splits into `### Public` and `### Internal`, public first. Public is what a caller outside the crate can name and what Python sees; internal is the rest, a `pub` item sealed behind a crate-private module included. A section with rows on one side only carries that heading alone.
- `Language` is `both` when Python reads the same after the conversions below and any note the section already carries. A `Python` row follows only where Python does something those do not already say.
- A `Rust` row carrying no `Python` row is not bound. A `not bound` cell is written only to give the reason.
- A file whose exports the bindings leave out says so in one line under its heading, and its rows then carry no `Python` row. A `crates/agentwerk-py` file names the library file it binds instead. A file that exports nothing carries no note.
- An enum variant, a trait impl, a module declaration, or a re-export is the exception: it takes a `Python` row only where Python does something the row above does not already imply.
- A `Python` row leaves `Visibility` empty. A trailing `: note` states a behavioral difference.
- `Visibility` is the reach: `pub`, `crate`, `super`, or `private`, so a `pub(in path)`, a `pub` member of a `crate` type, and a `pub` item in a crate-private module all record as `crate`. In `crates/agentwerk-py` it is `python` for anything a `#[pyclass]`, `#[pymethods]`, or `#[pyfunction]` exposes, whether or not Rust keeps it private.
- Struct fields sit inside the struct's row, and a trait's members inside the trait's. An enum gets its own row plus one row per variant, carrying the variant's payload where it has one.
- A hand-written trait impl is one row, written `impl Trait for Type` with the type as Rust spells it: `impl ProviderLike for Arc<T>`. Derived impls are not listed.
- `#[cfg(test)]` items, `test_util.rs`, and `codegrep/*_tests.rs` are not listed.

## Python conversions

The rules the tables never repeat.

- Every enum is its lowercase string: `task.get_status() == "in_progress"`.
- Every error type is `RuntimeError`.
- Every bound type keeps its Rust name, and its Rust fields are not Python attributes: `Knowledge`, `Anthropic`. A unit struct takes no constructor arguments: `GlobTool()`.
- Every `Duration` is a float named `seconds`. Every other parameter keeps its Rust name and drops its type.
- Every shared handle is a plain object, shared by passing it to several agents.
- Every `async` item is awaitable.
- An argument Rust takes by `Serialize` takes any JSON-serializable value.
- A `json` value is a dict, list, or scalar.
- Runtime readers use callable `get_*` methods in both languages.

## `crates/agentwerk/src/agents/agent.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Agent { label: string?, interactive: boolean, queue: QueueRef, id: OnceLock<string>, provider: Provider?, model: Model?, role: string, templates: [string, string][], handover: Task?, tools: ToolRegistry, dir: string, knowledge: Knowledge, directives: DirectiveStore }` | pub with private fields |
| Rust | `impl Clone for Agent` | pub |
| Rust | `.new(): this` | pub |
| Python | `Agent()` | |
| both | `.from_env(): this` | pub |
| Python | `.from_env()`: raises `RuntimeError` where Rust panics | |
| both | `.provider(provider: Provider): this` | pub |
| both | `.model(model: Model): this` | pub |
| Rust | `.role(role: Text): this` | pub |
| Python | `.role(role)`: a `str` is the role, an `os.PathLike` names the file holding it | |
| both | `.label(label: string): this` | pub |
| both | `.interactive(): this` | pub |
| both | `.template(key: string, value: string): this` | pub |
| Rust | `.templates(variables: [string, string][]): this` | pub |
| Python | `.templates(variables)`: a mapping, so the bulk bind applies in key order where Rust preserves insertion order | |
| both | `.handover(task: Task): this` | pub |
| both | `.tool(tool: Tool): this` | pub |
| both | `.tools(tools: Tool[]): this` | pub |
| both | `.dir(dir: string): this` | pub |
| both | `.knowledge(store: Knowledge): this` | pub |
| both | `.directives(compute: (key: string) => string?): this` | pub |
| both | `.get_id(): string` | pub |
| both | `.task(task: Task): string` | pub |
| both | `.task(task)`: a string or json value stands in for the `Task` | |
| both | `.start(): Queue` | pub |
| Python | `.start()`: raises `RuntimeError` where Rust panics on a missing provider or model | |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `AGENT_IDS: Record<string, number>` | private |
| Rust | `next_id(label: string?): string` | private |
| Rust | `QueueRef` | crate |
| Rust | `.Shared(Queue)` | crate |
| Rust | `.Private(Queue)` | crate |
| Rust | `.upgrade(): Queue?` | crate |
| Rust | `Agent.is_interactive(): boolean` | super |
| Rust | `.handles(agent_label: string?, task_label: string?): boolean` | super |
| Rust | `.get_tools(task: Task): ToolRegistry` | super |
| Rust | `.get_provider(): Provider` | super |
| Rust | `.get_model(): Model` | super |
| Rust | `.get_knowledge(): Knowledge` | super |
| Rust | `.get_directives(): DirectiveStore` | super |
| Rust | `.get_dir(): string` | super |
| Rust | `.require_provider_and_model(): void` | super |
| Rust | `.register_finish_tool(): void` | super |
| Rust | `.system_prompt(knowledge: string?, policy: Policy, stats: Stats, task_id: string): string` | super |
| Rust | `.expand_context(role: string, policy: Policy, stats: Stats, task_id: string): string` | private |
| Rust | `.interpolate(s: string): string` | private |
| Rust | `.dispatch(task: Task): string` | private |

## `crates/agentwerk/src/agents/compaction.rs`

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `compaction_token_threshold(window: number?, fraction: number?): number?` | crate |
| Rust | `estimate_next_request_tokens(history: TokenUsage[], messages: Message[], system_prompt: string, tools: Tool[]): number` | crate |
| Rust | `next_delta(history: TokenUsage[]): number` | private |
| Rust | `message_bytes(message: Message): number` | private |
| Rust | `block_bytes(block: ContentBlock): number` | private |
| Rust | `tool_bytes(tool: Tool): number` | private |
| Rust | `should_compact_proactively(window: number?, fraction: number?, history: TokenUsage[], messages: Message[], system_prompt: string, tools: Tool[]): boolean` | crate |
| Rust | `Compaction { provider: Provider, model: string, window: number?, on_progress: (completed: number, total: number) => void, directives: DirectiveStore }` | crate |
| Rust | `.new(provider: Provider, model: string, window: number?, on_progress: (completed: number, total: number) => void, directives: DirectiveStore): this` | crate |
| Rust | `.window(): number?` | crate |
| Rust | `.summarize(replies: Reply[]): Promise<string throws ProviderError>` | crate |
| Rust | `summarize_replies(compaction: Compaction, replies: Reply[]): Promise<Reply[] throws ProviderError>` | crate |
| Rust | `chunks_for_window(messages: Message[], window: number?): Message[][]` | crate |
| Rust | `chunks_within(messages: Message[], max_tokens_per_chunk: number): Message[][]` | private |
| Rust | `split_in_half(message: Message): Message[]?` | private |
| Rust | `find_split_index(text: string, target: number): number` | private |

## `crates/agentwerk/src/agents/policy.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PolicyViolation` | pub |
| Python | a string inside `Event.get_data()`: `data["policy"]` | |
| Rust | `.Turns`, `.InputTokens`, `.OutputTokens`, `.MaxSchemaRetries`, `.Time` | pub |
| Rust | `impl Display for PolicyViolation` | pub |
| Rust | `Policy { max_turns: number?, max_input_tokens: number?, max_output_tokens: number?, max_request_tokens: number?, max_schema_retries: number?, max_request_retries: number, request_retry_delay: number, max_time: number?, compaction_threshold: number? }` | pub |
| Python | `Policy(*, max_turns=None, ..., compaction_threshold=None)`: keyword-only, `max_time` and `request_retry_delay` in seconds, and a field left out takes its default rather than meaning "no limit" | |
| Rust | `.DEFAULT_MAX_SCHEMA_RETRIES: number = 10` | pub |
| Rust | `.DEFAULT_MAX_REQUEST_RETRIES: number = 10` | pub |
| Rust | `.DEFAULT_REQUEST_RETRY_DELAY: number = 500` | pub |
| Rust | `.DEFAULT_COMPACTION_THRESHOLD: number = 0.85` | pub |
| Python | not bound: read the field back through `Queue.get_policy()` | |
| Rust | `impl Default for Policy` | pub |

## `crates/agentwerk/src/agents/knowledge.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `KnowledgeError` | pub |
| Rust | `.PageRejected { message: string }` | pub |
| Rust | `.PageMissing { slug: string }` | pub |
| Rust | `.IoFailed { message: string, source: io::Error }` | pub |
| Rust | `impl Display for KnowledgeError` | pub |
| Rust | `impl Error for KnowledgeError` | pub |
| both | `Knowledge { knowledge_dir: string, index: IndexEntry[], write_lock: void, index_char_limit: number }` | pub with private fields |
| both | `.load(store_dir: string): this throws io::Error` | pub |
| both | `.set_char_limit(count: number): this` | pub |
| both | `.get_index_char_limit(): number` | pub |
| both | `.get_index(): string` | pub |
| both | `.get_pages(): Pages` | pub |
| both | `.clear(): void throws KnowledgeError` | pub |
| Rust | `Page { slug: string, kind: string, description: string, content: string, tags: string[] }` | pub |
| Python | `Page(slug, description, content, kind=.., tags=..)`: a struct literal becomes a constructor, so the optional fields move last | |
| both | `.get_slug(): string` | pub |
| both | `.get_kind(): string` | pub |
| both | `.get_description(): string` | pub |
| both | `.get_content(): string` | pub |
| both | `.get_tags(): string[]` | pub |
| Rust | `impl Persist for Page` | pub |
| both | `Pages { inner: Knowledge }` | pub with private fields |
| both | `.save(page: Page): void throws KnowledgeError` | pub |
| both | `.get_page(slug: string): Page throws KnowledgeError` | pub |
| both | `.get_pages(): Page[] throws KnowledgeError` | pub |
| both | `.remove(slug: string): void throws KnowledgeError` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `INDEX_FILE: string = "index.md"` | private |
| Rust | `BUNDLE_DIR: string = "knowledge"` | private |
| Rust | `PAGES_DIR: string = "pages"` | private |
| Rust | `DEFAULT_INDEX_CHAR_LIMIT: number = 12000` | private |
| Rust | `DEFAULT_PAGE_TYPE: string = "Knowledge"` | private |
| Rust | `LEGACY_MEMORY_FILE: string = "memory.jsonl"` | private |
| Rust | `MIGRATED_SUFFIX: string = ".migrated"` | private |
| Rust | `LOG_FILE: string = "log.md"` | private |
| Rust | `IndexEntry { slug: string, description: string, path: string }` | private |
| Rust | `io_failed(message: string): (error: io::Error) => KnowledgeError` | private |
| Rust | `Knowledge.full_index(): string` | crate |
| Rust | `.index_path(): string` | private |
| Rust | `.index_usage(): [number, number, number]` | crate |
| Rust | `page_path(knowledge_dir: string, slug: string): string` | private |
| Rust | `normalize_slug(raw: string): string throws KnowledgeError` | crate |
| Rust | `render_page(kind: string, description: string, content: string, tags: string[]): string` | private |
| Rust | `parse_page(raw: string): [string, string, string[], string]` | private |
| Rust | `render_entry(entry: IndexEntry): string` | private |
| Rust | `render_index(entries: IndexEntry[]): string` | private |
| Rust | `render_limited_index(entries: IndexEntry[], limit: number, path: string): string` | private |
| Rust | `index_directive(remaining: number, path: string): string` | private |
| Rust | `render_index_file(entries: IndexEntry[]): string` | private |
| Rust | `rebuild_index_from_pages(knowledge_dir: string): IndexEntry[] throws io::Error` | private |
| Rust | `collect_pages(root: string, dir: string, entries: IndexEntry[]): void throws io::Error` | private |
| Rust | `is_reserved(file_name: string): boolean` | private |
| Rust | `bundle_slug(root: string, path: string): string throws KnowledgeError` | private |
| Rust | `extract_h1_summary(body: string): string` | private |
| Rust | `LegacyMemoryRecord { content: string, added_at: number }` | private |
| Rust | `migrate_memory_jsonl(store_dir: string, knowledge_dir: string): IndexEntry[] throws io::Error` | private |
| Rust | `format_iso8601_now(): string` | private |

## `crates/agentwerk/src/agents/loop/agent.rs`

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `RESUME_OR_FINISH_DETAIL: string` | private |
| Rust | `TaskContext { agent: Agent, model: Model, queue: Queue, run: Run, task_id: string, system_prompt: string, policy: Policy, tools: ToolRegistry, consecutive_schema_failures: number }` | super |
| Rust | `.emit_event(event: Event): Event` | super |
| Rust | `.task(): Task?` | super |
| Rust | `.retry_directive(detail: string, event: Event): string` | super |
| Rust | `.fail_task(): void` | super |
| Rust | `.fail_with(reason: RequestErrorKind, message: string): void` | super |
| Rust | `run_agent(agent: Agent): Promise<void>` | super |
| Rust | `run_is_over(agent: Agent, queue: Queue): boolean` | private |
| Rust | `claim(agent: Agent, queue: Queue): TaskContext?` | private |
| Rust | `evaluate(context: TaskContext): Step?` | private |
| Rust | `silence_retry(context: TaskContext): Step?` | private |

## `crates/agentwerk/src/agents/loop/compact.rs`

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run(context: TaskContext, reason: CompactReason): Promise<Step?>` | super |
| Rust | `proactive_compaction_needed(context: TaskContext, task: Task): boolean` | super |

## `crates/agentwerk/src/agents/loop/main.rs`

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run_main_loop(queue: Queue): Promise<void>` | crate |

## `crates/agentwerk/src/agents/loop/mod.rs`

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod agent`, `mod compact`, `mod main`, `mod request`, `mod tool_call` | private |
| Rust | `POLL_INTERVAL: number = 50` | private |
| Rust | `CompactReason` | private |
| Rust | `.Proactive`, `.Reactive` | private |
| Rust | `Step` | private |
| Rust | `.Evaluate` | private |
| Rust | `.Compact(CompactReason)` | private |
| Rust | `.Request` | private |
| Rust | `.ToolCalls(ToolCall[])` | private |

## `crates/agentwerk/src/agents/loop/request.rs`

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run(context: TaskContext): Promise<Step?>` | super |

## `crates/agentwerk/src/agents/loop/tool_call.rs`

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run(context: TaskContext, calls: ToolCall[]): Promise<Step?>` | super |

## `crates/agentwerk/src/agents/mod.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod agent`, `mod policy`, `mod knowledge`, `mod loop`, `mod tasks` | pub |
| Rust | re-exports `Agent`, `Policy`, `PolicyViolation`, `Knowledge`, `Matcher`, `Query`, `QueryError`, `Reply`, `Status`, `Task`, `TaskError`, `Queue`, `Trajectory` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod compaction` | crate |
| Rust | `mod retry`, `mod stats` | crate |

## `crates/agentwerk/src/agents/retry.rs`

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Retry { try_consume(): number?, max_attempts(): number, delay(server_hint: number?): number }` | crate |
| Rust | `MAX_RETRY_DELAY: number = 32000` | private |
| Rust | `ExponentialRetry { base_delay: number, max_attempts: number, attempt: number }` | crate |
| Rust | `.new(base_delay: number, max_attempts: number): this` | crate |
| Rust | `impl Retry for ExponentialRetry` | crate |

## `crates/agentwerk/src/agents/stats.rs`

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Stats { event_counts: Record<string, number>, input_tokens: number, output_tokens: number, started_at: number, finished_at: number, token_usage: Record<string, TokenUsage[]> }` | crate |
| Rust | `.FILE: string = "events.jsonl"` | private |
| Rust | `.for_each_event(dir: string, visit: (event: Event) => void): void throws io::Error` | crate |
| Rust | `.event_count(name: string): number` | crate |
| Rust | `.input_tokens(): number` | crate |
| Rust | `.output_tokens(): number` | crate |
| Rust | `.execution_duration(): number?` | crate |
| Rust | `.new(): this` | crate |
| Rust | `.append(dir: string, event: Event): void throws io::Error` | crate |
| Rust | `.record(event: Event): void` | crate |
| Rust | `.usage_for_task(task_id: string): TokenUsage[]` | crate |
| Rust | `.reset_usage(task_id: string): void` | crate |
| Rust | `.restart_clock(): void` | crate |
| Rust | `.record_usage(task_id: string, usage: TokenUsage): void` | private |

## `crates/agentwerk/src/agents/query.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `trait Matcher<R: Queryable> { into_query(): Query<R> }` | pub |
| Rust | `impl Matcher<R> for F where F: Fn(R) => boolean + Send + Sync + 'static` | pub |
| Rust | `impl Matcher<R> for &str` | pub |
| Rust | `impl Matcher<R> for String` | pub |
| Rust | `impl Matcher<R> for Query<R>` | pub |
| Python | an AQL string or a callable stands in for the `Query` wherever one is accepted | |
| Rust | `Query<R: Queryable = Task>(Compiled<R.Field>)` | pub |
| Python | `Query(query: str)`: one class over both field sets, compiled over each at construction | |
| Rust | `.new(query: string): this throws QueryError` | pub |
| Rust | `impl From<&str> for Query<R>` | pub |
| Rust | `impl From<String> for Query<R>` | pub |
| Rust | `enum QueryError { Blank, UnknownField, UnknownStatus, InvalidTime, OperatorNotAllowed, RepeatedField, UnexpectedToken, UnexpectedEnd }` | pub |
| Rust | `impl Display for QueryError` | pub |
| Rust | `impl Error for QueryError` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `trait Queryable { Field }` | private |
| Rust | `impl Queryable for Task` | private |
| Rust | `impl Queryable for Event` | private |
| Rust | `Query.all(): this` | crate |
| Rust | `.matches(record: R): boolean` | crate |
| Rust | `.and(other: Query<R>): this` | crate |
| Rust | `.is_ordered(): boolean` | crate |
| Rust | `.sort(records: R[]): void` | crate |
| Rust | `Query<Task>.default_status(status: Status): this` | crate |
| Rust | `.and_status(status: Status): this` | crate |
| Rust | `.and_result(): this` | crate |
| Rust | `enum Condition<F: QueryField> { All(Condition<F>[]), Any(Condition<F>[]), Not(Condition<F>), Term(F, Match), Test(Predicate<F.Record>) }` | private |
| Rust | `.matches(record: F.Record): boolean` | private |
| Rust | `.mentions(field: F): boolean` | private |
| Rust | `Predicate<R>((record: R) => boolean)` | private |
| Rust | `impl Clone for Predicate<R>` | private |
| Rust | `impl Debug for Predicate<R>` | private |
| Rust | `Compiled<F: QueryField> { root: Condition<F>, order: Sort<F>? }` | private |
| Rust | `.new(query: string): this throws QueryError` | private |
| Rust | `.all(): this` | private |
| Rust | `.test(check: (record: F.Record) => boolean): this` | private |
| Rust | `.term(field: F, matcher: Match): this` | private |
| Rust | `.rooted(root: Condition<F>): this` | private |
| Rust | `.and(other: Compiled<F>): this` | private |
| Rust | `.matches(record: F.Record): boolean` | private |
| Rust | `.mentions(field: F): boolean` | private |
| Rust | `.is_ordered(): boolean` | private |
| Rust | `.sort(records: T[]): void` | private |
| Rust | `trait QueryField: Copy + PartialEq + Debug + 'static { Record, FIELDS, of, kind, is_optional, shorthand, label, tie_break, sort_unordered, canonical, compare, named, name, spellings, allows, operators }` | private |
| Rust | `enum TaskField { Id, Label, Status, Pending, Cancelled, Agent, Parent, Task, Result, Errors, Created, Started, Finished, Failed }` | private |
| Rust | `impl QueryField for TaskField` | private |
| Rust | `enum EventField { Event, Agent, Task, Label, Created, Payload }` | private |
| Rust | `impl QueryField for EventField` | private |
| Rust | `enum Kind { Value, Text, Time }` | private |
| Rust | `is_task_id(word: string): boolean` | private |
| Rust | `carried(value: string): string?` | private |
| Rust | `event_named(value: string): string?` | private |
| Rust | `as_text(value: json): string` | private |
| Rust | `Sort<F: QueryField> { field: F, descending: boolean }` | private |
| Rust | `.compare(left: F.Record, right: F.Record): Ordering` | private |
| Rust | `STATUSES: Status[]` | private |
| Rust | `millis_text(millis: number): string` | private |
| Rust | `bool_text(value: boolean): string` | private |
| Rust | `millis(value: string): number` | private |
| Rust | `time_value(field: F, value: string): number throws QueryError` | private |
| Rust | `ago(offset: string): number?` | private |
| Rust | `date_millis(value: string): number?` | private |
| Rust | `status_rank(value: string): number` | private |
| Rust | `enum Match { Is, IsNot, In, NotIn, Contains, Omits, After, NotBefore, Before, NotAfter, Empty, NotEmpty }` | private |
| Rust | `.test(value: string?): boolean` | private |
| Rust | `enum Token { Word, Quoted, Equals, NotEquals, Contains, Omits, After, NotBefore, Before, NotAfter, Open, Close, Comma }` | private |
| Rust | `.spelling(): string` | private |
| Rust | `.is_keyword(keyword: string): boolean` | private |
| Rust | `tokenize(query: string): Token[] throws QueryError` | private |
| Rust | `parse_query<F>(query: string): [Condition<F>, Sort<F>?] throws QueryError` | private |
| Rust | `Parser<F: QueryField> { tokens: Token[], at: number }` | private |
| Rust | `.at_order_by(): boolean` | private |
| Rust | `.order_by(): Sort<F>? throws QueryError` | private |
| Rust | `.any(): Condition<F> throws QueryError` | private |
| Rust | `.all(): Condition<F> throws QueryError` | private |
| Rust | `.unary(): Condition<F> throws QueryError` | private |
| Rust | `.term(): Condition<F> throws QueryError` | private |
| Rust | `.operator(field: F): Match throws QueryError` | private |
| Rust | `.value(field: F): string throws QueryError` | private |
| Rust | `.time(field: F): number throws QueryError` | private |
| Rust | `.values(field: F): string[] throws QueryError` | private |
| Rust | `one_or<F>(group: (Condition<F>[]) => Condition<F>, terms: Condition<F>[]): Condition<F>` | private |
| Rust | `reject_repeated_field<F>(terms: Condition<F>[]): void throws QueryError` | private |

## `crates/agentwerk/src/agents/tasks/error.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `TaskError` | pub |
| Python | `RuntimeError`: `TaskError::TaskMissing { id }` reads as `Task t-1 not found` | |
| Rust | `.TaskMissing { id: string }` | pub |
| Rust | `.TransitionRejected { from: Status, to: Status }` | pub |
| Rust | `.ResultRejected { message: string }` | pub |
| Rust | `impl Display for TaskError` | pub |
| Rust | `impl Error for TaskError` | pub |

## `crates/agentwerk/src/agents/tasks/mod.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | re-exports `Author`, `Reply`, `ReplyContent`, `Status`, `Task`, `TaskError`, `FinishReason`, `Queue`, `Trajectory` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod error`, `mod reply`, `mod store`, `mod task`, `mod queue`, `mod trajectory` | private |
| Rust | `policy_violated(policy: Policy, stats: Stats): [PolicyViolation, number]?` | crate |
| Rust | `now_millis(): number` | crate |
| Rust | `numeric_id(id: string): number` | crate |

## `crates/agentwerk/src/agents/tasks/reply.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Author` | pub |
| Python | the `author` string: `"system"`, `"user"`, or `"assistant"` | |
| Rust | `.System` | pub |
| Rust | `.User` | pub |
| Rust | `.Assistant` | pub |
| Rust | `Reply { author: Author, content: ReplyContent[], created_at: number }` | pub with crate-private fields |
| Rust | `Reply.new(author: Author, content: ReplyContent[]): this` | pub |
| Rust | `.get_author(): Author` | pub |
| Python | `.get_author(): string` | |
| both | `.get_content(): ReplyContent[]` | pub |
| Rust | `.get_content_mut(): ReplyContent[]` | pub |
| both | `.get_created_at(): number` | pub |
| Rust | `ReplyContent` | pub |
| Python | `ReplyContent.get_kind()` plus `.get_data()`. Built with `ReplyContent.text(..)`, `.tool_use(..)`, `.tool_result(..)`, `.thinking(..)`, `.redacted_thinking(..)` | |
| Rust | `.Text { text: string }` | pub |
| Rust | `.ToolUse { id: string, name: string, input: json }` | pub |
| Rust | `.ToolResult { tool_use_id: string, content: string, succeeded: boolean, path: string? }` | pub |
| Rust | `.Thinking { thinking: string, signature: string }` | pub |
| Rust | `.RedactedThinking { data: string }` | pub |
| Rust | `.get_kind(): string` and field-specific `get_*` readers | pub |
| both | `Reply.user_text(text: string): this` | pub |
| Python | `.user_text(text)`: builds a user reply | |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Reply.user(blocks: ContentBlock[], paths: Record<string, string>): this` | crate |
| Rust | `.assistant(blocks: ContentBlock[]): this` | crate |
| Rust | `.system_text(text: string): this` | crate |
| Rust | `.as_message(): Message?` | crate |
| Rust | `ReplyContent.from_block(b: ContentBlock, paths: Record<string, string>): this` | private |
| Rust | `.to_block(): ContentBlock` | private |

## `crates/agentwerk/src/agents/tasks/store.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `Queue.set_task_finished(id: string, result: json): void throws TaskError` | pub |
| both | `.set_task_failed(id: string): void throws TaskError` | pub |
| both | `.edit_replies(id: string, editor: (replies: Reply[]) => void): this` | pub |
| Python | `.edit_replies(id, editor)`: the editor returns the new list, or `None` to keep the old one, where Rust mutates in place. An editor that raises, or returns anything but `Reply` objects, raises here | |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `max_existing_task_id(dir: string): number` | private |
| Rust | `Queue.insert(task: Task, reporter: string): string` | crate |
| Rust | `.save_task(id: string): void` | private |
| Rust | `.write_tool_output(task_id: string, tool_use_id: string, content: string): string?` | crate |
| Rust | `.claim(query: Query, agent_id: string): string?` | crate |
| Rust | `.append_reply(id: string, reply: Reply): void` | crate |
| Rust | `.set_finished_by(id: string, agent: string): void throws TaskError` | crate |
| Rust | `.set_failed_by(id: string, agent: string): void throws TaskError` | crate |
| Rust | `.set_final_status(id: string, status: Status, agent: string): void throws TaskError` | private |
| Rust | `.set_result(id: string, result: json): [json, string[]] throws SchemaViolations` | crate |
| Rust | `.edit(id: string, task: json?, label: string?): void throws TaskError` | crate |

## `crates/agentwerk/src/agents/tasks/task.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Task { task: json, label: string?, schema: Schema?, id: string, status: Status, reporter: string, assignee: string?, created_at: number, started_at: number?, finished_at: number?, failed_at: number?, result: json?, errors: Event[], parent: string?, replies: Reply[] }` | pub with crate-private fields |
| Python | `Task`: values are read through the same `get_*` methods as Rust; replies and errors are converted on access | |
| Rust | `.new(task: json): this` | pub |
| Python | `Task(task)` | |
| Rust | `.labeled(label: string, task: json): this` | pub |
| Python | `Task(task, label=l)` | |
| Rust | `.label(label: string): this` | pub |
| Python | `Task(task, label=l)` | |
| Rust | `.schema(schema: Schema): this` | pub |
| Python | `Task(task, schema=s)` | |
| Rust | `.parent(id: string): this` | pub |
| Python | `Task(task, parent=id)` | |
| both | `.get_task(): json` | pub |
| both | `.get_label(): string?` | pub |
| both | `.get_schema(): Schema?` | pub |
| both | `.get_id(): string` | pub |
| both | `.get_status(): Status` | pub |
| both | `.get_reporter(): string` | pub |
| both | `.get_assignee(): string?` | pub |
| both | `.get_created_at(): number` | pub |
| both | `.get_started_at(): number?` | pub |
| both | `.get_finished_at(): number?` | pub |
| both | `.get_failed_at(): number?` | pub |
| both | `.get_result(): json?` | pub |
| both | `.get_errors(): Event[]` | pub |
| both | `.get_parent(): string?` | pub |
| both | `.get_replies(): Reply[]` | pub |
| both | `.is_todo(): boolean` | pub |
| both | `.is_finished(): boolean` | pub |
| both | `.is_failed(): boolean` | pub |
| both | `.is_in_progress(): boolean` | pub |
| both | `.is_pending(): boolean` | pub |
| both | `.is_cancelled(): boolean` | pub |
| Rust | `impl From<&str> for Task` | pub |
| Rust | `impl From<String> for Task` | pub |
| Rust | `impl From<json> for Task` | pub |
| Rust | `impl From<&Path> for Task`, `impl From<PathBuf> for Task`, and `impl From<&PathBuf> for Task`, reading the file as the task | pub |
| Rust | `impl Persist for Task` | pub |
| Rust | `impl AsUserMessage for Task` | pub |
| Rust | `Status` | pub |
| Python | a string: `"todo"`, `"in_progress"`, `"finished"`, `"failed"` | |
| Rust | `.Todo` | pub |
| Rust | `.InProgress` | pub |
| Rust | `.Finished` | pub |
| Rust | `.Failed` | pub |
| Rust | `impl Display for Status` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Task.is_waiting_for_response(): boolean` | crate |
| Rust | `.is_paused(): boolean` | crate |
| Rust | `.cancelled: boolean`: transient and excluded from serialization | crate |
| Rust | `.to_messages(): Message[]` | crate |
| Rust | `.stamp_transition(next: Status, now: number): void` | crate |
| Rust | `TaskResult { id: string, value: json? }` | crate |
| Rust | `impl Persist for TaskResult` | crate |
| Rust | `Replies { id: string, entries: Reply[] }` | crate |
| Rust | `.append(dir: string, id: string, reply: Reply): void throws io::Error` | crate |
| Rust | `impl Persist for Replies` | crate |
| Rust | `task_record_path(dir: string, id: string): string` | super |
| Rust | `replies_path(dir: string, id: string): string` | private |
| Rust | `result_path(dir: string, id: string): string` | super |

## `crates/agentwerk/src/agents/tasks/queue.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `FinishReason` | pub |
| Python | a string, such as `policy_violated(turns)` | |
| Rust | `.Drained`, `.PolicyViolated(PolicyViolation)`, `.Cancelled` | pub |
| Rust | `impl Display for FinishReason` | pub |
| both | `Queue { weak_self: Weak<Queue>, tasks: Record<string, Task>, agents: Agent[], policy: Policy, run: Run, cancel_filters: Query[], terminal_transitions_in_flight: number, stats: Stats, event_handlers: EventHandler[], awaited_events: AwaitedEvents, event_stream: Sender<Event>, dir: string, events_lock: void, join_handle: JoinHandle<void>?, next_task_id: number? }` | pub |
| Rust | `.new(): this` | pub |
| Python | `Queue()` | |
| both | `.load(tasks_dir: string): this throws io::Error` | pub |
| both | `.get_input_tokens(): number` | pub |
| both | `.get_output_tokens(): number` | pub |
| both | `.get_duration(): number?` | pub |
| both | `.on_event(handler: (queue: Queue, event: Event) => void): this` | pub |
| both | `.on_event_async(handler: (queue: Queue, event: Event) => Promise<void>): this` | pub |
| Python | `.on_event_async(handler)`: takes an `async def`, awaited on the event loop awaiting `finish`, so a handler that raises prints its traceback and does not stop the run | |
| both | `.on_result(handler: (queue: Queue, task: Task, result: json) => void): this` | pub |
| both | `.on_result_async(handler: (queue: Queue, task: Task, result: json) => Promise<void>): this` | pub |
| Python | `.on_result_async(handler)`: takes an `async def`, on the same terms as `on_event_async` | |
| both | `.on_failure(handler: (queue: Queue, event: Event, task: Task) => void): this` | pub |
| both | `.on_failure_async(handler: (queue: Queue, event: Event, task: Task) => Promise<void>): this` | pub |
| Python | `.on_failure_async(handler)`: takes an `async def`, on the same terms as `on_event_async` | |
| both | `.on_task(handler: (queue: Queue, event: Event, task: Task) => void): this` | pub |
| both | `.on_task_async(handler: (queue: Queue, event: Event, task: Task) => Promise<void>): this` | pub |
| Python | `.on_task_async(handler)`: takes an `async def`, on the same terms as `on_event_async` | |
| both | `.get_model_for_agent(agent_id: string): string?` | pub |
| both | `.set_policy(policy: Policy): this` | pub |
| both | `.get_policy(): Policy` | pub |
| both | `.set_dir(dir: string): this` | pub |
| both | `.get_dir(): string` | pub |
| both | `.add_task(task: Task): string` | pub |
| both | `.add_task(task)`: a string or json value stands in for the `Task` | |
| both | `.add_reply(id: string, content: string): this` | pub |
| both | `.emit_event(event: Event): Event` | pub |
| both | `.get_task(id: string): Task?` | pub |
| both | `.get_tasks(): Task[]` | pub |
| both | `.find_tasks(predicate: Matcher<Task>): Task[]` | pub |
| Python | `.find_tasks(predicate)`: accepts a `Query` or a callable | |
| both | `.find_task(predicate: Matcher<Task>): Task?` | pub |
| both | `.find_events(matcher: Matcher<Event>): Event[]`: an AQL string stands in for the `Query<Event>` | pub |
| both | `.find_event(matcher: Matcher<Event>): Event?` | pub |
| both | `.cancel_tasks(matches: Matcher<Task>): this` | pub |
| Python | `.cancel_tasks(matches)`: accepts a `Query` or a callable | |
| both | `.cancel_all_tasks(): this` | pub |
| both | `.add_agent(agent: Agent): this` | pub |
| both | `.start(): this` | pub |
| both | `.finish_tasks(matches: Matcher<Task>): Promise<json[]>` | pub |
| Python | `.finish_tasks(matches)`: accepts a `Query` or a callable | |
| both | `.finish_all_tasks(): Promise<json[]>` | pub |
| both | `.finish_task(matches: Matcher<Task>): Promise<json?>` | pub |
| Rust | `.get_finish_reason(): FinishReason?` | pub |
| Python | `.get_finish_reason(): str?`: the string it prints as, such as `policy_violated(turns)` | |
| both | `.get_results(): json[]` | pub |
| both | `.find_results(matches: Matcher<Task>): json[]` | pub |
| both | `.find_results(query)`: an AQL string stands in for the `Query` | |
| both | `.find_result(matches: Matcher<Task>): json?` | pub |
| both | `.find_result(query)`: an AQL string stands in for the `Query` | |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `EventHandler = (queue: Queue, event: Event) => void` | private |
| Rust | `EVENT_STREAM_CAPACITY: number = 1024` | private |
| Rust | `is_task_event(event: Event): boolean` | private |
| Rust | `is_failure(event: Event): boolean` | private |
| Rust | `is_recorded_failure(event: Event): boolean` | private |
| Rust | `AsyncHandler = (queue: Queue, event: Event, task: Task?) => HandlerWork` | private |
| Rust | `HandlerWork = Promise<void>` | private |
| Rust | `AwaitedHandler { matches: (event: Event) => boolean, call: AsyncHandler }` | private |
| Rust | `Delivery = [Event, Task?]` | private |
| Rust | `AwaitedEvents { handlers: AwaitedHandler[], queued: Delivery[], draining: void, queueing: void }` | super |
| Rust | `Run { phase: Phase }` | crate |
| Rust | `Phase` | private |
| Rust | `.Working` | private |
| Rust | `.Draining(FinishReason)` | private |
| Rust | `.Finished(FinishReason)` | private |
| Rust | `impl Default for Run` | crate |
| Rust | `Run.set_draining(reason: FinishReason): void` | crate |
| Rust | `.set_finished(): void` | crate |
| Rust | `.is_working(): boolean` | crate |
| Rust | `.is_finished(): boolean` | crate |
| Rust | `.reason(): FinishReason?` | crate |
| Rust | `.until_draining(): Promise<void>` | crate |
| Rust | `.until_finished(): Promise<void>` | crate |
| Rust | `.reset(): void` | private |
| Rust | `Queue.on_task_event(matches: (event: Event) => boolean, handler: (queue: Queue, event: Event, task: Task) => void): this` | private |
| Rust | `.on_awaited(matches: (event: Event) => boolean, call: AsyncHandler): this` | private |
| Rust | `.queue_events(): void` | private |
| Rust | `.await_handlers(): Promise<void>` | private |
| Rust | `.label_for(id: string): string?` | private |
| Rust | `.result_path(id: string): string` | crate |
| Rust | `.dispatch(task: Task): string` | private |
| Rust | `.matching_tasks(query: Query): Task[]` | private |
| Rust | `.first_matching_task(query: Query): Task?` | private |
| Python | `Queue.find_task(predicate)`: accepts a `Query` or a callable | |
| Rust | `.collect_events(query: Query<Event>, wanted: number): Event[]` | private |
| Rust | `.pending(matches: Query): boolean` | crate |
| Rust | `.ending_reason(): FinishReason?` | crate |
| Rust | `.anything_claimable(): boolean` | private |
| Rust | `.anything_pending(): boolean` | private |
| Rust | `.is_running(): boolean` | private |
| Rust | `.interactive_agents(): string[]` | private |
| Rust | `.bind_agent(agent: Agent): void` | crate |
| Rust | `.has_agent(id: string): boolean` | crate |
| Rust | `.clone_agents(): Agent[]` | crate |
| Rust | `.next_event_or_end(stream: Event): Promise<boolean>` | private |
| Rust | `results_of(matches: Matcher<Task>): Query` | private |

## `crates/agentwerk/src/agents/tasks/trajectory.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Trajectory { id: string, model: string?, replies: Reply[] }` | pub with crate-private fields |
| Python | `Trajectory`: values are read through `get_id()`, `get_model()`, and `get_replies()` | |
| both | `.from_task(agent_id: string, model: string?, task: Task): this` | pub |
| both | `.save(dir: string): void throws io::Error` | pub |
| both | `.get_id(): string` | pub |
| both | `.get_model(): string?` | pub |
| both | `.get_replies(): Reply[]` | pub |
| Rust | `impl Persist for Trajectory` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Trajectory.to_html(): string` | private |
| Rust | `HTML_HEAD: string` | private |
| Rust | `trajectory_path(dir: string, id: string): string` | private |

## `crates/agentwerk/src/codegrep/ast.rs`

Not bound: the whole `codegrep` module is reachable from Python through `GrepTool()` with `syntax="code"`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `MetavariableKind` | pub |
| Rust | `.Plain` | pub |
| Rust | `.Ellipsis` | pub |
| Rust | `Node` | pub |
| Rust | `.Word(string)` | pub |
| Rust | `.Other(string)` | pub |
| Rust | `.Newline` | pub |
| Rust | `.Ellipsis` | pub |
| Rust | `.LongEllipsis` | pub |
| Rust | `.Metavar(string)` | pub |
| Rust | `.MetavarEllipsis(string)` | pub |
| Rust | `.LongMetavarEllipsis(string)` | pub |
| Rust | `.Bracket(open: string, nodes: Node[], close: string)` | pub |
| Rust | `Pattern { nodes: Node[], conf: Conf }` | pub |
| Rust | `.nodes(): Node[]` | pub |
| Rust | `.conf(): Conf` | pub |
| Rust | `.metavariable_names(): string[]` | pub |
| Rust | `ParseError(string)` | pub |
| Rust | `impl Display for ParseError` | pub |
| Rust | `impl Error for ParseError` | pub |
| Rust | `Pattern.parse(source: string, conf: Conf): this throws ParseError` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `parse_seq_until(tokens: Token[], cursor: number, expected_close: string?): Node[] throws void` | private |
| Rust | `validate_metavariable_consistency(nodes: Node[]): void throws ParseError` | private |
| Rust | `walk_metavars(nodes: Node[], seen: Record<string, MetavariableKind>): void throws ParseError` | private |
| Rust | `record_kind(name: string, kind: MetavariableKind, seen: Record<string, MetavariableKind>): void throws ParseError` | private |

## `crates/agentwerk/src/codegrep/conf.rs`

Not bound, like the rest of `codegrep`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Conf { caseless: boolean, multiline: boolean, word_chars: string[], brackets: [string, string][] }` | pub |
| Rust | `.default_multiline(): this` | pub |
| Rust | `.default_singleline(): this` | pub |
| Rust | `.check(): void throws ConfError` | pub |
| Rust | `ConfError(string)` | pub |
| Rust | `impl Display for ConfError` | pub |
| Rust | `impl Error for ConfError` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `word_chars(): string[]` | private |

## `crates/agentwerk/src/codegrep/matcher.rs`

Not bound, like the rest of `codegrep`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Loc { start: number, length: number, substring: string }` | pub |
| Rust | `Metavariable { kind: MetavariableKind, bare_name: string }` | pub |
| Rust | `Match { loc: Loc, captures: [Metavariable, Loc][] }` | pub |
| Rust | `search(pattern: Pattern, target: string): Match[]` | pub |
| Rust | `search_tokens(pattern: Pattern, tokens: Token[], target: string): Match[]` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `MatchParams { caseless: boolean, multiline: boolean, word_chars: string[] }` | private |
| Rust | `Binding { kind: MetavariableKind, token_start: number, byte_start: number, byte_end: number }` | private |
| Rust | `MetavarEnv { bindings: Record<string, Binding> }` | private |
| Rust | `build_match(tokens: Token[], target: string, start: number, end: number, env: MetavarEnv): Match` | private |
| Rust | `byte_start_of(tokens: Token[], target: string, token_idx: number): number` | private |
| Rust | `byte_end_of(tokens: Token[], target: string, token_idx_exclusive: number): number` | private |
| Rust | `match_seq(nodes: Node[], tokens: Token[], position: number, target: string, env: MetavarEnv, excluded_close: string?, outer_prev: Node?, outer_next: Node?, params: MatchParams, close_after: string?): number?` | private |
| Rust | `consume_close_after(tokens: Token[], position: number, close_after: string?): number?` | private |
| Rust | `is_closing_token(token: Token, close_char: string): boolean` | private |
| Rust | `match_node(node: Node, tokens: Token[], cursor: number, target: string, env: MetavarEnv, params: MatchParams): number?` | private |
| Rust | `match_ellipsis(current: Node, tokens: Token[], start: number, rest: Node[], target: string, env: MetavarEnv, excluded_close: string?, next_for_anchor: Node?, outer_next: Node?, params: MatchParams, allow_newlines: boolean, bind_name: string?, close_after: string?): number?` | private |
| Rust | `is_excluded_close(token: Token, excluded_close: string?): boolean` | private |
| Rust | `find_matching_close(tokens: Token[], start: number, target_close: string, allow_newlines: boolean): number?` | private |
| Rust | `match_ellipsis_backref(tokens: Token[], target: string, cursor: number, binding: Binding, params: MatchParams): number?` | private |
| Rust | `byte_boundary_ok(target: string, byte_pos: number, word_chars: string[]): boolean` | private |
| Rust | `char_ending_at(target: string, byte_pos: number): string?` | private |
| Rust | `check_left_anchor(current: Node, prev: Node?, tokens: Token[], target: string, cursor: number, params: MatchParams): boolean` | private |
| Rust | `check_right_anchor(current: Node, next: Node?, tokens: Token[], target: string, cursor: number, params: MatchParams): boolean` | private |
| Rust | `is_singleline_ellipsis(node: Node, params: MatchParams): boolean` | private |
| Rust | `is_multiline_ellipsis(node: Node, params: MatchParams): boolean` | private |
| Rust | `word_eq(a: string, b: string, caseless: boolean): boolean` | private |

## `crates/agentwerk/src/codegrep/mod.rs`

Not bound, like the rest of `codegrep`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod ast`, `mod conf`, `mod matcher`, `mod token` | pub |
| Rust | re-exports `MetavariableKind`, `Node`, `ParseError`, `Pattern`, `Conf`, `ConfError`, `search`, `search_tokens`, `Loc`, `Match`, `Metavariable`, `tokenize_pattern`, `tokenize_target`, `Token` | pub |

## `crates/agentwerk/src/codegrep/token.rs`

Not bound, like the rest of `codegrep`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Token` | pub |
| Rust | `.Ellipsis` | pub |
| Rust | `.LongEllipsis` | pub |
| Rust | `.Metavar(string)` | pub |
| Rust | `.MetavarEllipsis(string)` | pub |
| Rust | `.LongMetavarEllipsis(string)` | pub |
| Rust | `.Word { text: string, start: number }` | pub |
| Rust | `.Open { open: string, close: string, start: number }` | pub |
| Rust | `.Close { close: string, start: number }` | pub |
| Rust | `.Newline { start: number }` | pub |
| Rust | `.Other { text: string, start: number }` | pub |
| Rust | `tokenize_pattern(source: string, conf: Conf): Token[]` | pub |
| Rust | `tokenize_target(source: string, conf: Conf): Token[]` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Token.start(): number` | crate |
| Rust | `scan(source: string, conf: Conf, pattern_mode: boolean): Token[]` | private |
| Rust | `is_blank(ch: string, multiline: boolean): boolean` | private |
| Rust | `read_metavar_name(rest: string): [string, number]?` | private |
| Rust | `is_name_start(c: string): boolean` | private |
| Rust | `is_name_continue(c: string): boolean` | private |

## `crates/agentwerk/src/event.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Event { name: string, data: json, task_id: string, agent_id: string, label: string?, created_at: number }` | pub with crate-private fields |
| both | `.RUN_STARTED`, `.RUN_FINISHED`, `.TASK_CREATED`, `.TASK_STARTED`, `.TASK_FINISHED`, `.TASK_FAILED`, `.TURN_STARTED`: string | pub |
| both | `.REQUEST_STARTED`, `.REQUEST_FINISHED`, `.REQUEST_FAILED`, `.REQUEST_RETRIED`, `.TEXT_CHUNK_RECEIVED`, `.TOOL_CALL_REPAIRED`: string | pub |
| both | `.TOOL_CALL_DECLINED`, `.TOOL_CALL_STARTED`, `.TOOL_CALL_FINISHED`, `.TOOL_CALL_FAILED`: string | pub |
| both | `.FILE_OPEN_FINISHED`, `.FILE_OPEN_FAILED`, `.KNOWLEDGE_WRITTEN`, `.KNOWLEDGE_READ`, `.KNOWLEDGE_REMOVED`, `.KNOWLEDGE_LISTED`, `.KNOWLEDGE_FAILED`: string | pub |
| both | `.POLICY_VIOLATED`, `.SCHEMA_RETRIED`, `.COMPACTION_STARTED`, `.COMPACTION_PROGRESS`, `.COMPACTION_FINISHED`, `.COMPACTION_FAILED`: string | pub |
| both | `Event.new(name: string): this` | pub |
| both | `.data(value: json): this` | pub |
| both | `.task_id(task_id: string): this` | pub |
| both | `.agent_id(agent_id: string): this` | pub |
| both | `.get_name(): string` | pub |
| both | `.get_data(): json` | pub |
| both | `.get_task_id(): string` | pub |
| both | `.get_agent_id(): string` | pub |
| both | `.get_label(): string?` | pub |
| both | `.get_created_at(): number` | pub |
| Rust | `default_logger(): (event: Event) => void` | pub |
| Python | not bound: pass your own handler to `Queue.on_event(handler)` | |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Event.BUILTIN_NAMES: string[]` | crate |
| Rust | `take(object: json, field: string): any` | private |
| Rust | `take_or(object: json, field: string, default: any): any` | private |
| Rust | `data_str(event: Event, key: string): string?` | private |
| Rust | `data_u64(event: Event, key: string): number?` | private |
| Rust | `compact_input(input: json): string` | private |

## `crates/agentwerk/src/lib.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod agents`, `mod codegrep`, `mod event`, `mod providers`, `mod schemas`, `mod tools` | pub |
| Rust | re-exports `Agent`, `Query`, `Reply`, `Status`, `Task`, `Queue`, `Policy`, `PolicyViolation`, `Knowledge`, `Trajectory`, `Schema`, `Event`, `FinishReason`, `Directive`, `Text` | pub |
| Python | `agentwerk` exports every bound class from one flat module | |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod persistence`, `mod prompts` | crate |

## `crates/agentwerk/src/persistence.rs`

Not bound: the crate writes its own files.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Persist { Key, save(dir: string): void throws io::Error, load(dir: string, key: Key): Self throws io::Error }` | crate |
| Rust | `TEMP_COUNTER: number` | private |
| Rust | `write_atomic(path: string, bytes: number[]): void throws io::Error` | crate |
| Rust | `append_line(path: string, line: string): void throws io::Error` | crate |
| Rust | `output_path(task_id: string, tool_use_id: string): string` | crate |

## `crates/agentwerk/src/prompts/builder.rs`

Not bound: `prompts` is crate-internal, reached through `Agent.role(..)`.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Prompt { system: string, task: string? }` | crate |
| Rust | `PromptBuilder { role: Section?, knowledge: Section?, task: Section?, directives: Section[] }` | crate |
| Rust | `.role(body: string): this` | crate |
| Rust | `.knowledge(body: string): this` | crate |
| Rust | `.task(body: string): this` | crate |
| Rust | `.append_directive(body: string): this` | crate |
| Rust | `.build(): Prompt` | crate |

## `crates/agentwerk/src/prompts/directives.rs`

One of the two public parts of `prompts`, beside `Text`: `Directive` reaches the caller as a root re-export and carries nothing but the key constants, one per catalogue heading. `Agent::directives` takes the function deciding all of them, which sees a key and answers a template. `DirectiveStore` is the crate-private carrier of that function, which no host names. The constants sit on `Directive` in both languages, so `Directive::GREP_FAILED` and `Directive.GREP_FAILED` are the same name.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Directive.REPLY_REJECTED: string = "reply_rejected"`, and one constant per catalogue heading, each also a crate-private `const` under the same name that the render sites write: `NO_TOOL_CALLED`, `ARGUMENTS_REJECTED`, `ARGUMENTS_EXPECTED`, `RESULT_SCHEMA_REQUIRED`, `SUMMARY_REQUESTED`, `KNOWLEDGE_INDEX_TRUNCATED`, `TOOL_NOT_FOUND`, `NO_TOOLS_REGISTERED`, `TOOL_PANICKED`, `TOOL_OUTPUT_EMPTY`, `TOOL_OUTPUT_OFFLOADED`, `EDIT_FILE_READ_FAILED`, `EDIT_FILE_OLD_STRING_NOT_FOUND`, `EDIT_FILE_OLD_STRING_NOT_UNIQUE`, `EDIT_FILE_WRITE_FAILED`, `WRITE_FILE_PARENT_NOT_CREATED`, `WRITE_FILE_FAILED`, `READ_FILE_PATH_IS_DIRECTORY`, `READ_FILE_PATH_IS_DIRECTORY_WITH_ENTRIES`, `READ_FILE_IS_BINARY`, `READ_FILE_NOT_FOUND`, `READ_FILE_FAILED`, `LIST_DIRECTORY_PATH_IS_FILE`, `LIST_DIRECTORY_NOT_FOUND`, `LIST_DIRECTORY_FAILED`, `PATH_HINT_DIRECTORY_LISTED`, `PATH_HINT_SUGGESTION`, `PATH_HINT_WORKING_DIRECTORY`, `COMMAND_CANCELLED`, `COMMAND_TIMED_OUT`, `COMMAND_NOT_STARTED`, `COMMAND_MISSING`, `COMMAND_SHELL_OPERATOR_FOUND`, `COMMAND_QUOTE_UNTERMINATED`, `COMMAND_CONTROL_CHARACTER_FOUND`, `COMMAND_ASSIGNMENT_FOUND`, `COMMAND_FLAG_DENIED`, `COMMAND_PATTERN_DENIED`, `COMMAND_NOT_ALLOWED`, `COMMAND_FLAG_NOT_ALLOWED`, `GREP_CANCELLED`, `GREP_TIMED_OUT`, `GREP_FAILED`, `GREP_GLOB_REJECTED`, `GREP_FILE_TYPE_UNKNOWN`, `GREP_PATTERN_REJECTED`, `CODE_PATTERN_REJECTED`, `CODE_CONSTRAINT_INCOMPLETE`, `CODE_CONSTRAINT_METAVARIABLE_UNKNOWN`, `CODE_CONSTRAINT_REGEX_REJECTED`, `FETCH_URL_TOO_LONG`, `FETCH_URL_SCHEME_MISSING`, `FETCH_URL_SCHEME_UNSUPPORTED`, `FETCH_URL_CREDENTIALS_PRESENT`, `FETCH_URL_HOST_MISSING`, `FETCH_URL_HOST_NOT_RESOLVABLE`, `FETCH_URL_TOO_MANY_REDIRECTS`, `FETCH_URL_REQUEST_FAILED`, `FETCH_URL_BODY_NOT_READ`, `FETCH_URL_RESPONSE_TOO_LARGE`, `FETCH_URL_REDIRECT_LOCATION_MISSING`, `KNOWLEDGE_PAGE_NOT_FOUND`, `KNOWLEDGE_WRITE_FAILED`, `KNOWLEDGE_REMOVE_FAILED`, `QUEUE_UNAVAILABLE`, `TASK_ID_MISSING`, `TASK_NOT_ASSIGNED`, `TASK_NOT_FOUND`, `TASK_RESULT_MISSING`, `TASK_QUERY_INVALID`, `TASK_EDIT_INCOMPLETE`, `TASK_TRANSITION_REJECTED`, `HANDOVER_RESULT_MISSING`, `HANDOVER_SCHEMA_INVALID`, `SCHEMA_FALSE_REJECTED`, `SCHEMA_TYPE_MISMATCHED`, `SCHEMA_CONST_MISMATCHED`, `SCHEMA_ENUM_MISMATCHED`, `SCHEMA_ANY_OF_UNMATCHED`, `SCHEMA_ONE_OF_AMBIGUOUS`, `SCHEMA_NOT_MATCHED`, `SCHEMA_PROPERTY_MISSING`, `SCHEMA_PROPERTY_UNEXPECTED`, `SCHEMA_ARRAY_TOO_SHORT`, `SCHEMA_ARRAY_TOO_LONG`, `SCHEMA_STRING_TOO_SHORT`, `SCHEMA_STRING_TOO_LONG`, `SCHEMA_PATTERN_UNMATCHED`, `SCHEMA_NUMBER_TOO_SMALL`, `SCHEMA_NUMBER_TOO_LARGE`, `SCHEMA_HINT_UNQUOTE`, `SCHEMA_HINT_JSON`, `SCHEMA_HINT_QUOTE` | pub |
| Rust | `.ALL: string[]` | pub |
| Python | `Directive.ALL` is not published; `register` walks it to set the key constants | |
| Rust | `Directive { }`, the key namespace | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `directives!(name = key, ..)`, declaring each key's crate-private `const`, its `Directive` constant, and its `ALL` entry | private |
| Rust | `CATALOGUE: string[]` | private |
| Rust | `DirectiveStore { compute: (key: string) => string? }` | crate |
| Rust | `.new(compute: (key: string) => string?): this` | crate |
| Rust | `.render(key: string, values: [string, string][]): string` | crate |
| Rust | `impl Default for DirectiveStore`, the built-in text | crate |
| Rust | `impl Debug for DirectiveStore` | crate |
| Rust | `built_in(key: string, values: [string, string][]): string` | crate |
| Rust | `bind(template: string, values: [string, string][]): string` | private |
| Rust | `entries(markdown: string): [string, string][]` | private |
| Rust | `catalogue(): Record<string, string>` | private |

## `crates/agentwerk/src/prompts/mod.rs`

Not bound, except what `directives.rs` and `text.rs` re-export through it.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod builder`, `mod section` | private |
| Rust | `pub(crate) mod directives`, `pub(crate) mod text` | crate |
| Rust | `CONTEXT_TEMPLATE: string` | private |
| Rust | `retry_directive(detail: string): string` | crate |
| Rust | `compaction_directive(): string` | crate |
| Rust | `schema_directive(schema: Schema): string` | crate |
| Rust | `arguments_retry_detail(tool_name: string, violations: string, schema: json?): string` | crate |
| Rust | `context_values(dir: string, policy: Policy, stats: Stats, task_id: string): [string, string][]` | crate |
| Rust | `optional(value: string?): string` | private |
| Rust | `render_context(values: [string, string][]): string` | crate |
| Rust | `format_current_date(): string` | private |

## `crates/agentwerk/src/prompts/section.rs`

Not bound, like the rest of `prompts`.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Section { heading: string?, body: string }` | crate |
| Rust | `.role(body: string): this` | crate |
| Rust | `.knowledge(body: string): this` | crate |
| Rust | `.task(body: string): this` | crate |
| Rust | `.directive(body: string): this` | crate |
| Rust | `.render(): string` | crate |

## `crates/agentwerk/src/prompts/text.rs`

Not bound: Python passes a `str` for the text and an `os.PathLike` for the file holding it, which `convert.py_to_text` reads.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Text(string)`, the trimmed text | pub |
| Rust | `.from_file(file: string): this throws io::Error` | pub |
| Rust | `.into_string(): string` | pub |
| Rust | `impl From<&str> for Text`, `impl From<String> for Text`, and `impl From<&String> for Text` | pub |
| Rust | `impl From<&Path> for Text`, `impl From<PathBuf> for Text`, and `impl From<&PathBuf> for Text`, reading the file | pub |
| Rust | `impl From<Text> for String` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Text.read(file: string): this`, panicking where `from_file` reports | private |

## `crates/agentwerk/src/providers/anthropic.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `Anthropic(Endpoint)` | pub |
| Rust | `.new(api_key: string): this` | pub |
| Python | `Anthropic(api_key, base_url=.., timeout=..)` | |
| Rust | `.base_url(url: string): this` | pub |
| Python | `Anthropic(api_key, base_url=..)` | |
| Rust | `.timeout(duration: number): this` | pub |
| Python | `Anthropic(api_key, timeout=..)` | |
| Rust | `impl ProviderLike for Anthropic` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_BASE_URL: string = "https://api.anthropic.com"` | private |
| Rust | `Anthropic.from_env(): this throws ProviderError` | crate |
| Rust | `AnthropicMessages` | crate |
| Rust | `impl Protocol for AnthropicMessages` | crate |
| Rust | `serialize_messages(messages: Message[]): json[]` | private |
| Rust | `serialize_content_blocks(blocks: ContentBlock[]): json[]` | private |
| Rust | `serialize_content_block(block: ContentBlock): json?` | private |
| Rust | `serialize_tool(tool: Tool): json` | private |
| Rust | `supports_adaptive_thinking(model: string): boolean` | private |
| Rust | `block_number(json: json): number` | private |
| Rust | `decode_message_start(json: json, reply: ResponseBuilder): void` | private |
| Rust | `decode_block_start(json: json, reply: ResponseBuilder): void` | private |
| Rust | `decode_block_delta(json: json, reply: ResponseBuilder): void` | private |
| Rust | `decode_message_delta(json: json, reply: ResponseBuilder): void` | private |
| Rust | `status_from_stop_reason(raw: string): ResponseStatus` | private |

## `crates/agentwerk/src/providers/endpoint.rs`

Not bound: every provider makes its HTTP call through this one type.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_REQUEST_TIMEOUT: number = 600000` | crate |
| Rust | `Endpoint { api_key: string, base_url: string, client: reqwest::Client, timeout: number }` | crate |
| Rust | `.new(api_key: string, base_url: string): this` | crate |
| Rust | `.base_url(url: string): this` | crate |
| Rust | `.timeout(duration: number): this` | crate |
| Rust | `.api_key(): string` | crate |
| Rust | `.post(path: string, body: json): reqwest::RequestBuilder` | crate |
| Rust | `.send(request: reqwest::RequestBuilder, classify: (status: number, body: string) => ProviderError?): Promise<reqwest::Response throws ProviderError>` | crate |
| Rust | `map_http_errors(response: reqwest::Response, classify: (status: number, body: string) => ProviderError?): Promise<reqwest::Response throws ProviderError>` | private |
| Rust | `retry_delay_from_headers(response: reqwest::Response): number?` | private |
| Rust | `classify_common_status(status: number, body: string): ProviderError?` | private |
| Rust | `fallback_http_error(status: number, body: string, retry_delay: number?): ProviderError` | private |
| Rust | `build_client(timeout: number): reqwest::Client` | private |
| Rust | `root_certificates_from(file: string?, dir: string?): reqwest::Certificate[]` | private |

## `crates/agentwerk/src/providers/environment.rs`

Not bound: `Provider.from_env()` and `Model.from_env()` read these variables.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DetectedProvider` | crate |
| Rust | `.Anthropic` | crate |
| Rust | `.Mistral` | crate |
| Rust | `.OpenAi` | crate |
| Rust | `.LiteLlm` | crate |
| Rust | `env_or(name: string, default: string): string` | crate |
| Rust | `env_required(name: string): string throws ProviderError` | crate |
| Rust | `env_opt(name: string): string?` | crate |
| Rust | `provider_from_env(): Provider throws ProviderError` | crate |
| Rust | `model_from_env(): string throws ProviderError` | crate |
| Rust | `model_from_env_with(get: (name: string) => string?): string throws ProviderError` | crate |
| Rust | `context_window_from_env(): number?` | crate |
| Rust | `context_window_from_env_with(get: (name: string) => string?): number?` | crate |
| Rust | `detect_provider_name(get_env: (name: string) => string?): DetectedProvider throws ProviderError` | crate |

## `crates/agentwerk/src/providers/error.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `ProviderError` | pub |
| Rust | `.AuthenticationFailed { message: string }` | pub |
| Rust | `.PermissionDenied { message: string }` | pub |
| Rust | `.ModelNotFound { message: string }` | pub |
| Rust | `.ContextWindowExceeded { message: string }` | pub |
| Rust | `.SafetyFilterTriggered { message: string }` | pub |
| Rust | `.RateLimited { message: string, status: number, retry_delay: number? }` | pub |
| Rust | `.StatusUnclassified { status: number, message: string, retryable: boolean, retry_delay: number? }` | pub |
| Rust | `.ConnectionFailed { message: string }` | pub |
| Rust | `.StreamInterrupted { message: string }` | pub |
| Rust | `.ResponseMalformed { message: string }` | pub |
| Rust | `.ProviderUnrecognized { message: string }` | pub |
| Rust | `.is_retryable(): boolean` | pub |
| Python | not bound: the error arrives as `RuntimeError` | |
| Rust | `.retry_delay(): number?` | pub |
| Python | not bound: the error arrives as `RuntimeError` | |
| Rust | `.kind(): RequestErrorKind` | pub |
| Python | not bound: the error arrives as `RuntimeError` | |
| Rust | `impl Display for ProviderError` | pub |
| Rust | `impl Error for ProviderError` | pub |
| Rust | `RequestErrorKind` | pub |
| Python | a string inside `Event.get_data()`: `data["kind"]` | |
| Rust | `.AuthenticationFailed` | pub |
| Rust | `.PermissionDenied` | pub |
| Rust | `.ModelNotFound` | pub |
| Rust | `.ContextWindowExceeded` | pub |
| Rust | `.SafetyFilterTriggered` | pub |
| Rust | `.RateLimited` | pub |
| Rust | `.StatusUnclassified` | pub |
| Rust | `.ConnectionFailed` | pub |
| Rust | `.StreamInterrupted` | pub |
| Rust | `.ResponseMalformed` | pub |
| Rust | `.ProviderUnrecognized` | pub |
| Rust | `.get_name(): string` | pub |
| Python | not bound: the kind is already a string | |
| Rust | `impl Display for RequestErrorKind` | pub |
| both | `ProviderResult<T> = T throws ProviderError` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `OVERFLOW_PATTERNS: string[]` | private |
| Rust | `RATE_LIMIT_PATTERNS: string[]` | private |
| Rust | `recover_wrapped_error(status: number, body: string, retry_delay: number?): ProviderError?` | crate |

## `crates/agentwerk/src/providers/frames.rs`

Not bound: it repairs a reply before the loop reads it.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `TOOL_CALL_OPEN: string = "<tool_call>"` | private |
| Rust | `TOOL_CALL_CLOSE: string = "</tool_call>"` | private |
| Rust | `FUNCTION_OPEN: string = "<function="` | private |
| Rust | `FUNCTION_CLOSE: string = "</function>"` | private |
| Rust | `PARAMETER_OPEN: string = "<parameter="` | private |
| Rust | `PARAMETER_CLOSE: string = "</parameter>"` | private |
| Rust | `recover_framed_calls(response: ModelResponse, on_event: (event: StreamEvent) => void): void` | crate |
| Rust | `find_framed_calls(response: ModelResponse): FramedCall[]` | private |
| Rust | `strip_framed_syntax(response: ModelResponse): void` | private |
| Rust | `report_declined(call: FramedCall, reason: ToolDeclineKind, on_event: (event: StreamEvent) => void): void` | private |
| Rust | `decline_reason(status: ResponseStatus): ToolDeclineKind?` | private |
| Rust | `apply_framed_calls(response: ModelResponse, framed: FramedCall[], on_event: (event: StreamEvent) => void): void` | private |
| Rust | `tool_call_name(block: ContentBlock): string?` | private |
| Rust | `nth_delivered_input(response: ModelResponse, name: string, at: number): json?` | private |
| Rust | `report_repaired(call: FramedCall, on_event: (event: StreamEvent) => void): void` | private |
| Rust | `arguments_as_object(call: FramedCall): json` | private |
| Rust | `is_same_call(framed: FramedCall, delivered: json): boolean` | private |
| Rust | `FramedCall { name: string, arguments: [string, string][] }` | private |
| Rust | `split_framed_calls(text: string): [string, FramedCall[]]` | private |
| Rust | `read_function_block(body: string): FramedCall?` | private |
| Rust | `read_parameters(body: string): [string, string][]` | private |
| Rust | `is_tool_name(name: string): boolean` | private |

## `crates/agentwerk/src/providers/litellm.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `LiteLlm(Endpoint)` | pub |
| Rust | `.new(api_key: string): this` | pub |
| Python | `LiteLlm(api_key, base_url=.., timeout=..)` | |
| Rust | `.base_url(url: string): this` | pub |
| Python | `LiteLlm(api_key, base_url=..)` | |
| Rust | `.timeout(duration: number): this` | pub |
| Python | `LiteLlm(api_key, timeout=..)` | |
| Rust | `impl ProviderLike for LiteLlm` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_BASE_URL: string = "http://localhost:4000"` | private |
| Rust | `LiteLlm.from_env(): this throws ProviderError` | crate |

## `crates/agentwerk/src/providers/mistral.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `Mistral(Endpoint)` | pub |
| Rust | `.new(api_key: string): this` | pub |
| Python | `Mistral(api_key, base_url=.., timeout=..)` | |
| Rust | `.base_url(url: string): this` | pub |
| Python | `Mistral(api_key, base_url=..)` | |
| Rust | `.timeout(duration: number): this` | pub |
| Python | `Mistral(api_key, timeout=..)` | |
| Rust | `impl ProviderLike for Mistral` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_BASE_URL: string = "https://api.mistral.ai"` | private |
| Rust | `Mistral.from_env(): this throws ProviderError` | crate |

## `crates/agentwerk/src/providers/mod.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod types` | pub |
| Rust | re-exports `Anthropic`, `ProviderError`, `ProviderResult`, `RequestErrorKind`, `LiteLlm`, `Mistral`, `Model`, `OpenAi`, `Provider`, `ProviderLike`, and the `types` values | pub |
| Python | the four providers are bound; the request and response types are not | |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod environment`, `mod model` | crate |
| Rust | `mod anthropic`, `mod endpoint`, `mod error`, `mod frames`, `mod litellm`, `mod mistral`, `mod openai`, `mod provider`, `mod stream` | private |

## `crates/agentwerk/src/providers/model.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Model { name: string, context_window: number?, reasoning_effort: ReasoningEffort }` | pub with crate-private fields |
| Rust | `.new(name: string): this` | pub |
| Python | `Model(name)` | |
| both | `.from_env(): this throws ProviderError` | pub |
| both | `.context_window(size: number): this` | pub |
| both | `.reasoning_effort(effort: ReasoningEffort): this` | pub |
| both | `.get_name(): string` | pub |
| both | `.get_context_window(): number?` | pub |
| both | `.get_reasoning_effort(): ReasoningEffort` | pub |
| Rust | `impl From<string> for Model` | pub |
| Python | not bound: `Model(name)` already takes the string | |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `context_window_for(name: string): number?` | private |

## `crates/agentwerk/src/providers/openai.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `OpenAi(Endpoint)` | pub |
| Rust | `.new(api_key: string): this` | pub |
| Python | `OpenAi(api_key, base_url=.., timeout=..)` | |
| Rust | `.base_url(url: string): this` | pub |
| Python | `OpenAi(api_key, base_url=..)` | |
| Rust | `.timeout(duration: number): this` | pub |
| Python | `OpenAi(api_key, timeout=..)` | |
| Rust | `impl ProviderLike for OpenAi` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_BASE_URL: string = "https://api.openai.com"` | private |
| Rust | `OpenAi.from_env(): this throws ProviderError` | crate |
| Rust | `OpenAiChat` | crate |
| Rust | `impl Protocol for OpenAiChat` | crate |
| Rust | `serialize_messages(request: ModelRequest): json[]` | private |
| Rust | `serialize_user_blocks(blocks: ContentBlock[]): json[]` | private |
| Rust | `serialize_assistant_message(blocks: ContentBlock[]): json` | private |
| Rust | `serialize_tool(tool: Tool): json` | private |
| Rust | `decode_reasoning(delta: json, reply: ResponseBuilder): void` | private |
| Rust | `decode_tool_calls(delta: json, reply: ResponseBuilder): void` | private |
| Rust | `status_from_finish_reason(raw: string): ResponseStatus` | private |

## `crates/agentwerk/src/providers/provider.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ProviderLike { respond(request: ModelRequest, on_event: (event: StreamEvent) => void): Promise<ModelResponse throws ProviderError> }` | pub |
| Python | not bound: implement it in Rust to write a new LLM provider | |
| Rust | `impl ProviderLike for Arc<T>` | pub |
| Rust | `Provider(ProviderLike)` | pub |
| Python | `Provider`: an opaque handle | |
| Rust | `.new(provider: ProviderLike): this` | pub |
| Python | not bound: the per-vendor constructors already hand back a `Provider` | |
| both | `.from_env(): this throws ProviderError` | pub |
| Rust | `.verify(model: string): Promise<void throws ProviderError>` | pub |
| Rust | `impl From<P> for Provider` | pub |
| Rust | `impl Deref for Provider` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Protocol { PATH: string, authenticate(posted: reqwest::RequestBuilder, api_key: string): reqwest::RequestBuilder, serialize(request: ModelRequest): json, classify_error(status: number, body: string): ProviderError?, decode(payload: json, reply: ResponseBuilder): void, recover(reply: ModelResponse, on_event: (event: StreamEvent) => void): void }` | crate |
| Rust | `respond(endpoint: Endpoint, request: ModelRequest, on_event: (event: StreamEvent) => void): Promise<ModelResponse throws ProviderError>` | crate |

## `crates/agentwerk/src/providers/stream.rs`

Not bound: it turns one HTTP response into a `ModelResponse`.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `read_reply(response: reqwest::Response, on_event: (event: StreamEvent) => void, decode: (payload: json, reply: ResponseBuilder) => void): Promise<ModelResponse throws ProviderError>` | crate |
| Rust | `read_stream(response: reqwest::Response, ingest: (payload: json) => void): Promise<void throws ProviderError>` | crate |
| Rust | `LineBuffer { buffer: number[] }` | private |
| Rust | `.new(): this` | private |
| Rust | `.push(chunk: number[]): json[]` | private |
| Rust | `read_data_line(line: number[]): json?` | private |
| Rust | `ToolCallKey` | crate |
| Rust | `.Numbered(number)` | crate |
| Rust | `.Unnumbered(number)` | crate |
| Rust | `ResponseBuilder { on_event: (event: StreamEvent) => void, model: string, status: ResponseStatus, overflowed: boolean, usage: TokenUsage, blocks: Block[] }` | crate |
| Rust | `.new(on_event: (event: StreamEvent) => void): this` | crate |
| Rust | `.set_model(name: string): void` | crate |
| Rust | `.set_status(status: ResponseStatus): void` | crate |
| Rust | `.set_context_window_exceeded(): void` | crate |
| Rust | `.set_input_tokens(tokens: number): void` | crate |
| Rust | `.set_output_tokens(tokens: number): void` | crate |
| Rust | `.add_text(fragment: string): void` | crate |
| Rust | `.add_thinking(fragment: string): void` | crate |
| Rust | `.add_signature(fragment: string): void` | crate |
| Rust | `.thinking_block(): Block` | private |
| Rust | `.add_redacted_thinking(data: string): void` | crate |
| Rust | `.open_tool_call(numbered: number?, id: string, name: string): ToolCallKey` | crate |
| Rust | `.add_arguments(key: ToolCallKey, fragment: string): void` | crate |
| Rust | `.into_response(): ModelResponse throws ProviderError` | crate |
| Rust | `.key_for(id: string): ToolCallKey` | private |
| Rust | `.tool_call_at(key: ToolCallKey): number` | private |
| Rust | `.tool_call_count(): number` | private |
| Rust | `.emit(event: StreamEvent): void` | private |
| Rust | `Block` | private |
| Rust | `.Text(string)` | private |
| Rust | `.Thinking { thinking: string, signature: string }` | private |
| Rust | `.RedactedThinking { data: string }` | private |
| Rust | `.ToolCall { key: ToolCallKey, id: string, name: string, arguments: string }` | private |
| Rust | `.tool_call_key(): ToolCallKey?` | private |
| Rust | `.holds_tool_call_id(wanted: string): boolean` | private |
| Rust | `.into_content(): ContentBlock` | private |
| Rust | `read_arguments(arguments: string): json` | crate |
| Rust | `model_or_unknown(model: string): string` | private |

## `crates/agentwerk/src/providers/types.rs`

Not bound, apart from `ReasoningEffort` and `ToolDeclineKind`: Python binds the four providers, not the shapes they are built from.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ReasoningEffort` | pub |
| Python | a string: `"off"`, `"low"`, `"medium"`, `"high"` | |
| Rust | `.Off` | pub |
| Rust | `.Low` | pub |
| Rust | `.Medium` | pub |
| Rust | `.High` | pub |
| Rust | `impl Display for ReasoningEffort` | pub |
| Rust | `ModelRequest { model: string, system_prompt: string, messages: Message[], tools: Tool[], max_request_tokens: number?, reasoning_effort: ReasoningEffort }` | pub |
| Rust | `Message` | pub |
| Rust | `.System { content: string }` | pub |
| Rust | `.User { content: ContentBlock[] }` | pub |
| Rust | `.Assistant { content: ContentBlock[] }` | pub |
| Rust | `.user(text: string): this` | pub |
| Rust | `.system(text: string): this` | pub |
| Rust | `.assistant(text: string): this` | pub |
| Rust | `AsUserMessage { as_user_message(): Message }` | pub |
| Rust | `ContentBlock` | pub |
| Rust | `.Text { text: string }` | pub |
| Rust | `.ToolUse { id: string, name: string, input: json }` | pub |
| Rust | `.ToolResult { tool_use_id: string, content: string, succeeded: boolean }` | pub |
| Rust | `.Thinking { thinking: string, signature: string }` | pub |
| Rust | `.RedactedThinking { data: string }` | pub |
| Rust | `ResponseStatus` | pub |
| Rust | `.EndTurn` | pub |
| Rust | `.StopSequence` | pub |
| Rust | `.ToolUse` | pub |
| Rust | `.OutputTruncated` | pub |
| Rust | `.Refused` | pub |
| Rust | `.PauseTurn` | pub |
| Rust | `TokenUsage { input_tokens: number, output_tokens: number }` | pub |
| Rust | `impl AddAssign<TokenUsage> for TokenUsage` | pub |
| Rust | `ModelResponse { content: ContentBlock[], status: ResponseStatus, usage: TokenUsage, model: string }` | pub |
| Rust | `ToolDeclineKind` | pub |
| Python | a string inside `Event.get_data()`: `data["kind"]` | |
| Rust | `.OutputTruncated` | pub |
| Rust | `.ReplyNotFinished` | pub |
| Rust | `.AlreadyDelivered` | pub |
| Rust | `.get_name(): string` | pub |
| Rust | `impl Display for ToolDeclineKind` | pub |
| Rust | `StreamEvent` | pub |
| Rust | `.TextDelta { text: string }` | pub |
| Rust | `.ToolCallRepaired { tool_name: string, call_id: string }` | pub |
| Rust | `.ToolCallDeclined { tool_name: string, kind: ToolDeclineKind }` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ReasoningEffort.label(): string?` | crate |
| Rust | `default_true(): boolean` | private |

## `crates/agentwerk/src/schemas/mod.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `Schema { inner: SchemaBody }` | pub |
| Rust | `.new(document: json): this throws SchemaParseError` | pub |
| Python | `Schema(document)` | |
| both | `.validate(value: json): [json, string[]] throws SchemaViolations` | pub |
| Rust | `.get_raw_schema(): json` | pub |
| Python | not bound: Python already holds the document it passed to `Schema(document)` | |
| Rust | `impl TryFrom<json> for Schema` | pub |
| Python | not bound: `Schema(document)` takes the Python object | |
| Rust | `impl TryFrom<string> for Schema` | pub |
| Python | not bound: a document read from a file is parsed before it gets there | |
| Rust | `impl Debug for Schema` | pub |
| Rust | `impl Serialize for Schema` | pub |
| Rust | `impl Deserialize for Schema` | pub |
| both | `SchemaViolation { instance_path: string, message: string }` | pub |
| Rust | `impl Display for SchemaViolation` | pub |
| both | `SchemaViolations(SchemaViolation[])` | pub |
| Rust | `impl Deref for SchemaViolations` | pub |
| Rust | `impl Display for SchemaViolations` | pub |
| Rust | `impl Error for SchemaViolations` | pub |
| both | `SchemaParseError { message: string }` | pub |
| Rust | `impl Display for SchemaParseError` | pub |
| Rust | `impl Error for SchemaParseError` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod store` | private |
| Rust | `SchemaBody { compiled: Node, raw_document: json }` | private |
| Rust | `Schema.check(instance: json): void throws SchemaViolation[]` | private |
| Rust | `Node { types: JsonType[]?, enum_values: json[]?, const_value: json?, all_of: Node[]?, any_of: Node[]?, one_of: Node[]?, not: Node?, if_schema: Node?, then_schema: Node?, else_schema: Node?, properties: [string, Node][]?, required: string[]?, additional_properties_forbidden: boolean, items: Node?, min_items: number?, max_items: number?, minimum: number?, maximum: number?, min_length: number?, max_length: number?, pattern: regex::Regex? }` | private |
| Rust | `JsonType` | private |
| Rust | `.Object` | private |
| Rust | `.Array` | private |
| Rust | `.String` | private |
| Rust | `.Number` | private |
| Rust | `.Integer` | private |
| Rust | `.Boolean` | private |
| Rust | `.Null` | private |
| Rust | `.parse(s: string): JsonType?` | private |
| Rust | `.matches(value: json): boolean` | private |
| Rust | `.name(): string` | private |
| Rust | `SUPPORTED_KEYWORDS: string[]` | private |
| Rust | `compile(value: json, schema_path: string): Node throws SchemaParseError` | private |
| Rust | `parse_subschema(obj: Record<string, json>, key: string, schema_path: string): Node? throws SchemaParseError` | private |
| Rust | `parse_subschema_array(obj: Record<string, json>, key: string, schema_path: string): Node[]? throws SchemaParseError` | private |
| Rust | `parse_type(s: string, schema_path: string, key: string): JsonType throws SchemaParseError` | private |
| Rust | `parse_number(v: json?, schema_path: string, key: string): number? throws SchemaParseError` | private |
| Rust | `parse_usize(v: json?, schema_path: string, key: string): number? throws SchemaParseError` | private |
| Rust | `compile_regex(pattern: string, schema_path: string, key: string): regex::Regex throws SchemaParseError` | private |
| Rust | `wrong_type(schema_path: string, key: string, expected: string, got: json): SchemaParseError` | private |
| Rust | `parse_err(schema_path: string, message: string): SchemaParseError` | private |
| Rust | `value_label(v: json): string` | private |
| Rust | `escape_pointer(segment: string): string` | private |
| Rust | `Node.check(instance: json, instance_path: string, out: SchemaViolation[]): void` | private |
| Rust | `.accepts(instance: json): boolean` | private |
| Rust | `.check_object(map: Record<string, json>, instance_path: string, out: SchemaViolation[]): void` | private |
| Rust | `.check_array(arr: json[], instance_path: string, out: SchemaViolation[]): void` | private |
| Rust | `.check_string(s: string, instance_path: string, out: SchemaViolation[]): void` | private |
| Rust | `.check_number(n: number, instance_path: string, out: SchemaViolation[]): void` | private |
| Rust | `.violation(instance_path: string, message: string, out: SchemaViolation[]): void` | private |
| Rust | `join_or(labels: string[]): string` | private |
| Rust | `retype_hint(types: JsonType[], instance: json): string?` | private |
| Rust | `Node.coerce(value: json, instance_path: string, out: string[]): void` | private |
| Rust | `.enum_candidate(value: json): json?` | private |
| Rust | `text_form(value: json): string` | private |
| Rust | `JsonType.retype(value: json): json?` | private |
| Rust | `retype_integer(text: string): json?` | private |
| Rust | `retype_number(text: string): json?` | private |
| Rust | `retype_boolean(text: string): json?` | private |
| Rust | `decode_json(text: string, fits: (value: json) => boolean): json?` | private |

## `crates/agentwerk/src/tools/code.rs`

Not bound: it backs `grep`'s `syntax: "code"` shape matching.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run(files: [string, string][], query: Query, interrupt: boolean, deadline: Instant): Event` | super |
| Rust | `for_each_file(files: [string, string][], interrupt: boolean, deadline: Instant, visit: (path: string, content: string) => void): void` | private |
| Rust | `line_and_byte_column(content: string, byte_offset: number): [number, number]` | private |
| Rust | `render_summary(substring: string): string` | private |
| Rust | `render_captures(captures: [Metavariable, Loc][]): string` | private |
| Rust | `truncate_to_chars(text: string, max_chars: number): string` | private |
| Rust | `parse_constraints(constraints: json, pattern: Pattern): [string, regex::Regex][] throws string` | private |
| Rust | `satisfies_constraints(found: Match, constraints: [string, regex::Regex][]): boolean` | private |

## `crates/agentwerk/src/tools/command/mod.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | re-exports `CommandTool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod parse`, `mod tool` | private |

## `crates/agentwerk/src/tools/command/parse.rs`

Not bound: it is how `CommandTool` reads one command line.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Refusal` | crate |
| Rust | `.OperatorFound(string)` | crate |
| Rust | `.Unterminated` | crate |
| Rust | `.ControlCharacterFound` | crate |
| Rust | `.Empty` | crate |
| Rust | `Command { program: string, arguments: string[] }` | crate |
| Rust | `.split(line: string): this throws Refusal` | crate |
| Rust | `.flags(): [string, Argument][]` | super |
| Rust | `.program_path(dir: string): string` | crate |
| Rust | `.normalized(): string` | crate |
| Rust | `Argument` | super |
| Rust | `.Escape` | super |
| Rust | `.Long(string)` | super |
| Rust | `.Short(string)` | super |
| Rust | `.Operand` | super |
| Rust | `.parse(argument: string): this` | super |
| Rust | `is_number(text: string): boolean` | private |
| Rust | `operator(c: string): boolean` | private |
| Rust | `Words { rest: string, word: string, quoted: boolean, words: string[] }` | private |
| Rust | `.new(line: string): this` | private |
| Rust | `.run(): string[] throws Refusal` | private |
| Rust | `.quoted(quote: string): void throws Refusal` | private |
| Rust | `.escaped(): void throws Refusal` | private |
| Rust | `.end_word(): void` | private |
| Rust | `is_control(c: string): boolean` | private |

## `crates/agentwerk/src/tools/command/tool.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `CommandTool { tool_name: string, allow: string[], allow_flags: string[], deny: string[], deny_flags: DeniedFlag[], description: string, custom_description: boolean, concurrent: boolean }` | pub |
| Python | `CommandTool`: a class carrying the builder methods, where every other built-in tool is a function returning a handle | |
| Rust | `.DEFAULT_TIMEOUT: number = 120000` | pub |
| Rust | `.MAX_TIMEOUT: number = 600000` | pub |
| Rust | `.new(name: string): this` | pub |
| Python | `CommandTool(name)` | |
| both | `.allow(pattern: string): this` | pub |
| both | `.allow_flag(flag: string): this` | pub |
| both | `.deny(pattern: string): this` | pub |
| both | `.deny_flag(flag: string): this` | pub |
| Rust | `.description(description: Text): this` | pub |
| Python | `.description(description)`: a `str` is the description, an `os.PathLike` names the file holding it | |
| both | `.concurrent(concurrent: boolean): this` | pub |
| Rust | `impl From<CommandTool> for Tool` | pub |
| Python | `CommandTool` converts when `Agent.tool(..)` receives it | |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFINITION: string` | private |
| Rust | `SCHEMA: string` | private |
| Rust | `CommandTool.render_description(): void` | private |
| Rust | `.allowed_line(): string` | private |
| Rust | `.check(line: string): Command throws string` | private |
| Rust | `.unreadable(line: string, refusal: Refusal): string` | private |
| Rust | `.allows_flag(found: Argument): boolean` | private |
| Rust | `.denies_flag(found: Argument): boolean` | private |
| Rust | `DeniedFlag { written: string, key: FlagKey }` | private |
| Rust | `.new(written: string): this` | private |
| Rust | `FlagKey` | private |
| Rust | `.Long(string)` | private |
| Rust | `.Letter(string)` | private |
| Rust | `.Cluster(string)` | private |
| Rust | `flag_rule(method: string, flag: string): string` | private |
| Rust | `is_assignment(token: string): boolean` | private |
| Rust | `quoted(patterns: string[]): string` | private |
| Rust | `CommandArgs { command: string, timeout_ms: number? }` | crate |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `CommandTool.run(args: CommandArgs, ctx: ToolContext): Promise<Event>` | private |

## `crates/agentwerk/src/tools/edit_file.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `EditFileTool` | pub |
| Python | `EditFileTool()`: the unit struct converts to a `Tool`; Python spells the conversion as a call | |
| Rust | `impl From<EditFileTool> for Tool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `EditFileArgs { path: string, old_string: string, new_string: string, replace_all: boolean }` | crate |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `run(args: EditFileArgs, ctx: ToolContext): Promise<Event>` | private |

## `crates/agentwerk/src/tools/event.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `EventTool` | pub |
| Rust | `impl From<EventTool> for Tool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFINITION: string` | private |
| Rust | `SCHEMA: string` | private |
| Rust | `FINISH_SCHEMA: string` | private |
| Rust | `EventTool.NAME: string = "event"` | crate |
| Rust | `.from_schema(schema: Schema?, handover: Task?): Tool` | crate |
| Rust | `task_finished_schema(schema: Schema?, handover: Task?): json` | super |
| Rust | `dispatch(input: json, ctx: ToolContext, schema: Schema?, handover: Task?, tool_name: string): Event throws Event` | super |
| Rust | `finish(queue: Queue, input: json, ctx: ToolContext, schema: Schema?, handover: Task?, tool_name: string): Event throws Event` | private |
| Rust | `hand_over(queue: Queue, parent_id: string, agent: string, result: json, schema: Schema?, tool_name: string, child: Task, directives: DirectiveStore): Event throws Event` | private |
| Rust | `resolve_handover(input: json, configured: Task?, directives: DirectiveStore): Task? throws Event` | private |
| Rust | `required_label(fields: Record<string, json>): string throws Event` | private |
| Rust | `invalid_handover_schema(error: string, directives: DirectiveStore): Event` | private |
| Rust | `mark_finished(queue: Queue, id: string, agent: string, directives: DirectiveStore): void throws Event` | private |
| Rust | `apply_handover_templates(task: json, parent_id: string, result_path: string, result: string): void` | private |
| Rust | `substitute_handover_text(text: string, parent_id: string, result_path: string, result: string): string` | private |
| Rust | `attach_result(queue: Queue, id: string, result: json, schema: Schema?, tool_name: string, directives: DirectiveStore): [json, string[]] throws Event` | private |

## `crates/agentwerk/src/tools/fetch_url.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `FetchTool { impersonate: boolean }` | pub |
| Python | `FetchTool`: a class carrying the builder method, where every other built-in tool except `CommandTool` is a function returning a handle | |
| Rust | `.new(): this` | pub |
| Python | `FetchTool()` | |
| both | `.impersonate(): this` | pub |
| Rust | `impl From<FetchTool> for Tool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `MAX_URL_LENGTH: number = 2000` | private |
| Rust | `MAX_RESPONSE_BYTES: number = 10485760` | private |
| Rust | `DEFAULT_MAX_LENGTH: number = 100000` | private |
| Rust | `FETCH_TIMEOUT_SECS: number = 60` | private |
| Rust | `MAX_REDIRECT_HOPS: number = 10` | private |
| Rust | `DEFAULT_USER_AGENT: string` | private |
| Rust | `BROWSER_USER_AGENT: string` | private |
| Rust | `BROWSER_CLIENT_HINT: string` | private |
| Rust | `BROWSER_ACCEPT: string` | private |
| Rust | `BROWSER_STREAM_WINDOW: number = 6291456` | private |
| Rust | `BROWSER_CONNECTION_WINDOW: number = 15728640` | private |
| Rust | `BROWSER_MAX_FRAME_SIZE: number = 16384` | private |
| Rust | `FetchUrlArgs { url: string, max_length: number }` | crate |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `default_max_length(): number` | private |
| Rust | `run(args: FetchUrlArgs, ctx: ToolContext, impersonate: boolean): Promise<Event>` | private |
| Rust | `FetchedContent` | private |
| Rust | `.Page { body: string, status: number, content_type: string, bytes: number }` | private |
| Rust | `.Redirect { original_url: string, redirect_url: string, status: number }` | private |
| Rust | `fetch_url(url: string, impersonate: boolean): Promise<FetchedContent throws string>` | private |
| Rust | `format_output(url: string, body: string, status: number, content_type: string, bytes: number, max_length: number): string` | private |
| Rust | `FollowResult` | private |
| Rust | `.Ok(reqwest::Response)` | private |
| Rust | `.CrossDomain { original_url: string, redirect_url: string, status: number }` | private |
| Rust | `request_headers(impersonate: boolean, first_hop: boolean): [string, string][]` | private |
| Rust | `follow_safe_redirects(client: reqwest::Client, url: string, impersonate: boolean): Promise<FollowResult throws string>` | private |
| Rust | `is_redirect(status: number): boolean` | private |
| Rust | `is_same_origin(original_url: string, redirect_url: string): boolean` | private |
| Rust | `UrlOrigin { scheme: string, host: string, port: string }` | private |
| Rust | `.bare_host(): string` | private |
| Rust | `parse_origin(url: string): UrlOrigin?` | private |
| Rust | `resolve_redirect_location(base_url: string, location: string): string` | private |
| Rust | `validate_url(url: string): string throws string` | private |
| Rust | `strip_html(html: string): string` | private |
| Rust | `decode_html_entity(chars: string): string` | private |
| Rust | `resolve_named_entity(name: string): string` | private |
| Rust | `decode_numeric_entity(digits: string, radix: number, original: string): string` | private |
| Rust | `collapse_whitespace(text: string): string` | private |

## `crates/agentwerk/src/tools/glob.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `GlobTool` | pub |
| Rust | `impl From<GlobTool> for Tool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `MAX_RESULTS: number = 200` | private |
| Rust | `GlobArgs { pattern: string, path: string }` | crate |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `here(): string` | private |
| Rust | `run(args: GlobArgs, ctx: ToolContext): Promise<Event>` | private |
| Rust | `collect_matches(current: string, base: string, pattern_segments: string[], results: [string, SystemTime][]): void` | private |
| Rust | `glob_matches(pattern: string[], path: string[]): boolean` | private |
| Rust | `glob_match_recursive(pattern: string[], path: string[]): boolean` | private |
| Rust | `segment_matches(pattern: string, text: string): boolean` | private |
| Rust | `seg_match_recursive(pat: number[], txt: number[]): boolean` | private |

## `crates/agentwerk/src/tools/grep.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `GrepTool` | pub |
| Rust | `impl From<GrepTool> for Tool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_HEAD_LIMIT: number = 250` | private |
| Rust | `MAX_LINE_COLUMNS: number = 250` | super |
| Rust | `SEARCH_TIMEOUT: number = 180000` | private |
| Rust | `run(args: GrepArgs, ctx: ToolContext): Promise<Event>` | private |
| Rust | `OutputMode` | crate |
| Python | not bound: the model sends `output_mode` as a string | |
| Rust | `.Content` | crate |
| Rust | `.FilesWithMatches` | crate |
| Rust | `.Count` | crate |
| Rust | `Syntax` | crate |
| Python | not bound: the model sends `syntax` as a string | |
| Rust | `.Regex` | crate |
| Rust | `.Code` | crate |
| Rust | `OutputMode.name(): string` | private |
| Rust | `Query { pattern: string, path: string?, glob: string?, output_mode: OutputMode, before_context: number, after_context: number, line_numbers: boolean, case_insensitive: boolean, file_type: string?, head_limit: number, offset: number, multiline: boolean, syntax: Syntax, constraints: json }` | super |
| Rust | `.from_args(args: GrepArgs): this` | private |
| Rust | `GrepArgs { pattern: string, path: string?, glob: string?, output_mode: OutputMode, context_before: number?, context_after: number?, context_both: number?, context: number?, line_numbers: boolean, case_insensitive: boolean, file_type: string?, head_limit: number, offset: number, multiline: boolean, syntax: Syntax, constraints: json }` | crate |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `yes(): boolean` | private |
| Rust | `default_head_limit(): number` | private |
| Rust | `search_corpus(dir: string, query: Query, interrupt: boolean, deadline: Instant): Event` | private |
| Rust | `run_regex(files: [string, string][], query: Query, interrupt: boolean, deadline: Instant): Event` | private |
| Rust | `collect_files(walk: ignore::Walk, dir: string, interrupt: boolean, deadline: Instant): [string, string][]` | private |
| Rust | `paginate(rows: T[], query: Query): [T[], boolean]` | private |
| Rust | `note_pagination(map: Record<string, json>, query: Query, truncated: boolean): void` | private |
| Rust | `object_result(map: Record<string, json>): Event` | private |
| Rust | `render_content(text: string, query: Query): Event` | super |
| Rust | `render_files(hits: string[], query: Query): Event` | super |
| Rust | `render_count(rows: [string, number][], query: Query): Event` | super |

## `crates/agentwerk/src/tools/knowledge.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `KnowledgeTool { store: Knowledge }` | pub |
| Rust | `.new(store: Knowledge): this` | pub |
| Python | `KnowledgeTool(store)` | |
| Rust | `impl From<KnowledgeTool> for Tool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `failure_reason(error: KnowledgeError): string` | private |
| Rust | `usage_line(message: string, store: Knowledge): string` | private |
| Rust | `KnowledgeArgs` | crate |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `.Write { slug: string, description: string, content: string }` | crate |
| Rust | `.Read { slug: string }` | crate |
| Rust | `.Remove { slug: string }` | crate |
| Rust | `.List` | crate |
| Rust | `run(store: Knowledge, args: KnowledgeArgs, ctx: ToolContext): Event` | private |

## `crates/agentwerk/src/tools/list_directory.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `ListDirectoryTool` | pub |
| Rust | `impl From<ListDirectoryTool> for Tool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ListDirectoryArgs { path: string, recursive: boolean }` | crate |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `here(): string` | private |
| Rust | `run(args: ListDirectoryArgs, ctx: ToolContext): Promise<Event>` | private |
| Rust | `EntryInfo { display_name: string, kind: string, size: number? }` | private |
| Rust | `list_entries(dir: string, base: string, recursive: boolean): EntryInfo[] throws io::Error` | private |

## `crates/agentwerk/src/tools/mod.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | re-exports `Tool`, `ToolContext`, `CommandTool`, `EditFileTool`, `EventTool`, `FetchTool`, `GlobTool`, `GrepTool`, `KnowledgeTool`, `ListDirectoryTool`, `ReadFileTool`, `FinishTool`, `TaskTool`, `WriteFileTool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod util` | crate |
| Rust | `mod tool`, `mod code`, `mod command`, `mod edit_file`, `mod event`, `mod fetch_url`, `mod glob`, `mod grep`, `mod knowledge`, `mod list_directory`, `mod read_file`, `mod tasks`, `mod write_file` | private |

## `crates/agentwerk/src/tools/read_file.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `ReadFileTool` | pub |
| Rust | `impl From<ReadFileTool> for Tool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ReadFileArgs { path: string, offset: number, limit: number?, column: number?, length: number? }` | crate |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `first_line(): number` | private |
| Rust | `run(args: ReadFileArgs, ctx: ToolContext): Promise<Event>` | private |
| Rust | `snap_to_char_boundary(s: string, pos: number): number` | private |

## `crates/agentwerk/src/tools/tasks/finish.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `FinishTool` | pub |
| Rust | `impl From<FinishTool> for Tool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFINITION: string` | private |
| Rust | `FinishTool.NAME: string = "finish"` | crate |
| Rust | `.from_schema(schema: Schema?, handover: Task?): Tool` | crate |
| Rust | `arguments_schema(schema: Schema?, envelope: json): json` | private |
| Rust | `normalize_input(input: json, envelope: json): json` | private |

## `crates/agentwerk/src/tools/tasks/mod.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | re-exports `FinishTool`, `TaskTool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod finish`, `mod tasks` | private |
| Rust | `TasksArgs` | crate |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `.Task { id: string? }` | crate |
| Rust | `.Result { id: string? }` | crate |
| Rust | `.List { aql: string? }` | crate |
| Rust | `.Create { task: json, label: string? }` | crate |
| Rust | `.Edit { id: string?, task: json?, label: string? }` | crate |
| Rust | `dispatch(args: TasksArgs, ctx: ToolContext): Event` | super |
| Rust | `resolve_id(queue: Queue, id: string?, ctx: ToolContext): string throws Event` | private |
| Rust | `resolve_current_id(queue: Queue, ctx: ToolContext): string throws Event` | super |
| Rust | `task_error_message(err: TaskError): string` | super |
| Rust | `render_task(t: Task): string` | private |
| Rust | `render_result(id: string, path: string, result: json): string` | private |
| Rust | `push_value(out: string, value: json): void` | private |
| Rust | `status_label(s: Status): string` | private |
| Rust | `truncate_for_preview(s: string, max: number): string` | private |
| Rust | `SummaryRow = [string, string, Status, string?]` | private |
| Rust | `render_summary_list(tasks: SummaryRow[]): string` | private |
| Rust | `task_preview(task: json): string` | private |
| Rust | `action_task(queue: Queue, id: string?, ctx: ToolContext): Event` | private |
| Rust | `action_result(queue: Queue, id: string?, ctx: ToolContext): Event` | private |
| Rust | `action_list(queue: Queue, aql: string?): Event` | private |
| Rust | `action_create(queue: Queue, task: json, label: string?, ctx: ToolContext): Event` | private |
| Rust | `action_edit(queue: Queue, id: string?, new_task: json?, new_label: string?, ctx: ToolContext): Event` | private |

## `crates/agentwerk/src/tools/tasks/tasks.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `TaskTool` | pub |
| Rust | `impl From<TaskTool> for Tool` | pub |

## `crates/agentwerk/src/tools/tool.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ToolContext { dir: string, run: Run?, queue: Queue?, agent_id: string?, task_id: string?, knowledge: Knowledge? }` | pub with crate-private fields |
| Python | not bound: a `@tool` function receives its input as keyword arguments only | |
| Rust | `.new(dir: string): this` | pub |
| Rust | `.get_dir(): string` | pub |
| Rust | `.cancelled(): Promise<void>` | pub |
| Rust | `impl Debug for ToolContext` | pub |
| both | terminal `Event`: `tool_call_finished` carries `data.output` plus optional `data.output_path` and `data.repairs`; `tool_call_failed` carries `data.message` and `data.kind` | pub |
| Python | custom tools are folded into the `@tool` decorator; Python does not expose incremental `Tool` configuration | |
| Rust | `Tool { name: string, description: string?, schema: Schema, concurrent: boolean, paths: string[], handler: ToolHandler? }` | pub with private fields |
| Python | `Tool`: an opaque handle the built-in tool functions return. An ad-hoc tool is a decorated function, not a `Tool` | |
| Rust | `impl Debug for Tool` | pub |
| Rust | `.new(name: string): Tool` | pub |
| Python | the `@tool` decorator: a decorated function carries the name, description, and schema | |
| Rust | `.call(input: json, ctx: ToolContext): Promise<Event>` | pub |
| Python | not bound: the loop calls the decorated function | |
| Rust | `.get_name(): string` | pub |
| Rust | `.get_description(): string` | pub |
| Rust | `.get_input_schema(): Schema` | pub |
| Rust | `.is_concurrent(): boolean` | pub |
| Rust | `.opened_paths(input: json): string[]` | pub |
| Rust | `.schema(schema: json): this` | pub |
| Python | `@tool(schema=..)`: raises `ValueError` when `.tool(fn)` registers it, one call later than the Rust panic | |
| Rust | `.concurrent(concurrent: boolean): this` | pub |
| Python | `@tool(concurrent=..)` | |
| Rust | `.paths(fields: string[]): this` | pub |
| Python | `@tool(paths=[..])` | |
| Rust | `.description(description: Text): this` | pub |
| Python | `@tool(description=..)`, defaulting to the decorated function's docstring: a `str` is the description, an `os.PathLike` names the file holding it | |
| Rust | `.handler(handler: (input: json, ctx: ToolContext) => Promise<Event>): this` | pub |
| Python | the decorated function itself | |
| Rust | registration panics when description or handler is missing | internal validation of public configuration |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `MAX_CONCURRENT_CALLS: number = 10` | private |
| Rust | `PER_TOOL_CAP: number = 50000` | private |
| Rust | `PER_TURN_CAP: number = 200000` | private |
| Rust | `PREVIEW_CHARS: number = 2000` | private |
| Rust | `ToolContext.run(run: Run): this` | crate |
| Rust | `.queue(queue: Queue): this` | crate |
| Rust | `.agent_id(name: string): this` | crate |
| Rust | `.task_id(id: string): this` | crate |
| Rust | `.knowledge(knowledge: Knowledge): this` | crate |
| Rust | `.emit_event(event: Event): void` | crate |
| Rust | `ToolCall { id: string, name: string, input: json }` | crate |
| Python | not bound: a call reaches Python as the decorated function's arguments | |
| Rust | `ToolRegistry { tools: Tool[] }` | crate |
| Rust | `impl Debug for ToolRegistry` | crate |
| Rust | `.register(tool: Tool): void` | crate |
| Rust | `.completion(schema: Schema?, handover: Task?): this` | crate |
| Rust | `.resolve(name: string): Tool throws string` | private |
| Rust | `.get(name: string): Tool?` | crate |
| Rust | `.contains(name: string): boolean` | crate |
| Rust | `.names(): string[]` | private |
| Rust | `.tools(): Tool[]` | crate |
| Rust | `.execute(calls: ToolCall[], ctx: ToolContext): Promise<Event[]>` | crate |
| Rust | `.run_concurrently(batch: [number, ToolCall][], ctx: ToolContext, semaphore: tokio::sync::Semaphore): Promise<[number, Event][]>` | private |
| Rust | `ToolBatch` | private |
| Rust | `.Concurrent([number, ToolCall][])` | private |
| Rust | `.Serial(number, ToolCall)` | private |
| Rust | `partition_tool_calls(calls: ToolCall[], registry: ToolRegistry): ToolBatch[]` | private |
| Rust | `answer_every_call(calls: ToolCall[], answers: Event?[]): Event[]` | private |
| Rust | `lookup_key(name: string): string` | private |
| Rust | `ToolHandler = (input: json, ctx: ToolContext) => Promise<Event>` | private |
| Rust | `read_arguments_then(name: string, handler: (input: json, ctx: ToolContext) => Promise<Event>): ToolHandler` | private |
| Rust | `invoke(resolved: Tool throws string, call: ToolCall, ctx: ToolContext): Promise<Event>` | private |
| Rust | `retype_message(pointer: string): string` | crate |
| Rust | `cap_results(calls: ToolCall[], results: Event[], ctx: ToolContext): void` | private |
| Rust | `replace_empty_output(result: Event, tool_name: string): void` | private |
| Rust | `cap_oversized_result(result: Event, ctx: ToolContext, call_id: string, per_tool_cap: number): void` | private |
| Rust | `cap_aggregate_outputs(calls: ToolCall[], results: Event[], ctx: ToolContext, per_turn_cap: number): void` | private |
| Rust | `largest_inline_success(calls: ToolCall[], results: Event[]): [ToolCall, string, string?]?` | private |
| Rust | `write_out(content: string, ctx: ToolContext, call_id: string): string?` | private |
| Rust | `persist_output(ctx: ToolContext, tool_use_id: string, content: string): PersistedOutput?` | private |
| Rust | `PersistedOutput { rel: string, display: string }` | private |
| Rust | `OVERSIZED_STUB_TAG_OPEN: string = "<persisted-output>"` | private |
| Rust | `OVERSIZED_STUB_TAG_CLOSE: string = "</persisted-output>"` | private |
| Rust | `format_oversized_tool_result(original_len: number, path: string, preview: string): string` | private |
| Rust | `truncate_preview(content: string): string` | private |
| Rust | `format_bytes(bytes: number): string` | private |
| Rust | `utf8_boundary_floor(content: string, index: number): number` | private |

## `crates/agentwerk/src/tools/util.rs`

Not bound: shared helpers behind the built-in tools.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run_command(command: Command, timeout: number, ctx: ToolContext): Promise<Event>` | crate |
| Rust | `glob_match(pattern: string, text: string): boolean` | crate |
| Rust | `glob_match_bytes(pattern: number[], text: number[]): boolean` | private |
| Rust | `MAX_DIR_ENTRIES: number = 100` | crate |
| Rust | `directory_entries(dir: string): string?` | crate |
| Rust | `nearest_existing_dir(path: string): string?` | private |
| Rust | `not_found_hint(ctx_dir: string, resolved: string): string` | crate |
| Rust | `suggest_path(ctx_dir: string, resolved: string): string?` | private |

## `crates/agentwerk/src/tools/write_file.rs`

### Public

| Language | Item | Visibility |
|----------|------|------------|
| both | `WriteFileTool` | pub |
| Rust | `impl From<WriteFileTool> for Tool` | pub |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `WriteFileArgs { path: string, content: string }` | crate |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `run(args: WriteFileArgs, ctx: ToolContext): Promise<Event>` | private |

## `crates/agentwerk-py/src/agent.rs`

Binds `agents/agent.rs`, whose section holds the Python spelling of each method.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyAgent { agent: Agent?, has_provider: boolean, has_model: boolean }` | python |
| Rust | `.new(): this` | python |
| Rust | `.from_env(): this throws PyErr` | python |
| Rust | `.provider(provider: PyProvider): this` | python |
| Rust | `.model(model: any): this throws PyErr` | python |
| Rust | `.role(role: any): this throws PyErr` | python |
| Rust | `.label(label: string): this` | python |
| Rust | `.get_id(): string` | python |
| Rust | `.interactive(): this` | python |
| Rust | `.template(key: string, value: string): this` | python |
| Rust | `.templates(variables: Record<string, string>): this` | python |
| Rust | `.handover(task: PyTask): this throws PyErr` | python |
| Rust | `.dir(dir: string): this` | python |
| Rust | `.knowledge(store: PyKnowledge): this` | python |
| Rust | `.directives(compute: any): this` | python |
| Rust | `.tool(tool: any): this throws PyErr` | python |
| Rust | `.tools(tools: any): this throws PyErr` | python |
| Rust | `.task(task: PyTask): string throws PyErr` | python |
| Rust | `.start(): PyQueue throws PyErr` | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyAgent.get(): Agent` | private |
| Rust | `.set(edit: (agent: Agent) => Agent): void` | private |
| Rust | `.ready(): Agent throws PyErr` | crate |

## `crates/agentwerk-py/src/policy.rs`

Binds `agents/policy.rs`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyPolicy { inner: Policy }` | python |
| Rust | `.new(max_turns: number?, max_input_tokens: number?, max_output_tokens: number?, max_request_tokens: number?, max_schema_retries: number?, max_request_retries: number?, request_retry_delay: number?, max_time: number?, compaction_threshold: number?): this` | python |
| Rust | `.max_turns(): number?` | python |
| Rust | `.max_input_tokens(): number?` | python |
| Rust | `.max_output_tokens(): number?` | python |
| Rust | `.max_request_tokens(): number?` | python |
| Rust | `.max_schema_retries(): number?` | python |
| Rust | `.max_request_retries(): number` | python |
| Rust | `.request_retry_delay(): number` | python |
| Rust | `.max_time(): number?` | python |
| Rust | `.compaction_threshold(): number?` | python |
| Python | every reader is an attribute, not a call | |

## `crates/agentwerk-py/src/convert.rs`

Not bound: the one JSON boundary between the two languages.

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `value_to_py(py: Python, value: json): any throws PyErr` | crate |
| Rust | `py_to_value(obj: any): json throws PyErr` | crate |
| Rust | `py_to_text(obj: any): string throws PyErr` | crate |
| Rust | `runtime_error(message: string): PyErr` | crate |

## `crates/agentwerk-py/src/directives.rs`

Binds `prompts/directives.rs`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyDirective { }`, exposed as `Directive`, the key namespace | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `compute(compute: any): (key: string) => string?`, which prints a raising call's traceback and keeps the catalogue text | crate |
| Rust | `register(module: PyModule): void throws PyErr`, which also sets each key as an uppercase attribute on `Directive` | crate |

## `crates/agentwerk-py/src/event.rs`

Binds `event.rs`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyEvent { inner: Event }` | python |
| Rust | built-in name class attributes matching `Event` | python |
| Rust | `.new(name: string): this` | python |
| Rust | `.data(value: any): this throws PyErr` | python |
| Rust | `.task_id(task_id: string): this` | python |
| Rust | `.agent_id(agent_id: string): this` | python |
| Rust | `.get_name(): string` | python |
| Rust | `.get_data(): any throws PyErr` | python |
| Rust | `.get_task_id(): string` | python |
| Rust | `.get_agent_id(): string` | python |
| Rust | `.get_label(): string?` | python |
| Rust | `.get_created_at(): number` | python |
| Rust | `.__repr__(): string` | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `to_py_event(event: Event): PyEvent` | crate |

## `crates/agentwerk-py/src/knowledge.rs`

Binds `agents/knowledge.rs`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyKnowledge { inner: Knowledge }` | python |
| Rust | `.load(store_dir: string): this throws PyErr` | python |
| Rust | `.set_char_limit(count: number): this` | python |
| Rust | `.get_index_char_limit(): number` | python |
| Rust | `.get_index(): string` | python |
| Rust | `.get_pages(): PyPages` | python |
| Rust | `.clear(): void throws PyErr` | python |
| Rust | `PyPages { store: Knowledge }` | python |
| Rust | `.save(page: PyPage): void throws PyErr` | python |
| Rust | `.get_page(slug: string): PyPage throws PyErr` | python |
| Rust | `.get_pages(): PyPage[] throws PyErr` | python |
| Rust | `.remove(slug: string): void throws PyErr` | python |
| Rust | `PyPage { inner: Page }` | python |
| Rust | `.new(slug: string, description: string, content: string, kind: string, tags: string[]?): this` | python |
| Rust | `.get_slug(): string` | python |
| Rust | `.get_kind(): string` | python |
| Rust | `.get_description(): string` | python |
| Rust | `.get_content(): string` | python |
| Rust | `.get_tags(): string[]` | python |
| Rust | `.__repr__(): string` | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyPages.collection(): Pages` | private |
| Rust | `PyPage.to_page(): Page` | private |

## `crates/agentwerk-py/src/lib.rs`

Registers every bound class and function in the `_agentwerk` module.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `_agentwerk(m: PyModule): void throws PyErr` | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod agent`, `mod policy`, `mod convert`, `mod directives`, `mod event`, `mod knowledge`, `mod providers`, `mod query`, `mod reply`, `mod schema`, `mod task`, `mod queue`, `mod tools`, `mod trajectory` | private |

## `crates/agentwerk-py/src/providers.rs`

Binds `providers/`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyProvider { inner: Provider }` | python |
| Rust | `.from_env(): this throws PyErr` | python |
| Rust | `PyModel { inner: Model }` | python |
| Rust | `.new(name: string): this` | python |
| Rust | `.from_env(): this throws PyErr` | python |
| Rust | `.get_name(): string` | python |
| Rust | `.context_window(size: number): this` | python |
| Rust | `.reasoning_effort(effort: string): this throws PyErr` | python |
| Rust | `.get_context_window(): number?` | python |
| Rust | `.get_reasoning_effort(): string` | python |
| Rust | `anthropic_provider(api_key: string, base_url: string?, timeout: number?): PyProvider` | python |
| Rust | `openai_provider(api_key: string, base_url: string?, timeout: number?): PyProvider` | python |
| Rust | `mistral_provider(api_key: string, base_url: string?, timeout: number?): PyProvider` | python |
| Rust | `litellm_provider(api_key: string, base_url: string?, timeout: number?): PyProvider` | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `register(m: PyModule): void throws PyErr` | crate |

## `crates/agentwerk-py/src/query.rs`

Binds `agents/query.rs`. One class covers both field sets: Python carries no type parameter, so the string is compiled over the task fields and the event fields at once.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyQuery { source: string, tasks: Query<Task> throws QueryError, events: Query<Event> throws QueryError }` | python |
| Rust | `.new(query: string): this throws PyErr`, raising only where both field sets reject the string | python |
| Rust | `.__repr__(): string` | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `rejected(over_tasks: QueryError, over_events: QueryError): PyErr` | private |
| Rust | `value_error(message: string): PyErr` | private |
| Rust | `to_task_matcher(arg: any): Query<Task> throws PyErr` | crate |
| Rust | `to_event_matcher(arg: any): Query<Event> throws PyErr` | crate |
| Rust | `task_predicate(predicate: any, task: Task): boolean` | private |
| Rust | `event_predicate(predicate: any, event: Event): boolean` | private |

## `crates/agentwerk-py/src/reply.rs`

Binds `agents/tasks/reply.rs`, and owns the two reply converters the editors on `Queue` use.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyReply { inner: Reply }` | python |
| Rust | `.user_text(text: string): this` | python |
| Rust | `.get_author(): string` | python |
| Rust | `.get_content(): PyReplyContent[]` | python |
| Rust | `.get_created_at(): number` | python |
| Rust | `.__repr__(): string` | python |
| Rust | `PyReplyContent { inner: ReplyContent }` | python |
| Rust | `.text(text: string): this` | python |
| Rust | `.tool_use(id: string, name: string, input: any): this throws PyErr` | python |
| Rust | `.tool_result(tool_use_id: string, content: string, succeeded: boolean, path: string?): this` | python |
| Rust | `.thinking(thinking: string, signature: string): this` | python |
| Rust | `.redacted_thinking(data: string): this` | python |
| Rust | `.get_kind(): string` | python |
| Rust | `.get_data(): any throws PyErr` | python |
| Rust | `.__repr__(): string` | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `replies_to_py(replies: Reply[]): PyReply[]` | crate |
| Rust | `py_to_replies(obj: any): Reply[] throws PyErr` | crate |

## `crates/agentwerk-py/src/schema.rs`

Binds `schemas/`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PySchema { inner: Schema }` | python |
| Rust | `.new(document: any): this throws PyErr` | python |
| Rust | `.validate(value: any): [any, string[]] throws PyErr` | python |

## `crates/agentwerk-py/src/task.rs`

Binds `agents/tasks/task.rs`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyTask { inner: Task }` | python |
| Rust | `.new(task: any, label: string?, schema: PySchema?, parent: string?): this throws PyErr` | python |
| Rust | `.is_todo(): boolean` | python |
| Rust | `.is_finished(): boolean` | python |
| Rust | `.is_failed(): boolean` | python |
| Rust | `.is_in_progress(): boolean` | python |
| Rust | `.is_pending(): boolean` | python |
| Rust | `.is_cancelled(): boolean` | python |
| Rust | `.get_id(): string` | python |
| Rust | `.get_status(): string` | python |
| Rust | `.get_task(): any throws PyErr` | python |
| Rust | `.get_result(): any? throws PyErr` | python |
| Rust | `.get_label(): string?` | python |
| Rust | `.get_schema(): PySchema?` | python |
| Rust | `.get_parent(): string?` | python |
| Rust | `.get_reporter(): string` | python |
| Rust | `.get_assignee(): string?` | python |
| Rust | `.get_created_at(): number` | python |
| Rust | `.get_started_at(): number?` | python |
| Rust | `.get_finished_at(): number?` | python |
| Rust | `.get_failed_at(): number?` | python |
| Rust | `.get_replies(): PyReply[]` | python |
| Rust | `.get_errors(): PyEvent[]` | python |
| Rust | `.__repr__(): string` | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `to_task(arg: any): Task throws PyErr`, reading an `os.PathLike` as the file holding the task | crate |
| Rust | `PyTask.from_task(task: Task): this` | crate |
| Rust | `.to_task(): Task` | crate |

## `crates/agentwerk-py/src/queue.rs`

Binds `agents/tasks/queue.rs` and `store.rs`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyQueue { inner: Queue }` | python |
| Rust | `.new(): this` | python |
| Rust | `.load(tasks_dir: string): this throws PyErr` | python |
| Rust | `.add_agent(agent: PyAgent): this throws PyErr` | python |
| Rust | `.add_task(task: PyTask): string throws PyErr` | python |
| Rust | `.add_reply(id: string, content: string): this` | python |
| Rust | `.emit_event(event: PyEvent): PyEvent` | python |
| Rust | `.set_task_finished(id: string, result: any): void throws PyErr` | python |
| Rust | `.set_task_failed(id: string): void throws PyErr` | python |
| Rust | `.set_policy(policy: PyPolicy): this` | python |
| Rust | `.get_policy(): PyPolicy` | python |
| Rust | `.set_dir(dir: string): this` | python |
| Rust | `.get_dir(): string` | python |
| Rust | `.on_event(handler: any): this` | python |
| Rust | `.on_event_async(handler: any): this` | python |
| Rust | `.on_result(handler: any): this` | python |
| Rust | `.on_result_async(handler: any): this` | python |
| Rust | `.on_failure(handler: any): this` | python |
| Rust | `.on_failure_async(handler: any): this` | python |
| Rust | `.get_model_for_agent(agent_id: string): string?` | python |
| Rust | `.get_task(id: string): PyTask? throws PyErr` | python |
| Rust | `.get_tasks(): PyTask[] throws PyErr` | python |
| Rust | `.find_tasks(predicate: any): PyTask[] throws PyErr` | python |
| Rust | `.find_task(predicate: any): PyTask? throws PyErr` | python |
| Rust | `.on_task(handler: any): this` | python |
| Rust | `.on_task_async(handler: any): this` | python |
| Rust | `.edit_replies(id: string, editor: any): this throws PyErr` | python |
| Rust | `.start(): this` | python |
| Rust | `.finish_tasks(matches: any): Promise<any[]> throws PyErr` | python |
| Rust | `.finish_all_tasks(): Promise<any[]> throws PyErr` | python |
| Rust | `.finish_task(matches: any): Promise<any?> throws PyErr` | python |
| Rust | `.get_finish_reason(): string?` | python |
| Rust | `.cancel_tasks(matches: any): this throws PyErr` | python |
| Rust | `.cancel_all_tasks(): this` | python |
| Rust | `.find_events(matches: any): PyEvent[] throws PyErr` | python |
| Rust | `.find_event(matches: any): PyEvent? throws PyErr` | python |
| Rust | `.get_input_tokens(): number` | python |
| Rust | `.get_output_tokens(): number` | python |
| Rust | `.get_duration(): number?` | python |
| Rust | `.get_results(): any[] throws PyErr` | python |
| Rust | `.find_results(query: any): any[] throws PyErr` | python |
| Rust | `.find_result(query: any): any? throws PyErr` | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `as_py_queue(py: Python, queue: Queue): any throws PyErr` | private |
| Rust | `call_with_result(py: Python, callable: any, queue: Queue, task: Task, result: json): any throws PyErr` | private |
| Rust | `call_with_task(py: Python, callable: any, queue: Queue, event: Event, task: Task): any throws PyErr` | private |
| Rust | `await_coroutine(coroutine: Promise<any throws PyErr> throws PyErr): Promise<void>` | private |

## `crates/agentwerk-py/src/tools.rs`

Binds `tools/`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyTool { inner: Tool }` | python |
| Rust | `PyEvent { inner: Event }` | python |
| Rust | `.success(content: string): this` | python |
| Rust | `.error(content: string): this` | python |
| Rust | `read_file_tool(): PyTool` | python |
| Rust | `write_file_tool(): PyTool` | python |
| Rust | `edit_file_tool(): PyTool` | python |
| Rust | `grep_tool(): PyTool` | python |
| Rust | `glob_tool(): PyTool` | python |
| Rust | `list_directory_tool(): PyTool` | python |
| Rust | `knowledge_tool(store: PyKnowledge): PyTool` | python |
| Rust | `event_tool(): PyTool` | python |
| Rust | `finish_tool(): PyTool` | python |
| Rust | `task_tool(): PyTool` | python |
| Rust | `PyFetchTool { inner: FetchTool }` | python |
| Rust | `.new(): this` | python |
| Rust | `.impersonate(): this` | python |
| Rust | `PyCommandTool { inner: CommandTool }` | python |
| Rust | `.new(name: string): this` | python |
| Rust | `.allow(pattern: string): this` | python |
| Rust | `.allow_flag(flag: string): this` | python |
| Rust | `.deny(pattern: string): this` | python |
| Rust | `.deny_flag(flag: string): this` | python |
| Rust | `.description(description: any): this throws PyErr` | python |
| Rust | `.concurrent(concurrent: boolean): this` | python |

### Internal

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `invoke_python(py: Python, func: any, input: json): Event throws PyErr` | private |
| Rust | `extract_tool(obj: any): Tool throws PyErr` | crate |
| Rust | `handle(inner: Tool): PyTool` | private |
| Rust | `register(m: PyModule): void throws PyErr` | crate |

## `crates/agentwerk-py/src/trajectory.rs`

Binds `agents/tasks/trajectory.rs`.

### Public

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyTrajectory { inner: Trajectory }` | python |
| Rust | `.from_task(agent_id: string, model: string?, task: PyTask): this` | python |
| Rust | `.save(dir: string): void throws PyErr` | python |
| Rust | `.get_id(): string` | python |
| Rust | `.get_model(): string?` | python |
| Rust | `.get_replies(): PyReply[]` | python |
| Rust | `.__repr__(): string` | python |
