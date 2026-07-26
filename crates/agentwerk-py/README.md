<h1 align="center">agentwerk (Python)</h1>

<p align="center">
  <strong>Python bindings for agentwerk: a minimal library for running many agents in parallel.</strong>
</p>

<p align="center">
  agentwerk is a Rust crate; this package is a thin veneer over it, so the Python
  API mirrors the Rust API one to one. See the <a href="../../README.md">main README</a>
  for the project overview.
</p>

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

## Agents

An `Agent` picks up **tickets**, uses tools to solve them, and writes the result
back onto each ticket. The builder is fluent: each call returns the builder, and
`build()` produces the agent.

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
| `tool(t)` | Register a tool the agent may call. |
| `dir(d)` | Set the directory the agent works in. |

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
or `model_from_env()` on the builder.

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
        Ticket(f"Fetch {url} and extract pricing tiers, limits, and features.").label("research")
    )

tickets.ticket(
    Ticket("Rank all products by value for a 10-person engineering team.")
    .label("analysis")
    .schema(comparison_schema)
)
```

| Method | Description |
|--------|-------------|
| `agent(agent)` | Add an agent to this ticket system. |
| `task(t)` | Submit a task and return its ticket key. |
| `ticket(t)` | Submit a `Ticket` with custom labels, a schema, or a parent link. |

Also on `TicketSystem`: `dir(d)` to relocate persisted state, `reply(key, c)`
to continue a multi-turn conversation on one ticket.

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
| `finish_reason()` | Return why the most recent `finish()` returned, as a string: `"Drained"`, `"PolicyViolated(..)"`, or `"Cancelled"`. |

### Reacting to the run

Steer a run from the outside while agents work: end it early, or enqueue
follow-up work. Predicates receive the finished ticket (or result, or event) and
return a truthy value.

```python
# Fail fast: end the run at the first malicious verdict.
tickets.cancel_on_result(lambda result: result["verdict"] == "malicious")

# Verify every analysis finding with a follow-up ticket for the review pool.
def review_finding(ticket):
    if "analysis" in ticket["labels"]:
        return Ticket("Verify this finding.").parent(ticket["key"]).label("review")
    return None

tickets.create_ticket_on_result(review_finding)
```

| Method | Description |
|--------|-------------|
| `cancel_on_event(p)` | End the run when an event matches. |
| `cancel_on_result(p)` | End the run when a finished result matches. |
| `cancel_label(l)` | Call off one label's agents. |
| `create_ticket_on_result(make)` | Enqueue a follow-up ticket from a finished ticket. |
| `save_trajectory_on_event(p)` | Write a ticket's trajectory to disk on a matching event. |
| `await wait_for_ticket(p)` | Wait for one matching ticket instead of draining the queue. |

### Reading results

Query the system after `await finish()` returns. Results are native Python values
(`dict`, `list`, `str`, ...), not JSON strings:

```python
await tickets.finish()

answer = tickets.last_result()
if answer is not None:
    print(answer)

for ticket in tickets.tickets():
    print(f"{ticket['key']}: {ticket['status']}")
```

| Method | Description |
|--------|-------------|
| `last_result()` | Return the most recent finished ticket's result, or `None`. |
| `results()` | Return every finished ticket's result, in creation order. |
| `results_for_label(l)` | Return every finished ticket carrying the label's result. |
| `tickets()` | Return every ticket as a dict, in creation order. |
| `find_ticket(p)` | Return the earliest ticket matching the predicate. |
| `find_tickets(p)` | Return every ticket matching the predicate. |
| `get_ticket(key)` | Return one ticket by key, or `None`. |

### Inspecting tickets

Each ticket is returned as a dict carrying its recorded result, labels, and
lifecycle timestamps. The `status` field is one of `"Todo"`, `"InProgress"`,
`"Finished"`, or `"Failed"` (matching the persisted `tickets.jsonl`):

```python
ticket = tickets.find_ticket(lambda t: "analysis" in t["labels"])
report = ticket["result"]          # already a dict; no JSON parsing needed
print(report["title"])
```

Ticket dict fields: `key`, `status`, `task`, `result`, `labels`, `parent`,
`reporter`, and the four lifecycle timestamps (`created_at`, `started_at`,
`finished_at`, `failed_at`).

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

tickets.ticket(Ticket("Write a report.").schema(schema))
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
| | `GrepTool()` | Search file contents. |
| | `ListDirectoryTool()` | List files and directories. |
| | `CodegrepTool()` | Structural code search. |
| **Shell** | `BashTool(name, pattern)` | Run a shell command matching an allowed pattern. |
| **Web** | `FetchUrlTool()` | Fetch a URL and read its body. |
| **Tickets** | `ManageTicketsTool()` | Read the ticket queue and create or edit tickets. |
| | `ReadTicketsTool()` | Read the ticket queue. |

`FinishTool` and `ManageKnowledgeTool` are registered automatically on every
agent. `FinishTool` writes the result for the current ticket and marks it
finished, optionally handing follow-up work to another agent.

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
calls in the same turn. A raised exception is reported back to the model as a
recoverable error, and the run continues. Async functions (`async def`) work too.

## Knowledge

A `Knowledge` store is the agent's long-term memory. It is written to disk, can
be shared across multiple agents, and is curated by the agent through its
knowledge tool.

Each page is an Open Knowledge Format (OKF) v0.1 concept file. A compact index of
one-line descriptions goes into the system prompt, so the agent picks which pages
to read. Because the store is a plain OKF bundle, `Knowledge.load` can open one
authored elsewhere to seed an agent.

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
│       ├── ticket.json                   the ticket without its transcript
│       ├── replies.jsonl                 transcript
│       └── outputs/<tool_use_id>.txt     full tool outputs spilled out of the transcript
├── pages/<slug>.md                       knowledge pages
└── index.md                              knowledge index
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

## Not yet in the Python bindings

The bindings cover the full agent, ticket, tool, provider, knowledge, and event
surface. A few Rust-only pieces are not exposed yet: run `Stats`
(`tickets.stats()`), the `on_failure` retry-message hook, `cancel_on(future)`,
and `cancel_label_on_event`. Open an issue if you need one.

## Development

Build and install the bindings from source with maturin:

```bash
make python        # maturin develop, from the repo root
make python_test   # build, then run the pytest suite
```

See the [main README](../../README.md) for the project overview and
[DEVELOPMENT.md](../../DEVELOPMENT.md) for the workspace layout.
