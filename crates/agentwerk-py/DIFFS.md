# The Rust API next to the Python one

## Type conversions

Seven rules the surface table below never repeats.

| Rust | Python |
|------|--------|
| `tickets.max_time(Duration::from_secs(30))` | `tickets.max_time(30.0)`: every `Duration` becomes float seconds, under the same parameter name. |
| `ticket.status == Status::InProgress` | `ticket.status == "in_progress"`: every enum becomes its lowercase string. |
| `Err(TicketError::TicketMissing { key })` | `RuntimeError("Ticket TICKET-1 not found")`: every error type collapses to one exception. |
| `agent.knowledge(&store)` where `store: Arc<Knowledge>` | `agent.knowledge(store)`: every `Arc<T>` becomes a plain object, shared by passing it to several agents. |
| `agent.finish().await` | `await agent.finish()`: every `async fn` becomes awaitable. |
| `agent.task(MyTask { goal, url })` | `agent.task({"goal": .., "url": ..})`: an argument Rust takes by `Serialize` takes any JSON-serializable value. |
| `work.last_result() -> Option<serde_json::Value>` | `work.last_result()`: a JSON return value becomes a dict, list, or scalar. |

## The full surface

| Rust | Python |
|------|--------|
| **Agent** | |
| `Agent::new() -> AgentBuilder<(), ()>` | `Agent()` |
| `Agent::empty() -> AgentBuilder<(), ()>` | `Agent.empty()` |
| `AgentBuilder<P, M>` | Folded into `Agent`: the type changes as the provider and model slots fill, which Python cannot hold across calls. |
| `AgentBuilder::from_env()` | `Agent.from_env()` |
| `AgentBuilder::provider(p)` | `Agent.provider(provider)` |
| `AgentBuilder::provider_from_env()` | `Agent.provider_from_env()` |
| `AgentBuilder::model(m)` | `Agent.model(model)` |
| `AgentBuilder::model_from_env()` | `Agent.model_from_env()` |
| `AgentBuilder::name(n)` | `Agent.name(name)` |
| `AgentBuilder::role(r)` | `Agent.role(role)` |
| `AgentBuilder::context(c)` | `Agent.context(context)` |
| `AgentBuilder::label(l)` | `Agent.label(label)` |
| `AgentBuilder::labels(iter)` | `Agent.labels(labels)` |
| `AgentBuilder::interactive()` | `Agent.interactive()` |
| `AgentBuilder::template_variable(key, value)` | `Agent.template_variable(key, value)` |
| `AgentBuilder::template_variables(vars)` | `Agent.template_variables(variables)` |
| `AgentBuilder::tool(t)` | `Agent.tool(tool)` |
| `AgentBuilder::tools(iter)` | `Agent.tools(tools)` |
| `AgentBuilder::dir(p)` | `Agent.dir(dir)` |
| `AgentBuilder::knowledge(store)` | `Agent.knowledge(store)` |
| `AgentBuilder::edit_directive(editor)` | `Agent.edit_directive(editor)`: the editor returns the replacement, or `None` to keep the default, where Rust rewrites in place. |
| `AgentBuilder::build(self) -> Agent` | `Agent.build() -> Agent`: returns the same object, armed. Configuring after it, or building twice, raises. |
| `Agent::ticket_system(sys)` | `Agent.ticket_system(system)` |
| `Agent::task(task) -> String` | `Agent.task(task) -> str` |
| `Agent::ticket(ticket) -> String` | `Agent.ticket(ticket) -> str` |
| `Agent::start() -> Arc<TicketSystem>` | `Agent.start()` |
| `Agent::finish().await` | `await Agent.finish()` |
| **TicketSystem** | |
| `TicketSystem::new() -> Arc<Self>` | `TicketSystem()` |
| `TicketSystem::load(dir)` | `TicketSystem.load(dir)` |
| `TicketSystem::agent(a)` | `TicketSystem.agent(agent)` |
| `TicketSystem::task(task)` | `TicketSystem.task(task)` |
| `TicketSystem::ticket(t)` | `TicketSystem.ticket(ticket)` |
| `TicketSystem::reply(key, content)` | `TicketSystem.reply(key, content)` |
| `TicketSystem::set_failed(key)` | `TicketSystem.set_failed(key)` |
| `TicketSystem::max_turns(n)` | `TicketSystem.max_turns(n)` |
| `TicketSystem::max_input_tokens(n)` | `TicketSystem.max_input_tokens(n)` |
| `TicketSystem::max_output_tokens(n)` | `TicketSystem.max_output_tokens(n)` |
| `TicketSystem::max_request_tokens(n)` | `TicketSystem.max_request_tokens(n)` |
| `TicketSystem::max_schema_retries(n)` | `TicketSystem.max_schema_retries(n)` |
| `TicketSystem::max_request_retries(n)` | `TicketSystem.max_request_retries(n)` |
| `TicketSystem::max_time(d)` | `TicketSystem.max_time(seconds)` |
| `TicketSystem::request_retry_delay(d)` | `TicketSystem.request_retry_delay(seconds)` |
| `TicketSystem::dir(dir)` | `TicketSystem.dir(dir)` |
| `TicketSystem::schema_for_label(label, schema)` | `TicketSystem.schema_for_label(label, schema)` |
| `TicketSystem::on_event(h)` | `TicketSystem.on_event(callback)` |
| `TicketSystem::on_ticket(handler)` | `TicketSystem.on_ticket(callback)` |
| `TicketSystem::cancel_on(trigger)` | `TicketSystem.cancel_on(awaitable)` |
| `TicketSystem::cancel_on_event(predicate)` | `TicketSystem.cancel_on_event(predicate)` |
| `TicketSystem::cancel_on_result(predicate)` | `TicketSystem.cancel_on_result(predicate)` |
| `TicketSystem::cancel_label(label)` | `TicketSystem.cancel_label(label)` |
| `TicketSystem::cancel_label_on_event(label, predicate)` | `TicketSystem.cancel_label_on_event(label, predicate)` |
| `TicketSystem::create_ticket_on_result(make)` | `TicketSystem.create_ticket_on_result(make)` |
| `TicketSystem::edit_replies(key, edit)` | `TicketSystem.edit_replies(key, editor)`: the editor returns the new list, or `None` to keep the old one, where Rust mutates in place. An editor that raises, or returns anything but `Reply` objects, raises here. |
| `TicketSystem::edit_replies_on_event(editor)` | `TicketSystem.edit_replies_on_event(editor)`: same return-instead-of-mutate shape. An editor that raises prints its traceback and changes nothing: it runs on an agent thread with no Python frame to raise into. |
| `TicketSystem::model_for_agent(name)` | `TicketSystem.model_for_agent(agent_name)` |
| `TicketSystem::get_ticket(key)` | `TicketSystem.get_ticket(key)` |
| `TicketSystem::tickets()` | `TicketSystem.tickets()` |
| `TicketSystem::find_tickets(predicate)` | `TicketSystem.find_tickets(predicate)` |
| `TicketSystem::find_ticket(predicate)` | `TicketSystem.find_ticket(predicate)` |
| `TicketSystem::wait_for_ticket(predicate).await` | `await TicketSystem.wait_for_ticket(predicate)` |
| `TicketSystem::start()` | `TicketSystem.start()` |
| `TicketSystem::finish().await` | `await TicketSystem.finish()` |
| `TicketSystem::cancel()` | `TicketSystem.cancel()` |
| `TicketSystem::is_cancelled()` | `TicketSystem.is_cancelled()` |
| `TicketSystem::finish_reason()` | `TicketSystem.finish_reason()` |
| `TicketSystem::stats()` | `TicketSystem.stats()` |
| `TicketSystem::last_result()` | `TicketSystem.last_result()` |
| `TicketSystem::results()` | `TicketSystem.results()` |
| `TicketSystem::results_for_label(label)` | `TicketSystem.results_for_label(label)` |
| **Ticket** | |
| `Ticket::new(task)` | `Ticket(task)` |
| `Ticket::label(l)` | `Ticket(task, labels=[l])` |
| `Ticket::labels(iter)` | `Ticket(task, labels=[..])` |
| `Ticket::schema(s)` | `Ticket(task, schema=s)` |
| `Ticket::parent(key)` | `Ticket(task, parent=key)` |
| `Ticket::has_label(label)` | `Ticket.has_label(label)` |
| `Ticket::is_todo()` | `Ticket.is_todo()` |
| `Ticket::is_in_progress()` | `Ticket.is_in_progress()` |
| `Ticket::is_finished()` | `Ticket.is_finished()` |
| `Ticket::is_failed()` | `Ticket.is_failed()` |
| `Ticket::is_pending()` | `Ticket.is_pending()` |
| `Ticket::is_resolved()` | `Ticket.is_resolved()` |
| `Ticket.key`, `.status`, `.task`, `.result`, `.labels`, `.schema`, `.parent`, `.reporter` | Same names, same meaning. |
| `Ticket.created_at`, `.started_at`, `.finished_at`, `.failed_at` | Same names, same meaning. |
| `Ticket.replies` | `Ticket.replies`: a list of `Reply`, converted on access. |
| `Status` | A string. The six `is_*` predicates read better than comparing it. |
| `TicketError` | `RuntimeError` |
| **Replies** | |
| `Reply { author, content, created_at }` | `Reply.author`, `.content`, `.created_at` |
| `Author` | The `author` string: `"system"`, `"user"`, or `"assistant"`. |
| `ReplyContent` | `ReplyContent.kind` plus `.data`, like `Event`. Built with `ReplyContent.text(..)`, `.tool_use(..)`, `.tool_result(..)`, `.thinking(..)`, `.redacted_thinking(..)`. |
| `Reply::user_text(text)` | `Reply.user_text(text)`: the only way to build a reply, since any other carries no timestamp the store would trust. |
| **Trajectory** | |
| `Trajectory::from_ticket(agent, model, ticket)` | `Trajectory.from_ticket(agent, model, ticket)` |
| `Trajectory::save(dir)` | `Trajectory.save(dir)` |
| `Trajectory.key`, `.model`, `.replies` | Same names. `replies` is a list of `Reply`. |
| **Knowledge** | |
| `Knowledge::load(dir)` | `Knowledge.load(dir)` |
| `Knowledge::index_char_limit(n)` | `Knowledge.index_char_limit(n)` |
| `Knowledge::index()` | `Knowledge.index()` |
| `Knowledge::pages()` | `Knowledge.pages()` |
| `Knowledge::clear()` | `Knowledge.clear()` |
| `Pages::save(page)` | `Pages.save(page)` |
| `Pages::load(slug)` | `Pages.load(slug)` |
| `Pages::remove(slug)` | `Pages.remove(slug)` |
| `Page { slug, kind, description, content, tags }` | `Page(slug, description, content, kind=.., tags=..)`: a struct literal becomes a constructor, so the optional fields move last. |
| `KnowledgeError` | `RuntimeError` |
| **Statistics** | |
| `Stats::stats_for_label(label)` | `Stats.stats_for_label(label)` |
| `Stats::usage_history(key)` | `Stats.usage_history(ticket_key)` |
| `Stats::tool_stats()` | `Stats.tool_stats()` |
| `Stats::file_stats()` | `Stats.file_stats()` |
| `Stats::knowledge_stats()` | `Stats.knowledge_stats()` |
| `Stats::model_stats()` | `Stats.model_stats()` |
| `Stats::turns()`, `::requests()`, `::tool_calls()`, `::errors()` | Same names. |
| `Stats::event_counts()` | `Stats.event_counts()` |
| `Stats::input_tokens()`, `::output_tokens()` | Same names. |
| `Stats::tickets_created()`, `::tickets_finished()`, `::tickets_failed()` | Same names. |
| `Stats::tickets_success_rate()` | `Stats.tickets_success_rate()` |
| `Stats::run_duration()` | `Stats.run_duration()` |
| `Stats::total_ticket_duration()`, `::avg_ticket_duration()` | Same names. |
| `Stats::total_work_duration()`, `::avg_work_duration()` | Same names. |
| `serde_json::to_value(&stats)` | `Stats.to_dict()`: Python cannot call `serde`, so reaching the `stats.json` shape needs a method. |
| `ToolStat { calls, not_found, execution_failed, schema_failed }` | Same fields, plus the same `errors()` and `error_rate()` methods. |
| `FileStat { opens, failed }` | Same fields. |
| `KnowledgeStat { writes, reads, removes, lists, misses }` | Same fields. |
| `ModelStat { requests, input_tokens, output_tokens }` | Same fields. |
| `TokenUsage` | A dict, as returned by `Stats.usage_history()`. |
| **Schema** | |
| `Schema::parse(document)` | `Schema(document)` |
| `Schema::validate(value)` | `Schema.validate(value)` |
| `SchemaViolation`, `SchemaViolations`, `SchemaParseError` | `RuntimeError` |
| **Events** | |
| `Event { agent_name, ticket_key, kind }` | `Event.agent_name`, `.ticket_key`, `.kind` |
| `EventKind` variant payload | `Event.data`: a dict of that variant's fields. |
| `EventKind`, `FinishReason` | Strings. |
| `CompactReason`, `PolicyKind`, `ToolFailureKind`, `KnowledgeOp` | Strings inside `Event.data`, under the field's own name: `data["policy"]`, `data["reason"]`, `data["op"]`. |
| `default_logger()` | `TicketSystem.on_event(handler)`, in both languages. |
| **LLM providers** | |
| `AnthropicProvider::new(key).base_url(url)` | `AnthropicProvider(api_key, base_url=..)` |
| `OpenAiProvider::new(key).base_url(url)` | `OpenAiProvider(api_key, base_url=..)` |
| `MistralProvider::new(key).base_url(url)` | `MistralProvider(api_key, base_url=..)` |
| `LiteLlmProvider::new(key).base_url(url)` | `LiteLlmProvider(api_key, base_url=..)` |
| `Provider` | An opaque handle. Write a new LLM provider in Rust. |
| `provider_from_env()` | `provider_from_env()` |
| `model_from_env()` | `model_from_env()` |
| `context_window_from_env()` | `context_window_from_env()` |
| `Model::from_name(name)` | `Model(name)` |
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
| `ReadTicketsTool`, `ManageTicketsTool`, `FinishTool` | `ReadTicketsTool()`, `ManageTicketsTool()`, `FinishTool()` |
| `ManageKnowledgeTool::new(store)` | `ManageKnowledgeTool(store)` |
| `BashTool::new(name, pattern).description(..).read_only(..)` | `BashTool(name, pattern, description=.., read_only=..)` |
| `BashTool::unrestricted()` | `UnrestrictedBashTool()`: a second constructor on one class becomes a second function. |
| **Code search** | |
| `codegrep::{Pattern, Conf, search, tokenize_pattern, ..}` | Not bound: reachable through `GrepTool()` with `syntax="code"`. |
