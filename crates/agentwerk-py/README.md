<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/logo.png" width="200" />
</div>

<h1 align="center">agentwerk (Python)</h1>

<div align="center">
  <strong>A minimal agentic loop for building efficient harnesses.</strong>
</div>

<div align="center">
  <a href="#installation">Installation</a> •
  <a href="https://github.com/canvascomputing/agentwerk/blob/main/README.md">Rust</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#api">API</a> •
  <a href="#use-cases">Use Cases</a> •
  <a href="#security">Security</a> •
  <a href="#development">Development</a>
</div>

<div align="center">agentwerk is a lightweight agentic loop optimized for small and fast LLMs: parallel agents, task-based coordination, built-in tools, schema-validated results, shared knowledge, and an event for every step.</div>

> [!WARNING]
> agentwerk is in beta. APIs stabilize in `0.2.0`.

---

<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/demo.gif" width="800" />
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
    )

    agent.task(
        "Find every `pub trait` defined under src/ and explain each in one sentence."
    )

    work = agent.start()
    result = await work.finish_result("ORDER BY created DESC")

    print(result)


asyncio.run(main())
```

## API

- [Agents](#agents): Define roles, behavior and tasks.
- [Tasks](#tasks): Coordinate complex work across agents.
- [Tools](#tools): Define accessible tooling.
- [Events](#events): Inspect requests, tool usage, failures and more.
- [Knowledge](#knowledge): Let agents share notes for collaboration.

## Agents

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/agents.gif" width="600" />
</div>

An `Agent` is the core entity of agentwerk. It has access to tools for solving tasks in the form of tasks.

```python
from agentwerk import Agent, ReadFileTool

agent = (
    Agent.from_env()
    .role("You are a release manager who prepares release notes.")
    .tool(ReadFileTool())
)

agent.task("Read CHANGELOG.md and summarize the entries added since the last release.")

agent.start()
```

<details>
<summary>All agent methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Configure** | `role(role)` | Define who the agent is and how it should work. |
| | `tool(tool)` / `tools(tools)` | Register a tool the agent may call. |
| | `label(label)` | Restrict the agent to tasks carrying this label. |
| | `dir(dir)` | Set the directory the agent has access to. |
| | `template(key, value)` | Inject data into prompts with template strings. |
| | `templates(variables)` | Inject more than one entry into prompts. |
| | `knowledge(store)` | Share a knowledge store with the agent. |
| | `interactive()` | Let the agent wait for new instructions to keep a task in-progress. |
| **Work** | `task(task)` | Submit a task, or a `Task` carrying a label or schema, and return its task ID. |
| | `start()` | Begin processing tasks. |
| | `get_id()` | Get the unique identifier of an agent. |

You can use the `{context}` variable to inject contextual information:

```markdown
- Task: t-7
- Date: 2026-05-06
- Working directory: /Users/caro
- Platform: darwin 25.1.0
- Turns remaining: 8
- Input tokens remaining: 95000
- Output tokens remaining: 12000
- Time remaining: 240s
```

Every value is a variable of its own: `{task_id}`, `{date}`, `{dir}`, `{platform}`, `{os_version}`, `{turns_remaining}`, `{input_tokens_remaining}`, `{output_tokens_remaining}`, and `{time_remaining}`.

#### Interactive

An interactive agent holds one task open across many turns, so a conversation spans a whole session.

```python
def show(work, task, result):
    print(f"{task.get_id()}: {result}")


agent = Agent.from_env().interactive()
id = agent.task("Where does the configuration get loaded?")

chat = agent.start()
chat.on_result(show)
await chat.finish_all_tasks()

chat.add_reply(id, "And which environment variables override it?")
await chat.finish_all_tasks()

chat.set_task_finished(id, "answered")
```

An interactive agent never finishes its own task, because that would end the conversation. Every answer pauses the task instead: it stays `InProgress` with its agent, and each `await chat.finish_all_tasks()` returns on the answer it waited for. `add_reply(id, content)` drives the next turn, and `set_task_finished(id, result)` ends the conversation, which is the result the hook reports. The answers in between arrive as [events](#events).

See more: [`Agent`](https://docs.rs/agentwerk/latest/agentwerk/agents/agent/struct.Agent.html).

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

## Tasks

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/tasks.gif" width="600" />
</div>

The `Queue` is the core data structure of agentwerk for coordinating complex interactions.

```python
from agentwerk import Agent, Task, Queue

analyst = (
    Agent.from_env()
    .label("analysis")
)

writer = (
    Agent.from_env()
    .label("report")
)

tasks = Queue()
tasks.add_agent(analyst).add_agent(writer)

tasks.add_task(Task("Rank all products by value.", label="analysis"))
tasks.add_task(Task("Write up the ranking.", label="report"))
```

<details>
<summary>All Queue methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Configure** | `set_policy(policy)` | Set execution limits and retry tuning. |
| | `get_policy()` | Get the policy in force. |
| | `set_dir(dir)` | Define where a session is stored. |
| | `get_dir()` | Get the session directory. |
| | `set_schemas(store)` | Enforce schemas for task results. |
| | `add_agent(agent)` | Add an agent to this task queue. |
| **Submit and interact** | `add_task(task)` | Submit a task and return its task ID. |
| | `add_reply(id, content)` | Add a reply to a task. |
| | `edit_replies(id, editor)` | Rewrite a task's replies now. |
| | `set_task_finished(id, result)` | Finish a task with a result. |
| | `set_task_failed(id)` | Fail a task. |
| **Observe** | `on_event(handler)` | Read every event as it is emitted. |
| | `on_event_async(handler)` | Read every event in an async handler. |
| | `on_result(handler)` | Read every finished task together with its result. |
| | `on_result_async(handler)` | Read every finished task and result in an async handler. |
| | `on_failure(handler)` | Read every failure together with its task. |
| | `on_failure_async(handler)` | Read every failure and task in an async handler. |
| | `on_task(handler)` | Read task lifecycle transitions. |
| | `on_task_async(handler)` | Read task lifecycle transitions in an async handler. |
| **Run** | `start()` | Begin processing tasks. |
| | `finish_result(query)` | Wait for matching tasks and get the first result in query order. |
| | `finish_results(query)` | Wait for matching tasks and get their results. |
| | `finish_all_tasks()` | Wait for every task and get every result. |
| **Cancel** | `cancel_tasks(query)` | Stop work on matching tasks. |
| | `cancel_all_tasks()` | Stop work on every task. |
| **Inspect tasks** | `get_task(id)` | Get one task by ID. |
| | `get_tasks()` | Get every task in creation order. |
| | `find_task(query)` | Get the first matching task. |
| | `find_tasks(query)` | Get every matching task. |
| **Inspect results and events** | `get_results()` | Get every finished task result in creation order. |
| | `find_result(query)` | Get the first result in query order. |
| | `find_results(query)` | Get every result in query order. |
| | `find_event(query)` | Get the first recorded event in query order. |
| | `find_events(query)` | Get every recorded event in query order. |
| **Inspect run metadata** | `get_finish_reason()` | Get why the last run ended. |
| | `get_model_for_agent(agent_id)` | Get the model used by an agent. |
| | `get_input_tokens()` | Get input tokens across finished requests. |
| | `get_output_tokens()` | Get output tokens across finished requests. |
| | `get_duration()` | Get the elapsed execution duration. |

See [`Queue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tasks/struct.Queue.html).

</details>

### Queries

Use AQL to filter and sort tasks. A query contains one or more `field operator value` conditions, joined with `AND`, `OR`, or `NOT`. An optional `ORDER BY` clause comes last.

For example, `label IN (scan, report) AND status = Finished ORDER BY finished DESC` selects finished tasks labelled `scan` or `report`, then puts the most recently finished task first.

```python
tasks.find_tasks("scan")
tasks.find_results("t-3")
tasks.find_tasks("id IN (t-3, t-4)")
tasks.find_tasks("label IN (scan, report) AND status = Finished")
tasks.find_results("scan ORDER BY finished DESC")
```

<details>
<summary>All query terms</summary>

#### Terms

| | Syntax | Meaning |
|-|--------|---------|
| **Match** | `field = value`, `field != value` | Include or exclude one exact value. |
| | `field IN (a, b)`, `field NOT IN (a, b)` | Include or exclude a list. |
| **Presence** | `field IS EMPTY`, `field IS NOT EMPTY` | Test whether an optional field has a value. |
| **Search** | `field ~ text`, `field !~ text` | Include or exclude case-insensitive text. |
| **Compare** | `field > value`, `>=`, `<`, `<=` | Compare a time field. |
| **Combine** | `A AND B`, `A OR B`, `NOT A`, `(A OR B)` | Combine or group conditions. |
| **Shorthand** | `scan`, `t-3` | Short for `label = scan` and `id = t-3`. |
| **Sort** | `ORDER BY field DESC` | Sort matches; `ASC` is the default. |

#### Fields

| Kind | Fields | Meaning |
|------|--------|---------|
| **Identity** | `id`, `label`, `status` | Task identity and lifecycle state. |
| **Run state** | `pending`, `cancelled` | Whether this run may schedule the task. |
| **Relationship** | `agent`, `parent` | Claiming agent and handover parent. |
| **Text** | `task`, `result`, `errors` | Task body, result, and recorded failures. |
| **Time** | `created`, `started`, `finished`, `failed` | Lifecycle timestamps. |

#### Rules

Write the field, followed by what it must match:

- Exact value: `label = scan`
- Contains text: `result ~ timeout`
- Time range: `failed > -1h`
- Has no value: `agent IS EMPTY`

Missing values do not match `!=`. For example, `label != scan` leaves out tasks
with no label. To include them, use `label IS EMPTY OR label != scan`.

Quote values containing spaces: `label = "needs review"`. Put lists in
parentheses: `label IN (scan, report)`.

Use parentheses when mixing `AND` and `OR` to make the order clear. `NOT` applies
to the condition or parenthesized group after it. Query words such as `AND` ignore
case; labels and IDs do not.

Relative times such as `-30m`, `-2h`, `-7d`, and `-1w` are measured when the
query runs.

`ORDER BY field` sorts from lowest to highest. Add `DESC` for the reverse. Tasks
with no value for that field come last. Without `ORDER BY`, tasks remain in
creation order.

#### Examples

```python
tasks.find_results("report AND result ~ risk")           # reports that mention risk
tasks.find_tasks("errors ~ tool_call_failed")          # saw a tool call fail
tasks.find_tasks("status = Todo AND agent IS EMPTY")   # waiting, never claimed
tasks.find_tasks("failed > -1h ORDER BY failed DESC")  # the last hour's failures
tasks.find_tasks(lambda t: len(t.get_replies()) > 4)       # a callable, for what no field carries
```

</details>

### Execution

The task queue schedules the work of your agents and returns their results.

```python
tasks.start()

answer = await tasks.finish_result("ORDER BY created DESC")
if answer is not None:
    print(answer)
```

<details>
<summary>All execution methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Run** | `start()` | Begin processing tasks. |
| | `await finish_result(query)` | Wait for matching tasks and get the first result in query order. |
| | `await finish_results(query)` | Wait for matching tasks and get their results. |
| | `await finish_all_tasks()` | Wait for every task and get every result. |
| **Cancel** | `cancel_tasks(query)` | Stop work on matching tasks. |
| | `cancel_all_tasks()` | Stop work on every task. |

Cancellation is run-local: it does not change `status` or persist with the task. `start()` clears cancellation so unfinished tasks can resume. Use `task.is_cancelled()` for a task you hold, or `cancelled = true` and `pending = true` to select by run state.

Task members:

| | Member | Description |
|-|--------|-------------|
| **Identity** | `get_id()` | Task ID, of the form `t-N`. |
| | `get_task()` | The work the agent is asked to do. |
| | `get_label()` | Label carried by the task. |
| | `get_parent()` | Identifier of the parent task if a handover was performed. |
| | `get_reporter()` | Identifier of the agent that created the task. |
| | `get_assignee()` | Identifier of the agent that claimed the task. |
| **Outcome** | `get_status()` | The task lifecycle status. |
| | `is_todo()` | Check whether the task is waiting to be claimed. |
| | `is_in_progress()` | Check whether an agent is working on the task. |
| | `is_finished()` | Check whether the task finished. |
| | `is_failed()` | Check whether the task failed. |
| | `is_pending()` | Check whether the task has work in this run. |
| | `is_cancelled()` | Check whether this run has taken the task off the queue. |
| | `get_result()` | The result the agent produced. |
| | `get_errors()` | The failures recorded against the task, as events. |
| | `get_replies()` | Messages exchanged with the model. |
| | `get_schema()` | Optional schema the result must satisfy. |
| **Timestamps** | `get_created_at()` | Creation time, in milliseconds. |
| | `get_started_at()` | Claim time, in milliseconds. |
| | `get_finished_at()` | Finish time, in milliseconds. |
| | `get_failed_at()` | Failure time, in milliseconds. |

See [`Task`](https://docs.rs/agentwerk/latest/agentwerk/agents/tasks/struct.Task.html).

</details>

### Handover

Agents can share the results of their work in the following ways:

1. **Create tasks**: the `finish` tool's `handover` option opens a child task carrying the result.
2. **Read tasks**: the `tasks` tool allows reading any finished task's result, by ID.
3. **Read result file**: the `read_file` tool allows reading a task's `result.json` in the session directory.
4. **Share knowledge**: the `knowledge` tool allows sharing knowledge with other agents.
5. **Register hooks**: the `on_result` hook allows creating follow-up tasks.

<details>
<summary>All ways agents pass data</summary>

#### 1. Create tasks

Name the receiving label in the role or in the task, and the agent hands over as it finishes:

```python
analyst = (
    Agent.from_env()
    .label("analysis")
    .role("Rank the products by value, then hand the ranking over to `report`.")
)

writer = (
    Agent.from_env()
    .label("report")
    .role("Write the board report from the ranking you were handed.")
)
```

The child task is filed under `report` and names the analysis task as its `parent`. Its body is the result that was handed over, unless the agent passes a task of its own, which may carry `{parent_id}`, `{parent_result}`, and `{parent_result_path}`. Either way the body ends with the parent's ID and the path of its result file.

#### 2. Read tasks

Give the writer `TasksTool()`, and it reads what any finished task produced, by ID:

```python
writer = Agent.from_env().label("report").tool(TasksTool())

writer.task("Read the result of t-1, then write the board report.")
```

#### 3. Read result file

Give the writer `ReadFileTool()` instead, and it opens the result file named at the end of its task:

```python
writer = Agent.from_env().label("report").tool(ReadFileTool())

writer.task("Read .agentwerk/tasks/t-1/result.json, then write the board report.")
```

Results live in the session directory, one `result.json` per task.

#### 4. Share knowledge

Hand both agents one store, and either can write a page the other reads:

```python
store = Knowledge.load(".agentwerk")

analyst = Agent.from_env().label("analysis").knowledge(store)
writer = Agent.from_env().label("report").knowledge(store)

analyst.task("Rank the products by value, then save the ranking to your knowledge.")
```

#### 5. Register hooks

Use hooks to create new tasks when certain results arrived:

```python
def hand_to_report(work, done, result):
    if done.get_label() == "research":
        work.add_task(Task(result, label="report"))


tasks.on_result(hand_to_report)
```

</details>

### Schemas

A `Schema` constrains a task result. agentwerk repairs simple representation errors such as quoted numbers; otherwise, it returns the violation to the model and retries up to `max_schema_retries`.

```python
from agentwerk import Schema, Task

schema = Schema(
    {
        "type": "object",
        "properties": {"title": {"type": "string"}},
        "required": ["title"],
    }
)

tasks.add_task(Task("Write a report.", schema=schema))
```

For small models, use shallow, focused schemas with few required fields, clear names, and simple enums. Split large results into labeled tasks with separate schemas, then combine them in a later task. Deep nesting, long property lists, and large `anyOf` or `oneOf` branches waste context and trigger retries.

<details>
<summary>All schema methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Schema** | `Schema(document)` | Create a schema. |
| | `validate(value)` | Validate content. |
| **SchemaStore** | `SchemaStore()` | Create a store of schemas bound to labels. |
| | `label(label, document)` | Bind a schema to a label. |
| | `get(label)` | Read back the schema bound to a label. |
| | `tasks.set_schemas(store)` | Enforce schemas for task results. |

A `SchemaStore` enforces schemas for all tasks with a certain label. Registering schemas centrally spares agents from passing complex schema structures during task creation (see `TasksTool`) and handovers (see `FinishTool`):

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

tasks.set_schemas(schemas)
```

See [`Schema`](https://docs.rs/agentwerk/latest/agentwerk/schemas/struct.Schema.html) and [`SchemaStore`](https://docs.rs/agentwerk/latest/agentwerk/schemas/struct.SchemaStore.html).

</details>

### Configuration

A `Policy` limits the turns, tokens, and time a run may spend, and allows configuring retries and compaction.

```python
tasks.set_policy(Policy(max_turns=40, max_time=300.0))
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

`set_policy(policy)` replaces the whole configuration, and `get_policy()` reads it back. A violated limit emits `Event.POLICY_VIOLATED`. `compaction_threshold` is the exception, see [Compaction](#compaction).

</details>

### Compaction

Compaction summarizes a task's older messages once they no longer fit the model's context window.

```python
tasks.set_policy(Policy(compaction_threshold=0.7))
```

<details>
<summary>When compaction runs and what it reports</summary>

`compaction_threshold` is a fraction of the model's context window, `0.85` by default. Reaching it summarizes the older messages and the agent carries on.

Compaction also runs after the LLM provider reports the window exceeded. `compaction_started`, `compaction_progress`, `compaction_finished`, and `compaction_failed` report each step, see [Events](#events).

```python
def watch(work, event):
    if event.get_name() == Event.COMPACTION_FINISHED:
        print(f"[{event.get_task_id()}] compacted {event.get_data()['reason']}")


tasks.on_event(watch)
```

Each of the compaction events carries the reason it ran: `proactive` ahead of the failure, `reactive` after it. Replies that still exceed the window after a reactive compaction fail the task.

</details>

### Directives

A directive tells the model how to recover: call a tool, match a schema, correct a path, narrow a search, or choose an allowed command. You can replace the built-in wording for your model or environment.

```python
from agentwerk import Agent, Directive


def tune(key):
    if key == Directive.GREP_FAILED:
        return "The search did not run. Narrow `path`."
    return None


agent = Agent.from_env().directives(tune)
```

<details>
<summary>All directive settings</summary>

| Method | Description |
|--------|-------------|
| `directives(compute)` | Decide every directive's text with one function. |

Return `None` to keep the default. Replacements may use template variables such as `{detail}`, `{attempt}`, and `{path}`. Include the failure and next action.

See [prompts/directives](https://github.com/canvascomputing/agentwerk/tree/main/crates/agentwerk/src/prompts/directives) for the built-in text.

</details>

### Sessions

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/sessions.gif" width="600" />
</div>

A `Queue` writes every task, reply, and event to its working directory (default `./.agentwerk`). You can continue a session from that directory.

```python
tasks = Queue.load(".agentwerk")
tasks.add_agent(my_agent)
tasks.start()
```

<details>
<summary>All session files</summary>

```
.agentwerk/
├── events.jsonl                          every event (one per line)
├── tasks/
│   └── t-1/
│       ├── task.json                   the task without its messages (id, status, label, timestamps)
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
| **Tasks** | `FinishTool()` | Write the result for the current task and mark it finished. |
| | `TasksTool()` | Read the task queue and create or edit tasks. |
| **Knowledge** | `KnowledgeTool(store)` | Write, read, remove, or list pages in a knowledge store. |

#### `FinishTool` and `KnowledgeTool`

`FinishTool()` and `KnowledgeTool(store)` are special tools, registered automatically on every agent. They are used for interacting with the `Queue` or knowledge base. An [interactive agent](#interactive) gets no `FinishTool()` by default, since finishing its task would end the conversation.

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
    if event.get_name() == Event.TASK_FINISHED:
        print(f"[{event.get_agent_id()}] done {event.get_task_id()} {event.get_label()}")


tasks.on_event(log)
```

<details>
<summary>All event names and readers</summary>

| | Kind | Description |
|-|------|-------------|
| **Run** | `run_started` | Execution began. |
| | `run_finished` | Execution ended, carrying the reason. |
| | `policy_violated` | A limit was breached and execution stopped. |
| **Task** | `task_started` | An agent claimed a task. |
| | `task_finished` | A task finished successfully. |
| | `task_failed` | A task failed. |
| | `turn_started` | The agent began another turn on its task. |
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
| | `tool_call_failed` | A tool invocation failed but the task continues. |
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
| **Application** | caller-defined name | An event published with `emit_event`. |

### Publish events

Publish custom events through the queue. Add agent or task context when relevant:

```python
from agentwerk import Event

tasks.emit_event(
    Event("document_indexed")
    .data({"documents": 42})
    .task_id("t-1")
    .agent_id("indexer-1")
)

tasks.emit_event(Event("index_refreshed"))
```

The first event's name, data, and context are stored as:

```json
{"name":"document_indexed","data":{"documents":42},"task_id":"t-1","agent_id":"indexer-1"}
```

`emit_event` adds the timestamp and a known task's label. Lowercase snake case is
conventional, but other names are accepted; quote names containing spaces or
punctuation in AQL. Built-in names are available as `Event` constants, such as
`Event.TASK_FINISHED`. Publishing one triggers its hooks, statistics, and
persistence behavior, but not its state transition.

Events are saved to `.agentwerk/events.jsonl`. Streamed text chunks are not
saved. Read events through the queue with these methods:

| Method | Description |
|--------|-------------|
| `emit_event(event)` | Publish an event for querying and observation. |
| `find_event(query)` | Get the first matching event in query order; without `ORDER BY`, this is the earliest one. |
| `find_events(query)` | Get every matching event in query order; without `ORDER BY`, this is oldest first. |
| `get_input_tokens()` / `get_output_tokens()` | Get token counts across the run's requests. |
| `get_duration()` | Get the elapsed execution duration. |

</details>

<details>
<summary>All event query fields and rules</summary>

You can query events with AQL syntax or callables.

```python
tasks.find_events("tool_call_failed")
tasks.find_events("event = request_finished AND agent = research-1")
tasks.find_events("task = t-3 ORDER BY created DESC")
tasks.find_events("payload ~ timeout AND created > -1h")
```

| | Field | Description |
|-|-------|-------------|
| **Match** | `event` | The event name, such as `run_started` or `tool_call_failed`. |
| | `agent` | The attributed agent ID, when the event has agent context. |
| | `task` | The attributed task ID, when the event has task context. |
| | `label` | The attributed task's label, when the task is known and labelled. |
| **Search** | `payload` | The event name and serialized event data, searched together as text. |
| **Compare** | `created` | When the event was recorded. |

Event queries follow the same [rules as task queries](#queries). Match `event`,
`agent`, `task`, and `label` exactly; use `~` to search `payload`; and use `<` or
`>` with `created`. Some events have no agent, task, or label. Find them with
`IS EMPTY`. Without `ORDER BY`, events remain oldest first.

You can also search with one word. Agentwerk reads it as:

1. A task ID such as `t-3` means `task = t-3`.
2. A built-in event name such as `tool_call_failed` means `event = tool_call_failed`.
3. Any other value such as `scan` means `label = scan`.

For your own event names, write `event = document_indexed`. If a label has the
same name as a built-in event, write `label = knowledge_read`.

A string that does not compile raises `ValueError`, as does `Query(query)`, which compiles one without running it.

</details>

See [`Event`](https://docs.rs/agentwerk/latest/agentwerk/event/struct.Event.html) and [`Queue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tasks/struct.Queue.html).

### Hooks

Hooks allow you to react to events.

```python
def triage(work, event, failed):
    if failed.get_label() == "scan":
        work.add_task(Task(failed.get_task(), label="triage"))


tasks.on_failure(triage)
```

<details>
<summary>All hooks</summary>

| | Method | Description |
|-|--------|-------------|
| **Observe** | `on_event(handler)` | Read every event as it is emitted. |
| | `on_event_async(handler)` | Read every event in an async handler. |
| | `on_result(handler)` | Read every finished task together with its result. |
| | `on_result_async(handler)` | Read every finished task with its result, in an async handler. |
| | `on_failure(handler)` | Read every failure together with the task it happened in. |
| | `on_failure_async(handler)` | Read every failure with its task, in an async handler. |
| | `on_task(handler)` | Read a task as it starts, finishes, or fails. |
| | `on_task_async(handler)` | Read a task lifecycle transition in an async handler. |

Save replies of every finished task as a training example:

```python
def capture(work, event, task):
    if event.get_name() == Event.TASK_FINISHED:
        model = work.get_model_for_agent(event.get_agent_id())
        Trajectory.from_task(event.get_agent_id(), model, task).save("datasets")


tasks.on_task(capture)
```

#### Async handlers

`on_result` is blocking and prevents an agent continuing its work till the hook is finished. If you perform time-consuming operations use `on_result_async` instead: storing results in a database, posting them to an HTTP API, or uploading them to object storage. It takes an `async def` and runs it on the event loop you await `finish` on.

```python
async def store(work, task, result):
    await database.insert(task.get_id(), result)


tasks.on_result_async(store)
```

See [`Queue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tasks/struct.Queue.html).

</details>

## Knowledge

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/knowledge.gif" width="600" />
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
| `get_index()` | Get the index, which is injected into the agent prompt. |
| `set_char_limit(count)` | Limit how much of the index is injected into the prompt. |
| `get_index_char_limit()` | Get the index size limit in force. |
| `get_pages()` | Get the page collection for reading and writing pages. |
| `get_pages().get_pages()` | Get every page in the store. |
| `clear()` | Remove every page from the store. |

The prompt includes up to 12 000 characters of the knowledge index by default. If the index exceeds the configured limit, the agent reads the remainder from `index.md`. Pages are always saved in full.

Programmatically create entries:

```python
from agentwerk import Page

store.get_pages().save(
    Page(
        "build-command",
        "How the project is built.",
        "Run `make` to compile.",
        tags=["build"],
    )
)

page = store.get_pages().get_page("build-command")
store.get_pages().remove("build-command")
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
