<h1 align="center">agentwerk (Python)</h1>

<p align="center">
  <strong>agentwerk: A minimal library for running many agents in parallel.</strong>
</p>

<p align="center">agentwerk is designed to tackle complex problems with fleets of agents through the simplest interface possible. It provides a ticket system which distributes tasks across agents running in parallel, validates results, retries on failure, and reports every step as an event.</p>

<p align="center"><em>agentwerk pairs "agent" with the German "Werk", a word for both factory and artwork: machinery for building agentic systems.</em></p>

---

## Installation

```bash
pip install agentwerk
```

Configure an LLM provider through environment variables (`ANTHROPIC_API_KEY`,
`OPENAI_API_KEY`, `MISTRAL_API_KEY`, or `LITELLM_API_KEY`), the same as the Rust
crate.

## Quick Start

```python
import asyncio
from agentwerk import Agent, ReadFileTool, GrepTool


async def main():
    agent = (
        Agent()
        .from_env()
        .role("You are a Rust developer who explores source files to answer questions.")
        .tool(ReadFileTool())
        .tool(GrepTool())
        .build()
    )

    agent.task("Find every `pub trait` defined under src/ and explain each in one sentence.")
    work = await agent.finish()

    print(work.last_result())


asyncio.run(main())
```

> The run is driven by `await agent.finish()`, so every snippet below that calls
> `finish()`, `wait_for_ticket()`, or reads results runs inside an `async`
> function like this one.

# API

- [Agents](#agents): Pick up tickets and produce results.
- [Tickets](#tickets): Coordinate complex work across agents.
- [Prompting](#prompting): Role, context, and task shaping the work of an agent.
- [Tools](#tools): Capabilities agents use to solve a ticket.
- [Knowledge](#knowledge): Durable memory agents share across tickets and runs.
- [Sessions](#sessions): Working directory layout and how to reopen a run.
- [Events](#events): Lifecycle events emitted while agents work.
- [Stats](#stats): Statistics for tickets, tokens, and activity.
- [Differences](DIFFS.md): The Rust API and this one, side by side.

## Agents

An `Agent` picks up **tickets**, uses tools to solve them, and writes the result
back onto each ticket. `Agent()` opens the fluent configuration: each call
returns the agent, and `build()` arms it. Configuring an agent after `build()`,
or building it twice, is rejected. Rust splits this across `AgentBuilder` and
`Agent`, one of the [differences](DIFFS.md) between the two APIs.

```python
from agentwerk import Agent, ReadFileTool

agent = (
    Agent()
    .name("agent_0")
    .label("math")
    .tool(ReadFileTool())
)
```

| Method | Description |
|--------|-------------|
| `name(s)` | Set an identifier for assigning tickets. |
| `label(l)` / `labels([..])` | Restrict the agent to tickets carrying matching labels. |
| `tool(t)` / `tools([..])` | Register a tool the agent may call. |
| `dir(d)` | Set the directory the agent works in. |
| `edit_directive_on_failure(f)` | Reword the retry message agentwerk sends when the model stalls or returns invalid output. |

`role` and `context` are covered under [Prompting](#prompting); `knowledge(store)`
under [Knowledge](#knowledge).

### Providers

A `Provider` connects the agent to an LLM service. agentwerk ships providers for
Anthropic, OpenAI, Mistral, and a LiteLLM proxy.

```python
from agentwerk import Agent, AnthropicProvider

agent = (
    Agent()
    .provider(AnthropicProvider(key))
    .model("claude-sonnet-4-20250514")
)

# Or pick from environment variables.
agent = Agent().from_env()
```

Each provider constructor takes the API key and an optional `base_url` to
override the endpoint (useful for proxied or self-hosted OpenAI-compatible
models):

```python
from agentwerk import OpenAiProvider

provider = OpenAiProvider(key, "https://my-endpoint.example/v1")
```

| Method | Description |
|--------|-------------|
| `provider(p)` | Set the LLM provider. |
| `model(m)` | Set the model the provider runs. |
| `from_env()` | Detect provider and model in one call. |

To read only the provider from the environment (and set the model explicitly),
or only the model (and set the provider explicitly), use `provider_from_env()`
or `model_from_env()` on the agent.

### Models

`.model(m)` accepts a model name or a `Model`. Names registered with a known
provider resolve to a context window automatically. For private or proxied
models, build a `Model` and pass an explicit window so automatic compaction
stays active:

```python
from agentwerk import Agent, Model

agent = Agent().model(Model("my-local-model").context_window(128_000))
```

Set `reasoning_effort` on a reasoning-capable model to make it think before
answering. The thinking is recorded in the ticket transcript alongside the
answer. It is off unless you set it. Effort is one of `"off"`, `"low"`,
`"medium"`, or `"high"`:

```python
from agentwerk import Agent, Model

agent = Agent().model(Model("claude-sonnet-4-6").reasoning_effort("high"))
```

## Tickets

The `TicketSystem` coordinates collaboration between agents. A `task` is the work
itself; a `Ticket` wraps it with metadata like labels and schemas. Labels assign
work to matching agents.

```python
from agentwerk import Agent, Ticket, TicketSystem, FetchUrlTool

tickets = TicketSystem()

for i in range(4):
    tickets.agent(
        Agent()
        .name(f"researcher_{i}")
        .label("research")
        .from_env()
        .tool(FetchUrlTool())
        .build()
    )

tickets.agent(
    Agent().name("analyst").label("analysis").from_env().build()
)

for url in pricing_pages:
    tickets.ticket(
        Ticket(
            f"Fetch {url} and extract pricing tiers, limits, and features.",
            labels=["research"],
        )
    )

tickets.ticket(
    Ticket(
        "Rank all products by value for a 10-person engineering team.",
        labels=["analysis"],
        schema=comparison_schema,
    )
)
```

| Method | Description |
|--------|-------------|
| `agent(agent)` | Add an agent to this ticket system. |
| `task(t)` | Submit a task and return its ticket key. |
| `ticket(t)` | Submit a `Ticket` with custom labels, a schema, or a parent link. |

Also on `TicketSystem`: `dir(d)` to relocate persisted state, `reply(key, c)`
to continue a multi-turn conversation on one ticket, and `set_failed(key)` to
fail a ticket from outside the run.

### Execution

Start, wait, and cancel a run:

```python
tickets.start()
await tickets.finish()
answer = tickets.last_result()
```

| Method | Description |
|--------|-------------|
| `start()` | Begin processing tickets in the background. |
| `await finish()` | Process every queued ticket and return. |
| `cancel()` | Cancel the run. |
| `finish_reason()` | Return why the most recent `finish()` returned, as a string: `"drained"`, `"policy_violated(..)"`, or `"cancelled"`. |

### Reacting to events

React to events as they arrive: stop early, call off one label's agents, queue
follow-up work, or read each ticket as it finishes. Predicates receive the
finished ticket (or result, or event) and return a truthy value.

```python
# Fail fast: end the run at the first malicious verdict.
tickets.cancel_on_result(lambda result: result["verdict"] == "malicious")

# Verify every analysis finding with a follow-up ticket for the review pool.
def review_finding(ticket):
    if ticket.has_label("analysis"):
        return Ticket("Verify this finding.", labels=["review"], parent=ticket.key)
    return None

tickets.create_ticket_on_result(review_finding)

# Keep the messages of every finished ticket as a training example.
def capture(event, ticket):
    if event.kind == "ticket_finished":
        model = tickets.model_for_agent(event.agent_name)
        Trajectory.from_ticket(event.agent_name, model, ticket).save("datasets")

tickets.on_ticket(capture)
```

| Method | Description |
|--------|-------------|
| `cancel_on_event(p)` | End the run when an event matches. |
| `cancel_on_result(p)` | End the run when a finished result matches. |
| `cancel_on(awaitable)` | End the run when an awaitable resolves. |
| `cancel_label(l)` | Call off one label's agents. |
| `cancel_label_on_event(l, p)` | Call off one label's agents when an event matches. |
| `create_ticket_on_result(make)` | Enqueue a follow-up ticket from a finished ticket. |
| `on_ticket(h)` | Read a ticket when it starts, finishes, or fails. |
| `await wait_for_ticket(p)` | Wait for one matching ticket instead of draining the queue. |
| `edit_replies_on_event(f)` | Rewrite a ticket's replies before its next request. One editor at a time: a second replaces it. |
| `edit_replies(key, f)` | Rewrite one ticket's replies now. |

An editor receives a list of `Reply` and returns the new list, or `None` to
leave them alone. Build a new one with `Reply.user_text(text)`. Each `Reply` has
`author`, `created_at`, and a `content` list whose entries carry a `kind` and a
`data` dict. Keep each tool call paired with its result: the model rejects a
conversation missing one half.

```python
from agentwerk import Reply

tickets.edit_replies(key, lambda replies: replies + [Reply.user_text("Try again.")])
```

### Reading results

Query the system after `await finish()` returns. Results are native Python values
(`dict`, `list`, `str`, ...), not JSON strings:

```python
await tickets.finish()

answer = tickets.last_result()
if answer is not None:
    print(answer)

for ticket in tickets.tickets():
    print(f"{ticket.key}: {ticket.status}")
```

| Method | Description |
|--------|-------------|
| `last_result()` | Return the most recent finished ticket's result, or `None`. |
| `results()` | Return every finished ticket's result, in creation order. |
| `results_for_label(l)` | Return every finished ticket carrying the label's result. |
| `tickets()` | Return every ticket, in creation order. |
| `find_ticket(p)` | Return the earliest ticket matching the predicate. |
| `find_tickets(p)` | Return every ticket matching the predicate. |
| `get_ticket(key)` | Return one ticket by key, or `None`. |
| `model_for_agent(name)` | Return the model that agent runs, or `None`. |
| `stats()` | Return the run statistics described under [Stats](#stats). |

### Inspecting tickets

Each `Ticket` carries the recorded result, its messages, and lifecycle
timestamps as attributes. Read them directly; a structured result is already a
dict:

```python
ticket = tickets.find_ticket(lambda t: t.has_label("analysis"))
print(ticket.result["title"])
```

Attributes: `key`, `status`, `task`, `result`, `labels`, `schema`, `parent`,
`reporter`, `replies`, and the four lifecycle timestamps (`created_at`,
`started_at`, `finished_at`, `failed_at`). `status` is `"todo"`,
`"in_progress"`, `"finished"`, or `"failed"`, matching the persisted
`tickets.jsonl`. Six predicates read better than comparing that string:
`is_todo()`, `is_in_progress()`, `is_finished()`, `is_failed()`,
`is_pending()` (todo or in progress), and `is_resolved()` (finished or
failed). `has_label(l)` does the same for `labels`.

To build a ticket, pass the settable fields to the constructor:
`Ticket(task, labels=[..], schema=s, parent=key)`.

### Policies

Configure execution policies on a ticket system. A breach emits a
`policy_violated` event and halts execution. Durations are in seconds:

```python
tickets = TicketSystem()
(
    tickets
    .max_turns(40)
    .max_time(300.0)
    .max_input_tokens(200_000)
    .max_output_tokens(50_000)
)
```

| Method | Description |
|--------|-------------|
| `max_turns(n)` | Limit the total number of turns. |
| `max_time(seconds)` | Limit the total elapsed duration, in seconds. |
| `max_input_tokens(n)` | Limit the total input tokens. |
| `max_output_tokens(n)` | Limit the total output tokens. |

Also on `TicketSystem` for retry and per-request limits: `max_schema_retries`,
`max_request_retries`, `request_retry_delay` (seconds), `max_request_tokens`.

### Schemas

A `Schema` constrains the result an agent must produce for a ticket. A violation
triggers a retry until `max_schema_retries` is exhausted.

```python
from agentwerk import Schema, Ticket

schema = Schema({
    "type": "object",
    "properties": {"title": {"type": "string"}},
    "required": ["title"],
})

tickets.ticket(Ticket("Write a report.", schema=schema))
```

Register a schema per label with `tickets.schema_for_label(label, schema)`: every
ticket of that label validates against it.

### Compaction

agentwerk compacts the transcript automatically when the model's context window
is near full; observe progress via the `compaction_started`,
`compaction_progress`, `compaction_finished`, and `compaction_failed` event
kinds.

## Prompting

Every prompt has three parts: `role` (who the agent is), `context` (the situation
it operates in), and `task` (work it should perform). `role` and `context` are
set on the agent; the task body arrives per ticket via `tickets.task()`. The
structure follows the [prompting guide](https://github.com/canvascomputing/prompting).

```python
agent = (
    Agent()
    .role("You are an arithmetic agent. Compute step by step and show your work.")
    .context("- Stage 2 of a math-tutor pipeline.\n- Attempts remaining: 2.")
    .template_variable("divisor", "8")
    .from_env()
    .build()
)

tickets.agent(agent)
tickets.task("Compute (47 * 92) / {divisor}, then round to the nearest integer.")
```

When `context(...)` is not set, agentwerk supplies a default block with the ticket
key, date, directory, platform, and remaining budgets.

## Tools

Give agents access to tools. Each tool exposes an action the agent can choose to
take. Built-in tools are constructed and passed to `.tool(...)`:

| | Tool | Description |
|-|------|-------------|
| **File** | `ReadFileTool()` | Read a file with line numbers, offset, and limit. |
| | `WriteFileTool()` | Create or overwrite a file. |
| | `EditFileTool()` | Replace text in a file. |
| **Search** | `GlobTool()` | Find files by pattern. |
| | `GrepTool()` | Search file contents by regex or code shape. |
| | `ListDirectoryTool()` | List files and directories. |
| **Shell** | `BashTool(name, pattern)` | Run a shell command matching an allowed pattern. |
| **Web** | `FetchUrlTool()` | Fetch a URL and read its body. |
| **Tickets** | `FinishTool()` | Write the result for the current ticket and mark it finished, optionally handing follow-up work to another agent. |
| | `ManageTicketsTool()` | Read the ticket queue and create or edit tickets. |
| | `ReadTicketsTool()` | Read the ticket queue. |
| **Knowledge** | `ManageKnowledgeTool(store)` | Write, read, remove, or list pages in a knowledge store. |
| **Discovery** | `FindToolsTool()` | Look up the tools held back until they are needed. |

`FinishTool()` and `ManageKnowledgeTool(store)` are registered automatically on
every agent built with `Agent()`. `knowledge(store)` is the usual way to
choose the store, since it also renders the store's index into the system prompt.
`Agent.empty()` opens an agent without the finish tool, for agents that only
read.

Registering a tool replaces any tool already registered under the same name, so
a later `tool(...)` wins over an automatic one.

### Bash

`BashTool` restricts execution to commands matching a glob pattern. The first
argument names the tool the model sees; the second is the allowed pattern.

```python
from agentwerk import Agent, BashTool, UnrestrictedBashTool

agent = (
    Agent()
    .tool(BashTool("git", "git *"))
    .tool(UnrestrictedBashTool())
)
```

`UnrestrictedBashTool()` removes the pattern check.

### Custom tools

Define custom tools with the `@tool` decorator. The tool input object is passed to
the function as keyword arguments; the return value is sent back to the model.
Declare a JSON-Schema for the inputs with `schema=`:

```python
from agentwerk import tool

@tool(read_only=True, schema={
    "type": "object",
    "properties": {"name": {"type": "string"}},
    "required": ["name"],
})
def greet(name: str) -> str:
    """Say hello."""
    return f"Hello, {name}!"

agent = Agent().tool(greet)
```

`read_only=True` allows the agent to run a tool concurrently with other read-only
calls in the same turn. `defer=True` holds the tool back until the agent looks it
up with `FindToolsTool()`, which keeps a large tool set out of every request.
`paths=["path"]` names the input fields carrying a file path, so the files the
tool opens show up in `Stats.file_stats()`. Async functions (`async def`) work
too.

A raised exception is reported back to the model as a recoverable error and the
run continues. Return a `ToolResult` to say so without raising:
`ToolResult.error(msg)` for a failure the model should work around, or
`ToolResult.schema_error(msg)` for input that did not match the tool's schema,
which counts against `max_schema_retries`.

## Knowledge

A `Knowledge` store is the agent's long-term memory. It is written to disk, can
be shared across multiple agents, and is curated by the agent through its
knowledge tool.

Each page is an Open Knowledge Format (OKF) v0.1 concept file. A compact index of
one-line descriptions goes into the system prompt, so the agent picks which pages
to read.

```python
from agentwerk import Agent, Knowledge

# Open a store and share it across agents:
store = Knowledge.load("./.agentwerk")
alice = Agent().knowledge(store)
bob = Agent().knowledge(store)

# Raise the rendered-index char budget (default 12 000):
store = Knowledge.load("./.agentwerk").index_char_limit(24_000)
agent = Agent().knowledge(store)
```

Seed or inspect the store yourself through `pages()`:

```python
from agentwerk import Knowledge, Page

store = Knowledge.load("./.agentwerk")
store.pages().save(
    Page("build-command", "How the project is built.", "Run `make` to compile.")
)

page = store.pages().load("build-command")
store.pages().remove("build-command")
```

| Method | Description |
|--------|-------------|
| `index()` | Return the rendered index the agent sees. |
| `index_char_limit(n)` | Limit the rendered index, in characters. |
| `pages()` | Return the page collection for reading and writing pages. |
| `clear()` | Remove every page from the store. |

## Sessions

A `TicketSystem` writes every ticket, transcript, statistic, and lifecycle event
to its working directory (default `./.agentwerk`). That directory is the session:
stop the process, and `TicketSystem.load(dir)` reopens it from disk and continues
from where it stopped.

```python
tickets = TicketSystem.load(".agentwerk")
tickets.agent(my_agent)
tickets.start()
```

Layout:

```
.agentwerk/
├── stats.json                            run statistics
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

## Events

Events report everything that happens while your agents work. Log them, display
them, or react to them. An `Event` carries `kind` (a snake_case string like
`"ticket_finished"`), `agent_name`, `ticket_key`, and a `data` dict with the
variant's payload.

```python
def log(event):
    if event.kind == "ticket_finished":
        print(f"[{event.agent_name}] done {event.ticket_key}")

tickets.on_event(log)
```

| | Kind | Description |
|-|------|-------------|
| **Ticket** | `ticket_started` | An agent claimed a ticket. |
| | `ticket_finished` | A ticket finished successfully. |
| | `ticket_failed` | A ticket failed. |
| **Provider** | `request_finished` | A provider request finished and reported its token usage. |
| | `request_retried` | A transient provider error triggered a retry. |
| **Tool** | `tool_call_finished` | A tool invocation finished. |
| | `tool_call_failed` | A tool invocation failed but the ticket continues. |
| **Compaction** | `compaction_started` | Compaction is about to summarize the conversation tail. |
| | `compaction_finished` | Compaction finished and replaced the tail with a summary. |
| **Run** | `policy_violated` | A policy limit was breached and execution stopped. |

Also: `run_started`, `run_finished`, `turn_started`, `request_started`,
`request_failed`, `text_chunk_received`, `tool_call_started`,
`file_open_finished`, `file_open_failed`, `knowledge_used`, `knowledge_missed`,
`schema_retried`, `compaction_progress`, `compaction_failed`.

Every category the bindings turn into a string uses the same snake_case form,
whether it names an event, a status, or a payload field:

| Where | Values |
|-------|--------|
| `event.kind` | The names listed above. |
| `ticket.status` | `"todo"`, `"in_progress"`, `"finished"`, `"failed"`. |
| `finish_reason()` | `"drained"`, `"cancelled"`, `"policy_violated(kind)"`. |
| `data["policy"]` on `policy_violated` | `"turns"`, `"input_tokens"`, `"output_tokens"`, `"max_schema_retries"`, `"time"`. |
| `data["reason"]` on a request or tool failure | `"rate_limited"`, `"connection_failed"`, `"tool_not_found"`, and their siblings. |
| `data["reason"]` on compaction | `"proactive"`, `"reactive"`. |
| `data["op"]` on `knowledge_used` | `"write"`, `"read"`, `"remove"`, `"list"`. |

## Stats

`tickets.stats()` reports what a run did. Every duration is in seconds.

```python
stats = tickets.stats()
print(stats.requests(), stats.input_tokens(), stats.tickets_success_rate())

for name, stat in stats.tool_stats().items():
    print(name, stat.calls, stat.error_rate())
```

| Method | Description |
|--------|-------------|
| `run_duration()` | Return the run's elapsed duration. |
| `tickets_success_rate()` | Return `finished / (finished + failed)`. |
| `input_tokens()` / `output_tokens()` | Return token totals across responses. |
| `tool_stats()` | Return per-tool call and failure counts, broken down by failure kind. |
| `file_stats()` | Return per-path open and failure counts for the files tools opened. |
| `knowledge_stats()` | Return Knowledge-store usage: write, read, remove, list, and miss counts. |
| `event_counts()` | Return per-event counts keyed by event name. |
| `stats_for_label(label)` | Return a statistics slice scoped to one label. |
| `to_dict()` | Return the same numbers as one dict, matching `stats.json`. |

Also: `turns()`, `requests()`, `tool_calls()`, `errors()`, `tickets_created()`,
`tickets_finished()`, `tickets_failed()`, `total_ticket_duration()`,
`avg_ticket_duration()`, `total_work_duration()`, `avg_work_duration()`, and
`usage_history(ticket_key)`.

## Development

Build and install the bindings from source with maturin:

```bash
make python        # maturin develop, from the repo root
make python_test   # build, then run the pytest suite
```

`examples/divide_and_conquer.py` runs a full multi-agent workflow against a real
provider: labelled tickets, a schema-validated result, a custom `@tool`, and the
run statistics at the end.

```bash
python examples/divide_and_conquer.py 200 4 2
```

See the [main README](../../README.md) for the project overview and
[DEVELOPMENT.md](../../DEVELOPMENT.md) for the workspace layout.
