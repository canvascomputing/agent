<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/logo.png" width="200" />
</div>

<h1 align="center">agentwerk (Python)</h1>

<div align="center">
  <strong>A minimal Python library for running many agents in parallel.</strong>
</div>

<div align="center">
  <a href="#installation">Installation</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#agent-swarms">Agent Swarms</a> •
  <a href="#demo">Demo</a> •
  <a href="#use-cases">Use Cases</a> •
  <a href="#development">Development</a>
</div>

<div align="center">agentwerk is designed to tackle complex problems with fleets of agents through the simplest interface possible. It provides a ticket queue which distributes tasks across agents running in parallel, validates results, retries on failure, and reports every step as an event.</div>

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
        Agent()
        .from_env()
        .role("You are a Rust developer who explores source files to answer questions.")
        .tool(ReadFileTool())
        .tool(GrepTool())
        .build()
    )

    agent.task(
        "Find every `pub trait` defined under src/ and explain each in one sentence."
    )
    work = await agent.finish()

    print(work.last_result())


asyncio.run(main())
```

## Agent Swarms

Run many agents in parallel and let them share what they learn:

```python
from agentwerk import Agent, Knowledge, Ticket, TicketQueue
from agentwerk import GrepTool, ManageTicketsTool, ReadFileTool

tickets = TicketQueue()
store = Knowledge.load("./notes")

for i in range(4):
    tickets.agent(
        Agent()
        .name(f"scout_{i}")
        .label("scan")
        .role(
            "Find code that can panic. File a `report` ticket per finding, and note what you learn."
        )
        .knowledge(store)
        .from_env()
        .tool(GrepTool())
        .tool(ReadFileTool())
        .tool(ManageTicketsTool())
        .build()
    )

tickets.agent(
    Agent()
    .name("writer")
    .label("report")
    .role("Read the cited file and explain the fix in two sentences.")
    .knowledge(store)
    .from_env()
    .tool(ReadFileTool())
    .build()
)

for dir in ["src/api", "src/db", "src/web", "src/cli"]:
    tickets.ticket(Ticket(f"Audit {dir}.", labels=["scan"]))

await tickets.finish()

for fix in tickets.results_for_label("report"):
    print(fix)
```

## Demo

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/demo.gif" width="600" />
</div>

## Use Cases

Example projects built with agentwerk:

- [Terminal REPL](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/terminal_repl/): minimal interactive chat
- [Divide and Conquer](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/divide_and_conquer/): arithmetic problem shared across agents, ported in [examples/divide_and_conquer.py](https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/divide_and_conquer.py)
- [Deep Research](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/deep_research/): deep research pipeline (requires `BRAVE_API_KEY`)
- [Malware Scanner](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/malware_scanner/): identify indicators of compromise in a software package

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

An `Agent` is the core entity of agentwerk. It has access to tools for solving tasks in the form of tickets.

```python
from agentwerk import Agent, ReadFileTool

agent = (
    Agent()
    .name("agent_0")
    .label("math")
    .role("You are an arithmetic agent. Compute step by step and show your work.")
    .tool(ReadFileTool())
    .from_env()
    .build()
)

tickets.agent(agent)
tickets.task("Compute (47 * 92) / 8, then round to the nearest integer.")
```

<details>
<summary>All agent builder methods</summary>

| Method | Description |
|--------|-------------|
| `Agent.empty()` | Create an agent with no tools pre-registered. |
| `name(name)` | Set a name or identifier for assigning tickets. |
| `role(role)` | Define who the agent is and how it should work. |
| `label(label)` / `labels(labels)` | Restrict the agent to tickets carrying a matching label. |
| `tool(tool)` / `tools(tools)` | Register a tool the agent may call. |
| `template(key, value)` | Inject data into prompts with template strings. |
| `templates(pairs)` | Inject more than one entry into prompts. |
| `dir(dir)` | Set the directory the agent has access to. |
| `interactive()` | Let the agent wait for new instructions to keep a ticket in-progress. |
| `edit_directive_on_retry(editor)` | Override the prompt that corrects an agent asked to try again. |
| `build()` | Create the agent. |
| `ticket_queue(queue)` | Attach a built agent to a ticket queue. |

You can use the `{context}` variable to inject contextual information:

```markdown
You work within a ticket queue. Each task arrives as a ticket; each reply you generate is one turn.

- Ticket: TICKET-7
- Date: 2026-05-06
- Working directory: /Users/caro
- Platform: darwin 25.1.0
- Turns remaining: 8
- Input tokens remaining: 95000
- Output tokens remaining: 12000
- Time remaining: 240s

Execution stops when any budget reaches zero, mid-ticket. Finish before then.
```

See more: [`AgentBuilder`](https://docs.rs/agentwerk/latest/agentwerk/agents/agent/struct.AgentBuilder.html).

</details>

### Providers

Connect to a `Provider` to give agents access to LLMs. agentwerk supports: Anthropic, OpenAI, Mistral, and a LiteLLM proxy.

```python
from agentwerk import Agent, AnthropicProvider

agent = Agent().provider(AnthropicProvider(key)).model("claude-sonnet-4-20250514")

# Or read both from the environment.
agent = Agent().from_env()
```

<details>
<summary>Provider selection and endpoints</summary>

| Method | Description |
|--------|-------------|
| `provider(provider)` | Define the LLM provider. |
| `model(model)` | Set the model. |
| `from_env()` | Read environment variables for configuration (see [DEVELOPMENT.md](https://github.com/canvascomputing/agentwerk/blob/main/DEVELOPMENT.md)). |

You can explicitly read the model or provider from environment variables with: `provider_from_env()` or `model_from_env()`.

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
<summary>Model settings</summary>

| Method | Description |
|--------|-------------|
| `context_window(size)` | Set the context window size for a model. |
| `get_context_window()` | Get the configured window size. |
| `reasoning_effort(effort)` | Set the reasoning level. |
| `get_reasoning_effort()` | Get the configured effort. |

You can use `context_window_from_env()` to read the context window size from environment variables, see [DEVELOPMENT.md](https://github.com/canvascomputing/agentwerk/blob/main/DEVELOPMENT.md).

</details>

## Tickets

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/tickets.jpg" width="600" />
</div>

The `TicketQueue` is the core data structure of agentwerk allowing to coordinate complex interactions.

```python
tickets.agent(Agent().name("analyst").label("analysis").from_env().build())

tickets.ticket(
    Ticket(
        "Rank all products by value for a 10-person engineering team.",
        labels=["analysis"],
        schema=comparison_schema,
    )
)
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
| `schema_for_label(label, schema)` | Register a schema every ticket of that label validates against. |

See [`TicketQueue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketQueue.html).

</details>

### Execution

```python
tickets.start()
await tickets.finish()
answer = tickets.last_result()
```

<details>
<summary>Lifecycle</summary>

| Method | Description |
|--------|-------------|
| `start()` | Begin processing tickets. |
| `await finish()` | Process every queued ticket. |
| `cancel()` | Cancel the execution. |
| `is_cancelled()` | Check whether the execution was cancelled. |
| `finish_reason()` | Check the reason for the finishing. |
| `cancel_label(label)` | Stop one label's agents. |
| `label_cancelled(label)` | Check whether one label's agents have been stopped. |

</details>

### Results

Access the results of the agents' work:

```python
await tickets.finish()

answer = tickets.last_result()
if answer is not None:
    print(answer)

for ticket in tickets.tickets():
    print(f"{ticket.key}: {ticket.status}")
```

Each `Ticket` carries a result as free text or JSON validated by schemas:

```python
ticket = tickets.find_ticket(lambda t: t.has_label("analysis"))
print(ticket.result["title"])
```

<details>
<summary>Working with results</summary>

| Method | Description |
|--------|-------------|
| `last_result()` | Get the most recent ticket result. |
| `results()` | Get every ticket's result in creation order. |
| `results_for_label(label)` | Get every ticket's result carrying a specific label. |
| `tickets()` | Get every ticket in creation order. |
| `tickets_for_label(label)` | Get every ticket carrying a specific label. |
| `find_ticket(condition)` | Get the earliest ticket matching a condition. |
| `find_tickets(condition)` | Get every ticket matching a condition. |
| `get_ticket(key)` | Get one ticket by key. |
| `await wait_for_ticket(condition)` | Wait for one matching ticket instead of draining the queue. |
| `model_for_agent(name)` | Get the model that agent runs. |
| `stats()` | Get execution statistics, see [Stats](#stats). |

Ticket members:

| | Members |
|-|---------|
| **Identity** | `key`, `task`, `labels`, `parent`, `reporter` |
| **Outcome** | `status`, `result`, `replies`, `schema` |
| **Timestamps** | `created_at`, `started_at`, `finished_at`, `failed_at` |
| **Checks** | `has_label(label)`, `is_todo()`, `is_in_progress()`, `is_finished()`, `is_failed()`, `is_pending()`, `is_resolved()` |

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

<details>
<summary>Schema validation and retries</summary>

| Method | Description |
|--------|-------------|
| `Schema(document)` | Create a schema. |
| `Schema.validate(value)` | Validate content. |
| `tickets.schema_for_label(label, schema)` | Register a schema for all tickets with a certain label. |

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

agent = Agent().tool(ReadFileTool()).tool(GrepTool()).tool(BashTool("git", "git *"))
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
<summary>Tool options</summary>

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

# Stop execution at the first malicious verdict.
tickets.cancel_on_result(lambda ticket, result: result["verdict"] == "malicious")
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

<details>
<summary>Hooks</summary>

| | Method | Description |
|-|--------|-------------|
| **Observe** | `on_event(handler)` | Read every event as it is emitted. |
| | `on_result(handler)` | Read every finished ticket together with its result. |
| | `on_failure(handler)` | Read every failure together with the ticket it happened in. |
| | `on_ticket(handler)` | Read a ticket as it starts, finishes, or fails. |
| **Stop the run** | `cancel_on(awaitable)` | Stop execution when another task you supply finishes. |
| | `cancel_on_event(condition)` | Stop execution when an event matches. |
| | `cancel_on_result(condition)` | Stop execution when a finished result matches. |
| | `cancel_on_failure(condition)` | Stop execution when a failure matches. |
| **Stop one label** | `cancel_label_on_event(label, condition)` | Stop one label's agents while the rest keep working. |
| | `cancel_label_on_result(label, condition)` | Stop one label's agents when a finished result matches. |
| | `cancel_label_on_failure(label, condition)` | Stop one label's agents when a failure matches. |
| **Add work** | `create_ticket_on_event(make)` | Enqueue a follow-up ticket from any event. |
| | `create_ticket_on_result(make)` | Enqueue a follow-up ticket from a finished ticket. |
| | `create_ticket_on_failure(make)` | Enqueue a retry for a ticket that failed. |
| **Rewrite replies** | `edit_replies_on_event(editor)` | Rewrite a ticket's replies before its next request. |
| | `edit_replies_on_compaction(editor)` | Decide what compaction does with a ticket's replies. |

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
| `agent_duration()` | Get the time agents spent working, summed across every agent and averaged per ticket. |
| `event_count(name)` | Get how many events of one kind were recorded, such as `"turn_started"`. |
| `input_tokens()` / `output_tokens()` | Get token counts across requests. |
| `tool_stats()` | Get per-tool call and failure counts. |
| `file_stats()` | Get per-filepath open and failure counts. |
| `knowledge_stats()` | Get per-operation attempt and failure counts. |
| `model_stats()` | Get per-model requests, failures, and token usage. |
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
<summary>Reading and writing pages</summary>

| Method | Description |
|--------|-------------|
| `index()` | Get the index, which is injected into the agent prompt. |
| `index_char_limit(count)` | Limit the index size. |
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
<summary>Session directory layout</summary>

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
