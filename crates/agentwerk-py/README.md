<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/logo.png" width="200" />
</div>

<h1 align="center">agentwerk (Python)</h1>

<div align="center">
  <strong>A minimal agentic loop written in Rust & Python for building efficient harnesses.</strong>
</div>

<div align="center">
  <a href="#installation">Installation</a> •
  <a href="crates/agentwerk-py/README.md">Python</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#api">API</a> •
  <a href="#use-cases">Use Cases</a> •
  <a href="#security">Security</a> •
  <a href="#development">Development</a>
</div>

<div align="center">agentwerk is a lightweight agentic loop optimized for small and fast LLMs: parallel agents, ticket-based coordination, built-in tools, schema-validated results, shared knowledge, and an event for every step.</div>

---

<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/demo.gif" width="800" />
</div>
<div align="center"><a href="https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/apparat_fabrik.py">Apparat Fabrik</a></div>
<div align="center"><em>agentwerk pairs "agent" with the German "Werk", a word for both factory and artwork: machinery for building agentic systems.</em></div>

---

## Why use agentwerk?

- **Simple interface:** create agents with a few lines of code.
- **Efficient harness:** optimized for small and fast LLMs with low memory footprint.
- **Complex interactions:** allow agents to collaborate through queues, event hooks and shared knowledge.
- **Deep observability:** inspect every request, tool call, and failure.
- **Facilitate training:** store trajectories based on granular events for fine-tuning models.

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

    agent.ticket(
        "Find every `pub trait` defined under src/ and explain each in one sentence."
    )

    work = agent.start()
    result = await work.finish_last()

    print(result)


asyncio.run(main())
```

## API

- [Agents](#agents): Define roles, behavior and tasks.
- [Tickets](#tickets): Coordinate complex work across agents.
- [Tools](#tools): Define accessible tooling.
- [Events](#events): Inspect requests, tool usage, failures and more.
- [Knowledge](#knowledge): Let agents share notes for collaboration.

## Agents

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/agents.gif" width="600" />
</div>

An `Agent` is the core entity of agentwerk. It has access to tools for solving tasks in the form of tickets.

```python
from agentwerk import Agent, ReadFileTool

agent = (
    Agent.from_env()
    .role("You are a release manager who prepares release notes.")
    .tool(ReadFileTool())
    .build()
)

agent.ticket("Read CHANGELOG.md and summarize the entries added since the last release.")

agent.start()
```

<details>
<summary>All agent methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Configure** | `role(role)` | Define who the agent is and how it should work. |
| | `tool(tool)` / `tools(tools)` | Register a tool the agent may call. |
| | `label(label)` | Restrict the agent to tickets carrying this label. |
| | `dir(dir)` | Set the directory the agent has access to. |
| | `template(key, value)` | Inject data into prompts with template strings. |
| | `templates(variables)` | Inject more than one entry into prompts. |
| | `knowledge(store)` | Share a knowledge store with the agent. |
| | `interactive()` | Let the agent wait for new instructions to keep a ticket in-progress. |
| | `build()` | Create the agent. |
| **Work** | `ticket(task)` | Submit a task, or a `Ticket` carrying a label or schema, and return its ticket key. |
| | `start()` | Begin processing tickets. |
| | `id` | Get the unique identifier of an agent. |

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

#### Interactive

An interactive agent holds one ticket open across many turns, so a conversation spans a whole session.

```python
def show(work, ticket, result):
    print(f"{ticket.key}: {result}")


agent = Agent.from_env().interactive().build()
key = agent.ticket("Where does the configuration get loaded?")

chat = agent.start()
chat.on_result(show)
await chat.finish_all()

chat.reply(key, "And which environment variables override it?")
await chat.finish_all()

chat.set_finished(key, "answered")
```

An interactive agent never finishes its own ticket, because that would end the conversation. Every answer pauses the ticket instead: it stays `InProgress` with its agent, and each `await chat.finish_all()` returns on the answer it waited for. `reply(key, content)` drives the next turn, and `set_finished(key, result)` ends the conversation, which is the result the hook reports. The answers in between arrive as [events](#events).

See more: [`AgentBuilder`](https://docs.rs/agentwerk/latest/agentwerk/agents/agent/struct.AgentBuilder.html).

</details>

### Providers

A `Provider` gives agents access to LLMs: Anthropic, OpenAI, Mistral, and a LiteLLM proxy.

```python
from agentwerk import Agent, Anthropic

agent = (
    Agent()
    .provider(Anthropic(key))
    .model("claude-sonnet-4-20250514")
)
```

<details>
<summary>All provider and model settings</summary>

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

You can configure models to set a custom context window size or the applied reasoning. Claude, GPT, Mistral, and Qwen families are pre-configured.

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

Configure a custom model:

```python
from agentwerk import Agent, Model

agent = Agent().model(
    Model("my-local-model").context_window(128_000).reasoning_effort("high")
)
```

See [`Provider`](https://docs.rs/agentwerk/latest/agentwerk/providers/struct.Provider.html) and [`Model`](https://docs.rs/agentwerk/latest/agentwerk/providers/struct.Model.html).

</details>

## Tickets

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/tickets.gif" width="600" />
</div>

The `TicketQueue` is the core data structure of agentwerk for coordinating complex interactions.

```python
from agentwerk import Agent, Ticket, TicketQueue

analyst = (
    Agent.from_env()
    .label("analysis")
    .build()
)

writer = (
    Agent.from_env()
    .label("report")
    .build()
)

tickets = TicketQueue()
tickets.agent(analyst).agent(writer)

tickets.ticket(Ticket("Rank all products by value.", label="analysis"))
tickets.ticket(Ticket("Write up the ranking.", label="report"))
```

<details>
<summary>All ticket methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Configure** | `agent(agent)` | Add an agent to this ticket queue. |
| | `schemas(store)` | Enforce schemas for ticket results. |
| | `dir(dir)` | Define where a session is stored. |
| | `get_dir()` | Get the session directory. |
| **Submit** | `ticket(task)` | Submit a task, or a `Ticket` carrying a label or schema, and return its ticket key. |
| **Read** | `results()` | Get the result of every finished ticket, in creation order. |
| | `find_results(query)` | Get every result whose ticket matches an AQL query. |
| | `find_result(query)` | Get the first result whose ticket matches an AQL query. |
| | `tickets()` | Get every ticket in creation order. |
| | `find_ticket(query)` | Get the first ticket matching an AQL query. |
| | `find_tickets(query)` | Get every ticket matching an AQL query. |
| | `get_ticket(key)` | Get one ticket by key. |
| **Drive** | `reply(key, content)` | Add a reply to a ticket. |
| | `edit_replies(key, editor)` | Rewrite a ticket's replies now. |
| **Resolve** | `set_finished(key, result)` | Finish a ticket with a result. |
| | `set_failed(key)` | Fail a ticket. |

See [`TicketQueue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketQueue.html).

</details>

### Queries

You can query tickets with AQL, the agentwerk query syntax.

```python
tickets.find_tickets("scan")
tickets.find_results("TICKET-3")
tickets.find_tickets("key IN (TICKET-3, TICKET-4)")
tickets.find_tickets("label IN (scan, report) AND status = finished")
tickets.find_results("scan ORDER BY finished DESC")
```

<details>
<summary>All query terms</summary>

#### Terms

| | Term | Description |
|-|------|-------------|
| **Match** | `label = scan` | Select the tickets carrying the label `scan`. |
| | `label != scan` | Exclude that label, and every ticket carrying none. |
| | `label IN (scan, report)` | Select the tickets carrying either label. |
| | `label NOT IN (scan, report)` | Exclude both labels. |
| | `label IS EMPTY` | Select the tickets carrying no label. |
| | `label IS NOT EMPTY` | Select the tickets carrying one. |
| **Search** | `task ~ "retry budget"` | Search the task body, ignoring case. |
| | `task !~ draft` | Exclude the tasks the text appears in. |
| **Compare** | `failed > -1h` | Select the tickets that failed inside the last hour. |
| | `created >= 2026-08-24` | Select the tickets submitted on that date or later. |
| **Combine** | `A AND B` | Require both terms; `AND` binds tighter than `OR`. |
| | `A OR B` | Require either term. |
| | `NOT A` | Invert a term or a group. |
| | `(A OR B) AND C` | Group terms with parentheses. |
| **Shorten** | `scan` | Select the label `scan`, the short form of `label = scan`. |
| | `TICKET-3` | Select one ticket by key, the short form of `key = TICKET-3`. |
| **Sort** | `ORDER BY finished DESC` | Answer with the most recently finished first. |
| | `ORDER BY created` | Answer in creation order, which `ASC` also says. |

#### Fields

What a field holds decides the operators it takes.

| | Field | Description |
|-|-------|-------------|
| **Value** | `key` | Match the ticket key, of the form `TICKET-N`. |
| | `label` | Match the label the ticket carries. |
| | `status` | Match `todo`, `in_progress`, `finished`, or `failed`. |
| | `agent` | Match the agent that claimed the ticket. |
| | `parent` | Match the ticket a handover came from. |
| **Text** | `task` | Search the work the agent was asked to do. |
| | `result` | Search the result the agent produced. |
| | `errors` | Search the failures recorded against the ticket. |
| **Time** | `created` | Compare or sort by when the ticket was submitted. |
| | `started` | Compare or sort by when an agent claimed the ticket. |
| | `finished` | Compare or sort by when the ticket reached the `finished` status. |
| | `failed` | Compare or sort by when the ticket reached the `failed` status. |

#### Rules

- A value field takes `=`, `!=`, `IN`, and `NOT IN`, which match exactly.
- A text field takes `~` and `!~`, which ignore case.
- A time field takes `>`, `>=`, `<`, and `<=`.
- `IS EMPTY` and `IS NOT EMPTY` read the eight fields a ticket can leave unset, every one but `key`, `status`, `task`, and `created`. So `finished IS EMPTY` selects the tickets still open.
- A field holds one value per ticket, so `label = a AND label = b` is rejected and names `IN` as the fix.
- A compared moment is a `YYYY-MM-DD` date at midnight UTC, an offset back from now spelled `-30m`, `-2h`, `-7d`, or `-1w`, or milliseconds since the epoch. An offset is resolved when the query compiles, so one query answers one set however long it is held.
- `ORDER BY` names one field and closes the query. It may be the whole query, which then selects every ticket.
- Every field sorts, `key` by its number and `status` along the lifecycle. A ticket missing the field sorts last.
- Without `ORDER BY` tickets arrive in creation order, which also breaks a tie.
- A string that does not compile raises `ValueError`, as does `Query(query)`, which compiles one without running it.

#### Examples

Read the results of finished tickets:

```python
tickets.find_result("TICKET-3")                   # one ticket's result
tickets.find_results("report AND result ~ risk")  # reports that mention risk
```

Select the tickets themselves:

```python
tickets.find_ticket("task ~ migration")                    # the first migration ticket
tickets.find_tickets("errors ~ tool_call_failed")          # saw a tool call fail
tickets.find_tickets("status = todo AND agent IS EMPTY")   # waiting, never claimed
tickets.find_tickets("failed > -1h ORDER BY failed DESC")  # the last hour's failures
tickets.find_tickets("(scan OR audit) AND NOT status = failed")
```

Every method that takes a query also takes a callable, for a condition no field carries:

```python
tickets.find_tickets(lambda t: len(t.replies) > 4)
```

</details>

### Execution

The ticket queue schedules the work of your agents and returns their results.

```python
tickets.start()

answer = await tickets.finish_last()
if answer is not None:
    print(answer)
```

<details>
<summary>All execution methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Run** | `start()` | Begin processing tickets. |
| **Wait** | `await finish(query)` | Wait for the matching tickets to be done and get their results. |
| | `await finish_all()` | Wait for every ticket to be finished and get every result. |
| | `await finish_last()` | Wait for every ticket to be finished and get the last result. |
| | `finish_reason()` | Get why the last run ended. |
| **Stop** | `cancel(query)` | Stop work on the matching tickets. |
| | `cancel_all()` | Stop work on every ticket. |
| | `is_cancelled(ticket)` | Check whether a ticket has been cancelled. |

Ticket members:

| | Member | Description |
|-|--------|-------------|
| **Identity** | `key` | Ticket key, of the form `TICKET-N`. |
| | `task` | The work the agent is asked to do. |
| | `label` | Label carried by the ticket. |
| | `parent` | Identifier of the parent ticket if a handover was performed. |
| | `reporter` | Identifier of the agent that created the ticket. |
| | `assignee` | Identifier of the agent that claimed the ticket. |
| **Outcome** | `status` | The ticket lifecycle status. |
| | `result` | The result the agent produced. |
| | `errors` | The failures recorded against the ticket, as events. |
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

### Handover

Agents can share the results of their work in the following ways:

1. **Create tickets**: the `finish` tool's `handover` option opens a child ticket carrying the result.
2. **Read tickets**: the `tickets` tool allows reading any finished ticket's result, by key.
3. **Read result file**: the `read_file` tool allows reading a ticket's `result.json` in the session directory.
4. **Share knowledge**: the `knowledge` tool allows sharing knowledge with other agents.
5. **Register hooks**: the `on_result` hook allows creating follow-up tickets.

<details>
<summary>All ways agents pass data</summary>

#### 1. Create tickets

Name the receiving label in the role or in the task, and the agent hands over as it finishes:

```python
analyst = (
    Agent.from_env()
    .label("analysis")
    .role("Rank the products by value, then hand the ranking over to `report`.")
    .build()
)

writer = (
    Agent.from_env()
    .label("report")
    .role("Write the board report from the ranking you were handed.")
    .build()
)
```

The child ticket is filed under `report` and names the analysis ticket as its `parent`. Its body is the result that was handed over, unless the agent passes a task of its own, which may carry `{parent_key}`, `{parent_result}`, and `{parent_result_path}`. Either way the body ends with the parent's key and the path of its result file.

#### 2. Read tickets

Give the writer `TicketsTool()`, and it reads what any finished ticket produced, by key:

```python
writer = Agent.from_env().label("report").tool(TicketsTool()).build()

writer.ticket("Read the result of TICKET-1, then write the board report.")
```

#### 3. Read result file

Give the writer `ReadFileTool()` instead, and it opens the result file named at the end of its ticket:

```python
writer = Agent.from_env().label("report").tool(ReadFileTool()).build()

writer.ticket("Read .agentwerk/tickets/TICKET-1/result.json, then write the board report.")
```

Results live in the session directory, one `result.json` per ticket.

#### 4. Share knowledge

Hand both agents one store, and either can write a page the other reads:

```python
store = Knowledge.load(".agentwerk")

analyst = Agent.from_env().label("analysis").knowledge(store).build()
writer = Agent.from_env().label("report").knowledge(store).build()

analyst.ticket("Rank the products by value, then save the ranking to your knowledge.")
```

#### 5. Register hooks

Use hooks to create new tickets when certain results arrived:

```python
def hand_to_report(work, done, result):
    if done.has_label("research"):
        work.ticket(Ticket(result, label="report"))


tickets.on_result(hand_to_report)
```

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
<summary>All schema methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Schema** | `Schema(document)` | Create a schema. |
| | `validate(value)` | Validate content. |
| **SchemaStore** | `SchemaStore()` | Create a store of schemas bound to labels. |
| | `label(label, document)` | Bind a schema to a label. |
| | `get(label)` | Read back the schema bound to a label. |
| | `tickets.schemas(store)` | Enforce schemas for ticket results. |

A `SchemaStore` enforces schemas for all tickets with a certain label. Registering schemas centrally spares agents from passing complex schema structures during ticket creation (see `TicketsTool`) and handovers (see `FinishTool`):

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

See [`Schema`](https://docs.rs/agentwerk/latest/agentwerk/schemas/struct.Schema.html) and [`SchemaStore`](https://docs.rs/agentwerk/latest/agentwerk/schemas/struct.SchemaStore.html).

</details>

### Configuration

A `Policy` limits the turns, tokens, and time a run may spend, and allows configuring retries and compaction.

```python
tickets.policy(Policy(max_turns=40, max_time=300.0))
```

<details>
<summary>All configuration fields</summary>

| Field | Description |
|-------|-------------|
| `max_turns` | Limit the total number of turns. |
| `max_time` | Limit the total elapsed duration. |
| `max_input_tokens` | Limit the total input tokens. |
| `max_output_tokens` | Limit the total output tokens. |
| `max_request_tokens` | Limit the output tokens of a single request. |
| `max_schema_retries` | Limit the consecutive turns without a valid tool call. |
| `max_request_retries` | Limit how often a failing request is retried. |
| `request_retry_delay` | Wait this long between retries. |
| `compaction_threshold` | Compact once the next request would fill this share of the window. |

`policy(policy)` replaces the whole configuration, and `get_policy()` reads it back. A violated limit emits a `policy_violated` event, see [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html). `compaction_threshold` is the exception, see [Compaction](#compaction).

</details>

### Compaction

Compaction summarizes a ticket's older messages once they no longer fit the model's context window.

```python
tickets.policy(Policy(compaction_threshold=0.7))
```

<details>
<summary>When compaction runs and what it reports</summary>

`compaction_threshold` is a fraction of the model's context window, `0.85` by default. Reaching it summarizes the older messages and the agent carries on.

Compaction also runs after the LLM provider reports the window exceeded. `compaction_started`, `compaction_progress`, `compaction_finished`, and `compaction_failed` report each step, see [Events](#events).

```python
def watch(work, event):
    if event.kind == "compaction_finished":
        print(f"[{event.ticket_key}] compacted {event.data['reason']}")


tickets.on_event(watch)
```

Each of the compaction events carries the reason it ran: `proactive` ahead of the failure, `reactive` after it. Replies that still exceed the window after a reactive compaction fail the ticket.

</details>

### Directives

A directive is used when a model fails to perform a specific task. It is a message for correcting the agent's behavior. Directives have been optimized with many hours of testing. Still, you can change them to your needs.

```python
from agentwerk import Agent, Directive


def tune(key):
    if key == Directive.GREP_FAILED:
        return "The search did not run. Narrow `path`."
    return None


agent = Agent.from_env().directives(tune).build()
```

<details>
<summary>All directive settings</summary>

| Method | Description |
|--------|-------------|
| `directives(compute)` | Decide every directive's text with one function. |

The function returns a directive template. So you can access template variables, like `{detail}`, `{attempt}`, and `{path}`.

See [prompts/directives](https://github.com/canvascomputing/agentwerk/tree/main/crates/agentwerk/src/prompts/directives) for the built-in text.

</details>

### Sessions

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/sessions.gif" width="600" />
</div>

A `TicketQueue` writes every ticket, reply, and event to its working directory (default `./.agentwerk`). You can continue a session from that directory.

```python
tickets = TicketQueue.load(".agentwerk")
tickets.agent(my_agent)
tickets.start()
```

<details>
<summary>All session files</summary>

```
.agentwerk/
├── events.jsonl                          every event (one per line)
├── tickets/
│   └── TICKET-1/
│       ├── ticket.json                   the ticket without its messages (key, status, label, timestamps)
│       ├── result.json                   the result the agent produced
│       ├── replies.jsonl                 every message exchanged with the model, one per line
│       └── outputs/<tool_use_id>.txt     full tool outputs spilled out of the messages
└── knowledge/
    ├── pages/<slug>.md                   knowledge pages
    └── index.md                          knowledge index
```

</details>

## Tools

Tools allow agents to perform their work.

```python
from agentwerk import Agent, CommandTool, GrepTool, ReadFileTool

agent = (
    Agent()
    .tool(ReadFileTool())
    .tool(GrepTool())
    .tool(CommandTool("git").allow("git *"))
)
```

<details>
<summary>All built-in and custom tools</summary>

| | Tool | Description |
|-|------|-------------|
| **File** | `ReadFileTool()` | Read a file with line numbers, offset, and limit. |
| | `WriteFileTool()` | Create or overwrite a file. |
| | `EditFileTool()` | Replace text in a file. |
| **Search** | `GlobTool()` | Find files by pattern. |
| | `GrepTool()` | Search file contents by regular expression, or by code shape with `syntax: "code"`. |
| | `ListDirectoryTool()` | List files and directories. |
| **Command** | `CommandTool(name)` | Give access to specific commands. |
| **Web** | `FetchUrlTool()` | Fetch a URL and read its body. |
| **Tickets** | `FinishTool()` | Write the result for the current ticket and mark it finished. |
| | `TicketsTool()` | Read the ticket queue and create or edit tickets. |
| **Knowledge** | `KnowledgeTool(store)` | Write, read, remove, or list pages in a knowledge store. |

#### `FinishTool` and `KnowledgeTool`

`FinishTool()` and `KnowledgeTool(store)` are special tools, registered automatically on every agent. They are used for interacting with the `TicketQueue` or knowledge base. An [interactive agent](#interactive) gets no `FinishTool()` by default, since finishing its ticket would end the conversation.

#### CommandTool

The `CommandTool` allows you to granularly define what commands are allowed and what commands are denied.

```python
git = (
    CommandTool("git")
    .allow("git status")
    .allow("git log *")
    .deny("git push*")
    .deny_flag("--force")
)
```

With an `allow_flag` set, a command carrying any other flag is refused:

```python
cargo = CommandTool("cargo").allow("cargo test*").allow_flag("--all-features")
```

#### FetchUrlTool

The `FetchUrlTool` fetches a URL and returns its text, requesting it with the user agent `agentwerk/<version>`. `impersonate()` swaps in the headers and HTTP/2 settings a browser sends.

```python
web = FetchUrlTool().impersonate()
```

#### Custom Tools

You can define custom tools for specific needs with the following parameters:

| Method | Description |
|--------|-------------|
| `concurrent=True` | If a tool has no side-effects you can run it in parallel with this option. |
| `paths=["path"]` | Name file path used for a tool call, so the files are included in statistics. |

Describe the tool, then hand it the code it runs:

```python
from agentwerk import tool


@tool(
    concurrent=True,
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

See [`Tool`](https://docs.rs/agentwerk/latest/agentwerk/tools/struct.Tool.html).

</details>

## Events

Events allow you to inspect all activities of your agents.

```python
def log(work, event):
    if event.kind == "ticket_finished":
        print(f"[{event.agent_id}] done {event.ticket_key} {event.label}")


tickets.on_event(log)
```

<details>
<summary>All event kinds and readers</summary>

| | Kind | Description |
|-|------|-------------|
| **Run** | `run_started` | Execution began. |
| | `run_finished` | Execution ended, carrying the reason. |
| | `policy_violated` | A limit was breached and execution stopped. |
| **Ticket** | `ticket_started` | An agent claimed a ticket. |
| | `ticket_finished` | A ticket finished successfully. |
| | `ticket_failed` | A ticket failed. |
| | `turn_started` | The agent began another turn on its ticket. |
| | `schema_retried` | A tool call or result the model created was invalid. |
| **LLM provider** | `request_started` | A request went out to the model. |
| | `request_finished` | A request finished and reported its token usage. |
| | `request_failed` | A request failed and was not retried. |
| | `request_retried` | A transient provider error triggered a retry. |
| | `text_chunk_received` | A piece of the reply arrived. |
| | `response_repaired` | A tool call or value the model created was invalid and was corrected. |
| **Tool** | `tool_call_declined` | A tool call proposed by the model was declined. |
| | `tool_call_started` | A tool invocation began. |
| | `tool_call_finished` | A tool invocation finished. |
| | `tool_call_failed` | A tool invocation failed but the ticket continues. |
| **File** | `file_open_finished` | A tool opened a file. |
| | `file_open_failed` | A tool could not open a file. |
| **Knowledge** | `knowledge_written` | A page was written. |
| | `knowledge_read` | A page was read. |
| | `knowledge_removed` | A page was removed. |
| | `knowledge_listed` | The pages were listed. |
| | `knowledge_failed` | An action against the store did not go through. |
| **Compaction** | `compaction_started` | Compaction is about to rewrite the older messages. |
| | `compaction_progress` | Compaction finished part of the work. |
| | `compaction_finished` | Compaction replaced the older messages. |
| | `compaction_failed` | Compaction could not finish. |

Every event is written to the session log. You read events from the ticket queue, or from the session directory in `.agentwerk/events.jsonl`:

| Method | Description |
|--------|-------------|
| `find_event(query)` | Get the earliest recorded event matching an AQL query, or the first in the order it names. |
| `find_events(query)` | Get every recorded event matching an AQL query, oldest first. |
| `input_tokens()` / `output_tokens()` | Get token counts across the run's requests. |
| `execution_duration()` | Get the elapsed execution duration. |

Events are queried in the same AQL a ticket is, over a field set of their own. A callable still works, and `EventQuery(query)` compiles a filter once for a long log.

```python
queue.find_events("tool_call_failed")
queue.find_events("event = request_finished AND agent = research-1")
queue.find_events("ticket = TICKET-3 ORDER BY created DESC")
queue.find_events("payload ~ timeout AND created > -1h")
```

| | Field | Description |
|-|-------|-------------|
| **Identity** | `event` | Match the kind, as `run_started`, `tool_call_failed`, and the rest are spelled. |
| | `agent` | Match the agent that emitted the event. |
| | `ticket` | Match the ticket the event concerns, empty on `run_started` and `run_finished`. |
| | `label` | Match the label that ticket carries. |
| **Body** | `payload` | Search what the kind carries, its name included. |
| **Time** | `created` | Compare or sort by when the event happened. |

The operators, the combinators, and `ORDER BY` are the ones [tickets take](#queries). `IS EMPTY` reads `agent`, `ticket`, and `label`; `~` and `!~` read `payload`. A lone word is the event where it names one and the label otherwise, and a lone `TICKET-N` is that ticket. Without an `ORDER BY` events arrive in the order they were logged.

See [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html) and [`TicketQueue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketQueue.html).

</details>

### Hooks

Hooks allow you to react to events.

```python
def triage(work, event, failed):
    if failed.has_label("scan"):
        work.ticket(Ticket(failed.task, label="triage"))


tickets.on_failure(triage)
```

<details>
<summary>All hooks</summary>

| | Method | Description |
|-|--------|-------------|
| **Observe** | `on_event(handler)` | Read every event as it is emitted. |
| | `on_result(handler)` | Read every finished ticket together with its result. |
| | `on_failure(handler)` | Read every failure together with the ticket it happened in. |
| | `on_ticket(handler)` | Read a ticket as it starts, finishes, or fails. |
| **Await** | `on_event_async(handler)` | Read every event in an async handler. |
| | `on_result_async(handler)` | Read every finished ticket with its result, in an async handler. |
| | `on_failure_async(handler)` | Read every failure with its ticket, in an async handler. |
| | `on_ticket_async(handler)` | Read a ticket lifecycle transition in an async handler. |

Save replies of every finished ticket as a training example:

```python
def capture(work, event, ticket):
    if event.kind == "ticket_finished":
        model = work.model_for_agent(event.agent_id)
        Trajectory.from_ticket(event.agent_id, model, ticket).save("datasets")


tickets.on_ticket(capture)
```

#### Async handlers

`on_result` is blocking and prevents an agent continuing its work till the hook is finished. If you perform time-consuming operations use `on_result_async` instead: storing results in a database, posting them to an HTTP API, or uploading them to object storage. It takes an `async def` and runs it on the event loop you await `finish` on.

```python
async def store(work, ticket, result):
    await database.insert(ticket.key, result)


tickets.on_result_async(store)
```

See [`TicketQueue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketQueue.html).

</details>

## Knowledge

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/knowledge.gif" width="600" />
</div>

`Knowledge` allows agents to share insights or learnings. Knowledge pages are created in the Open Knowledge Format (OKF).

```python
from agentwerk import Agent, Knowledge

store = Knowledge.load("./notes")
alice = Agent().knowledge(store)
bob = Agent().knowledge(store)
```

Each page is written to `./notes/knowledge/pages/<slug>.md`, and every page gets one line in `./notes/knowledge/index.md`. That list is injected into the prompt of every agent sharing the store, so each of them knows which pages it can read.

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

The prompt carries the index up to `index_char_limit`, 12 000 characters by default. Past it the prompt lists the pages that fit and names `index.md` for the agent to read the rest. No page is refused for the length of the index, and page bodies are never shortened.

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

See [`Knowledge`](https://docs.rs/agentwerk/latest/agentwerk/agents/knowledge/struct.Knowledge.html).

</details>

## Use Cases

Example projects built with agentwerk:

- [Hello World](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/hello_world/): basic example, ported in [examples/hello_world.py](https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/hello_world.py)
- [Terminal REPL](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/terminal_repl/): minimal interactive chat
- [Divide and Conquer](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/divide_and_conquer/): arithmetic problem shared across agents, ported in [examples/divide_and_conquer.py](https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/divide_and_conquer.py)
- [Deep Research](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/deep_research/): deep research pipeline (requires `BRAVE_API_KEY`)
- [Malware Scanner](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/malware_scanner/): identify indicators of compromise in a software package
- [Apparat Fabrik](https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/apparat_fabrik.py): a shift on the line of an apparatus works

> Configure an LLM provider first (see [Environment](https://github.com/canvascomputing/agentwerk/blob/main/DEVELOPMENT.md#environment)).

```bash
python examples/divide_and_conquer.py 200 4 2
```

## Security

Report a vulnerability to security@canvascomputing.org, not in a public issue. See [SECURITY.md](https://github.com/canvascomputing/agentwerk/blob/main/SECURITY.md).

## Development

See [DEVELOPMENT.md](https://github.com/canvascomputing/agentwerk/blob/main/DEVELOPMENT.md).
