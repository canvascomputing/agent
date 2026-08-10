<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/logo.png" width="200" />
</div>

<h1 align="center">agentwerk (Python)</h1>

<div align="center">
  <strong>A minimal Rust & Python library for running many agents in parallel.</strong>
</div>

<div align="center">
  <a href="#installation">Installation</a> •
  <a href="crates/agentwerk-py/README.md">Python</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#api">API</a> •
  <a href="#development">Development</a>
</div>

<div align="center">agentwerk is designed to tackle complex problems with fleets of agents through the simplest interface possible. It provides a ticket queue which distributes tasks across agents running in parallel, validates results, retries on failure, and reports every step as an event.</div>

---

<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/demo.gif" width="800" />
</div>
<div align="center"><a href="https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/apparat_fabrik.py">Apparat Fabrik</a></div>
<div align="center"><em>agentwerk pairs "agent" with the German "Werk", a word for both factory and artwork: machinery for building agentic systems.</em></div>

---

## Why use agentwerk?

- **Minimal interface:** create agents with a few lines of code.
- **Complex workflows:** allow agents to interact through shared knowledge and tickets.
- **Deep observability:** inspect every request, message and failure.
- **Ease of integration:** apply agents as simple as HTTP calls.
- **Facilitate training:** collect trajectories for fine-tuning models.

## Installation

### Python

```bash
pip install agentwerk
```

Also see: [Rust implementation](https://github.com/canvascomputing/agentwerk/blob/main/README.md).

## Quick Start

```python
import asyncio
from agentwerk import Agent, GrepTool, ReadFileTool


async def main():
    agent = (
        Agent.from_env()
        .role("You are a Rust developer who explores source files to answer questions.")
        .tool(ReadFileTool())
        .tool(GrepTool())
        .build()
    )

    agent.task(
        "Find every `pub trait` defined under src/ and explain each in one sentence."
    )
    work = agent.start()
    results = await work.finish_all()

    print(results[-1])


asyncio.run(main())
```

## Use Cases

Example projects built with agentwerk:

- [Terminal REPL](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/terminal_repl/): minimal interactive chat
- [Divide and Conquer](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/divide_and_conquer/): arithmetic problem shared across agents, ported in [examples/divide_and_conquer.py](https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/divide_and_conquer.py)
- [Deep Research](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/deep_research/): deep research pipeline (requires `BRAVE_API_KEY`)
- [Malware Scanner](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/malware_scanner/): identify indicators of compromise in a software package
- [Apparat Fabrik](https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/apparat_fabrik.py): a shift on the line of an apparatus works

> Configure an LLM provider first (see [Environment](https://github.com/canvascomputing/agentwerk/blob/main/DEVELOPMENT.md#environment)).

```bash
python examples/divide_and_conquer.py 200 4 2
```

## API

The API, section by section:

- [Agents](#agents): Define roles, behavior and actions.
- [Tickets](#tickets): Coordinate complex work across agents.
- [Tools](#tools): Define accessible tooling.
- [Events](#events): Requests, tool usage, failures and more.
- [Stats](#stats): Metrics about tickets, tokens and time.
- [Knowledge](#knowledge): Notes agents can share for collaboration.
- [Sessions](#sessions): Directory layout of data agents create.

## Agents

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/agents.gif" width="600" />
</div>

An `Agent` is the core entity of agentwerk. It has access to tools for solving tasks in the form of tickets.

```python
from agentwerk import Agent, ReadFileTool

agent = (
    Agent.from_env()
    .name("agent_0")
    .label("math")
    .role("You are an arithmetic agent. Compute step by step and show your work.")
    .tool(ReadFileTool())
    .build()
)

tickets.agent(agent)
tickets.task("Compute (47 * 92) / 8, then round to the nearest integer.")
```

<details>
<summary>All builder methods</summary>

| Method | Description |
|--------|-------------|
| `name(name)` | Set a name or identifier for assigning tickets. |
| `role(role)` | Define who the agent is and how it should work. |
| `label(label)` / `labels(labels)` | Restrict the agent to tickets carrying a matching label. |
| `tool(tool)` / `tools(tools)` | Register a tool the agent may call. |
| `template(key, value)` | Inject data into prompts with template strings. |
| `templates(pairs)` | Inject more than one entry into prompts. |
| `dir(dir)` | Set the directory the agent has access to. |
| `interactive()` | Let the agent wait for new instructions to keep a ticket in-progress. |
| `build()` | Create the agent. |

You can use the `{context}` variable to inject contextual information:

```markdown
- Ticket: TICKET-7
- Date: 2026-05-06
- Working directory: /Users/caro
- Platform: darwin 25.1.0
- Turns remaining: 8
- Input tokens remaining: 95000
- Output tokens remaining: 12000
- Time remaining: 240s
```

Every value is a variable of its own: `{ticket}`, `{date}`, `{dir}`, `{platform}`, `{os_version}`, `{turns_remaining}`, `{input_tokens_remaining}`, `{output_tokens_remaining}`, and `{time_remaining}`.

See more: [`AgentBuilder`](https://docs.rs/agentwerk/latest/agentwerk/agents/agent/struct.AgentBuilder.html).

</details>

### Providers

Connect to a `Provider` to give agents access to LLMs. agentwerk supports: Anthropic, OpenAI, Mistral, and a LiteLLM proxy.

```python
from agentwerk import Agent, AnthropicProvider

agent = (
    Agent()
    .provider(AnthropicProvider(key))
    .model("claude-sonnet-4-20250514")
)

# Or read both from the environment.
agent = Agent.from_env()
```

<details>
<summary>All provider settings</summary>

| Method | Description |
|--------|-------------|
| `provider(provider)` | Define the LLM provider. |
| `model(model)` | Set the model. |
| `Agent.from_env()` | Read the provider and the model from environment variables. |

You can also read the model or provider individually: `.provider(Provider.from_env())` or `.model(Model.from_env())`.

| Variable | Description |
|----------|-------------|
| `LITELLM_PROVIDER` | Choose `anthropic`, `mistral`, `openai`, or `litellm` outright, ahead of the keys below. |
| `LITELLM_API_KEY`, `MISTRAL_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` | Authenticate against that vendor. The first one set picks the provider, in this order. |
| `LITELLM_BASE_URL`, `MISTRAL_BASE_URL`, `ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL` | Point that vendor at a different endpoint. |
| `SSL_CERT_FILE`, `SSL_CERT_DIR` | Trust these CA certificates instead of the built-in root store. |

</details>

### Models

You can configure models to set a custom context window size or the applied reasoning:

```python
from agentwerk import Agent, Model

agent = Agent().model(
    Model("my-local-model").context_window(128_000).reasoning_effort("high")
)
```

Claude, GPT, Mistral, and Qwen families are pre-configured.

<details>
<summary>All model settings</summary>

| Method | Description |
|--------|-------------|
| `context_window(size)` | Set the context window size for a model. |
| `get_context_window()` | Get the configured window size. |
| `reasoning_effort(effort)` | Set the reasoning level. |
| `get_reasoning_effort()` | Get the configured effort. |

| Variable | Description |
|----------|-------------|
| `MODEL` | Model name. |
| `ANTHROPIC_MODEL`, `OPENAI_MODEL`, `MISTRAL_MODEL`, `LITELLM_MODEL` | Model name for the detected provider, read when `MODEL` is unset. |
| `MODEL_CONTEXT_WINDOW` | Context window size in tokens. |

</details>

## Tickets

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/tickets.gif" width="600" />
</div>

The `TicketQueue` is the core data structure of agentwerk allowing to coordinate complex interactions.

```python
analyst = (
    Agent.from_env()
    .name("analyst")
    .label("analysis")
    .build()
)

tickets.agent(analyst)
tickets.ticket(Ticket("Rank all products by value.", labels=["analysis"]))
```

<details>
<summary>All ticket entry points</summary>

| Method | Description |
|--------|-------------|
| `agent(agent)` | Add an agent to this ticket queue. |
| `task(task)` | Submit a task and return its ticket key. |
| `ticket(ticket)` | Submit a `Ticket` with custom labels or schema. |
| `reply(key, content)` | Add a reply to a ticket. |
| `edit_replies(key, editor)` | Rewrite one ticket's replies now. |
| `set_finished(key, result)` | Finish a ticket with a result. |
| `set_failed(key)` | Fail a ticket. |
| `dir(dir)` | Define where a session is stored. |
| `get_dir()` | Get the session directory. |
| `schemas(store)` | Enforce schemas for ticket results. |

See [`TicketQueue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketQueue.html).

</details>

### Execution

```python
tickets.start()
answer = (await tickets.finish_all())[-1]
```

<details>
<summary>All execution methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Run** | `start()` | Begin processing tickets. |
| **Wait** | `await finish(matches)` | Wait for the matching tickets to be done and get their results. |
| | `await finish_all()` | Wait for every ticket to be finished and get every result. |
| | `get_finish_reason()` | Get why the last run ended. |
| **Stop** | `cancel(matches)` | Stop work on the matching tickets. |
| | `cancel_all()` | Stop work on every ticket. |
| | `is_cancelled(ticket)` | Check whether a ticket has been cancelled. |

</details>

### Results

Access the results of the agents' work:

```python
await tickets.finish_all()

answers = tickets.results()
if answers:
    print(answers[-1])

for ticket in tickets.tickets():
    print(f"{ticket.key}: {ticket.status}")
```

Each `Ticket` carries a result as free text or JSON validated by schemas:

```python
ticket = tickets.find_ticket(lambda t: t.has_label("analysis"))
print(ticket.result["title"])
```

<details>
<summary>All result and ticket accessors</summary>

| Method | Description |
|--------|-------------|
| `results()` | Get the result of every finished ticket, in creation order. |
| `tickets()` | Get every ticket in creation order. |
| `find_ticket(condition)` | Get the earliest ticket matching a condition. |
| `find_tickets(condition)` | Get every ticket matching a condition. |
| `get_ticket(key)` | Get one ticket by key. |

Ticket members:

| | Member | Description |
|-|--------|-------------|
| **Identity** | `key` | Ticket key, of the form `TICKET-N`. |
| | `task` | The work the agent is asked to do. |
| | `labels` | Labels carried by the ticket. |
| | `parent` | The parent ticket if a handover was performed. |
| | `reporter` | Name of the agent that created the ticket. |
| | `assignee` | Name of the agent that claimed the ticket. |
| **Outcome** | `status` | The ticket lifecycle status. |
| | `result` | The result the agent produced. |
| | `replies` | Messages exchanged with the model. |
| | `schema` | Optional schema the result must satisfy. |
| **Timestamps** | `created_at` | Creation time, in milliseconds. |
| | `started_at` | Claim time, in milliseconds. |
| | `finished_at` | Finish time, in milliseconds. |
| | `failed_at` | Failure time, in milliseconds. |
| **Checks** | `has_label(label)` | Check whether the ticket carries a label. |
| | `is_todo()` | Check whether the ticket is waiting to be claimed. |
| | `is_in_progress()` | Check whether an agent is working on the ticket. |
| | `is_finished()` | Check whether the ticket finished. |
| | `is_failed()` | Check whether the ticket failed. |
| | `is_pending()` | Check whether the ticket is still todo or in progress. |

See [`Ticket`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.Ticket.html).

</details>

### Schemas

A `Schema` constrains the result an agent produces for a ticket. A violation triggers a retry until `max_schema_retries` is exhausted.

```python
from agentwerk import Schema, Ticket

schema = Schema(
    {
        "type": "object",
        "properties": {"title": {"type": "string"}},
        "required": ["title"],
    }
)

tickets.ticket(Ticket("Write a report.", schema=schema))
```

Enforce schemas for all tickets with a certain label. Registering schemas centrally spares agents from passing complex schema structures during ticket creation (see `ManageTicketsTool`) and handovers (see `FinishTool`).

```python
from agentwerk import SchemaStore

schemas = SchemaStore()
schemas.label(
    "report",
    {
        "type": "object",
        "properties": {"title": {"type": "string"}},
        "required": ["title"],
    },
)

tickets.schemas(schemas)
```

<details>
<summary>All schema methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Schema** | `Schema(document)` | Create a schema. |
| | `validate(value)` | Validate content. |
| **SchemaStore** | `SchemaStore()` | Create a store of schemas bound to labels. |
| | `label(label, document)` | Bind a schema to a label. |
| | `get(label)` | Read back the schema bound to a label. |
| | `tickets.schemas(store)` | Enforce schemas for ticket results. |

</details>

### Policies

Policies allow you to define execution limits:

```python
(
    tickets.max_turns(40)
    .max_time(300.0)
    .max_input_tokens(200_000)
    .max_output_tokens(50_000)
)
```

<details>
<summary>All limits</summary>

| Method | Description |
|--------|-------------|
| `max_turns(count)` / `get_max_turns()` | Limit the total number of turns. |
| `max_time(seconds)` / `get_max_time()` | Limit the total elapsed duration. |
| `max_input_tokens(count)` / `get_max_input_tokens()` | Limit the total input tokens. |
| `max_output_tokens(count)` / `get_max_output_tokens()` | Limit the total output tokens. |
| `max_request_tokens(count)` / `get_max_request_tokens()` | Limit the output tokens of a single request. |
| `max_schema_retries(count)` / `get_max_schema_retries()` | Limit how often a result may fail its schema before the ticket fails. |
| `max_request_retries(count)` / `get_max_request_retries()` | Limit how often a failing request is retried. |
| `request_retry_delay(seconds)` / `get_request_retry_delay()` | Wait this long between retries. |
| `compact_at(fraction)` / `get_compact_at()` | Compact once the context window is this full. |

A violated limit emits a `policy_violated` event, see [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html). `compact_at` is the exception: reaching it compacts the ticket and execution continues.

</details>

## Tools

Tools allow agents to perform their work.

```python
from agentwerk import Agent, BashTool, GrepTool, ReadFileTool

agent = (
    Agent()
    .tool(ReadFileTool())
    .tool(GrepTool())
    .tool(BashTool("git", "git *"))
)
```

`FinishTool()` and `ManageKnowledgeTool(store)` are special tools, registered automatically on every agent. They are used for interacting with the `TicketQueue`.

<details>
<summary>All built-in tools</summary>

| | Tool | Description |
|-|------|-------------|
| **File** | `ReadFileTool()` | Read a file with line numbers, offset, and limit. |
| | `WriteFileTool()` | Create or overwrite a file. |
| | `EditFileTool()` | Replace text in a file. |
| **Search** | `GlobTool()` | Find files by pattern. |
| | `GrepTool()` | Search file contents by regular expression, or by code shape with `syntax: "code"`. |
| | `ListDirectoryTool()` | List files and directories. |
| **Shell** | `BashTool(name, pattern)` | Run a shell command matching an allowed pattern. |
| **Web** | `FetchUrlTool()` | Fetch a URL and read its body. |
| **Tickets** | `FinishTool()` | Write the result for the current ticket and mark it finished. |
| | `ManageTicketsTool()` | Read the ticket queue and create or edit tickets. |
| | `ReadTicketsTool()` | Read the ticket queue. |
| **Knowledge** | `ManageKnowledgeTool(store)` | Write, read, remove, or list pages in a knowledge store. |
| **Discovery** | `FindToolsTool()` | Look up the tools held back until they are needed. |

</details>

### Custom tools

You can define custom tools for specific needs:

```python
from agentwerk import tool


@tool(
    read_only=True,
    schema={
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"],
    },
)
def greet(name: str) -> str:
    """Say hello."""
    return f"Hello, {name}!"
```

<details>
<summary>All tool options</summary>

| Method | Description |
|--------|-------------|
| `read_only=True` | Let the agent run this tool concurrently with other read-only calls in the same turn. |
| `defer=True` | Hold the tool back until the agent looks it up with `FindToolsTool()`. |
| `paths=["path"]` | Name file path used for a tool call, so the files are included in statistics. |

Return `ToolResult.error(message)` for a failure the model should work around.

</details>

## Events

Events give you insights to the lifecycle and activities of your agents' work.

```python
def log(event):
    if event.kind == "ticket_finished":
        print(f"[{event.agent_name}] done {event.ticket_key}")


tickets.on_event(log)
```

<details>
<summary>All event kinds</summary>

| | Kind | Description |
|-|------|-------------|
| **Run** | `run_started` | Execution began. |
| | `run_finished` | Execution ended, carrying the reason. |
| | `policy_violated` | A limit was breached and execution stopped. |
| **Ticket** | `ticket_started` | An agent claimed a ticket. |
| | `ticket_finished` | A ticket finished successfully. |
| | `ticket_failed` | A ticket failed. |
| | `turn_started` | The agent began another turn on its ticket. |
| | `schema_retried` | A result missed its schema and the agent was asked again. |
| **LLM provider** | `request_started` | A request went out to the model. |
| | `request_finished` | A request finished and reported its token usage. |
| | `request_failed` | A request failed and was not retried. |
| | `request_retried` | A transient provider error triggered a retry. |
| | `text_chunk_received` | A piece of the reply arrived. |
| **Tool** | `tool_call_started` | A tool invocation began. |
| | `tool_call_finished` | A tool invocation finished. |
| | `tool_call_failed` | A tool invocation failed but the ticket continues. |
| **File** | `file_open_finished` | A tool opened a file. |
| | `file_open_failed` | A tool could not open a file. |
| **Knowledge** | `knowledge_used` | A page was written, read, removed, or listed. |
| | `knowledge_missed` | A page the agent asked for was not there. |
| **Compaction** | `compaction_started` | Compaction is about to rewrite the older messages. |
| | `compaction_progress` | Compaction finished part of the work. |
| | `compaction_finished` | Compaction replaced the older messages. |
| | `compaction_failed` | Compaction could not finish. |

See [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html).

</details>

### Hooks

Hooks allow you to react to events:

```python

tickets.create_ticket_on_failure(
    lambda event, ticket: Ticket(ticket.task, labels=["retry"])
)
```

<details>
<summary>All hooks</summary>

| | Method | Description |
|-|--------|-------------|
| **Observe** | `on_event(handler)` | Read every event as it is emitted. |
| | `on_result(handler)` | Read every finished ticket together with its result. |
| | `on_failure(handler)` | Read every failure together with the ticket it happened in. |
| | `on_ticket(handler)` | Read a ticket as it starts, finishes, or fails. |
| **Add work** | `create_ticket_on_event(make)` | Enqueue a follow-up ticket from any event. |
| | `create_ticket_on_result(make)` | Enqueue a follow-up ticket from a finished ticket. |
| | `create_ticket_on_failure(make)` | Enqueue a retry for a ticket that failed. |
| **Rewrite** | `edit_replies_on_event(editor)` | Rewrite a ticket's replies before its next request. |
| | `edit_replies_on_compaction(editor)` | Decide what compaction does with a ticket's replies. |
| | `edit_directive_on_retry(editor)` | Override the prompt that corrects an agent's behavior. |

Save replies of every finished ticket as a training example:

```python
def capture(event, ticket):
    if event.kind == "ticket_finished":
        model = tickets.model_for_agent(event.agent_name)
        Trajectory.from_ticket(event.agent_name, model, ticket).save("datasets")


tickets.on_ticket(capture)
```

</details>

## Stats

Statistics give you deep insights into behavior of your agents: working time, tickets, failure rates, bottlenecks etc.

```python
stats = tickets.stats()
print(stats.event_count("request_finished"), stats.input_tokens())

for name, stat in stats.tool_stats().items():
    print(name, stat.calls)
```

<details>
<summary>All statistics</summary>

| Method | Description |
|--------|-------------|
| `execution_duration()` | Get the elapsed execution duration. |
| `ticket_duration()` | Get the time from creation to resolution, summed and averaged over resolved tickets. |
| `work_duration()` | Get the time agents spent working, summed across every agent and averaged per ticket. |
| `event_count(name)` | Get how many events of one kind were recorded, such as `"turn_started"`. |
| `input_tokens()` / `output_tokens()` | Get token counts across requests. |
| `tool_stats()` | Get per-tool call counts and the failures they ended in. |
| `file_stats()` | Get per-filepath open counts and the failures they ended in. |
| `knowledge_stats()` | Get per-operation attempt counts and the failures they ended in. |
| `model_stats()` | Get per-model requests, token usage, and the failures they ended in. |
| `event_counts()` | Get per-event counts. |
| `stats_for_label(label)` | Get statistics scoped to one label. |
| `stats_for_agent(agent_name)` | Get statistics scoped to one agent. |

See [`Stats`](https://docs.rs/agentwerk/latest/agentwerk/agents/stats/struct.Stats.html).

</details>

## Knowledge

`Knowledge` allows agents to share insights or learnings. Knowledge pages are created in the Open Knowledge Format (OKF).

```python
from agentwerk import Agent, Knowledge

store = Knowledge.load("./notes")
alice = Agent().knowledge(store)
bob = Agent().knowledge(store)
```

<details>
<summary>All knowledge methods</summary>

| Method | Description |
|--------|-------------|
| `index()` | Get the index, which is injected into the agent prompt. |
| `index_char_limit(count)` | Limit how much of the index is injected into the prompt. |
| `get_index_char_limit()` | Get the index size limit in force. |
| `pages()` | Get the page collection for reading and writing pages. |
| `pages().list()` | Get every page in the store. |
| `clear()` | Remove every page from the store. |

Programmatically create entries:

```python
from agentwerk import Page

store.pages().save(
    Page(
        "build-command",
        "How the project is built.",
        "Run `make` to compile.",
        tags=["build"],
    )
)

page = store.pages().load("build-command")
store.pages().remove("build-command")
```

</details>

## Sessions

A `TicketQueue` writes every ticket, reply, statistic, and lifecycle event to its working directory (default `./.agentwerk`). You can continue a session from that directory.

```python
tickets = TicketQueue.load(".agentwerk")
tickets.agent(my_agent)
tickets.start()
```

<details>
<summary>All session files</summary>

```
.agentwerk/
├── stats.json                            execution statistics
├── tickets.jsonl                         lifecycle events (one per line)
├── results.jsonl                         finished results (one per line)
├── tickets/
│   └── TICKET-1/
│       ├── ticket.json                   the ticket without its messages (key, status, labels, timestamps, result)
│       ├── replies.jsonl                 every message exchanged with the model, one per line
│       └── outputs/<tool_use_id>.txt     full tool outputs spilled out of the messages
└── knowledge/
    ├── pages/<slug>.md                   knowledge pages
    └── index.md                          knowledge index
```

</details>

## Development

See [DEVELOPMENT.md](https://github.com/canvascomputing/agentwerk/blob/main/DEVELOPMENT.md).
