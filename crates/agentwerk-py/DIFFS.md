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
| `AgentBuilder::label(l)` | `Agent.label(label)` |
| `AgentBuilder::labels(iter)` | `Agent.labels(labels)` |
| `AgentBuilder::interactive()` | `Agent.interactive()` |
| `AgentBuilder::template(key, value)` | `Agent.template(key, value)` |
| `AgentBuilder::templates(vars)` | `Agent.templates(variables)`: a mapping, so the bulk bind applies in key order where Rust preserves insertion order. |
| `AgentBuilder::tool(t)` | `Agent.tool(tool)` |
| `AgentBuilder::tools(iter)` | `Agent.tools(tools)` |
| `AgentBuilder::dir(p)` | `Agent.dir(dir)` |
| `AgentBuilder::knowledge(store)` | `Agent.knowledge(store)` |
| `AgentBuilder::edit_directive_on_retry(editor)` | `Agent.edit_directive_on_retry(editor)`: the editor returns the replacement, or `None` to keep the default, where Rust rewrites in place. |
| `AgentBuilder::build(self) -> Agent` | `Agent.build() -> Agent`: returns the same object, armed. Configuring after it, or building twice, raises. |
| `Agent::ticket_queue(queue)` | `Agent.ticket_queue(queue)` |
| `Agent::task(task) -> String` | `Agent.task(task) -> str` |
| `Agent::ticket(ticket) -> String` | `Agent.ticket(ticket) -> str` |
| `Agent::start() -> Arc<TicketQueue>` | `Agent.start()` |
| `Agent::finish().await` | `await Agent.finish()` |
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
| `TicketQueue::schema_for_label(label, schema)` | `TicketQueue.schema_for_label(label, schema)` |
| `TicketQueue::on_event(h)` | `TicketQueue.on_event(callback)` |
| `TicketQueue::on_result(handler)` | `TicketQueue.on_result(callback)` |
| `TicketQueue::on_failure(handler)` | `TicketQueue.on_failure(callback)` |
| `TicketQueue::on_ticket(handler)` | `TicketQueue.on_ticket(callback)` |
| `TicketQueue::cancel_on(trigger)` | `TicketQueue.cancel_on(awaitable)` |
| `TicketQueue::cancel_on_event(predicate)` | `TicketQueue.cancel_on_event(predicate)` |
| `TicketQueue::cancel_on_result(predicate)` | `TicketQueue.cancel_on_result(predicate)` |
| `TicketQueue::cancel_on_failure(predicate)` | `TicketQueue.cancel_on_failure(predicate)` |
| `TicketQueue::cancel_label(label)` | `TicketQueue.cancel_label(label)` |
| `TicketQueue::cancel_label_on_event(label, predicate)` | `TicketQueue.cancel_label_on_event(label, predicate)` |
| `TicketQueue::cancel_label_on_result(label, predicate)` | `TicketQueue.cancel_label_on_result(label, predicate)` |
| `TicketQueue::cancel_label_on_failure(label, predicate)` | `TicketQueue.cancel_label_on_failure(label, predicate)` |
| `TicketQueue::label_cancelled(label)` | `TicketQueue.label_cancelled(label)` |
| `TicketQueue::create_ticket_on_event(make)` | `TicketQueue.create_ticket_on_event(make)` |
| `TicketQueue::create_ticket_on_result(make)` | `TicketQueue.create_ticket_on_result(make)` |
| `TicketQueue::create_ticket_on_failure(make)` | `TicketQueue.create_ticket_on_failure(make)` |
| `TicketQueue::edit_replies(key, edit)` | `TicketQueue.edit_replies(key, editor)`: the editor returns the new list, or `None` to keep the old one, where Rust mutates in place. An editor that raises, or returns anything but `Reply` objects, raises here. |
| `TicketQueue::edit_replies_on_event(editor)` | `TicketQueue.edit_replies_on_event(editor)`: same return-instead-of-mutate shape. An editor that raises prints its traceback and changes nothing: it runs on an agent thread with no Python frame to raise into. |
| `TicketQueue::edit_replies_on_compaction(editor)` | `TicketQueue.edit_replies_on_compaction(editor)`: the editor returns the new list, or `None` to keep the current one, where Rust returns a `Result`. Define it with `async def` to await `Compaction.summarize`; a coroutine is driven on a worker thread of its own. An editor that raises prints its traceback and changes nothing, like the event editor. |
| `TicketQueue::model_for_agent(name)` | `TicketQueue.model_for_agent(agent_name)` |
| `TicketQueue::get_ticket(key)` | `TicketQueue.get_ticket(key)` |
| `TicketQueue::tickets()` | `TicketQueue.tickets()` |
| `TicketQueue::tickets_for_label(label)` | `TicketQueue.tickets_for_label(label)` |
| `TicketQueue::find_tickets(predicate)` | `TicketQueue.find_tickets(predicate)` |
| `TicketQueue::find_ticket(predicate)` | `TicketQueue.find_ticket(predicate)` |
| `TicketQueue::wait_for_ticket(predicate).await` | `await TicketQueue.wait_for_ticket(predicate)` |
| `TicketQueue::start()` | `TicketQueue.start()` |
| `TicketQueue::finish().await` | `await TicketQueue.finish()` |
| `TicketQueue::cancel()` | `TicketQueue.cancel()` |
| `TicketQueue::is_cancelled()` | `TicketQueue.is_cancelled()` |
| `TicketQueue::finish_reason()` | `TicketQueue.finish_reason()` |
| `TicketQueue::stats()` | `TicketQueue.stats()` |
| `TicketQueue::last_result()` | `TicketQueue.last_result()` |
| `TicketQueue::results()` | `TicketQueue.results()` |
| `TicketQueue::results_for_label(label)` | `TicketQueue.results_for_label(label)` |
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
| **Compaction** | |
| `Compaction::reason()` | `Compaction.reason()`: the string `"proactive"` or `"reactive"`. |
| `Compaction::ticket()` | `Compaction.ticket()` |
| `Compaction::window()` | `Compaction.window()` |
| `Compaction::summarize(replies).await` | `await Compaction.summarize(replies)` |
| **Trajectory** | |
| `Trajectory::from_ticket(agent, model, ticket)` | `Trajectory.from_ticket(agent, model, ticket)` |
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
| `Stats::stats_for_label(label)` | `Stats.stats_for_label(label)` |
| `Stats::stats_for_agent(agent_name)` | `Stats.stats_for_agent(agent_name)` |
| `Stats::tool_stats()` | `Stats.tool_stats()` |
| `Stats::file_stats()` | `Stats.file_stats()` |
| `Stats::knowledge_stats()` | `Stats.knowledge_stats()` |
| `Stats::model_stats()` | `Stats.model_stats()` |
| `Stats::event_count(event)` | `Stats.event_count(name)`: Python names the kind with the same string `Event.kind` reports, since the `EventName` enum has no Python counterpart. An unknown name raises. |
| `Stats::event_counts()` | `Stats.event_counts()`: keyed by that same string rather than by `EventName`. |
| `Stats::input_tokens()`, `::output_tokens()` | Same names. |
| `Stats::tickets_created()`, `::tickets_finished()`, `::tickets_failed()` | Same names. |
| `Stats::tickets_success_rate()` | `Stats.tickets_success_rate()` |
| `Stats::run_duration()` | `Stats.run_duration()` |
| `Stats::total_ticket_duration()`, `::avg_ticket_duration()` | Same names. |
| `Stats::total_work_duration()`, `::avg_work_duration()` | Same names. |
| `serde_json::to_value(&stats)` | `Stats.to_dict()`: Python cannot call `serde`, so reaching the `stats.json` shape needs a method. |
| `ToolStat { calls, not_found, execution_failed, schema_failed }` | Same fields, plus the same `errors()` and `error_rate()` methods. |
| `FileStat { opens, failed }` | Same, including `errors()` and `error_rate()`. |
| `KnowledgeStat { attempts, failed }` | Same, including `errors()` and `error_rate()`. |
| `ModelStat { requests, failed, input_tokens, output_tokens }` | Same, including `errors()` and `error_rate()`. |
| **Schema** | |
| `Schema::parse(document)` | `Schema(document)` |
| `Schema::validate(value)` | `Schema.validate(value)` |
| `SchemaViolation`, `SchemaViolations`, `SchemaParseError` | `RuntimeError` |
| **Events** | |
| `Event { agent_name, ticket_key, kind }` | `Event.agent_name`, `.ticket_key`, `.kind` |
| `EventKind` variant payload | `Event.data`: a dict of that variant's fields. |
| `EventKind`, `EventName`, `FinishReason` | Strings. |
| `EventKind::is_failure()` | Not bound: `Event.kind` is a string, so ask `TicketQueue.on_failure(handler)` for the same six kinds. |
| `CompactReason`, `PolicyKind`, `ToolFailureKind`, `KnowledgeOp` | Strings inside `Event.data`, under the field's own name: `data["policy"]`, `data["reason"]`, `data["op"]`. |
| `default_logger()` | Not bound: pass your own handler to `TicketQueue.on_event(handler)`. |
| **LLM providers** | |
| `AnthropicProvider::new(key).base_url(url).timeout(d)` | `AnthropicProvider(api_key, base_url=.., timeout=..)` |
| `OpenAiProvider::new(key).base_url(url).timeout(d)` | `OpenAiProvider(api_key, base_url=.., timeout=..)` |
| `MistralProvider::new(key).base_url(url).timeout(d)` | `MistralProvider(api_key, base_url=.., timeout=..)` |
| `LiteLlmProvider::new(key).base_url(url).timeout(d)` | `LiteLlmProvider(api_key, base_url=.., timeout=..)` |
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
