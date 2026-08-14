# The Rust API next to the Python one

## Type conversions

Seven rules the surface table below never repeats.

| Rust | Python |
|------|--------|
| `tickets.max_time(Duration::from_secs(30))` | `tickets.max_time(30.0)`: every `Duration` becomes float seconds, under the same parameter name. |
| `ticket.status == Status::InProgress` | `ticket.status == "in_progress"`: every enum becomes its lowercase string. |
| `Err(TicketError::TicketMissing { key })` | `RuntimeError("Ticket TICKET-1 not found")`: every error type collapses to one exception. |
| `agent.knowledge(&store)` where `store: Arc<Knowledge>` | `agent.knowledge(store)`: every `Arc<T>` becomes a plain object, shared by passing it to several agents. |
| `tickets.finish(..).await` | `await tickets.finish(..)`: every `async fn` becomes awaitable. |
| `agent.task(MyTask { goal, url })` | `agent.task({"goal": .., "url": ..})`: an argument Rust takes by `Serialize` takes any JSON-serializable value. |
| `ticket.result -> Option<serde_json::Value>` | `ticket.result`: a JSON value becomes a dict, list, or scalar. |

## The full surface

| Rust | Python |
|------|--------|
| **Agent** | |
| `Agent::new() -> AgentBuilder<(), ()>` | `Agent()` |
| `Agent::from_env() -> AgentBuilder<Provider, Model>` | `Agent.from_env()`: raises `RuntimeError` where Rust panics. |
| `Agent::from_dot_env() -> AgentBuilder<Provider, Model>` | `Agent.from_dot_env()`: raises `RuntimeError` where Rust panics. |
| `AgentBuilder<P, M>` | Folded into `Agent`: the type changes as the provider and model slots fill, which Python cannot hold across calls. |
| `AgentBuilder::provider(p)` | `Agent.provider(provider)` |
| `AgentBuilder::model(m)` | `Agent.model(model)` |
| `AgentBuilder::role(r)` | `Agent.role(role)` |
| `AgentBuilder::label(l)` | `Agent.label(label)` |
| `Agent::get_id()` | `Agent.id`: a property, and a `RuntimeError` before `build()`. |
| `AgentBuilder::interactive()` | `Agent.interactive()` |
| `AgentBuilder::template(key, value)` | `Agent.template(key, value)` |
| `AgentBuilder::templates(vars)` | `Agent.templates(variables)`: a mapping, so the bulk bind applies in key order where Rust preserves insertion order. |
| `AgentBuilder::tool(t)` | `Agent.tool(tool)` |
| `AgentBuilder::tools(iter)` | `Agent.tools(tools)` |
| `AgentBuilder::dir(p)` | `Agent.dir(dir)` |
| `AgentBuilder::knowledge(store)` | `Agent.knowledge(store)` |
| `AgentBuilder::build(self) -> Agent` | `Agent.build() -> Agent`: returns the same object, armed. Configuring after it, or building twice, raises. |
| `Agent::task(task) -> String` | `Agent.task(task) -> str` |
| `Agent::ticket(ticket) -> String` | `Agent.ticket(ticket) -> str` |
| `Agent::start() -> Arc<TicketQueue>` | `Agent.start()` |
| **TicketQueue** | |
| `TicketQueue::new() -> Arc<Self>` | `TicketQueue()` |
| `TicketQueue::load(dir)` | `TicketQueue.load(dir)` |
| `TicketQueue::agent(a)` | `TicketQueue.agent(agent)` |
| `TicketQueue::task(task)` | `TicketQueue.task(task)` |
| `TicketQueue::ticket(t)` | `TicketQueue.ticket(ticket)` |
| `TicketQueue::reply(key, content)` | `TicketQueue.reply(key, content)` |
| `TicketQueue::set_finished(key, result)` | `TicketQueue.set_finished(key, result)` |
| `TicketQueue::set_failed(key)` | `TicketQueue.set_failed(key)` |
| `TicketQueue::max_turns(n)` | `TicketQueue.max_turns(n)` |
| `TicketQueue::max_input_tokens(n)` | `TicketQueue.max_input_tokens(n)` |
| `TicketQueue::max_output_tokens(n)` | `TicketQueue.max_output_tokens(n)` |
| `TicketQueue::max_request_tokens(n)` | `TicketQueue.max_request_tokens(n)` |
| `TicketQueue::max_schema_retries(n)` | `TicketQueue.max_schema_retries(n)` |
| `TicketQueue::max_request_retries(n)` | `TicketQueue.max_request_retries(n)` |
| `TicketQueue::max_time(d)` | `TicketQueue.max_time(seconds)` |
| `TicketQueue::request_retry_delay(d)` | `TicketQueue.request_retry_delay(seconds)` |
| `TicketQueue::compact_at(fraction)` | `TicketQueue.compact_at(fraction)` |
| `TicketQueue::get_max_turns()` | `TicketQueue.get_max_turns()` |
| `TicketQueue::get_max_input_tokens()` | `TicketQueue.get_max_input_tokens()` |
| `TicketQueue::get_max_output_tokens()` | `TicketQueue.get_max_output_tokens()` |
| `TicketQueue::get_max_request_tokens()` | `TicketQueue.get_max_request_tokens()` |
| `TicketQueue::get_max_schema_retries()` | `TicketQueue.get_max_schema_retries()` |
| `TicketQueue::get_max_request_retries()` | `TicketQueue.get_max_request_retries()` |
| `TicketQueue::get_max_time()` | `TicketQueue.get_max_time()` |
| `TicketQueue::get_request_retry_delay()` | `TicketQueue.get_request_retry_delay()` |
| `TicketQueue::get_compact_at()` | `TicketQueue.get_compact_at()` |
| `TicketQueue::dir(dir)` | `TicketQueue.dir(dir)` |
| `TicketQueue::get_dir()` | `TicketQueue.get_dir()` |
| `TicketQueue::schemas(&store)` | `TicketQueue.schemas(store)` |
| `TicketQueue::on_event(h)` | `TicketQueue.on_event(callback)` |
| `TicketQueue::on_result(handler)` | `TicketQueue.on_result(callback)` |
| `TicketQueue::on_results(handler)` | `TicketQueue.on_results(callback)`: the results arrive as a list. |
| `TicketQueue::on_failure(handler)` | `TicketQueue.on_failure(callback)` |
| `TicketQueue::on_ticket(handler)` | `TicketQueue.on_ticket(callback)` |
| `TicketQueue::create_ticket_on_event(make)` | `TicketQueue.create_ticket_on_event(make)` |
| `TicketQueue::create_ticket_on_result(make)` | `TicketQueue.create_ticket_on_result(make)` |
| `TicketQueue::create_tickets_on_results(make)` | `TicketQueue.create_tickets_on_results(make)`: hand back any sequence of tickets. `None` adds nothing, where Rust has only the empty `Vec`. |
| `TicketQueue::create_ticket_on_failure(make)` | `TicketQueue.create_ticket_on_failure(make)` |
| `TicketQueue::edit_replies(key, edit)` | `TicketQueue.edit_replies(key, editor)`: the editor returns the new list, or `None` to keep the old one, where Rust mutates in place. An editor that raises, or returns anything but `Reply` objects, raises here. |
| `TicketQueue::edit_replies_on_event(editor)` | `TicketQueue.edit_replies_on_event(editor)`: same return-instead-of-mutate shape. An editor that raises prints its traceback and changes nothing: it runs on an agent thread with no Python frame to raise into. |
| `TicketQueue::edit_replies_on_compaction(editor)` | `TicketQueue.edit_replies_on_compaction(editor)`: the editor returns the new list, or `None` to keep the current one, where Rust returns a `Result`. Define it with `async def` to await `Compaction.summarize`; a coroutine is driven on a worker thread of its own. An editor that raises prints its traceback and changes nothing, like the event editor. |
| `TicketQueue::edit_directive_on_retry(editor)` | `TicketQueue.edit_directive_on_retry(editor)`: the editor returns the replacement, or `None` to keep the default, where Rust rewrites in place. An editor that raises prints its traceback and changes nothing, like the reply editors. |
| `TicketQueue::model_for_agent(agent_id)` | `TicketQueue.model_for_agent(agent_id)` |
| `TicketQueue::get_ticket(key)` | `TicketQueue.get_ticket(key)` |
| `TicketQueue::tickets()` | `TicketQueue.tickets()` |
| `TicketQueue::find_tickets(predicate)` | `TicketQueue.find_tickets(predicate)` |
| `TicketQueue::find_ticket(predicate)` | `TicketQueue.find_ticket(predicate)` |
| `TicketQueue::start()` | `TicketQueue.start()` |
| `TicketQueue::finish(matches).await` | `await TicketQueue.finish(matches)`: the results become a list. |
| `TicketQueue::finish_all().await` | `await TicketQueue.finish_all()`: the results become a list. |
| `TicketQueue::get_finish_reason()` | `TicketQueue.get_finish_reason()`: the `FinishReason` becomes the string it prints as, such as `policy_violated(turns)`. |
| `TicketQueue::cancel(matches)` | `TicketQueue.cancel(matches)` |
| `TicketQueue::cancel_all()` | `TicketQueue.cancel_all()` |
| `TicketQueue::is_cancelled(ticket)` | `TicketQueue.is_cancelled(ticket)` |
| `TicketQueue::stats()` | `TicketQueue.stats()` |
| `TicketQueue::results()` | `TicketQueue.results()` |
| **Ticket** | |
| `Ticket::new(task)` | `Ticket(task)` |
| `Ticket::label(l)` | `Ticket(task, label=l)` |
| `Ticket::schema(s)` | `Ticket(task, schema=s)` |
| `Ticket::parent(key)` | `Ticket(task, parent=key)` |
| `Ticket::has_label(label)` | `Ticket.has_label(label)` |
| `Ticket::is_todo()` | `Ticket.is_todo()` |
| `Ticket::is_in_progress()` | `Ticket.is_in_progress()` |
| `Ticket::is_finished()` | `Ticket.is_finished()` |
| `Ticket::is_failed()` | `Ticket.is_failed()` |
| `Ticket::is_pending()` | `Ticket.is_pending()` |
| `Ticket.key`, `.status`, `.task`, `.result`, `.label`, `.schema`, `.parent`, `.reporter`, `.assignee` | Same names, same meaning. |
| `Ticket.created_at`, `.started_at`, `.finished_at`, `.failed_at` | Same names, same meaning. |
| `Ticket.replies` | `Ticket.replies`: a list of `Reply`, converted on access. |
| `Status` | A string. The five `is_*` predicates read better than comparing it. |
| `TicketError` | `RuntimeError` |
| **Replies** | |
| `Reply { author, content, created_at }` | `Reply.author`, `.content`, `.created_at` |
| `Author` | The `author` string: `"system"`, `"user"`, or `"assistant"`. |
| `ReplyContent` | `ReplyContent.kind` plus `.data`, like `Event`. Built with `ReplyContent.text(..)`, `.tool_use(..)`, `.tool_result(..)`, `.thinking(..)`, `.redacted_thinking(..)`. |
| `Reply::user_text(text)` | `Reply.user_text(text)`: the only way to build a reply, since any other carries no timestamp the store would trust. |
| **Compaction** | |
| `Compaction::reason()` | `Compaction.reason()`: the string `"proactive"` or `"reactive"`. |
| `Compaction::ticket()` | `Compaction.ticket()` |
| `Compaction::window()` | `Compaction.window()` |
| `Compaction::summarize(replies).await` | `await Compaction.summarize(replies)` |
| **Trajectory** | |
| `Trajectory::from_ticket(agent_id, model, ticket)` | `Trajectory.from_ticket(agent_id, model, ticket)` |
| `Trajectory::save(dir)` | `Trajectory.save(dir)` |
| `Trajectory.key`, `.model`, `.replies` | Same names. `replies` is a list of `Reply`. |
| **Knowledge** | |
| `Knowledge::load(dir)` | `Knowledge.load(dir)` |
| `Knowledge::index_char_limit(n)` | `Knowledge.index_char_limit(n)` |
| `Knowledge::get_index_char_limit()` | `Knowledge.get_index_char_limit()` |
| `Knowledge::index()` | `Knowledge.index()` |
| `Knowledge::pages()` | `Knowledge.pages()` |
| `Knowledge::clear()` | `Knowledge.clear()` |
| `Pages::save(page)` | `Pages.save(page)` |
| `Pages::load(slug)` | `Pages.load(slug)` |
| `Pages::list()` | `Pages.list()` |
| `Pages::remove(slug)` | `Pages.remove(slug)` |
| `Page { slug, kind, description, content, tags }` | `Page(slug, description, content, kind=.., tags=..)`: a struct literal becomes a constructor, so the optional fields move last. |
| `KnowledgeError` | `RuntimeError` |
| **Statistics** | |
| `Stats::event_count(event)` | `Stats.event_count(name)`: the name as a string; an unknown one raises. |
| `Stats::event_counts()` | `Stats.event_counts()`: keyed by that string. |
| `Stats::input_tokens()`, `::output_tokens()` | Same names. |
| `Stats::execution_duration()` | `Stats.execution_duration()` |
| `serde_json::to_value(&stats)` | `Stats.to_dict()`: Python cannot call `serde`, so reaching the `stats.json` shape needs a method. |
| **Schema** | |
| `Schema::new(document)` | `Schema(document)` |
| `Schema::validate(value)` | `Schema.validate(value)` |
| `SchemaViolation`, `SchemaViolations`, `SchemaParseError` | `RuntimeError` |
| **SchemaStore** | |
| `SchemaStore::new()` | `SchemaStore()` |
| `SchemaStore::label(label, document)` | `SchemaStore.label(label, document)`: raises on a document that is not a schema, where Rust returns `SchemaParseError`. |
| `SchemaStore::get(label)` | `SchemaStore.get(label)` |
| **Events** | |
| `Event { agent_id, ticket_key, label, kind }` | `Event.agent_id`, `.ticket_key`, `.label`, `.kind` |
| `EventKind` variant payload | `Event.data`: a dict of that variant's fields. |
| `EventKind`, `FinishReason` | Strings. |
| `EventName` | `EventName`: string constants, so `Event.kind == EventName.TURN_STARTED`. |
| `EventKind::is_failure()` | Not bound: `Event.kind` is a string, so ask `TicketQueue.on_failure(handler)` for the same six kinds. |
| `CompactReason`, `PolicyKind`, `ToolFailureKind`, `KnowledgeFailureKind`, `KnowledgeOp` | Strings inside `Event.data`, under the field's own name: `data["policy"]`, `data["reason"]`, `data["op"]`. |
| `default_logger()` | Not bound: pass your own handler to `TicketQueue.on_event(handler)`. |
| **LLM providers** | |
| `AnthropicProvider::new(key).base_url(url).timeout(d)` | `AnthropicProvider(api_key, base_url=.., timeout=..)` |
| `OpenAiProvider::new(key).base_url(url).timeout(d)` | `OpenAiProvider(api_key, base_url=.., timeout=..)` |
| `MistralProvider::new(key).base_url(url).timeout(d)` | `MistralProvider(api_key, base_url=.., timeout=..)` |
| `LiteLlmProvider::new(key).base_url(url).timeout(d)` | `LiteLlmProvider(api_key, base_url=.., timeout=..)` |
| `Provider` | An opaque handle. |
| `ProviderLike` | Not bound: implement it in Rust to write a new LLM provider. |
| `Provider::from_env()` | `Provider.from_env()` |
| `Provider::from_dot_env()` | `Provider.from_dot_env()` |
| `Provider::new(p)` | Not bound: the per-vendor constructors already hand back a `Provider`. |
| `Model::from_name(name)` | `Model(name)` |
| `Model::from_env()` | `Model.from_env()` |
| `Model::from_dot_env()` | `Model.from_dot_env()` |
| `Model.name` | `Model.name` |
| `Model::context_window(size)` | `Model.context_window(size)` |
| `Model::reasoning_effort(effort)` | `Model.reasoning_effort(effort)` |
| `Model::get_context_window()` | `Model.get_context_window()` |
| `Model::get_reasoning_effort()` | `Model.get_reasoning_effort()` |
| `ReasoningEffort` | A string. |
| `ProviderError`, `ProviderResult`, `RequestErrorKind` | `RuntimeError` |
| `ModelRequest`, `ProviderToolDefinition`, `ToolChoice`, `Message`, `AsUserMessage`, `ContentBlock`, `ModelResponse`, `ResponseStatus`, `StreamEvent` | Not bound: the shapes an LLM provider is built from. Python binds the four providers, not what they are built out of. |
| **Tools** | |
| `Tool::new(name, description).schema(..).handler(..).build()` | The `@tool` decorator: a decorated function carries the name, description, and schema a `ToolBuilder` collects. |
| `ToolBuilder<H>` | Folded into the `@tool` decorator: the type changes once a handler is attached, which Python cannot hold across calls. |
| `Tool` | `Tool`: an opaque handle the built-in tool functions return. An ad-hoc tool is a decorated function, not a `Tool`. |
| `Tool::from_tool_file(definition)` | Not bound: write the name, description, and schema on the Python function instead. |
| `ToolBuilder::read_only(b)` | `@tool(read_only=..)` |
| `ToolBuilder::defer(b)` | `@tool(defer=..)` |
| `ToolBuilder::paths(fields)` | `@tool(paths=[..])` |
| `ToolLike` | A `@tool`-decorated callable. Python cannot implement a Rust trait. |
| `ToolContext` | Not bound: a `@tool` function receives its input as keyword arguments only. |
| `ToolResult::success(c)`, `::error(c)`, `::schema_error(c)` | `ToolResult.success(content)`, `.error(content)`, `.schema_error(content)` |
| `ToolError` | `RuntimeError` |
| `ReadFileTool` | `ReadFileTool()`: a unit struct becomes a function returning a handle. |
| `WriteFileTool`, `EditFileTool` | `WriteFileTool()`, `EditFileTool()` |
| `GrepTool`, `GlobTool`, `ListDirectoryTool` | `GrepTool()`, `GlobTool()`, `ListDirectoryTool()` |
| `FetchUrlTool`, `FindToolsTool` | `FetchUrlTool()`, `FindToolsTool()` |
| `TicketsTool`, `FinishTool` | `TicketsTool()`, `FinishTool()` |
| `KnowledgeTool::new(store)` | `KnowledgeTool(store)` |
| `BashTool::new(name)` | `BashTool(name)`: a class carrying the builder methods below, where every other built-in tool is a function returning a handle. |
| `BashTool::allow(pattern)`, `::deny(pattern)`, `::description(..)`, `::read_only(..)` | `BashTool.allow(pattern)`, `.deny(pattern)`, `.description(..)`, `.read_only(..)` |
| `BashTool::unrestricted()` | `UnrestrictedBashTool()`: a second constructor on one class becomes a second function. |
| **Code search** | |
| `codegrep::{Pattern, Conf, search, tokenize_pattern, ..}` | Not bound: reachable through `GrepTool()` with `syntax="code"`. |
