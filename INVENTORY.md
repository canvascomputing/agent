# Inventory

Every declaration in `crates/agentwerk/src` and `crates/agentwerk-py/src`, one table per source file. `python/agentwerk/__init__.py` adds only `EventName` and `@tool`, which sit with the Rust items they wrap.

> A commit that adds, renames, removes, or re-types an item changes this file in the same commit.

## Notation

Signatures use one language-independent notation, so a Rust row and a Python row read alike.

- Receivers, borrows, and lifetimes drop: `fn record(&self, event: &Event)` is `Stats.record(event: Event): void`.
- A method carries its type: `Owner.name(..)`. A free function carries none.
- `u8`, `u32`, `u64`, `usize`, `f32`, `f64`, and `Duration` are `number`, and a `Duration` constant shows milliseconds.
- `&str`, `String`, `impl Into<String>`, `&Path`, and `PathBuf` are `string`.
- `bool` is `boolean`, `()` is `void`, `serde_json::Value` is `json`.
- `Vec<T>` and `&[T]` are `T[]`, `HashMap<K, V>` and `BTreeMap<K, V>` are `Record<K, V>`.
- `Option<T>` is `T?`, written that way rather than as a union because a `|` splits a table cell.
- `Result<T, E>` is `T throws E`, `io::Result<T>` is `T throws io::Error`.
- An `async fn` returns `Promise<T>`.
- `Arc<T>`, `Box<T>`, `Rc<T>`, `Mutex<T>`, `RwLock<T>`, and `&T` unwrap to `T`. Every other wrapper keeps its name: `Weak<TicketQueue>`, `Sender<Event>`, `JoinHandle<void>`.
- A callback becomes an arrow: `Arc<dyn Fn(&Event) + Send + Sync>` is `(event: Event) => void`.
- A generic bound erases to what the caller passes: `T: Serialize` is `json`. A type's own parameters stay: `AgentBuilder<P, M>`, `ProviderResult<T>`.
- Domain type names stay as the code writes them: `Ticket`, `Event`, `Status`, `PolicyKind`.
- A constant shows `= value` when the value is one scalar or short string, and nothing when it is longer or computed.
- Names are never re-cased: `is_todo`, never `isTodo`. Every name here is greppable in `src/`.

## Rows

- `Language` is `both` when Rust and Python read the same after normalization, and a `Python` row may still follow one to note a difference in behavior. Otherwise a `Rust` row is followed by its `Python` row.
- Every item the crate exports is either half of a `both` row or has a `Python` row. `not bound` is the Python cell for an item the bindings leave out, with the reason where there is one.
- A file whose exports the bindings leave out says so in one line under its heading, and its rows then carry no `Python` row. A `crates/agentwerk-py` file names the library file it binds instead. A file that exports nothing carries no note.
- An enum variant, an associated constant, a trait impl, a module declaration, or a re-export takes a `Python` row only where Python does something the row above does not already imply.
- A `Python` row leaves `Visibility` empty. A trailing `: note` states a behavioral difference.
- `Visibility` is the reach: `pub`, `crate`, `super`, or `private`, so a `pub(in path)` and a `pub` member of a `crate` type both record as `crate`. In `crates/agentwerk-py` it is `python` for anything a `#[pyclass]`, `#[pymethods]`, or `#[pyfunction]` exposes, whether or not Rust keeps it private.
- Struct fields sit inside the struct's row, and a trait's members inside the trait's. An enum gets its own row plus one row per variant, carrying the variant's payload where it has one.
- A hand-written trait impl is one row, written `impl Trait for Type` with the type as Rust spells it: `impl ProviderLike for Arc<T>`. Derived impls are not listed.
- `#[cfg(test)]` items, `test_util.rs`, and `codegrep/*_tests.rs` are not listed.

## Python conversions

The rules the tables never repeat.

- Every enum is its lowercase string: `ticket.status == "in_progress"`.
- Every error type is `RuntimeError`.
- Every `Duration` is a float named `seconds`. Every other parameter keeps its Rust name.
- Every shared handle is a plain object, shared by passing it to several agents.
- Every `async` item is awaitable.
- An argument Rust takes by `Serialize` takes any JSON-serializable value.
- A `json` value is a dict, list, or scalar.
- A reader with no arguments is an attribute: `ticket.key`, `agent.id`.

## `crates/agentwerk/src/agents/agent.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `AGENT_IDS: Record<string, number>` | private |
| Rust | `next_id(label: string?): string` | private |
| Rust | `AgentBuilder<P, M> { provider: P, model: M, role: string, label: string?, interactive: boolean, templates: [string, string][], tools: ToolRegistry, dir: string, knowledge: Knowledge }` | pub |
| Python | folded into `Agent`: the type changes as the provider and model slots fill, which Python cannot hold across calls | |
| Rust | `AgentBuilder.new(): AgentBuilder` | pub |
| Python | `Agent()` | |
| Rust | `AgentBuilder.provider(provider: Provider): AgentBuilder` | pub |
| Python | `Agent.provider(provider): Agent` | |
| Rust | `AgentBuilder.model(model: Model): AgentBuilder` | pub |
| Python | `Agent.model(model): Agent` | |
| Rust | `AgentBuilder.role(role: string): AgentBuilder` | pub |
| Python | `Agent.role(role): Agent` | |
| Rust | `AgentBuilder.label(label: string): AgentBuilder` | pub |
| Python | `Agent.label(label): Agent` | |
| Rust | `AgentBuilder.interactive(): AgentBuilder` | pub |
| Python | `Agent.interactive(): Agent` | |
| Rust | `AgentBuilder.template(key: string, value: string): AgentBuilder` | pub |
| Python | `Agent.template(key, value): Agent` | |
| Rust | `AgentBuilder.templates(variables: [string, string][]): AgentBuilder` | pub |
| Python | `Agent.templates(variables): Agent`: a mapping, so the bulk bind applies in key order where Rust preserves insertion order | |
| Rust | `AgentBuilder.tool(tool: Tool): AgentBuilder` | pub |
| Python | `Agent.tool(tool): Agent` | |
| Rust | `AgentBuilder.tools(tools: Tool[]): AgentBuilder` | pub |
| Python | `Agent.tools(tools): Agent` | |
| Rust | `AgentBuilder.dir(dir: string): AgentBuilder` | pub |
| Python | `Agent.dir(dir): Agent` | |
| Rust | `AgentBuilder.knowledge(store: Knowledge): AgentBuilder` | pub |
| Python | `Agent.knowledge(store): Agent` | |
| Rust | `AgentBuilder.build(): Agent` | pub |
| Python | `Agent.build(): Agent`: returns the same object, armed. Configuring after it, or building twice, raises | |
| Rust | `TicketQueueRef` | crate |
| Rust | `TicketQueueRef.Shared(TicketQueue)` | crate |
| Rust | `TicketQueueRef.Private(TicketQueue)` | crate |
| Rust | `TicketQueueRef.upgrade(): TicketQueue?` | crate |
| Rust | `Agent { id: string, model: Model, label: string?, interactive: boolean, ticket_queue: TicketQueueRef, provider: Provider, role: string, templates: [string, string][], tools: ToolRegistry, dir: string, knowledge: Knowledge }` | pub |
| Python | `Agent`: also carries every `AgentBuilder` method | |
| Rust | `impl Clone for Agent` | pub |
| Rust | `Agent.new(): AgentBuilder` | pub |
| Python | `Agent()` | |
| both | `Agent.from_env(): Agent` | pub |
| Python | `Agent.from_env()`: raises `RuntimeError` where Rust panics | |
| Rust | `Agent.id(): string` | pub |
| Python | `Agent.id`: a property, and a `RuntimeError` before `build()` | |
| Rust | `Agent.is_interactive(): boolean` | super |
| Rust | `Agent.handles(ticket_label: string?): boolean` | super |
| Rust | `Agent.tool_registry(): ToolRegistry` | super |
| Rust | `Agent.provider(): Provider` | super |
| Rust | `Agent.knowledge(): Knowledge` | super |
| Rust | `Agent.dir(): string` | super |
| Rust | `Agent.system_prompt(knowledge: string?, policies: Policies, stats: Stats, ticket_key: string): string` | super |
| Rust | `Agent.expand_context(role: string, policies: Policies, stats: Stats, ticket_key: string): string` | private |
| Rust | `Agent.interpolate(s: string): string` | private |
| both | `Agent.task(task: json): string` | pub |
| both | `Agent.ticket(ticket: Ticket): string` | pub |
| both | `Agent.start(): TicketQueue` | pub |
| Rust | `Agent.dispatch(ticket: Ticket): string` | private |

## `crates/agentwerk/src/agents/compaction.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_COMPACT_AT: number = 0.85` | private |
| Rust | `compaction_threshold(window: number?, compact_at: number?): number?` | crate |
| Rust | `estimate_next_request_tokens(history: TokenUsage[], messages: Message[], system_prompt: string, tools: Tool[]): number` | crate |
| Rust | `next_delta(history: TokenUsage[]): number` | private |
| Rust | `message_bytes(message: Message): number` | private |
| Rust | `block_bytes(block: ContentBlock): number` | private |
| Rust | `tool_bytes(tool: Tool): number` | private |
| Rust | `should_compact_proactively(window: number?, compact_at: number?, history: TokenUsage[], messages: Message[], system_prompt: string, tools: Tool[]): boolean` | crate |
| Rust | `Compaction { reason: CompactReason, ticket: Ticket, provider: Provider, model: string, window: number?, on_progress: (completed: number, total: number) => void }` | pub |
| Python | `Compaction` | |
| Rust | `Compaction.new(reason: CompactReason, ticket: Ticket, provider: Provider, model: string, window: number?, on_progress: (completed: number, total: number) => void): Compaction` | crate |
| Rust | `Compaction.reason(): CompactReason` | pub |
| Python | `Compaction.reason(): str`: the string `"proactive"` or `"reactive"` | |
| both | `Compaction.ticket(): Ticket` | pub |
| both | `Compaction.window(): number?` | pub |
| both | `Compaction.summarize(replies: Reply[]): Promise<string throws ProviderError>` | pub |
| Rust | `default_editor(compaction: Compaction, replies: Reply[]): Promise<Reply[] throws ProviderError>` | crate |
| Rust | `chunks_for_window(messages: Message[], window: number?): Message[][]` | crate |
| Rust | `chunks_within(messages: Message[], max_tokens_per_chunk: number): Message[][]` | private |
| Rust | `split_in_half(message: Message): Message[]?` | private |
| Rust | `find_split_index(text: string, target: number): number` | private |

## `crates/agentwerk/src/agents/knowledge.rs`

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
| Rust | `KnowledgeError` | pub |
| Python | `RuntimeError` | |
| Rust | `KnowledgeError.PageRejected { message: string }` | pub |
| Rust | `KnowledgeError.PageMissing { slug: string }` | pub |
| Rust | `KnowledgeError.IoFailed { message: string, source: io::Error }` | pub |
| Rust | `impl Display for KnowledgeError` | pub |
| Rust | `impl Error for KnowledgeError` | pub |
| Rust | `io_failed(message: string): (error: io::Error) => KnowledgeError` | private |
| Rust | `Knowledge { knowledge_dir: string, index: IndexEntry[], write_lock: void, index_char_limit: number }` | pub |
| Python | `Knowledge` | |
| both | `Knowledge.load(store_dir: string): Knowledge throws io::Error` | pub |
| both | `Knowledge.index_char_limit(count: number): Knowledge` | pub |
| both | `Knowledge.get_index_char_limit(): number` | pub |
| both | `Knowledge.index(): string` | pub |
| Rust | `Knowledge.full_index(): string` | crate |
| Rust | `Knowledge.index_path(): string` | private |
| both | `Knowledge.pages(): Pages` | pub |
| both | `Knowledge.clear(): void throws KnowledgeError` | pub |
| Rust | `Knowledge.index_usage(): [number, number, number]` | crate |
| Rust | `Page { slug: string, kind: string, description: string, content: string, tags: string[] }` | pub |
| Python | `Page(slug, description, content, kind=.., tags=..)`: a struct literal becomes a constructor, so the optional fields move last | |
| Rust | `impl Persist for Page` | pub |
| Rust | `Pages { inner: Knowledge }` | pub |
| Python | `Pages` | |
| both | `Pages.save(page: Page): void throws KnowledgeError` | pub |
| both | `Pages.load(slug: string): Page throws KnowledgeError` | pub |
| both | `Pages.list(): Page[] throws KnowledgeError` | pub |
| both | `Pages.remove(slug: string): void throws KnowledgeError` | pub |
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

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `RESUME_OR_FINISH_DETAIL: string` | private |
| Rust | `TicketContext { agent: Agent, model: Model, ticket_queue: TicketQueue, run: Run, ticket_key: string, system_prompt: string, policies: Policies, tools: ToolRegistry, consecutive_schema_failures: number }` | super |
| Rust | `TicketContext.emit(kind: EventKind): Event` | super |
| Rust | `TicketContext.ticket(): Ticket?` | super |
| Rust | `TicketContext.retry_directive(detail: string, event: Event): string` | super |
| Rust | `TicketContext.fail_ticket(): void` | super |
| Rust | `TicketContext.fail_with(reason: RequestErrorKind, message: string): void` | super |
| Rust | `run_agent(agent: Agent): Promise<void>` | super |
| Rust | `run_is_over(agent: Agent, ticket_queue: TicketQueue): boolean` | private |
| Rust | `claim(agent: Agent, ticket_queue: TicketQueue): TicketContext?` | private |
| Rust | `evaluate(context: TicketContext): Step?` | private |
| Rust | `silence_retry(context: TicketContext): Step?` | private |

## `crates/agentwerk/src/agents/loop/compact.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run(context: TicketContext, reason: CompactReason): Promise<Step?>` | super |
| Rust | `proactive_compaction_needed(context: TicketContext, ticket: Ticket): boolean` | super |

## `crates/agentwerk/src/agents/loop/main.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run_main_loop(ticket_queue: TicketQueue): Promise<void>` | crate |

## `crates/agentwerk/src/agents/loop/mod.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod agent`, `mod compact`, `mod main`, `mod request`, `mod tool_call` | private |
| Rust | `POLL_INTERVAL: number = 50` | private |
| Rust | `Step` | private |
| Rust | `Step.Evaluate` | private |
| Rust | `Step.Compact(CompactReason)` | private |
| Rust | `Step.Request` | private |
| Rust | `Step.ToolCalls(ToolCall[])` | private |

## `crates/agentwerk/src/agents/loop/request.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run(context: TicketContext): Promise<Step?>` | super |

## `crates/agentwerk/src/agents/loop/tool_call.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run(context: TicketContext, calls: ToolCall[]): Promise<Step?>` | super |

## `crates/agentwerk/src/agents/mod.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod agent`, `mod compaction`, `mod knowledge`, `mod loop`, `mod tickets` | pub |
| Rust | `mod policy`, `mod retry`, `mod stats` | crate |
| Rust | re-exports `Agent`, `AgentBuilder`, `Compaction`, `Knowledge`, `Reply`, `Status`, `Ticket`, `TicketError`, `TicketQueue`, `Trajectory` | pub |

## `crates/agentwerk/src/agents/policy.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Policies { max_turns: number?, max_input_tokens: number?, max_output_tokens: number?, max_request_tokens: number?, max_schema_retries: number?, max_request_retries: number, request_retry_delay: number, max_time: number?, compact_at: number? }` | crate |
| Rust | `Policies.DEFAULT_MAX_SCHEMA_RETRIES: number = 10` | crate |
| Rust | `Policies.DEFAULT_MAX_REQUEST_RETRIES: number = 10` | crate |
| Rust | `Policies.DEFAULT_REQUEST_RETRY_DELAY: number = 500` | crate |
| Rust | `impl Default for Policies` | crate |

## `crates/agentwerk/src/agents/retry.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Retry { try_consume(): number?, max_attempts(): number, delay(server_hint: number?): number }` | crate |
| Rust | `MAX_RETRY_DELAY: number = 32000` | private |
| Rust | `ExponentialRetry { base_delay: number, max_attempts: number, attempt: number }` | crate |
| Rust | `ExponentialRetry.new(base_delay: number, max_attempts: number): ExponentialRetry` | crate |
| Rust | `impl Retry for ExponentialRetry` | crate |

## `crates/agentwerk/src/agents/stats.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Stats { event_counts: Record<EventName, number>, input_tokens: number, output_tokens: number, started_at: number, finished_at: number, token_usage: Record<string, TokenUsage[]> }` | crate |
| Rust | `Stats.FILE: string = "events.jsonl"` | private |
| Rust | `Stats.load(dir: string): Stats throws io::Error` | crate |
| Rust | `Stats.for_each_event(dir: string, visit: (event: Event) => void): void throws io::Error` | crate |
| Rust | `Stats.event_count(event: EventName): number` | crate |
| Rust | `Stats.input_tokens(): number` | crate |
| Rust | `Stats.output_tokens(): number` | crate |
| Rust | `Stats.execution_duration(): number?` | crate |
| Rust | `Stats.new(): Stats` | crate |
| Rust | `Stats.append(dir: string, event: Event): void throws io::Error` | crate |
| Rust | `Stats.record(event: Event): void` | crate |
| Rust | `Stats.usage_for_ticket(ticket_key: string): TokenUsage[]` | crate |
| Rust | `Stats.reset_usage(ticket_key: string): void` | crate |
| Rust | `Stats.restart_clock(): void` | crate |
| Rust | `Stats.record_usage(ticket_key: string, usage: TokenUsage): void` | private |

## `crates/agentwerk/src/agents/tickets/error.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `TicketError` | pub |
| Python | `RuntimeError`: `TicketError::TicketMissing { key }` reads as `Ticket TICKET-1 not found` | |
| Rust | `TicketError.TicketMissing { key: string }` | pub |
| Rust | `TicketError.TransitionRejected { from: Status, to: Status }` | pub |
| Rust | `TicketError.ResultRejected { message: string }` | pub |
| Rust | `impl Display for TicketError` | pub |
| Rust | `impl Error for TicketError` | pub |

## `crates/agentwerk/src/agents/tickets/mod.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod error`, `mod reply`, `mod store`, `mod ticket`, `mod ticket_queue`, `mod trajectory` | private |
| Rust | re-exports `Author`, `Reply`, `ReplyContent`, `Status`, `Ticket`, `TicketError`, `TicketQueue`, `Trajectory` | pub |
| Rust | `policy_violated_kind(policies: Policies, stats: Stats): [PolicyKind, number]?` | crate |
| Rust | `now_millis(): number` | crate |
| Rust | `numeric_id(key: string): number` | crate |

## `crates/agentwerk/src/agents/tickets/reply.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Author` | pub |
| Python | the `author` string: `"system"`, `"user"`, or `"assistant"` | |
| Rust | `Author.System` | pub |
| Rust | `Author.User` | pub |
| Rust | `Author.Assistant` | pub |
| Rust | `Reply { author: Author, content: ReplyContent[], created_at: number }` | pub |
| Python | `Reply.author`, `.content`, `.created_at` | |
| Rust | `ReplyContent` | pub |
| Python | `ReplyContent.kind` plus `.data`, like `Event`. Built with `ReplyContent.text(..)`, `.tool_use(..)`, `.tool_result(..)`, `.thinking(..)`, `.redacted_thinking(..)` | |
| Rust | `ReplyContent.Text { text: string }` | pub |
| Rust | `ReplyContent.ToolUse { id: string, name: string, input: json }` | pub |
| Rust | `ReplyContent.ToolResult { tool_use_id: string, content: string, succeeded: boolean, path: string? }` | pub |
| Rust | `ReplyContent.Thinking { thinking: string, signature: string }` | pub |
| Rust | `ReplyContent.RedactedThinking { data: string }` | pub |
| Rust | `Reply.user(blocks: ContentBlock[], paths: Record<string, string>): Reply` | crate |
| both | `Reply.user_text(text: string): Reply` | pub |
| Python | `Reply.user_text(text)`: the only way to build a reply, since any other carries no timestamp the store would trust | |
| Rust | `Reply.assistant(blocks: ContentBlock[]): Reply` | crate |
| Rust | `Reply.system_text(text: string): Reply` | crate |
| Rust | `Reply.as_message(): Message?` | crate |
| Rust | `ReplyContent.from_block(b: ContentBlock, paths: Record<string, string>): ReplyContent` | private |
| Rust | `ReplyContent.to_block(): ContentBlock` | private |

## `crates/agentwerk/src/agents/tickets/store.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `max_existing_ticket_id(dir: string): number` | private |
| Rust | `TicketQueue.insert(ticket: Ticket, reporter: string): string` | crate |
| Rust | `TicketQueue.save_ticket(key: string): void` | private |
| Rust | `TicketQueue.write_tool_output(key: string, tool_use_id: string, content: string): string?` | crate |
| Rust | `TicketQueue.claim(predicate: (ticket: Ticket) => boolean, agent_id: string): string?` | crate |
| Rust | `TicketQueue.add_reply(key: string, reply: Reply): void` | crate |
| Rust | `TicketQueue.set_finished_by(key: string, agent: string): void throws TicketError` | crate |
| both | `TicketQueue.set_finished(key: string, result: json): void throws TicketError` | pub |
| both | `TicketQueue.set_failed(key: string): void throws TicketError` | pub |
| Rust | `TicketQueue.set_failed_by(key: string, agent: string): void throws TicketError` | crate |
| Rust | `TicketQueue.set_final_status(key: string, status: Status, agent: string): void throws TicketError` | private |
| Rust | `TicketQueue.set_result(key: string, result: json): [json, string[]] throws SchemaViolations` | crate |
| both | `TicketQueue.edit_replies(key: string, editor: (replies: Reply[]) => void): TicketQueue` | pub |
| Python | `TicketQueue.edit_replies(key, editor)`: the editor returns the new list, or `None` to keep the old one, where Rust mutates in place. An editor that raises, or returns anything but `Reply` objects, raises here | |
| Rust | `TicketQueue.edit(key: string, task: json?, label: string?): void throws TicketError` | crate |

## `crates/agentwerk/src/agents/tickets/ticket.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Ticket { task: json, label: string?, schema: Schema?, key: string, status: Status, reporter: string, assignee: string?, created_at: number, started_at: number?, finished_at: number?, failed_at: number?, result: json?, parent: string?, replies: Reply[] }` | pub |
| Python | `Ticket`: same field names. `replies` is a list of `Reply`, converted on access | |
| Rust | `Ticket.new(task: json): Ticket` | pub |
| Python | `Ticket(task)` | |
| Rust | `Ticket.label(label: string): Ticket` | pub |
| Python | `Ticket(task, label=l)` | |
| Rust | `Ticket.schema(schema: Schema): Ticket` | pub |
| Python | `Ticket(task, schema=s)` | |
| Rust | `Ticket.parent(key: string): Ticket` | pub |
| Python | `Ticket(task, parent=key)` | |
| both | `Ticket.has_label(label: string): boolean` | pub |
| both | `Ticket.is_todo(): boolean` | pub |
| both | `Ticket.is_finished(): boolean` | pub |
| both | `Ticket.is_failed(): boolean` | pub |
| both | `Ticket.is_in_progress(): boolean` | pub |
| both | `Ticket.is_pending(): boolean` | pub |
| Rust | `Ticket.is_waiting_for_response(): boolean` | crate |
| Rust | `Ticket.is_paused(): boolean` | crate |
| Rust | `Ticket.to_messages(): Message[]` | crate |
| Rust | `Ticket.stamp_transition(next: Status, now: number): void` | crate |
| Rust | `impl Persist for Ticket` | pub |
| Rust | `TicketResult { key: string, value: json? }` | crate |
| Rust | `impl Persist for TicketResult` | crate |
| Rust | `Replies { key: string, entries: Reply[] }` | crate |
| Rust | `Replies.append(dir: string, key: string, reply: Reply): void throws io::Error` | crate |
| Rust | `impl Persist for Replies` | crate |
| Rust | `ticket_record_path(dir: string, key: string): string` | super |
| Rust | `replies_path(dir: string, key: string): string` | private |
| Rust | `result_path(dir: string, key: string): string` | super |
| Rust | `impl AsUserMessage for Ticket` | pub |
| Rust | `Status` | pub |
| Python | a string: `"todo"`, `"in_progress"`, `"finished"`, `"failed"`. The five `is_*` predicates read better than comparing it | |
| Rust | `Status.Todo` | pub |
| Rust | `Status.InProgress` | pub |
| Rust | `Status.Finished` | pub |
| Rust | `Status.Failed` | pub |
| Rust | `impl Display for Status` | pub |

## `crates/agentwerk/src/agents/tickets/ticket_queue.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `EventHandler = (event: Event) => void` | private |
| Rust | `TicketFilter = (ticket: Ticket) => boolean` | crate |
| Rust | `EVENT_STREAM_CAPACITY: number = 1024` | private |
| Rust | `AsyncResultHandler = (ticket: Ticket, result: json) => HandlerWork` | private |
| Rust | `AsyncResultsHandler = (results: json[]) => HandlerWork` | private |
| Rust | `HandlerWork = Promise<void>` | private |
| Rust | `ReplyEditor = (events: Event[], replies: Reply[]) => void` | private |
| Rust | `CompactionEditor = (compaction: Compaction, replies: Reply[]) => EditedReplies` | private |
| Rust | `DirectiveEditor = (event: Event, directive: string) => void` | private |
| Rust | `EditedReplies = Promise<Reply[] throws ProviderError>` | private |
| Rust | `Delivery = [Ticket, json, json[]]` | private |
| Rust | `AwaitedResults { per_result: AsyncResultHandler[], all_results: AsyncResultsHandler[], queued: Delivery[], draining: void, queueing: void }` | super |
| Rust | `ReplyEditing { editor: ReplyEditor?, pending: Record<string, Event[]> }` | super |
| Rust | `Run { phase: Phase }` | crate |
| Rust | `Phase` | private |
| Rust | `Phase.Working` | private |
| Rust | `Phase.Draining(FinishReason)` | private |
| Rust | `Phase.Finished(FinishReason)` | private |
| Rust | `impl Default for Run` | crate |
| Rust | `Run.set_draining(reason: FinishReason): void` | crate |
| Rust | `Run.set_finished(): void` | crate |
| Rust | `Run.is_working(): boolean` | crate |
| Rust | `Run.is_finished(): boolean` | crate |
| Rust | `Run.reason(): FinishReason?` | crate |
| Rust | `Run.until_draining(): Promise<void>` | crate |
| Rust | `Run.until_finished(): Promise<void>` | crate |
| Rust | `Run.reset(): void` | private |
| Rust | `TicketQueue { weak_self: Weak<TicketQueue>, tickets: Record<string, Ticket>, agents: Agent[], policies: Policies, run: Run, cancel_filters: TicketFilter[], terminal_transitions_in_flight: number, stats: Stats, event_handlers: EventHandler[], awaited_results: AwaitedResults, event_stream: Sender<Event>, reply_editing: ReplyEditing, compaction_editor: CompactionEditor?, directive_editor: DirectiveEditor?, schemas: SchemaStore?, dir: string, events_lock: void, join_handle: JoinHandle<void>?, next_ticket_id: number? }` | pub |
| Python | `TicketQueue` | |
| Rust | `TicketQueue.new(): TicketQueue` | pub |
| Python | `TicketQueue()` | |
| both | `TicketQueue.load(tickets_dir: string): TicketQueue throws io::Error` | pub |
| both | `TicketQueue.input_tokens(): number` | pub |
| both | `TicketQueue.output_tokens(): number` | pub |
| both | `TicketQueue.execution_duration(): number?` | pub |
| both | `TicketQueue.on_event(handler: (event: Event) => void): TicketQueue` | pub |
| Rust | `TicketQueue.on_ticket_event(wanted: (kind: EventKind) => boolean, handler: (queue: TicketQueue, event: Event, ticket: Ticket) => void): TicketQueue` | private |
| both | `TicketQueue.on_result(handler: (ticket: Ticket, result: json) => void): TicketQueue` | pub |
| both | `TicketQueue.on_result_async(handler: (ticket: Ticket, result: json) => Promise<void>): TicketQueue` | pub |
| Python | `TicketQueue.on_result_async(handler)`: takes an `async def`, awaited on the event loop awaiting `finish`, so a handler that raises prints its traceback and does not stop the run | |
| both | `TicketQueue.on_results_async(handler: (results: json[]) => Promise<void>): TicketQueue` | pub |
| Python | `TicketQueue.on_results_async(handler)`: takes an `async def`, on the same terms as `on_result_async` | |
| Rust | `TicketQueue.queue_finished_results(): void` | private |
| Rust | `TicketQueue.await_result_handlers(): Promise<void>` | private |
| both | `TicketQueue.on_results(handler: (results: json[]) => void): TicketQueue` | pub |
| Python | `TicketQueue.on_results(handler)`: the results arrive as a list | |
| both | `TicketQueue.on_failure(handler: (event: Event, ticket: Ticket) => void): TicketQueue` | pub |
| both | `TicketQueue.edit_replies_on_event(editor: (events: Event[], replies: Reply[]) => void): TicketQueue` | pub |
| Python | `TicketQueue.edit_replies_on_event(editor)`: the editor returns the new list, or `None` to keep the old one, where Rust mutates in place. An editor that raises prints its traceback and changes nothing | |
| both | `TicketQueue.edit_replies_on_compaction(editor: (compaction: Compaction, replies: Reply[]) => Promise<Reply[] throws ProviderError>): TicketQueue` | pub |
| Python | `TicketQueue.edit_replies_on_compaction(editor)`: the editor returns the new list, or `None` to keep the current one. Define it with `async def` to await `Compaction.summarize`; a coroutine is driven on a worker thread of its own | |
| Rust | `TicketQueue.compaction_editor(): CompactionEditor?` | crate |
| both | `TicketQueue.edit_directive_on_retry(editor: (event: Event, directive: string) => void): TicketQueue` | pub |
| Python | `TicketQueue.edit_directive_on_retry(editor)`: the editor returns the replacement, or `None` to keep the default, where Rust rewrites in place | |
| Rust | `TicketQueue.edit_directive(event: Event, directive: string): void` | crate |
| Rust | `TicketQueue.emit(key: string, agent: string, kind: EventKind): Event` | crate |
| Rust | `TicketQueue.run_reply_editor(key: string): void` | crate |
| Rust | `TicketQueue.label_for(key: string): string?` | private |
| both | `TicketQueue.model_for_agent(agent_id: string): string?` | pub |
| Rust | `TicketQueue.policies(): Policies` | crate |
| both | `TicketQueue.max_turns(count: number): TicketQueue` | pub |
| both | `TicketQueue.max_input_tokens(count: number): TicketQueue` | pub |
| both | `TicketQueue.max_output_tokens(count: number): TicketQueue` | pub |
| both | `TicketQueue.max_request_tokens(count: number): TicketQueue` | pub |
| both | `TicketQueue.max_schema_retries(count: number): TicketQueue` | pub |
| both | `TicketQueue.max_request_retries(count: number): TicketQueue` | pub |
| Rust | `TicketQueue.request_retry_delay(duration: number): TicketQueue` | pub |
| Python | `TicketQueue.request_retry_delay(seconds)` | |
| Rust | `TicketQueue.max_time(duration: number): TicketQueue` | pub |
| Python | `TicketQueue.max_time(seconds)` | |
| both | `TicketQueue.compact_at(fraction: number): TicketQueue` | pub |
| both | `TicketQueue.get_max_turns(): number?` | pub |
| both | `TicketQueue.get_max_input_tokens(): number?` | pub |
| both | `TicketQueue.get_max_output_tokens(): number?` | pub |
| both | `TicketQueue.get_max_request_tokens(): number?` | pub |
| both | `TicketQueue.get_max_schema_retries(): number?` | pub |
| both | `TicketQueue.get_max_request_retries(): number` | pub |
| both | `TicketQueue.get_request_retry_delay(): number` | pub |
| both | `TicketQueue.get_max_time(): number?` | pub |
| both | `TicketQueue.get_compact_at(): number?` | pub |
| both | `TicketQueue.create_ticket_on_event(make: (event: Event) => Ticket?): TicketQueue` | pub |
| both | `TicketQueue.create_ticket_on_result(make: (ticket: Ticket, result: json) => Ticket?): TicketQueue` | pub |
| both | `TicketQueue.create_tickets_on_results(make: (results: json[]) => Ticket[]): TicketQueue` | pub |
| Python | `TicketQueue.create_tickets_on_results(make)`: hand back any sequence of tickets. `None` adds nothing, where Rust has only the empty list | |
| both | `TicketQueue.create_ticket_on_failure(make: (event: Event, ticket: Ticket) => Ticket?): TicketQueue` | pub |
| both | `TicketQueue.on_ticket(handler: (event: Event, ticket: Ticket) => void): TicketQueue` | pub |
| both | `TicketQueue.dir(dir: string): TicketQueue` | pub |
| both | `TicketQueue.get_dir(): string` | pub |
| Rust | `TicketQueue.result_path(key: string): string` | crate |
| both | `TicketQueue.schemas(store: SchemaStore): TicketQueue` | pub |
| both | `TicketQueue.task(task: json): string` | pub |
| both | `TicketQueue.ticket(ticket: Ticket): string` | pub |
| both | `TicketQueue.reply(key: string, content: string): TicketQueue` | pub |
| Rust | `TicketQueue.dispatch(ticket: Ticket): string` | private |
| both | `TicketQueue.get_ticket(key: string): Ticket?` | pub |
| both | `TicketQueue.tickets(): Ticket[]` | pub |
| both | `TicketQueue.find_tickets(predicate: (ticket: Ticket) => boolean): Ticket[]` | pub |
| both | `TicketQueue.find_ticket(predicate: (ticket: Ticket) => boolean): Ticket?` | pub |
| both | `TicketQueue.find_events(predicate: (event: Event) => boolean): Event[]` | pub |
| both | `TicketQueue.find_event(predicate: (event: Event) => boolean): Event?` | pub |
| both | `TicketQueue.cancel(matches: (ticket: Ticket) => boolean): TicketQueue` | pub |
| both | `TicketQueue.cancel_all(): TicketQueue` | pub |
| both | `TicketQueue.is_cancelled(ticket: Ticket): boolean` | pub |
| Rust | `TicketQueue.work_left(matches: (ticket: Ticket) => boolean): boolean` | crate |
| Rust | `TicketQueue.ending_reason(): FinishReason?` | crate |
| Rust | `TicketQueue.anything_pending(): boolean` | private |
| Rust | `TicketQueue.is_running(): boolean` | private |
| Rust | `TicketQueue.interactive_agents(): string[]` | private |
| Rust | `TicketQueue.bind_agent(agent: Agent): void` | crate |
| Rust | `TicketQueue.clone_agents(): Agent[]` | crate |
| both | `TicketQueue.agent(agent: Agent): TicketQueue` | pub |
| both | `TicketQueue.start(): TicketQueue` | pub |
| both | `TicketQueue.finish(matches: (ticket: Ticket) => boolean): Promise<json[]>` | pub |
| both | `TicketQueue.finish_all(): Promise<json[]>` | pub |
| both | `TicketQueue.finish_last(): Promise<json?>` | pub |
| Rust | `TicketQueue.finish_reason(): FinishReason?` | pub |
| Python | `TicketQueue.finish_reason(): str?`: the string it prints as, such as `policy_violated(turns)` | |
| Rust | `TicketQueue.next_event_or_end(stream: Event): Promise<boolean>` | private |
| both | `TicketQueue.results(): json[]` | pub |

## `crates/agentwerk/src/agents/tickets/trajectory.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Trajectory { key: string, model: string?, replies: Reply[] }` | pub |
| Python | `Trajectory`: same field names. `replies` is a list of `Reply` | |
| both | `Trajectory.from_ticket(agent_id: string, model: string?, ticket: Ticket): Trajectory` | pub |
| both | `Trajectory.save(dir: string): void throws io::Error` | pub |
| Rust | `Trajectory.to_html(): string` | private |
| Rust | `HTML_HEAD: string` | private |
| Rust | `impl Persist for Trajectory` | pub |
| Rust | `trajectory_path(dir: string, key: string): string` | private |

## `crates/agentwerk/src/codegrep/ast.rs`

Not bound: the whole `codegrep` module is reachable from Python through `GrepTool()` with `syntax="code"`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `MetavariableKind` | pub |
| Rust | `MetavariableKind.Plain` | pub |
| Rust | `MetavariableKind.Ellipsis` | pub |
| Rust | `Node` | pub |
| Rust | `Node.Word(string)` | pub |
| Rust | `Node.Other(string)` | pub |
| Rust | `Node.Newline` | pub |
| Rust | `Node.Ellipsis` | pub |
| Rust | `Node.LongEllipsis` | pub |
| Rust | `Node.Metavar(string)` | pub |
| Rust | `Node.MetavarEllipsis(string)` | pub |
| Rust | `Node.LongMetavarEllipsis(string)` | pub |
| Rust | `Node.Bracket(open: string, nodes: Node[], close: string)` | pub |
| Rust | `Pattern { nodes: Node[], conf: Conf }` | pub |
| Rust | `Pattern.nodes(): Node[]` | pub |
| Rust | `Pattern.conf(): Conf` | pub |
| Rust | `Pattern.metavariable_names(): string[]` | pub |
| Rust | `ParseError(string)` | pub |
| Rust | `impl Display for ParseError` | pub |
| Rust | `impl Error for ParseError` | pub |
| Rust | `Pattern.parse(source: string, conf: Conf): Pattern throws ParseError` | pub |
| Rust | `parse_seq_until(tokens: Token[], cursor: number, expected_close: string?): Node[] throws void` | private |
| Rust | `validate_metavariable_consistency(nodes: Node[]): void throws ParseError` | private |
| Rust | `walk_metavars(nodes: Node[], seen: Record<string, MetavariableKind>): void throws ParseError` | private |
| Rust | `record_kind(name: string, kind: MetavariableKind, seen: Record<string, MetavariableKind>): void throws ParseError` | private |

## `crates/agentwerk/src/codegrep/conf.rs`

Not bound, like the rest of `codegrep`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Conf { caseless: boolean, multiline: boolean, word_chars: string[], brackets: [string, string][] }` | pub |
| Rust | `Conf.default_multiline(): Conf` | pub |
| Rust | `Conf.default_singleline(): Conf` | pub |
| Rust | `Conf.check(): void throws ConfError` | pub |
| Rust | `ConfError(string)` | pub |
| Rust | `impl Display for ConfError` | pub |
| Rust | `impl Error for ConfError` | pub |
| Rust | `word_chars(): string[]` | private |

## `crates/agentwerk/src/codegrep/matcher.rs`

Not bound, like the rest of `codegrep`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Loc { start: number, length: number, substring: string }` | pub |
| Rust | `Metavariable { kind: MetavariableKind, bare_name: string }` | pub |
| Rust | `Match { loc: Loc, captures: [Metavariable, Loc][] }` | pub |
| Rust | `MatchParams { caseless: boolean, multiline: boolean, word_chars: string[] }` | private |
| Rust | `Binding { kind: MetavariableKind, token_start: number, byte_start: number, byte_end: number }` | private |
| Rust | `MetavarEnv { bindings: Record<string, Binding> }` | private |
| Rust | `search(pattern: Pattern, target: string): Match[]` | pub |
| Rust | `search_tokens(pattern: Pattern, tokens: Token[], target: string): Match[]` | pub |
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

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod ast`, `mod conf`, `mod matcher`, `mod token` | pub |
| Rust | re-exports `MetavariableKind`, `Node`, `ParseError`, `Pattern`, `Conf`, `ConfError`, `search`, `search_tokens`, `Loc`, `Match`, `Metavariable`, `tokenize_pattern`, `tokenize_target`, `Token` | pub |

## `crates/agentwerk/src/codegrep/token.rs`

Not bound, like the rest of `codegrep`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Token` | pub |
| Rust | `Token.Ellipsis` | pub |
| Rust | `Token.LongEllipsis` | pub |
| Rust | `Token.Metavar(string)` | pub |
| Rust | `Token.MetavarEllipsis(string)` | pub |
| Rust | `Token.LongMetavarEllipsis(string)` | pub |
| Rust | `Token.Word { text: string, start: number }` | pub |
| Rust | `Token.Open { open: string, close: string, start: number }` | pub |
| Rust | `Token.Close { close: string, start: number }` | pub |
| Rust | `Token.Newline { start: number }` | pub |
| Rust | `Token.Other { text: string, start: number }` | pub |
| Rust | `Token.start(): number` | crate |
| Rust | `tokenize_pattern(source: string, conf: Conf): Token[]` | pub |
| Rust | `tokenize_target(source: string, conf: Conf): Token[]` | pub |
| Rust | `scan(source: string, conf: Conf, pattern_mode: boolean): Token[]` | private |
| Rust | `is_blank(ch: string, multiline: boolean): boolean` | private |
| Rust | `read_metavar_name(rest: string): [string, number]?` | private |
| Rust | `is_name_start(c: string): boolean` | private |
| Rust | `is_name_continue(c: string): boolean` | private |

## `crates/agentwerk/src/event.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `CompactReason` | pub |
| Python | a string inside `Event.data`, under the field's own name: `data["reason"]` | |
| Rust | `CompactReason.Proactive` | pub |
| Rust | `CompactReason.Reactive` | pub |
| Rust | `impl Display for CompactReason` | pub |
| Rust | `PolicyKind` | pub |
| Python | a string inside `Event.data`: `data["policy"]` | |
| Rust | `PolicyKind.Turns` | pub |
| Rust | `PolicyKind.InputTokens` | pub |
| Rust | `PolicyKind.OutputTokens` | pub |
| Rust | `PolicyKind.MaxSchemaRetries` | pub |
| Rust | `PolicyKind.Time` | pub |
| Rust | `impl Display for PolicyKind` | pub |
| Rust | `FinishReason` | pub |
| Python | a string, such as `policy_violated(turns)` | |
| Rust | `FinishReason.Drained` | pub |
| Rust | `FinishReason.PolicyViolated(PolicyKind)` | pub |
| Rust | `FinishReason.Cancelled` | pub |
| Rust | `impl Display for FinishReason` | pub |
| Rust | `ToolFailureKind` | pub |
| Python | a string inside `Event.data`: `data["reason"]` | |
| Rust | `ToolFailureKind.ToolNotFound` | pub |
| Rust | `ToolFailureKind.ExecutionFailed` | pub |
| Rust | `ToolFailureKind.SchemaValidationFailed` | pub |
| Rust | `ToolFailureKind.name(): string` | pub |
| Python | not bound: the kind is already a string | |
| Rust | `impl Display for ToolFailureKind` | pub |
| Rust | `RepairKind` | pub |
| Python | a string inside `Event.data`: `data["reason"]` | |
| Rust | `RepairKind.CallMalformed` | pub |
| Rust | `RepairKind.ValueMistyped` | pub |
| Rust | `RepairKind.name(): string` | pub |
| Python | not bound: the kind is already a string | |
| Rust | `impl Display for RepairKind` | pub |
| Rust | `KnowledgeFailureKind` | pub |
| Python | a string inside `Event.data`: `data["reason"]` | |
| Rust | `KnowledgeFailureKind.PageMissing` | pub |
| Rust | `KnowledgeFailureKind.StoreRefused` | pub |
| Rust | `KnowledgeFailureKind.name(): string` | pub |
| Python | not bound: the kind is already a string | |
| Rust | `impl Display for KnowledgeFailureKind` | pub |
| Rust | `KnowledgeOp` | pub |
| Python | a string inside `Event.data`: `data["op"]` | |
| Rust | `KnowledgeOp.Write` | pub |
| Rust | `KnowledgeOp.Read` | pub |
| Rust | `KnowledgeOp.Remove` | pub |
| Rust | `KnowledgeOp.List` | pub |
| Rust | `KnowledgeOp.name(): string` | pub |
| Python | not bound: the op is already a string | |
| Rust | `impl Display for KnowledgeOp` | pub |
| Rust | `Event { created_at: number, agent_id: string, ticket_key: string, label: string?, kind: EventKind }` | pub |
| Python | `Event.created_at`, `.agent_id`, `.ticket_key`, `.label`, `.kind`, plus `.data`: a dict of the kind's fields | |
| Rust | `Event.new(agent_id: string, ticket_key: string, label: string?, kind: EventKind): Event` | crate |
| Rust | `EventKind` | pub |
| Python | a string on `Event.kind`, its payload on `Event.data` | |
| Rust | `EventKind.RunStarted` | pub |
| Rust | `EventKind.RunFinished { reason: FinishReason }` | pub |
| Rust | `EventKind.TicketCreated` | pub |
| Rust | `EventKind.TicketStarted` | pub |
| Rust | `EventKind.TicketFinished` | pub |
| Rust | `EventKind.TicketFailed` | pub |
| Rust | `EventKind.TurnStarted` | pub |
| Rust | `EventKind.RequestStarted { model: string }` | pub |
| Rust | `EventKind.RequestFinished { model: string, usage: TokenUsage }` | pub |
| Rust | `EventKind.RequestFailed { model: string, reason: RequestErrorKind, message: string }` | pub |
| Rust | `EventKind.RequestRetried { model: string, attempt: number, max_attempts: number, reason: RequestErrorKind, message: string }` | pub |
| Rust | `EventKind.TextChunkReceived { content: string }` | pub |
| Rust | `EventKind.ResponseRepaired { tool_name: string, reason: RepairKind, message: string }` | pub |
| Rust | `EventKind.ToolCallDeclined { tool_name: string, reason: ToolDeclineKind }` | pub |
| Rust | `EventKind.ToolCallStarted { tool_name: string, call_id: string, input: json }` | pub |
| Rust | `EventKind.ToolCallFinished { tool_name: string, call_id: string, output: string }` | pub |
| Rust | `EventKind.ToolCallFailed { tool_name: string, call_id: string, reason: ToolFailureKind, message: string }` | pub |
| Rust | `EventKind.FileOpenFinished { path: string }` | pub |
| Rust | `EventKind.FileOpenFailed { path: string, reason: ToolFailureKind }` | pub |
| Rust | `EventKind.KnowledgeUsed { op: KnowledgeOp }` | pub |
| Rust | `EventKind.KnowledgeFailed { op: KnowledgeOp, reason: KnowledgeFailureKind }` | pub |
| Rust | `EventKind.PolicyViolated { policy: PolicyKind, limit: number }` | pub |
| Rust | `EventKind.SchemaRetried { attempt: number, max_attempts: number, message: string }` | pub |
| Rust | `EventKind.CompactionStarted { reason: CompactReason, total: number }` | pub |
| Rust | `EventKind.CompactionProgress { reason: CompactReason, completed: number, total: number }` | pub |
| Rust | `EventKind.CompactionFinished { reason: CompactReason }` | pub |
| Rust | `EventKind.CompactionFailed { reason: CompactReason, message: string }` | pub |
| Rust | `EventKind.name(): string` | pub |
| Python | not bound: `Event.kind` is already that name | |
| Rust | `EventKind.event_name(): EventName` | pub |
| Python | not bound: `Event.kind` is already that name | |
| Rust | `EventKind.is_failure(): boolean` | pub |
| Python | not bound: `Event.kind` is a string, so ask `TicketQueue.on_failure(handler)` for the same six kinds | |
| Rust | `impl Display for EventKind` | pub |
| Rust | `EventName` | pub |
| Python | `EventName`: string constants, so `Event.kind == EventName.TURN_STARTED` | |
| Rust | `EventName.RunStarted` | pub |
| Rust | `EventName.RunFinished` | pub |
| Rust | `EventName.TicketCreated` | pub |
| Rust | `EventName.TicketStarted` | pub |
| Rust | `EventName.TicketFinished` | pub |
| Rust | `EventName.TicketFailed` | pub |
| Rust | `EventName.TurnStarted` | pub |
| Rust | `EventName.RequestStarted` | pub |
| Rust | `EventName.RequestFinished` | pub |
| Rust | `EventName.RequestFailed` | pub |
| Rust | `EventName.RequestRetried` | pub |
| Rust | `EventName.TextChunkReceived` | pub |
| Rust | `EventName.ResponseRepaired` | pub |
| Rust | `EventName.ToolCallDeclined` | pub |
| Rust | `EventName.ToolCallStarted` | pub |
| Rust | `EventName.ToolCallFinished` | pub |
| Rust | `EventName.ToolCallFailed` | pub |
| Rust | `EventName.FileOpenFinished` | pub |
| Rust | `EventName.FileOpenFailed` | pub |
| Rust | `EventName.KnowledgeUsed` | pub |
| Rust | `EventName.KnowledgeFailed` | pub |
| Rust | `EventName.PolicyViolated` | pub |
| Rust | `EventName.SchemaRetried` | pub |
| Rust | `EventName.CompactionStarted` | pub |
| Rust | `EventName.CompactionProgress` | pub |
| Rust | `EventName.CompactionFinished` | pub |
| Rust | `EventName.CompactionFailed` | pub |
| Rust | `EventName.ALL: EventName[]` | pub |
| Rust | `EventName.name(): string` | pub |
| Python | not bound: the constants are already strings | |
| Rust | `impl Display for EventName` | pub |
| Rust | `default_logger(): (event: Event) => void` | pub |
| Python | not bound: pass your own handler to `TicketQueue.on_event(handler)` | |
| Rust | `compact_input(input: json): string` | private |

## `crates/agentwerk/src/lib.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod agents`, `mod codegrep`, `mod event`, `mod providers`, `mod schemas`, `mod tools` | pub |
| Rust | `mod persistence`, `mod prompts` | crate |
| Rust | re-exports `Agent`, `AgentBuilder`, `Reply`, `Status`, `Ticket`, `TicketQueue`, `Compaction`, `Knowledge`, `Trajectory`, `Schema`, `SchemaStore`, `Event`, `EventKind`, `FinishReason` | pub |
| Python | `agentwerk` exports every bound class from one flat module | |

## `crates/agentwerk/src/persistence.rs`

Not bound: the crate writes its own files.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Persist { Key, save(dir: string): void throws io::Error, load(dir: string, key: Key): Self throws io::Error }` | crate |
| Rust | `TEMP_COUNTER: number` | private |
| Rust | `write_atomic(path: string, bytes: number[]): void throws io::Error` | crate |
| Rust | `append_line(path: string, line: string): void throws io::Error` | crate |
| Rust | `output_path(key: string, id: string): string` | crate |

## `crates/agentwerk/src/prompts/builder.rs`

Not bound: `prompts` is crate-internal, reached through `Agent.role(..)`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Prompt { system: string, task: string? }` | pub |
| Rust | `PromptBuilder { role: Section?, knowledge: Section?, task: Section?, directives: Section[] }` | pub |
| Rust | `PromptBuilder.role(body: string): PromptBuilder` | pub |
| Rust | `PromptBuilder.knowledge(body: string): PromptBuilder` | pub |
| Rust | `PromptBuilder.task(body: string): PromptBuilder` | pub |
| Rust | `PromptBuilder.append_directive(body: string): PromptBuilder` | pub |
| Rust | `PromptBuilder.build(): Prompt` | pub |

## `crates/agentwerk/src/prompts/mod.rs`

Not bound, like the rest of `prompts`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod builder`, `mod section` | private |
| Rust | `CONTEXT_TEMPLATE: string` | private |
| Rust | `RETRY_TEMPLATE: string` | private |
| Rust | `COMPACTION_TEMPLATE: string` | private |
| Rust | `retry_directive(detail: string): string` | crate |
| Rust | `compaction_directive(): string` | crate |
| Rust | `schema_directive(schema: Schema): string` | crate |
| Rust | `arguments_retry_detail(tool_name: string, violations: string, schema: json?): string` | crate |
| Rust | `context_values(dir: string, policies: Policies, stats: Stats, ticket_key: string): [string, string][]` | crate |
| Rust | `optional(value: string?): string` | private |
| Rust | `render_context(values: [string, string][]): string` | crate |
| Rust | `format_current_date(): string` | private |

## `crates/agentwerk/src/prompts/section.rs`

Not bound, like the rest of `prompts`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Section { heading: string?, body: string }` | crate |
| Rust | `Section.role(body: string): Section` | crate |
| Rust | `Section.knowledge(body: string): Section` | crate |
| Rust | `Section.task(body: string): Section` | crate |
| Rust | `Section.directive(body: string): Section` | crate |
| Rust | `Section.render(): string` | crate |

## `crates/agentwerk/src/providers/anthropic.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_BASE_URL: string = "https://api.anthropic.com"` | private |
| Rust | `Anthropic(Endpoint)` | pub |
| Python | `Anthropic` | |
| Rust | `Anthropic.new(api_key: string): Anthropic` | pub |
| Python | `Anthropic(api_key, base_url=.., timeout=..)` | |
| Rust | `Anthropic.base_url(url: string): Anthropic` | pub |
| Python | `Anthropic(api_key, base_url=..)` | |
| Rust | `Anthropic.timeout(duration: number): Anthropic` | pub |
| Python | `Anthropic(api_key, timeout=..)` | |
| Rust | `Anthropic.from_env(): Anthropic throws ProviderError` | crate |
| Rust | `impl ProviderLike for Anthropic` | pub |
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

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_REQUEST_TIMEOUT: number = 600000` | crate |
| Rust | `Endpoint { api_key: string, base_url: string, client: reqwest::Client, timeout: number }` | crate |
| Rust | `Endpoint.new(api_key: string, base_url: string): Endpoint` | crate |
| Rust | `Endpoint.base_url(url: string): Endpoint` | crate |
| Rust | `Endpoint.timeout(duration: number): Endpoint` | crate |
| Rust | `Endpoint.api_key(): string` | crate |
| Rust | `Endpoint.post(path: string, body: json): reqwest::RequestBuilder` | crate |
| Rust | `Endpoint.send(request: reqwest::RequestBuilder, classify: (status: number, body: string) => ProviderError?): Promise<reqwest::Response throws ProviderError>` | crate |
| Rust | `map_http_errors(response: reqwest::Response, classify: (status: number, body: string) => ProviderError?): Promise<reqwest::Response throws ProviderError>` | private |
| Rust | `retry_delay_from_headers(response: reqwest::Response): number?` | private |
| Rust | `classify_common_status(status: number, body: string): ProviderError?` | private |
| Rust | `fallback_http_error(status: number, body: string, retry_delay: number?): ProviderError` | private |
| Rust | `build_client(timeout: number): reqwest::Client` | private |
| Rust | `root_certificates_from(file: string?, dir: string?): reqwest::Certificate[]` | private |

## `crates/agentwerk/src/providers/environment.rs`

Not bound: `Provider.from_env()` and `Model.from_env()` read these variables.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DetectedProvider` | crate |
| Rust | `DetectedProvider.Anthropic` | crate |
| Rust | `DetectedProvider.Mistral` | crate |
| Rust | `DetectedProvider.OpenAi` | crate |
| Rust | `DetectedProvider.LiteLlm` | crate |
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

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ProviderError` | pub |
| Python | `RuntimeError` | |
| Rust | `ProviderError.AuthenticationFailed { message: string }` | pub |
| Rust | `ProviderError.PermissionDenied { message: string }` | pub |
| Rust | `ProviderError.ModelNotFound { message: string }` | pub |
| Rust | `ProviderError.ContextWindowExceeded { message: string }` | pub |
| Rust | `ProviderError.SafetyFilterTriggered { message: string }` | pub |
| Rust | `ProviderError.RateLimited { message: string, status: number, retry_delay: number? }` | pub |
| Rust | `ProviderError.StatusUnclassified { status: number, message: string, retryable: boolean, retry_delay: number? }` | pub |
| Rust | `ProviderError.ConnectionFailed { message: string }` | pub |
| Rust | `ProviderError.StreamInterrupted { message: string }` | pub |
| Rust | `ProviderError.ResponseMalformed { message: string }` | pub |
| Rust | `ProviderError.ProviderUnrecognized { message: string }` | pub |
| Rust | `ProviderError.is_retryable(): boolean` | pub |
| Python | not bound: the error arrives as `RuntimeError` | |
| Rust | `ProviderError.retry_delay(): number?` | pub |
| Python | not bound: the error arrives as `RuntimeError` | |
| Rust | `ProviderError.kind(): RequestErrorKind` | pub |
| Python | not bound: the error arrives as `RuntimeError` | |
| Rust | `impl Display for ProviderError` | pub |
| Rust | `impl Error for ProviderError` | pub |
| Rust | `RequestErrorKind` | pub |
| Python | a string inside `Event.data`: `data["reason"]` | |
| Rust | `RequestErrorKind.AuthenticationFailed` | pub |
| Rust | `RequestErrorKind.PermissionDenied` | pub |
| Rust | `RequestErrorKind.ModelNotFound` | pub |
| Rust | `RequestErrorKind.ContextWindowExceeded` | pub |
| Rust | `RequestErrorKind.SafetyFilterTriggered` | pub |
| Rust | `RequestErrorKind.RateLimited` | pub |
| Rust | `RequestErrorKind.StatusUnclassified` | pub |
| Rust | `RequestErrorKind.ConnectionFailed` | pub |
| Rust | `RequestErrorKind.StreamInterrupted` | pub |
| Rust | `RequestErrorKind.ResponseMalformed` | pub |
| Rust | `RequestErrorKind.ProviderUnrecognized` | pub |
| Rust | `RequestErrorKind.name(): string` | pub |
| Python | not bound: the kind is already a string | |
| Rust | `impl Display for RequestErrorKind` | pub |
| Rust | `ProviderResult<T> = T throws ProviderError` | pub |
| Python | `RuntimeError` | |
| Rust | `OVERFLOW_PATTERNS: string[]` | private |
| Rust | `RATE_LIMIT_PATTERNS: string[]` | private |
| Rust | `recover_wrapped_error(status: number, body: string, retry_delay: number?): ProviderError?` | crate |

## `crates/agentwerk/src/providers/frames.rs`

Not bound: it repairs a reply before the loop reads it.

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

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_BASE_URL: string = "http://localhost:4000"` | private |
| Rust | `LiteLlm(Endpoint)` | pub |
| Python | `LiteLlm` | |
| Rust | `LiteLlm.new(api_key: string): LiteLlm` | pub |
| Python | `LiteLlm(api_key, base_url=.., timeout=..)` | |
| Rust | `LiteLlm.base_url(url: string): LiteLlm` | pub |
| Python | `LiteLlm(api_key, base_url=..)` | |
| Rust | `LiteLlm.timeout(duration: number): LiteLlm` | pub |
| Python | `LiteLlm(api_key, timeout=..)` | |
| Rust | `LiteLlm.from_env(): LiteLlm throws ProviderError` | crate |
| Rust | `impl ProviderLike for LiteLlm` | pub |

## `crates/agentwerk/src/providers/mistral.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_BASE_URL: string = "https://api.mistral.ai"` | private |
| Rust | `Mistral(Endpoint)` | pub |
| Python | `Mistral` | |
| Rust | `Mistral.new(api_key: string): Mistral` | pub |
| Python | `Mistral(api_key, base_url=.., timeout=..)` | |
| Rust | `Mistral.base_url(url: string): Mistral` | pub |
| Python | `Mistral(api_key, base_url=..)` | |
| Rust | `Mistral.timeout(duration: number): Mistral` | pub |
| Python | `Mistral(api_key, timeout=..)` | |
| Rust | `Mistral.from_env(): Mistral throws ProviderError` | crate |
| Rust | `impl ProviderLike for Mistral` | pub |

## `crates/agentwerk/src/providers/mod.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod types` | pub |
| Rust | `mod environment`, `mod model` | crate |
| Rust | `mod anthropic`, `mod endpoint`, `mod error`, `mod frames`, `mod litellm`, `mod mistral`, `mod openai`, `mod provider`, `mod stream` | private |
| Rust | re-exports `Anthropic`, `ProviderError`, `ProviderResult`, `RequestErrorKind`, `LiteLlm`, `Mistral`, `Model`, `OpenAi`, `Provider`, `ProviderLike`, and the `types` values | pub |
| Python | the four providers are bound; the request and response types are not | |

## `crates/agentwerk/src/providers/model.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Model { name: string, context_window: number?, reasoning_effort: ReasoningEffort }` | pub |
| Python | `Model.name` | |
| Rust | `Model.new(name: string): Model` | pub |
| Python | `Model(name)` | |
| both | `Model.from_env(): Model throws ProviderError` | pub |
| both | `Model.context_window(size: number): Model` | pub |
| both | `Model.reasoning_effort(effort: ReasoningEffort): Model` | pub |
| both | `Model.get_context_window(): number?` | pub |
| both | `Model.get_reasoning_effort(): ReasoningEffort` | pub |
| Rust | `impl From<string> for Model` | pub |
| Python | not bound: `Model(name)` already takes the string | |
| Rust | `context_window_for(name: string): number?` | private |

## `crates/agentwerk/src/providers/openai.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFAULT_BASE_URL: string = "https://api.openai.com"` | private |
| Rust | `OpenAi(Endpoint)` | pub |
| Python | `OpenAi` | |
| Rust | `OpenAi.new(api_key: string): OpenAi` | pub |
| Python | `OpenAi(api_key, base_url=.., timeout=..)` | |
| Rust | `OpenAi.base_url(url: string): OpenAi` | pub |
| Python | `OpenAi(api_key, base_url=..)` | |
| Rust | `OpenAi.timeout(duration: number): OpenAi` | pub |
| Python | `OpenAi(api_key, timeout=..)` | |
| Rust | `OpenAi.from_env(): OpenAi throws ProviderError` | crate |
| Rust | `impl ProviderLike for OpenAi` | pub |
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

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ProviderLike { respond(request: ModelRequest, on_event: (event: StreamEvent) => void): Promise<ModelResponse throws ProviderError> }` | pub |
| Python | not bound: implement it in Rust to write a new LLM provider | |
| Rust | `impl ProviderLike for Arc<T>` | pub |
| Rust | `Provider(ProviderLike)` | pub |
| Python | `Provider`: an opaque handle | |
| Rust | `Provider.new(provider: ProviderLike): Provider` | pub |
| Python | not bound: the per-vendor constructors already hand back a `Provider` | |
| both | `Provider.from_env(): Provider throws ProviderError` | pub |
| Rust | `Provider.verify(model: string): Promise<void throws ProviderError>` | pub |
| Python | not bound | |
| Rust | `impl From<P> for Provider` | pub |
| Rust | `impl Deref for Provider` | pub |
| Rust | `Protocol { PATH: string, authenticate(posted: reqwest::RequestBuilder, api_key: string): reqwest::RequestBuilder, serialize(request: ModelRequest): json, classify_error(status: number, body: string): ProviderError?, decode(payload: json, reply: ResponseBuilder): void, recover(reply: ModelResponse, on_event: (event: StreamEvent) => void): void }` | crate |
| Rust | `respond(endpoint: Endpoint, request: ModelRequest, on_event: (event: StreamEvent) => void): Promise<ModelResponse throws ProviderError>` | crate |

## `crates/agentwerk/src/providers/stream.rs`

Not bound: it turns one HTTP response into a `ModelResponse`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `read_reply(response: reqwest::Response, on_event: (event: StreamEvent) => void, decode: (payload: json, reply: ResponseBuilder) => void): Promise<ModelResponse throws ProviderError>` | crate |
| Rust | `read_stream(response: reqwest::Response, ingest: (payload: json) => void): Promise<void throws ProviderError>` | crate |
| Rust | `LineBuffer { buffer: number[] }` | private |
| Rust | `LineBuffer.new(): LineBuffer` | private |
| Rust | `LineBuffer.push(chunk: number[]): json[]` | private |
| Rust | `read_data_line(line: number[]): json?` | private |
| Rust | `ToolCallKey` | crate |
| Rust | `ToolCallKey.Numbered(number)` | crate |
| Rust | `ToolCallKey.Unnumbered(number)` | crate |
| Rust | `ResponseBuilder { on_event: (event: StreamEvent) => void, model: string, status: ResponseStatus, overflowed: boolean, usage: TokenUsage, blocks: Block[] }` | crate |
| Rust | `ResponseBuilder.new(on_event: (event: StreamEvent) => void): ResponseBuilder` | crate |
| Rust | `ResponseBuilder.set_model(name: string): void` | crate |
| Rust | `ResponseBuilder.set_status(status: ResponseStatus): void` | crate |
| Rust | `ResponseBuilder.set_context_window_exceeded(): void` | crate |
| Rust | `ResponseBuilder.set_input_tokens(tokens: number): void` | crate |
| Rust | `ResponseBuilder.set_output_tokens(tokens: number): void` | crate |
| Rust | `ResponseBuilder.add_text(fragment: string): void` | crate |
| Rust | `ResponseBuilder.add_thinking(fragment: string): void` | crate |
| Rust | `ResponseBuilder.add_signature(fragment: string): void` | crate |
| Rust | `ResponseBuilder.thinking_block(): Block` | private |
| Rust | `ResponseBuilder.add_redacted_thinking(data: string): void` | crate |
| Rust | `ResponseBuilder.open_tool_call(numbered: number?, id: string, name: string): ToolCallKey` | crate |
| Rust | `ResponseBuilder.add_arguments(key: ToolCallKey, fragment: string): void` | crate |
| Rust | `ResponseBuilder.into_response(): ModelResponse throws ProviderError` | crate |
| Rust | `ResponseBuilder.key_for(id: string): ToolCallKey` | private |
| Rust | `ResponseBuilder.tool_call_at(key: ToolCallKey): number` | private |
| Rust | `ResponseBuilder.tool_call_count(): number` | private |
| Rust | `ResponseBuilder.emit(event: StreamEvent): void` | private |
| Rust | `Block` | private |
| Rust | `Block.Text(string)` | private |
| Rust | `Block.Thinking { thinking: string, signature: string }` | private |
| Rust | `Block.RedactedThinking { data: string }` | private |
| Rust | `Block.ToolCall { key: ToolCallKey, id: string, name: string, arguments: string }` | private |
| Rust | `Block.tool_call_key(): ToolCallKey?` | private |
| Rust | `Block.holds_tool_call_id(wanted: string): boolean` | private |
| Rust | `Block.into_content(): ContentBlock` | private |
| Rust | `read_arguments(arguments: string): json` | crate |
| Rust | `model_or_unknown(model: string): string` | private |

## `crates/agentwerk/src/providers/types.rs`

Not bound, apart from `ReasoningEffort` and `ToolDeclineKind`: Python binds the four providers, not the shapes they are built from.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ReasoningEffort` | pub |
| Python | a string: `"off"`, `"low"`, `"medium"`, `"high"` | |
| Rust | `ReasoningEffort.Off` | pub |
| Rust | `ReasoningEffort.Low` | pub |
| Rust | `ReasoningEffort.Medium` | pub |
| Rust | `ReasoningEffort.High` | pub |
| Rust | `ReasoningEffort.label(): string?` | crate |
| Rust | `impl Display for ReasoningEffort` | pub |
| Rust | `ModelRequest { model: string, system_prompt: string, messages: Message[], tools: Tool[], max_request_tokens: number?, reasoning_effort: ReasoningEffort }` | pub |
| Rust | `Message` | pub |
| Rust | `Message.System { content: string }` | pub |
| Rust | `Message.User { content: ContentBlock[] }` | pub |
| Rust | `Message.Assistant { content: ContentBlock[] }` | pub |
| Rust | `Message.user(text: string): Message` | pub |
| Rust | `Message.system(text: string): Message` | pub |
| Rust | `Message.assistant(text: string): Message` | pub |
| Rust | `AsUserMessage { as_user_message(): Message }` | pub |
| Rust | `ContentBlock` | pub |
| Rust | `ContentBlock.Text { text: string }` | pub |
| Rust | `ContentBlock.ToolUse { id: string, name: string, input: json }` | pub |
| Rust | `ContentBlock.ToolResult { tool_use_id: string, content: string, succeeded: boolean }` | pub |
| Rust | `ContentBlock.Thinking { thinking: string, signature: string }` | pub |
| Rust | `ContentBlock.RedactedThinking { data: string }` | pub |
| Rust | `default_true(): boolean` | private |
| Rust | `ResponseStatus` | pub |
| Rust | `ResponseStatus.EndTurn` | pub |
| Rust | `ResponseStatus.StopSequence` | pub |
| Rust | `ResponseStatus.ToolUse` | pub |
| Rust | `ResponseStatus.OutputTruncated` | pub |
| Rust | `ResponseStatus.Refused` | pub |
| Rust | `ResponseStatus.PauseTurn` | pub |
| Rust | `TokenUsage { input_tokens: number, output_tokens: number }` | pub |
| Rust | `impl AddAssign<TokenUsage> for TokenUsage` | pub |
| Rust | `ModelResponse { content: ContentBlock[], status: ResponseStatus, usage: TokenUsage, model: string }` | pub |
| Rust | `ToolDeclineKind` | pub |
| Python | a string inside `Event.data`: `data["reason"]` | |
| Rust | `ToolDeclineKind.OutputTruncated` | pub |
| Rust | `ToolDeclineKind.ReplyNotFinished` | pub |
| Rust | `ToolDeclineKind.AlreadyDelivered` | pub |
| Rust | `ToolDeclineKind.name(): string` | pub |
| Rust | `impl Display for ToolDeclineKind` | pub |
| Rust | `StreamEvent` | pub |
| Rust | `StreamEvent.TextDelta { text: string }` | pub |
| Rust | `StreamEvent.ToolCallRepaired { tool_name: string }` | pub |
| Rust | `StreamEvent.ToolCallDeclined { tool_name: string, reason: ToolDeclineKind }` | pub |

## `crates/agentwerk/src/schemas/mod.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod store` | private |
| Rust | `Schema { inner: SchemaBody }` | pub |
| Python | `Schema` | |
| Rust | `SchemaBody { compiled: Node, raw_document: json }` | private |
| Rust | `Schema.new(document: json): Schema throws SchemaParseError` | pub |
| Python | `Schema(document)` | |
| both | `Schema.validate(value: json): [json, string[]] throws SchemaViolations` | pub |
| Rust | `Schema.get_raw_schema(): json` | pub |
| Python | not bound: Python already holds the document it passed to `Schema(document)` | |
| Rust | `Schema.check(instance: json): void throws SchemaViolation[]` | private |
| Rust | `impl TryFrom<json> for Schema` | pub |
| Python | not bound: `Schema(document)` takes the Python object | |
| Rust | `impl TryFrom<string> for Schema` | pub |
| Python | not bound: a document read from a file is parsed before it gets there | |
| Rust | `impl Debug for Schema` | pub |
| Rust | `impl Serialize for Schema` | pub |
| Rust | `impl Deserialize for Schema` | pub |
| Rust | `SchemaViolation { instance_path: string, message: string }` | pub |
| Python | `RuntimeError` | |
| Rust | `impl Display for SchemaViolation` | pub |
| Rust | `SchemaViolations(SchemaViolation[])` | pub |
| Python | `RuntimeError` | |
| Rust | `impl Deref for SchemaViolations` | pub |
| Rust | `impl Display for SchemaViolations` | pub |
| Rust | `impl Error for SchemaViolations` | pub |
| Rust | `SchemaParseError { message: string }` | pub |
| Python | `RuntimeError` | |
| Rust | `impl Display for SchemaParseError` | pub |
| Rust | `impl Error for SchemaParseError` | pub |
| Rust | `Node { types: JsonType[]?, enum_values: json[]?, const_value: json?, all_of: Node[]?, any_of: Node[]?, one_of: Node[]?, not: Node?, if_schema: Node?, then_schema: Node?, else_schema: Node?, properties: [string, Node][]?, required: string[]?, additional_properties_forbidden: boolean, items: Node?, min_items: number?, max_items: number?, minimum: number?, maximum: number?, min_length: number?, max_length: number?, pattern: regex::Regex? }` | private |
| Rust | `JsonType` | private |
| Rust | `JsonType.Object` | private |
| Rust | `JsonType.Array` | private |
| Rust | `JsonType.String` | private |
| Rust | `JsonType.Number` | private |
| Rust | `JsonType.Integer` | private |
| Rust | `JsonType.Boolean` | private |
| Rust | `JsonType.Null` | private |
| Rust | `JsonType.parse(s: string): JsonType?` | private |
| Rust | `JsonType.matches(value: json): boolean` | private |
| Rust | `JsonType.name(): string` | private |
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
| Rust | `Node.accepts(instance: json): boolean` | private |
| Rust | `Node.check_object(map: Record<string, json>, instance_path: string, out: SchemaViolation[]): void` | private |
| Rust | `Node.check_array(arr: json[], instance_path: string, out: SchemaViolation[]): void` | private |
| Rust | `Node.check_string(s: string, instance_path: string, out: SchemaViolation[]): void` | private |
| Rust | `Node.check_number(n: number, instance_path: string, out: SchemaViolation[]): void` | private |
| Rust | `Node.violation(instance_path: string, message: string, out: SchemaViolation[]): void` | private |
| Rust | `join_or(labels: string[]): string` | private |
| Rust | `retype_hint(types: JsonType[], instance: json): string?` | private |
| Rust | `Node.coerce(value: json, instance_path: string, out: string[]): void` | private |
| Rust | `Node.enum_candidate(value: json): json?` | private |
| Rust | `text_form(value: json): string` | private |
| Rust | `JsonType.retype(value: json): json?` | private |
| Rust | `retype_integer(text: string): json?` | private |
| Rust | `retype_number(text: string): json?` | private |
| Rust | `retype_boolean(text: string): json?` | private |
| Rust | `decode_json(text: string, fits: (value: json) => boolean): json?` | private |

## `crates/agentwerk/src/schemas/store.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `SchemaStore { entries: Record<string, Schema> }` | pub |
| Python | `SchemaStore` | |
| Rust | `SchemaStore.new(): SchemaStore` | pub |
| Python | `SchemaStore()` | |
| Rust | `SchemaStore.label(label: string, document: json): SchemaStore throws SchemaParseError` | pub |
| Python | `SchemaStore.label(label, document)`: raises on a document that is not a schema | |
| both | `SchemaStore.get(label: string): Schema?` | pub |
| Rust | `impl Debug for SchemaStore` | pub |

## `crates/agentwerk/src/tools/code.rs`

Not bound: it backs `grep`'s `syntax: "code"` shape matching.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run(files: [string, string][], query: Query, interrupt: boolean, deadline: Instant): ToolResult` | super |
| Rust | `for_each_file(files: [string, string][], interrupt: boolean, deadline: Instant, visit: (path: string, content: string) => void): void` | private |
| Rust | `line_and_byte_column(content: string, byte_offset: number): [number, number]` | private |
| Rust | `render_summary(substring: string): string` | private |
| Rust | `render_captures(captures: [Metavariable, Loc][]): string` | private |
| Rust | `truncate_to_chars(text: string, max_chars: number): string` | private |
| Rust | `parse_constraints(constraints: json, pattern: Pattern): [string, regex::Regex][] throws string` | private |
| Rust | `satisfies_constraints(found: Match, constraints: [string, regex::Regex][]): boolean` | private |

## `crates/agentwerk/src/tools/command/mod.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod parse`, `mod tool` | private |
| Rust | re-exports `CommandTool` | pub |

## `crates/agentwerk/src/tools/command/parse.rs`

Not bound: it is how `CommandTool` reads one command line.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `Refusal` | crate |
| Rust | `Refusal.OperatorFound(string)` | crate |
| Rust | `Refusal.Unterminated` | crate |
| Rust | `Refusal.ControlCharacterFound` | crate |
| Rust | `Refusal.Empty` | crate |
| Rust | `Command { program: string, arguments: string[] }` | crate |
| Rust | `Command.split(line: string): Command throws Refusal` | crate |
| Rust | `Command.flags(): [string, Argument][]` | super |
| Rust | `Command.program_path(dir: string): string` | crate |
| Rust | `Command.normalized(): string` | crate |
| Rust | `Argument` | super |
| Rust | `Argument.Escape` | super |
| Rust | `Argument.Long(string)` | super |
| Rust | `Argument.Short(string)` | super |
| Rust | `Argument.Operand` | super |
| Rust | `Argument.parse(argument: string): Argument` | super |
| Rust | `is_number(text: string): boolean` | private |
| Rust | `operator(c: string): boolean` | private |
| Rust | `Words { rest: string, word: string, quoted: boolean, words: string[] }` | private |
| Rust | `Words.new(line: string): Words` | private |
| Rust | `Words.run(): string[] throws Refusal` | private |
| Rust | `Words.quoted(quote: string): void throws Refusal` | private |
| Rust | `Words.escaped(): void throws Refusal` | private |
| Rust | `Words.end_word(): void` | private |
| Rust | `is_control(c: string): boolean` | private |

## `crates/agentwerk/src/tools/command/tool.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `DEFINITION: string` | private |
| Rust | `SCHEMA: string` | private |
| Rust | `CommandTool { tool_name: string, allow: string[], allow_flags: string[], deny: string[], deny_flags: DeniedFlag[], description: string, custom_description: boolean, concurrent: boolean }` | pub |
| Python | `CommandTool`: a class carrying the builder methods, where every other built-in tool is a function returning a handle | |
| Rust | `CommandTool.DEFAULT_TIMEOUT: number = 120000` | pub |
| Python | not bound | |
| Rust | `CommandTool.MAX_TIMEOUT: number = 600000` | pub |
| Python | not bound | |
| Rust | `CommandTool.new(name: string): CommandTool` | pub |
| Python | `CommandTool(name)` | |
| both | `CommandTool.allow(pattern: string): CommandTool` | pub |
| both | `CommandTool.allow_flag(flag: string): CommandTool` | pub |
| both | `CommandTool.deny(pattern: string): CommandTool` | pub |
| both | `CommandTool.deny_flag(flag: string): CommandTool` | pub |
| both | `CommandTool.description(description: string): CommandTool` | pub |
| both | `CommandTool.concurrent(concurrent: boolean): CommandTool` | pub |
| Rust | `CommandTool.render_description(): void` | private |
| Rust | `CommandTool.allowed_line(): string` | private |
| Rust | `CommandTool.check(line: string): Command throws string` | private |
| Rust | `CommandTool.unreadable(line: string, refusal: Refusal): string` | private |
| Rust | `CommandTool.allows_flag(found: Argument): boolean` | private |
| Rust | `CommandTool.denies_flag(found: Argument): boolean` | private |
| Rust | `DeniedFlag { written: string, key: FlagKey }` | private |
| Rust | `DeniedFlag.new(written: string): DeniedFlag` | private |
| Rust | `FlagKey` | private |
| Rust | `FlagKey.Long(string)` | private |
| Rust | `FlagKey.Letter(string)` | private |
| Rust | `FlagKey.Cluster(string)` | private |
| Rust | `flag_rule(method: string, flag: string): string` | private |
| Rust | `is_assignment(token: string): boolean` | private |
| Rust | `quoted(patterns: string[]): string` | private |
| Rust | `CommandArgs { command: string, timeout_ms: number? }` | pub |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `CommandTool.run(args: CommandArgs, ctx: ToolContext): Promise<ToolResult>` | private |
| Rust | `impl From<CommandTool> for Tool` | pub |
| Python | `CommandTool` converts when `Agent.tool(..)` receives it | |

## `crates/agentwerk/src/tools/edit_file.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `EditFileTool` | pub |
| Python | `EditFileTool()`: the unit struct converts to a `Tool`; Python spells the conversion as a call | |
| Rust | `EditFileArgs { path: string, old_string: string, new_string: string, replace_all: boolean }` | pub |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `impl From<EditFileTool> for Tool` | pub |
| Rust | `run(args: EditFileArgs, ctx: ToolContext): Promise<ToolResult>` | private |

## `crates/agentwerk/src/tools/fetch_url.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `MAX_URL_LENGTH: number = 2000` | private |
| Rust | `MAX_RESPONSE_BYTES: number = 10485760` | private |
| Rust | `DEFAULT_MAX_LENGTH: number = 100000` | private |
| Rust | `FETCH_TIMEOUT_SECS: number = 60` | private |
| Rust | `MAX_REDIRECT_HOPS: number = 10` | private |
| Rust | `FetchUrlTool` | pub |
| Python | `FetchUrlTool()` | |
| Rust | `FetchUrlArgs { url: string, max_length: number }` | pub |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `default_max_length(): number` | private |
| Rust | `impl From<FetchUrlTool> for Tool` | pub |
| Rust | `run(args: FetchUrlArgs, ctx: ToolContext): Promise<ToolResult>` | private |
| Rust | `FetchedContent` | private |
| Rust | `FetchedContent.Page { body: string, status: number, content_type: string, bytes: number }` | private |
| Rust | `FetchedContent.Redirect { original_url: string, redirect_url: string, status: number }` | private |
| Rust | `fetch_url(url: string): Promise<FetchedContent throws string>` | private |
| Rust | `format_output(url: string, body: string, status: number, content_type: string, bytes: number, max_length: number): string` | private |
| Rust | `FollowResult` | private |
| Rust | `FollowResult.Ok(reqwest::Response)` | private |
| Rust | `FollowResult.CrossDomain { original_url: string, redirect_url: string, status: number }` | private |
| Rust | `follow_safe_redirects(client: reqwest::Client, url: string): Promise<FollowResult throws string>` | private |
| Rust | `is_redirect(status: number): boolean` | private |
| Rust | `is_same_origin(original_url: string, redirect_url: string): boolean` | private |
| Rust | `UrlOrigin { scheme: string, host: string, port: string }` | private |
| Rust | `UrlOrigin.bare_host(): string` | private |
| Rust | `parse_origin(url: string): UrlOrigin?` | private |
| Rust | `resolve_redirect_location(base_url: string, location: string): string` | private |
| Rust | `validate_url(url: string): string throws string` | private |
| Rust | `strip_html(html: string): string` | private |
| Rust | `decode_html_entity(chars: string): string` | private |
| Rust | `resolve_named_entity(name: string): string` | private |
| Rust | `decode_numeric_entity(digits: string, radix: number, original: string): string` | private |
| Rust | `collapse_whitespace(text: string): string` | private |

## `crates/agentwerk/src/tools/glob.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `GlobTool` | pub |
| Python | `GlobTool()` | |
| Rust | `MAX_RESULTS: number = 200` | private |
| Rust | `GlobArgs { pattern: string, path: string }` | pub |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `here(): string` | private |
| Rust | `impl From<GlobTool> for Tool` | pub |
| Rust | `run(args: GlobArgs, ctx: ToolContext): Promise<ToolResult>` | private |
| Rust | `collect_matches(current: string, base: string, pattern_segments: string[], results: [string, SystemTime][]): void` | private |
| Rust | `glob_matches(pattern: string[], path: string[]): boolean` | private |
| Rust | `glob_match_recursive(pattern: string[], path: string[]): boolean` | private |
| Rust | `segment_matches(pattern: string, text: string): boolean` | private |
| Rust | `seg_match_recursive(pat: number[], txt: number[]): boolean` | private |

## `crates/agentwerk/src/tools/grep.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `GrepTool` | pub |
| Python | `GrepTool()` | |
| Rust | `DEFAULT_HEAD_LIMIT: number = 250` | private |
| Rust | `MAX_LINE_COLUMNS: number = 250` | super |
| Rust | `SEARCH_TIMEOUT: number = 180000` | private |
| Rust | `impl From<GrepTool> for Tool` | pub |
| Rust | `run(args: GrepArgs, ctx: ToolContext): Promise<ToolResult>` | private |
| Rust | `OutputMode` | pub |
| Python | not bound: the model sends `output_mode` as a string | |
| Rust | `OutputMode.Content` | pub |
| Rust | `OutputMode.FilesWithMatches` | pub |
| Rust | `OutputMode.Count` | pub |
| Rust | `Syntax` | pub |
| Python | not bound: the model sends `syntax` as a string | |
| Rust | `Syntax.Regex` | pub |
| Rust | `Syntax.Code` | pub |
| Rust | `OutputMode.name(): string` | private |
| Rust | `Query { pattern: string, path: string?, glob: string?, output_mode: OutputMode, before_context: number, after_context: number, line_numbers: boolean, case_insensitive: boolean, file_type: string?, head_limit: number, offset: number, multiline: boolean, syntax: Syntax, constraints: json }` | super |
| Rust | `Query.from_args(args: GrepArgs): Query` | private |
| Rust | `GrepArgs { pattern: string, path: string?, glob: string?, output_mode: OutputMode, context_before: number?, context_after: number?, context_both: number?, context: number?, line_numbers: boolean, case_insensitive: boolean, file_type: string?, head_limit: number, offset: number, multiline: boolean, syntax: Syntax, constraints: json }` | pub |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `yes(): boolean` | private |
| Rust | `default_head_limit(): number` | private |
| Rust | `search_corpus(dir: string, query: Query, interrupt: boolean, deadline: Instant): ToolResult` | private |
| Rust | `run_regex(files: [string, string][], query: Query, interrupt: boolean, deadline: Instant): ToolResult` | private |
| Rust | `collect_files(walk: ignore::Walk, dir: string, interrupt: boolean, deadline: Instant): [string, string][]` | private |
| Rust | `paginate(rows: T[], query: Query): [T[], boolean]` | private |
| Rust | `note_pagination(map: Record<string, json>, query: Query, truncated: boolean): void` | private |
| Rust | `object_result(map: Record<string, json>): ToolResult` | private |
| Rust | `render_content(text: string, query: Query): ToolResult` | super |
| Rust | `render_files(hits: string[], query: Query): ToolResult` | super |
| Rust | `render_count(rows: [string, number][], query: Query): ToolResult` | super |

## `crates/agentwerk/src/tools/knowledge.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `KnowledgeTool { store: Knowledge }` | pub |
| Python | `KnowledgeTool` | |
| Rust | `KnowledgeTool.new(store: Knowledge): KnowledgeTool` | pub |
| Python | `KnowledgeTool(store)` | |
| Rust | `failure_kind(error: KnowledgeError): KnowledgeFailureKind` | private |
| Rust | `usage_line(message: string, store: Knowledge): string` | private |
| Rust | `KnowledgeArgs` | pub |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `KnowledgeArgs.Write { slug: string, description: string, content: string }` | pub |
| Rust | `KnowledgeArgs.Read { slug: string }` | pub |
| Rust | `KnowledgeArgs.Remove { slug: string }` | pub |
| Rust | `KnowledgeArgs.List` | pub |
| Rust | `impl From<KnowledgeTool> for Tool` | pub |
| Rust | `run(store: Knowledge, args: KnowledgeArgs, ctx: ToolContext): ToolResult` | private |

## `crates/agentwerk/src/tools/list_directory.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ListDirectoryTool` | pub |
| Python | `ListDirectoryTool()` | |
| Rust | `ListDirectoryArgs { path: string, recursive: boolean }` | pub |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `here(): string` | private |
| Rust | `impl From<ListDirectoryTool> for Tool` | pub |
| Rust | `run(args: ListDirectoryArgs, ctx: ToolContext): Promise<ToolResult>` | private |
| Rust | `EntryInfo { display_name: string, kind: string, size: number? }` | private |
| Rust | `list_entries(dir: string, base: string, recursive: boolean): EntryInfo[] throws io::Error` | private |

## `crates/agentwerk/src/tools/mod.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod util` | crate |
| Rust | `mod tool`, `mod code`, `mod command`, `mod edit_file`, `mod fetch_url`, `mod glob`, `mod grep`, `mod knowledge`, `mod list_directory`, `mod read_file`, `mod tickets`, `mod write_file` | private |
| Rust | re-exports `Tool`, `ToolContext`, `ToolResult`, `CommandTool`, `EditFileTool`, `FetchUrlTool`, `GlobTool`, `GrepTool`, `KnowledgeTool`, `ListDirectoryTool`, `ReadFileTool`, `FinishTool`, `TicketsTool`, `WriteFileTool` | pub |

## `crates/agentwerk/src/tools/read_file.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `ReadFileTool` | pub |
| Python | `ReadFileTool()` | |
| Rust | `ReadFileArgs { path: string, offset: number, limit: number?, column: number?, length: number? }` | pub |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `first_line(): number` | private |
| Rust | `impl From<ReadFileTool> for Tool` | pub |
| Rust | `run(args: ReadFileArgs, ctx: ToolContext): Promise<ToolResult>` | private |
| Rust | `snap_to_char_boundary(s: string, pos: number): number` | private |

## `crates/agentwerk/src/tools/tickets/finish.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `NAME: string = "finish"` | private |
| Rust | `DEFINITION: string` | private |
| Rust | `SCHEMA: string` | private |
| Rust | `FinishTool` | pub |
| Python | `FinishTool()` | |
| Rust | `impl From<FinishTool> for Tool` | pub |
| Rust | `FinishTool.from_schema(schema: Schema?): Tool` | crate |
| Rust | `finish(input: json, ctx: ToolContext, schema: Schema?): ToolResult throws ToolResult` | private |
| Rust | `hand_over(ticket_queue: TicketQueue, input: json, parent_key: string, agent: string, result: json, schema: Schema?, handover: string): ToolResult throws ToolResult` | private |
| Rust | `control_string(input: json, key: string): string? throws ToolResult` | private |
| Rust | `mark_finished(ticket_queue: TicketQueue, key: string, agent: string): void throws ToolResult` | private |
| Rust | `apply_handover_templates(task: string, parent_key: string, result_path: string, result: string): string` | private |
| Rust | `append_parent_reference(body: string, parent_key: string, result_path: string): string` | private |
| Rust | `attach_result(ticket_queue: TicketQueue, key: string, result: json, schema: Schema?): [json, string[]] throws ToolResult` | private |

## `crates/agentwerk/src/tools/tickets/mod.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod finish`, `mod tickets` | private |
| Rust | re-exports `FinishTool`, `TicketsTool` | pub |
| Rust | `TicketsArgs` | pub |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `TicketsArgs.Ticket { key: string? }` | pub |
| Rust | `TicketsArgs.Result { key: string? }` | pub |
| Rust | `TicketsArgs.List { status: string?, label: string? }` | pub |
| Rust | `TicketsArgs.Search { query: string }` | pub |
| Rust | `TicketsArgs.Create { task: json, label: string? }` | pub |
| Rust | `TicketsArgs.Edit { key: string?, task: json?, label: string? }` | pub |
| Rust | `dispatch(args: TicketsArgs, ctx: ToolContext): ToolResult` | super |
| Rust | `resolve_key(ticket_queue: TicketQueue, key: string?, ctx: ToolContext): string throws ToolResult` | private |
| Rust | `resolve_current_key(ticket_queue: TicketQueue, ctx: ToolContext): string throws ToolResult` | super |
| Rust | `ticket_error_message(err: TicketError): string` | super |
| Rust | `render_ticket(t: Ticket): string` | private |
| Rust | `render_result(key: string, path: string, result: json): string` | private |
| Rust | `push_value(out: string, value: json): void` | private |
| Rust | `status_label(s: Status): string` | private |
| Rust | `parse_status_for_list(s: string): Status throws ToolResult` | private |
| Rust | `truncate_for_preview(s: string, max: number): string` | private |
| Rust | `SummaryRow = [string, string, Status, string?]` | private |
| Rust | `render_summary_list(tickets: SummaryRow[]): string` | private |
| Rust | `task_preview(task: json): string` | private |
| Rust | `action_ticket(ticket_queue: TicketQueue, key: string?, ctx: ToolContext): ToolResult` | private |
| Rust | `action_result(ticket_queue: TicketQueue, key: string?, ctx: ToolContext): ToolResult` | private |
| Rust | `action_list(ticket_queue: TicketQueue, status: string?, label: string?): ToolResult` | private |
| Rust | `action_search(ticket_queue: TicketQueue, query: string): ToolResult` | private |
| Rust | `action_create(ticket_queue: TicketQueue, task: json, label: string?, ctx: ToolContext): ToolResult` | private |
| Rust | `action_edit(ticket_queue: TicketQueue, key: string?, new_task: json?, new_label: string?, ctx: ToolContext): ToolResult` | private |

## `crates/agentwerk/src/tools/tickets/tickets.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `TicketsTool` | pub |
| Python | `TicketsTool()` | |
| Rust | `impl From<TicketsTool> for Tool` | pub |

## `crates/agentwerk/src/tools/tool.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `MAX_CONCURRENT_CALLS: number = 10` | private |
| Rust | `PER_TOOL_CAP: number = 50000` | private |
| Rust | `PER_TURN_CAP: number = 200000` | private |
| Rust | `PREVIEW_CHARS: number = 2000` | private |
| Rust | `ToolContext { dir: string, run: Run?, ticket_queue: TicketQueue?, agent_id: string?, ticket_key: string?, knowledge: Knowledge? }` | pub |
| Python | not bound: a `@tool` function receives its input as keyword arguments only | |
| Rust | `ToolContext.new(dir: string): ToolContext` | pub |
| Python | not bound | |
| Rust | `ToolContext.run(run: Run): ToolContext` | crate |
| Rust | `ToolContext.ticket_queue(queue: TicketQueue): ToolContext` | crate |
| Rust | `ToolContext.agent_id(name: string): ToolContext` | crate |
| Rust | `ToolContext.ticket_key(key: string): ToolContext` | crate |
| Rust | `ToolContext.knowledge(knowledge: Knowledge): ToolContext` | crate |
| Rust | `ToolContext.emit(kind: EventKind): void` | crate |
| Rust | `ToolContext.cancelled(): Promise<void>` | pub |
| Python | not bound | |
| Rust | `impl Debug for ToolContext` | pub |
| Rust | `ToolCall { id: string, name: string, input: json }` | pub |
| Python | not bound: a call reaches Python as the decorated function's arguments | |
| Rust | `ToolResult` | pub |
| Python | `ToolResult` | |
| Rust | `ToolResult.Success { content: string, offloaded: string?, repaired: string[] }` | pub |
| Rust | `ToolResult.Error { content: string, kind: ToolFailureKind }` | pub |
| both | `ToolResult.success(content: string): ToolResult` | pub |
| both | `ToolResult.error(content: string): ToolResult` | pub |
| Rust | `ToolResult.content(): string` | pub |
| Python | not bound | |
| Rust | `ToolResult.into_content(): string` | pub |
| Python | not bound | |
| Rust | `ToolRegistry { tools: Tool[] }` | crate |
| Rust | `impl Debug for ToolRegistry` | crate |
| Rust | `ToolRegistry.register(tool: Tool): void` | crate |
| Rust | `ToolRegistry.resolve(name: string): Tool throws string` | private |
| Rust | `ToolRegistry.get(name: string): Tool?` | crate |
| Rust | `ToolRegistry.names(): string[]` | private |
| Rust | `ToolRegistry.tools(): Tool[]` | crate |
| Rust | `ToolRegistry.execute(calls: ToolCall[], ctx: ToolContext): Promise<ToolResult[]>` | crate |
| Rust | `ToolRegistry.run_concurrently(batch: [number, ToolCall][], ctx: ToolContext, semaphore: tokio::sync::Semaphore): Promise<[number, ToolResult][]>` | private |
| Rust | `ToolBatch` | private |
| Rust | `ToolBatch.Concurrent([number, ToolCall][])` | private |
| Rust | `ToolBatch.Serial(number, ToolCall)` | private |
| Rust | `partition_tool_calls(calls: ToolCall[], registry: ToolRegistry): ToolBatch[]` | private |
| Rust | `answer_every_call(calls: ToolCall[], answers: ToolResult?[]): ToolResult[]` | private |
| Rust | `lookup_key(name: string): string` | private |
| Rust | `ToolHandler = (input: json, ctx: ToolContext) => Promise<ToolResult>` | private |
| Rust | `ToolBuilder<D, H> { name: string, description: D, schema: Schema, concurrent: boolean, paths: string[], handler: H }` | pub |
| Python | folded into the `@tool` decorator: the type changes as the description and handler are attached, which Python cannot hold across calls | |
| Rust | `Tool { name: string, description: string, schema: Schema, concurrent: boolean, paths: string[], handler: ToolHandler }` | pub |
| Python | `Tool`: an opaque handle the built-in tool functions return. An ad-hoc tool is a decorated function, not a `Tool` | |
| Rust | `impl Debug for Tool` | pub |
| Rust | `Tool.new(name: string): ToolBuilder` | pub |
| Python | the `@tool` decorator: a decorated function carries the name, description, and schema | |
| Rust | `Tool.call(input: json, ctx: ToolContext): Promise<ToolResult>` | pub |
| Python | not bound: the loop calls the decorated function | |
| Rust | `Tool.name(): string` | pub |
| Python | not bound | |
| Rust | `Tool.description(): string` | pub |
| Python | not bound | |
| Rust | `Tool.input_schema(): Schema` | pub |
| Python | not bound | |
| Rust | `Tool.is_concurrent(): boolean` | pub |
| Python | not bound | |
| Rust | `Tool.opened_paths(input: json): string[]` | pub |
| Python | not bound | |
| Rust | `ToolBuilder.schema(schema: json): ToolBuilder` | pub |
| Python | `@tool(schema=..)`: raises `ValueError` when `.tool(fn)` registers it, one call later than the Rust panic | |
| Rust | `ToolBuilder.concurrent(concurrent: boolean): ToolBuilder` | pub |
| Python | `@tool(concurrent=..)` | |
| Rust | `ToolBuilder.paths(fields: string[]): ToolBuilder` | pub |
| Python | `@tool(paths=[..])` | |
| Rust | `ToolBuilder.description(description: string): ToolBuilder` | pub |
| Python | `@tool(description=..)`, defaulting to the decorated function's docstring | |
| Rust | `ToolBuilder.handler(handler: (input: json, ctx: ToolContext) => Promise<ToolResult>): ToolBuilder` | pub |
| Python | the decorated function itself | |
| Rust | `ToolBuilder.build(): Tool` | pub |
| Python | not bound: the decorator builds the tool | |
| Rust | `read_arguments_then(name: string, handler: (input: json, ctx: ToolContext) => Promise<ToolResult>): ToolHandler` | private |
| Rust | `invoke(resolved: Tool throws string, call: ToolCall, ctx: ToolContext): Promise<ToolResult>` | private |
| Rust | `retype_message(pointer: string): string` | crate |
| Rust | `cap_results(calls: ToolCall[], results: ToolResult[], ctx: ToolContext): void` | private |
| Rust | `replace_empty_output(result: ToolResult, tool_name: string): void` | private |
| Rust | `cap_oversized_result(result: ToolResult, ctx: ToolContext, call_id: string, per_tool_cap: number): void` | private |
| Rust | `cap_aggregate_outputs(calls: ToolCall[], results: ToolResult[], ctx: ToolContext, per_turn_cap: number): void` | private |
| Rust | `largest_inline_success(calls: ToolCall[], results: ToolResult[]): [ToolCall, string, string?]?` | private |
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

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `run_command(command: Command, timeout: number, ctx: ToolContext): Promise<ToolResult>` | crate |
| Rust | `glob_match(pattern: string, text: string): boolean` | crate |
| Rust | `glob_match_bytes(pattern: number[], text: number[]): boolean` | private |
| Rust | `MAX_DIR_ENTRIES: number = 100` | crate |
| Rust | `directory_entries(dir: string): string?` | crate |
| Rust | `nearest_existing_dir(path: string): string?` | private |
| Rust | `not_found_hint(ctx_dir: string, resolved: string): string` | crate |
| Rust | `suggest_path(ctx_dir: string, resolved: string): string?` | private |

## `crates/agentwerk/src/tools/write_file.rs`

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `WriteFileTool` | pub |
| Python | `WriteFileTool()` | |
| Rust | `WriteFileArgs { path: string, content: string }` | pub |
| Python | not bound: the model sends these fields as the tool's input | |
| Rust | `impl From<WriteFileTool> for Tool` | pub |
| Rust | `run(args: WriteFileArgs, ctx: ToolContext): Promise<ToolResult>` | private |

## `crates/agentwerk-py/src/agent.rs`

Binds `agents/agent.rs`, whose section holds the Python spelling of each method.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyAgent { role: string?, label: string?, templates: [string, string][], dir: string?, interactive: boolean, provider: Provider?, model: Model?, tools: Tool[], knowledge: Knowledge?, agent: Agent? }` | python |
| Rust | `PyAgent.create(): PyAgent` | private |
| Rust | `PyAgent.built(): Agent throws PyErr` | crate |
| Rust | `PyAgent.ensure_unbuilt(): void throws PyErr` | private |
| Rust | `PyAgent.assemble(): Agent throws PyErr` | private |
| Rust | `PyAgent.new(): PyAgent` | python |
| Rust | `PyAgent.from_env(): PyAgent throws PyErr` | python |
| Rust | `PyAgent.provider(provider: PyProvider): PyAgent throws PyErr` | python |
| Rust | `PyAgent.model(model: any): PyAgent throws PyErr` | python |
| Rust | `PyAgent.role(role: string): PyAgent throws PyErr` | python |
| Rust | `PyAgent.label(label: string): PyAgent throws PyErr` | python |
| Rust | `PyAgent.id(): string throws PyErr` | python |
| Rust | `PyAgent.interactive(): PyAgent throws PyErr` | python |
| Rust | `PyAgent.template(key: string, value: string): PyAgent throws PyErr` | python |
| Rust | `PyAgent.templates(variables: Record<string, string>): PyAgent throws PyErr` | python |
| Rust | `PyAgent.dir(dir: string): PyAgent throws PyErr` | python |
| Rust | `PyAgent.knowledge(store: PyKnowledge): PyAgent throws PyErr` | python |
| Rust | `PyAgent.tool(tool: any): PyAgent throws PyErr` | python |
| Rust | `PyAgent.tools(tools: any): PyAgent throws PyErr` | python |
| Rust | `PyAgent.build(): PyAgent throws PyErr` | python |
| Rust | `PyAgent.task(task: any): string throws PyErr` | python |
| Rust | `PyAgent.ticket(ticket: PyTicket): string throws PyErr` | python |
| Rust | `PyAgent.start(): PyTicketQueue throws PyErr` | python |

## `crates/agentwerk-py/src/compaction.rs`

Binds `agents/compaction.rs`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyCompaction { inner: Compaction }` | python |
| Rust | `PyCompaction.reason(): string` | python |
| Rust | `PyCompaction.ticket(): PyTicket` | python |
| Rust | `PyCompaction.window(): number?` | python |
| Rust | `PyCompaction.summarize(replies: PyReply[]): Promise<string> throws PyErr` | python |
| Rust | `into_replies(replies: PyReply[]): Reply[]` | private |
| Rust | `invoke_editor(py: Python, editor: any, compaction: Compaction, replies: Reply[]): Reply[]? throws PyErr` | crate |

## `crates/agentwerk-py/src/convert.rs`

Not bound: the one JSON boundary between the two languages.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `value_to_py(py: Python, value: json): any throws PyErr` | pub |
| Rust | `py_to_value(obj: any): json throws PyErr` | pub |
| Rust | `runtime_error(message: string): PyErr` | pub |

## `crates/agentwerk-py/src/event.rs`

Binds `event.rs`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `event_names(): string[]` | python |
| Rust | `PyEvent { kind: string, created_at: number, agent_id: string, ticket_key: string, label: string?, data: json }` | python |
| Rust | `PyEvent.data(): any throws PyErr` | python |
| Rust | `PyEvent.__repr__(): string` | python |
| Rust | `to_py_event(event: Event): PyEvent` | pub |
| Rust | `payload(kind: EventKind): json` | private |

## `crates/agentwerk-py/src/knowledge.rs`

Binds `agents/knowledge.rs`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyKnowledge { inner: Knowledge }` | python |
| Rust | `PyKnowledge.load(store_dir: string): PyKnowledge throws PyErr` | python |
| Rust | `PyKnowledge.index_char_limit(count: number): PyKnowledge` | python |
| Rust | `PyKnowledge.get_index_char_limit(): number` | python |
| Rust | `PyKnowledge.index(): string` | python |
| Rust | `PyKnowledge.pages(): PyPages` | python |
| Rust | `PyKnowledge.clear(): void throws PyErr` | python |
| Rust | `PyPages { store: Knowledge }` | python |
| Rust | `PyPages.pages(): Pages` | private |
| Rust | `PyPages.save(page: PyPage): void throws PyErr` | python |
| Rust | `PyPages.load(slug: string): PyPage throws PyErr` | python |
| Rust | `PyPages.list(): PyPage[] throws PyErr` | python |
| Rust | `PyPages.remove(slug: string): void throws PyErr` | python |
| Rust | `PyPage { inner: Page }` | python |
| Rust | `PyPage.to_page(): Page` | private |
| Rust | `PyPage.new(slug: string, description: string, content: string, kind: string, tags: string[]?): PyPage` | python |
| Rust | `PyPage.slug(): string` | python |
| Rust | `PyPage.kind(): string` | python |
| Rust | `PyPage.description(): string` | python |
| Rust | `PyPage.content(): string` | python |
| Rust | `PyPage.tags(): string[]` | python |
| Rust | `PyPage.__repr__(): string` | python |

## `crates/agentwerk-py/src/lib.rs`

Registers every bound class and function in the `_agentwerk` module.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `mod agent`, `mod compaction`, `mod convert`, `mod event`, `mod knowledge`, `mod providers`, `mod reply`, `mod schema`, `mod ticket`, `mod ticket_queue`, `mod tools`, `mod trajectory` | private |
| Rust | `_agentwerk(m: PyModule): void throws PyErr` | python |

## `crates/agentwerk-py/src/providers.rs`

Binds `providers/`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyProvider { inner: Provider }` | python |
| Rust | `PyProvider.from_env(): PyProvider throws PyErr` | python |
| Rust | `PyModel { inner: Model }` | python |
| Rust | `PyModel.new(name: string): PyModel` | python |
| Rust | `PyModel.from_env(): PyModel throws PyErr` | python |
| Rust | `PyModel.name(): string` | python |
| Rust | `PyModel.context_window(size: number): PyModel` | python |
| Rust | `PyModel.reasoning_effort(effort: string): PyModel throws PyErr` | python |
| Rust | `PyModel.get_context_window(): number?` | python |
| Rust | `PyModel.get_reasoning_effort(): string` | python |
| Rust | `anthropic_provider(api_key: string, base_url: string?, timeout: number?): PyProvider` | python |
| Rust | `openai_provider(api_key: string, base_url: string?, timeout: number?): PyProvider` | python |
| Rust | `mistral_provider(api_key: string, base_url: string?, timeout: number?): PyProvider` | python |
| Rust | `litellm_provider(api_key: string, base_url: string?, timeout: number?): PyProvider` | python |
| Rust | `register(m: PyModule): void throws PyErr` | pub |

## `crates/agentwerk-py/src/reply.rs`

Binds `agents/tickets/reply.rs`, and owns the two reply converters the editors on `TicketQueue` use.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyReply { inner: Reply }` | python |
| Rust | `PyReply.user_text(text: string): PyReply` | python |
| Rust | `PyReply.author(): string` | python |
| Rust | `PyReply.content(): PyReplyContent[]` | python |
| Rust | `PyReply.created_at(): number` | python |
| Rust | `PyReply.__repr__(): string` | python |
| Rust | `PyReplyContent { inner: ReplyContent }` | python |
| Rust | `PyReplyContent.text(text: string): PyReplyContent` | python |
| Rust | `PyReplyContent.tool_use(id: string, name: string, input: any): PyReplyContent throws PyErr` | python |
| Rust | `PyReplyContent.tool_result(tool_use_id: string, content: string, succeeded: boolean, path: string?): PyReplyContent` | python |
| Rust | `PyReplyContent.thinking(thinking: string, signature: string): PyReplyContent` | python |
| Rust | `PyReplyContent.redacted_thinking(data: string): PyReplyContent` | python |
| Rust | `PyReplyContent.kind(): string` | python |
| Rust | `PyReplyContent.data(): any throws PyErr` | python |
| Rust | `PyReplyContent.__repr__(): string` | python |
| Rust | `replies_to_py(replies: Reply[]): PyReply[]` | crate |
| Rust | `py_to_replies(obj: any): Reply[] throws PyErr` | crate |

## `crates/agentwerk-py/src/schema.rs`

Binds `schemas/`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PySchema { inner: Schema }` | python |
| Rust | `PySchema.new(document: any): PySchema throws PyErr` | python |
| Rust | `PySchema.validate(value: any): [any, string[]] throws PyErr` | python |
| Rust | `PySchemaStore { inner: SchemaStore }` | python |
| Rust | `PySchemaStore.new(): PySchemaStore` | python |
| Rust | `PySchemaStore.label(label: string, document: any): PySchemaStore throws PyErr` | python |
| Rust | `PySchemaStore.get(label: string): PySchema?` | python |

## `crates/agentwerk-py/src/ticket.rs`

Binds `agents/tickets/ticket.rs`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyTicket { inner: Ticket }` | python |
| Rust | `PyTicket.new(task: any, label: string?, schema: PySchema?, parent: string?): PyTicket throws PyErr` | python |
| Rust | `PyTicket.has_label(label: string): boolean` | python |
| Rust | `PyTicket.is_todo(): boolean` | python |
| Rust | `PyTicket.is_finished(): boolean` | python |
| Rust | `PyTicket.is_failed(): boolean` | python |
| Rust | `PyTicket.is_in_progress(): boolean` | python |
| Rust | `PyTicket.is_pending(): boolean` | python |
| Rust | `PyTicket.key(): string` | python |
| Rust | `PyTicket.status(): string` | python |
| Rust | `PyTicket.task(): any throws PyErr` | python |
| Rust | `PyTicket.result(): any? throws PyErr` | python |
| Rust | `PyTicket.label(): string?` | python |
| Rust | `PyTicket.schema(): PySchema?` | python |
| Rust | `PyTicket.parent(): string?` | python |
| Rust | `PyTicket.reporter(): string` | python |
| Rust | `PyTicket.assignee(): string?` | python |
| Rust | `PyTicket.created_at(): number` | python |
| Rust | `PyTicket.started_at(): number?` | python |
| Rust | `PyTicket.finished_at(): number?` | python |
| Rust | `PyTicket.failed_at(): number?` | python |
| Rust | `PyTicket.replies(): PyReply[]` | python |
| Rust | `PyTicket.__repr__(): string` | python |
| Rust | `PyTicket.from_ticket(ticket: Ticket): PyTicket` | pub |
| Rust | `PyTicket.to_ticket(): Ticket` | pub |

## `crates/agentwerk-py/src/ticket_queue.rs`

Binds `agents/tickets/ticket_queue.rs` and `store.rs`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyTicketQueue { inner: TicketQueue }` | python |
| Rust | `PyTicketQueue.new(): PyTicketQueue` | python |
| Rust | `PyTicketQueue.load(tickets_dir: string): PyTicketQueue throws PyErr` | python |
| Rust | `PyTicketQueue.agent(agent: PyAgent): PyTicketQueue throws PyErr` | python |
| Rust | `PyTicketQueue.task(task: any): string throws PyErr` | python |
| Rust | `PyTicketQueue.ticket(ticket: PyTicket): string` | python |
| Rust | `PyTicketQueue.reply(key: string, content: string): PyTicketQueue` | python |
| Rust | `PyTicketQueue.set_finished(key: string, result: any): void throws PyErr` | python |
| Rust | `PyTicketQueue.set_failed(key: string): void throws PyErr` | python |
| Rust | `PyTicketQueue.max_turns(count: number): PyTicketQueue` | python |
| Rust | `PyTicketQueue.max_input_tokens(count: number): PyTicketQueue` | python |
| Rust | `PyTicketQueue.max_output_tokens(count: number): PyTicketQueue` | python |
| Rust | `PyTicketQueue.max_request_tokens(count: number): PyTicketQueue` | python |
| Rust | `PyTicketQueue.max_schema_retries(count: number): PyTicketQueue` | python |
| Rust | `PyTicketQueue.max_request_retries(count: number): PyTicketQueue` | python |
| Rust | `PyTicketQueue.max_time(seconds: number): PyTicketQueue` | python |
| Rust | `PyTicketQueue.compact_at(fraction: number): PyTicketQueue` | python |
| Rust | `PyTicketQueue.request_retry_delay(seconds: number): PyTicketQueue` | python |
| Rust | `PyTicketQueue.get_max_turns(): number?` | python |
| Rust | `PyTicketQueue.get_max_input_tokens(): number?` | python |
| Rust | `PyTicketQueue.get_max_output_tokens(): number?` | python |
| Rust | `PyTicketQueue.get_max_request_tokens(): number?` | python |
| Rust | `PyTicketQueue.get_max_schema_retries(): number?` | python |
| Rust | `PyTicketQueue.get_max_request_retries(): number` | python |
| Rust | `PyTicketQueue.get_max_time(): number?` | python |
| Rust | `PyTicketQueue.get_compact_at(): number?` | python |
| Rust | `PyTicketQueue.get_request_retry_delay(): number` | python |
| Rust | `PyTicketQueue.dir(dir: string): PyTicketQueue` | python |
| Rust | `PyTicketQueue.get_dir(): string` | python |
| Rust | `PyTicketQueue.schemas(store: PySchemaStore): PyTicketQueue` | python |
| Rust | `PyTicketQueue.on_event(handler: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.on_result(handler: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.on_result_async(handler: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.on_results_async(handler: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.on_results(handler: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.on_failure(handler: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.create_ticket_on_event(make: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.create_ticket_on_result(make: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.create_tickets_on_results(make: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.create_ticket_on_failure(make: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.model_for_agent(agent_id: string): string?` | python |
| Rust | `PyTicketQueue.get_ticket(key: string): PyTicket? throws PyErr` | python |
| Rust | `PyTicketQueue.tickets(): PyTicket[] throws PyErr` | python |
| Rust | `PyTicketQueue.find_tickets(predicate: any): PyTicket[] throws PyErr` | python |
| Rust | `PyTicketQueue.find_ticket(predicate: any): PyTicket? throws PyErr` | python |
| Rust | `PyTicketQueue.on_ticket(handler: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.edit_replies_on_event(editor: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.edit_replies_on_compaction(editor: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.edit_directive_on_retry(editor: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.edit_replies(key: string, editor: any): PyTicketQueue throws PyErr` | python |
| Rust | `PyTicketQueue.start(): PyTicketQueue` | python |
| Rust | `PyTicketQueue.finish(matches: any): Promise<any[]> throws PyErr` | python |
| Rust | `PyTicketQueue.finish_all(): Promise<any[]> throws PyErr` | python |
| Rust | `PyTicketQueue.finish_last(): Promise<any?> throws PyErr` | python |
| Rust | `PyTicketQueue.finish_reason(): string?` | python |
| Rust | `PyTicketQueue.cancel(matches: any): PyTicketQueue` | python |
| Rust | `PyTicketQueue.cancel_all(): PyTicketQueue` | python |
| Rust | `PyTicketQueue.is_cancelled(ticket: PyTicket): boolean` | python |
| Rust | `PyTicketQueue.find_events(predicate: any): PyEvent[]` | python |
| Rust | `PyTicketQueue.find_event(predicate: any): PyEvent?` | python |
| Rust | `PyTicketQueue.input_tokens(): number` | python |
| Rust | `PyTicketQueue.output_tokens(): number` | python |
| Rust | `PyTicketQueue.execution_duration(): number?` | python |
| Rust | `PyTicketQueue.results(): any[] throws PyErr` | python |
| Rust | `call_with_result(py: Python, callable: any, ticket: Ticket, result: json): any throws PyErr` | private |
| Rust | `call_with_ticket(py: Python, callable: any, event: Event, ticket: Ticket): any throws PyErr` | private |
| Rust | `call_with_results(py: Python, callable: any, results: json[]): any throws PyErr` | private |
| Rust | `built_ticket(produced: any): Ticket?` | private |
| Rust | `built_tickets(produced: any): Ticket[]` | private |
| Rust | `event_predicate(predicate: any, event: Event): boolean` | private |
| Rust | `ticket_predicate(predicate: any, ticket: Ticket): boolean` | private |

## `crates/agentwerk-py/src/tools.rs`

Binds `tools/`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyTool { inner: Tool }` | python |
| Rust | `invoke_python(py: Python, func: any, input: json): ToolResult throws PyErr` | private |
| Rust | `PyToolResult { inner: ToolResult }` | python |
| Rust | `PyToolResult.success(content: string): PyToolResult` | python |
| Rust | `PyToolResult.error(content: string): PyToolResult` | python |
| Rust | `extract_tool(obj: any): Tool throws PyErr` | pub |
| Rust | `handle(inner: Tool): PyTool` | private |
| Rust | `read_file_tool(): PyTool` | python |
| Rust | `write_file_tool(): PyTool` | python |
| Rust | `edit_file_tool(): PyTool` | python |
| Rust | `grep_tool(): PyTool` | python |
| Rust | `glob_tool(): PyTool` | python |
| Rust | `list_directory_tool(): PyTool` | python |
| Rust | `fetch_url_tool(): PyTool` | python |
| Rust | `knowledge_tool(store: PyKnowledge): PyTool` | python |
| Rust | `finish_tool(): PyTool` | python |
| Rust | `tickets_tool(): PyTool` | python |
| Rust | `PyCommandTool { inner: CommandTool }` | python |
| Rust | `PyCommandTool.new(name: string): PyCommandTool` | python |
| Rust | `PyCommandTool.allow(pattern: string): PyCommandTool` | python |
| Rust | `PyCommandTool.allow_flag(flag: string): PyCommandTool` | python |
| Rust | `PyCommandTool.deny(pattern: string): PyCommandTool` | python |
| Rust | `PyCommandTool.deny_flag(flag: string): PyCommandTool` | python |
| Rust | `PyCommandTool.description(description: string): PyCommandTool` | python |
| Rust | `PyCommandTool.concurrent(concurrent: boolean): PyCommandTool` | python |
| Rust | `register(m: PyModule): void throws PyErr` | pub |

## `crates/agentwerk-py/src/trajectory.rs`

Binds `agents/tickets/trajectory.rs`.

| Language | Item | Visibility |
|----------|------|------------|
| Rust | `PyTrajectory { inner: Trajectory }` | python |
| Rust | `PyTrajectory.from_ticket(agent_id: string, model: string?, ticket: PyTicket): PyTrajectory` | python |
| Rust | `PyTrajectory.save(dir: string): void throws PyErr` | python |
| Rust | `PyTrajectory.key(): string` | python |
| Rust | `PyTrajectory.model(): string?` | python |
| Rust | `PyTrajectory.replies(): PyReply[]` | python |
| Rust | `PyTrajectory.__repr__(): string` | python |
