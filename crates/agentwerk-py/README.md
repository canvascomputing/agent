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

<div align="center">Coordinate agent fleets across complex tasks, with shared knowledge and detailed observability.</div>

<br />

<div align="center"><strong>Beta:</strong> The API might introduce breaking changes before <code>0.2.0</code>.</div>

---

<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/demo.gif" width="800" />
</div>
<div align="center"><a href="https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/apparat_fabrik.py">Apparat Fabrik</a></div>
<div align="center"><em>“Werk” is German for both a factory and a work of art.</em></div>

---

## Why use agentwerk?

- **Simple interface:** create agents with a few lines of code.
- **Efficient harness:** optimized for small and fast LLMs with low memory footprint.
- **Complex interactions:** let agents collaborate through a shared Werk, event hooks, and knowledge.
- **Deep observability:** inspect every request, tool call, and failure.
- **Facilitate training:** store trajectories based on granular events for fine-tuning models.

## Installation

Install the Python package from PyPI or follow the [separate guide](https://github.com/canvascomputing/agentwerk/blob/main/README.md) for the Rust crate.

### Python

```bash
pip install agentwerk
```

## Quick Start

This example gives one agent read-only tools to search Rust source files, then waits for one result.

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

    agent.add_task(
        "Find every `pub trait` defined under src/ and explain each in one sentence."
    )

    werk = agent.start()
    result = await werk.finish_task("ORDER BY created DESC")

    print(result)


asyncio.run(main())
```

## API

The public API has five parts, ordered by how you build and inspect an agent system:

- [Agents](#agents): Set agent roles, behavior, and tasks.
- [Werk](#werk): Assign work and collect results across agents.
- [Tools](#tools): Give agents controlled ways to act.
- [Events](#events): Inspect requests, tool calls, and failures.
- [Knowledge](#knowledge): Share durable memory across agents and tasks.

## Agents

An `Agent` uses a language model and the tools you provide to complete tasks.

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/agents.gif" width="600" />
</div>

```python
from agentwerk import Agent, ReadFileTool

agent = (
    Agent.from_env()
    .role("You are a release manager who prepares release notes.")
    .tool(ReadFileTool())
)

agent.add_task("Read CHANGELOG.md and summarize the entries added since the last release.")

agent.start()
```

<details>
<summary>All agent methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Configure** | `role(role)` | Define who the agent is and how it should work. |
| | `tool(tool)` / `tools(tools)` | Register a tool the agent may call. |
| | `label(label)` | Restrict the agent to tasks carrying this label. |
| | `handover(task)` | Set the task this agent creates when it finishes. |
| | `dir(dir)` | Set the directory the agent has access to. |
| | `template(key, value)` | Inject data into prompts with template strings. |
| | `templates(variables)` | Inject more than one entry into prompts. |
| | `knowledge(store)` | Share a knowledge store with the agent. |
| | `interactive()` | Let the agent wait for new instructions to keep a task in-progress. |
| **Work** | `add_task(task)` | Submit a task, or a `Task` carrying a label or schema, and return its task ID. |
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
def show(werk, task, result):
    print(f"{task.get_id()}: {result}")


agent = Agent.from_env().interactive()
id = agent.add_task("Where does the configuration get loaded?")

werk = agent.start()
werk.on_result(show)
await werk.finish_all_tasks()

werk.add_reply(id, "And which environment variables override it?")
await werk.finish_all_tasks()

werk.set_task_finished(id, "answered")
```

An interactive agent never finishes its own task, because that would end the conversation. Every answer pauses the task instead: it stays `in_progress` with its agent, and each `await werk.finish_all_tasks()` returns on the answer it waited for. `add_reply(id, content)` supplies the next message, and `set_task_finished(id, result)` ends the conversation with the result reported to the hook. The answers in between arrive as [events](#events).

See more: [`Agent`](https://docs.rs/agentwerk/latest/agentwerk/agents/agent/struct.Agent.html).

</details>

### Providers

An LLM provider connects agents to Anthropic, OpenAI, Mistral, or a LiteLLM proxy.

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
| `await provider.verify(model)` | Verify that the provider can answer with a model. |
| `Anthropic(key, base_url=..., timeout=...)` | Configure an Anthropic endpoint. `OpenAi`, `Mistral`, and `LiteLlm` accept the same options. |

You can also read the model or provider individually: `.provider(Provider.from_env())` or `.model(Model.from_env())`.

| Variable | Description |
|----------|-------------|
| `LITELLM_PROVIDER` | Choose `anthropic`, `mistral`, `openai`, or `litellm` outright, ahead of the keys below. |
| `LITELLM_API_KEY`, `MISTRAL_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` | Authenticate with that vendor. The first one set picks the LLM provider, in this order. |
| `LITELLM_BASE_URL`, `MISTRAL_BASE_URL`, `ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL` | Set a different API address for that vendor. |
| `SSL_CERT_FILE`, `SSL_CERT_DIR` | Trust these CA certificates instead of the built-in root store. |

Set a model's context window or reasoning level when the defaults do not fit. Claude, GPT, Mistral, and Qwen families have built-in settings.

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

## Werk

A `Werk` stores tasks, assigns them to matching agents, and records their results.

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/werk.gif" width="600" />
</div>

```python
from agentwerk import Agent, Task, Werk

analyst = (
    Agent.from_env()
    .label("analysis")
)

writer = (
    Agent.from_env()
    .label("report")
)

werk = Werk()
werk.add_agent(analyst).add_agent(writer)

werk.add_task(Task("Rank all products by value.", label="analysis"))
werk.add_task(Task("Write up the ranking.", label="report"))
```

<details>
<summary>All Werk methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Configure** | `set_policy(policy)` | Set execution limits and retry settings. |
| | `get_policy()` | Get the policy in force. |
| | `set_dir(dir)` | Define where a session is stored. |
| | `get_dir()` | Get the session directory. |
| | `add_agent(agent)` | Add an agent to this Werk. |
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
| | `on_task(handler)` | Read task state changes. |
| | `on_task_async(handler)` | Read task state changes in an async handler. |
| **Run** | `start()` | Begin processing tasks. |
| | `finish_task(query)` | Wait for matching tasks and get the first result in query order. |
| | `finish_tasks(query)` | Wait for matching tasks and get their results. |
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
| **Inspect execution** | `get_finish_reason()` | Get why the last execution ended. |
| | `get_model_for_agent(agent_id)` | Get the model used by an agent. |
| | `get_input_tokens()` | Get input tokens across finished requests. |
| | `get_output_tokens()` | Get output tokens across finished requests. |
| | `get_duration()` | Get the elapsed execution duration. |

See [`Werk`](https://docs.rs/agentwerk/latest/agentwerk/struct.Werk.html).

</details>

### Queries

Agentwerk Query Language (AQL) filters and sorts tasks by fields such as label, status, and creation time.

For example, `label IN (scan, report) AND status = finished ORDER BY finished DESC` selects finished tasks labelled `scan` or `report`, then puts the most recently finished task first.

```python
werk.find_tasks("scan")
werk.find_results("t-3")
werk.find_tasks("id IN (t-3, t-4)")
werk.find_tasks("label IN (scan, report) AND status = finished")
werk.find_results("scan ORDER BY finished DESC")
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
| **Identity** | `id`, `label`, `status` | Task identity and current state. |
| **Run state** | `pending`, `cancelled` | Whether this run may schedule the task. |
| **Relationship** | `agent`, `parent` | Claiming agent and handover parent. |
| **Text** | `task`, `result`, `errors` | Task body, result, and recorded failures. |
| **Time** | `created`, `started`, `finished`, `failed` | Creation, start, finish, and failure times. |

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
werk.find_results("report AND result ~ risk")           # reports that mention risk
werk.find_tasks("errors ~ tool_call_failed")          # saw a tool call fail
werk.find_tasks("status = todo AND agent IS EMPTY")   # waiting, never claimed
werk.find_tasks("failed > -1h ORDER BY failed DESC")  # the last hour's failures
werk.find_tasks(lambda t: len(t.get_replies()) > 4)       # a callable, for what no field carries
```

</details>

### Execution

The Werk schedules the work of your agents and returns their results.

```python
werk.start()

answer = await werk.finish_task("ORDER BY created DESC")
if answer is not None:
    print(answer)
```

<details>
<summary>All execution methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Run** | `start()` | Begin processing tasks. |
| | `await finish_task(query)` | Wait for matching tasks and get the first result in query order. |
| | `await finish_tasks(query)` | Wait for matching tasks and get their results. |
| | `await finish_all_tasks()` | Wait for every task and get every result. |
| **Cancel** | `cancel_tasks(query)` | Stop work on matching tasks. |
| | `cancel_all_tasks()` | Stop work on every task. |

Cancellation applies only to the current execution: it does not change `status` or remain attached to the task. `start()` clears cancellation so unfinished tasks can resume. Use `task.is_cancelled()` for a task you hold, or `cancelled = true` and `pending = true` to select by execution state.

Task members:

| | Member | Description |
|-|--------|-------------|
| **Identity** | `get_id()` | Task ID, of the form `t-N`. |
| | `get_task()` | The work the agent is asked to do. |
| | `get_label()` | Label carried by the task. |
| | `get_parent()` | Identifier of the parent task if a handover was performed. |
| | `get_reporter()` | Identifier of the agent that created the task. |
| | `get_assignee()` | Identifier of the agent that claimed the task. |
| **Outcome** | `get_status()` | Get the task's current status. |
| | `is_todo()` | Check whether the task is waiting to be claimed. |
| | `is_in_progress()` | Check whether an agent is working on the task. |
| | `is_finished()` | Check whether the task finished. |
| | `is_failed()` | Check whether the task failed. |
| | `is_pending()` | Check whether the task has work in this run. |
| | `is_cancelled()` | Check whether this run has excluded the task from scheduling. |
| | `get_result()` | The result the agent produced. |
| | `get_errors()` | The failures recorded against the task, as events. |
| | `get_replies()` | Messages exchanged with the model. |
| | `get_schema()` | Optional schema the result must satisfy. |
| **Timestamps** | `get_created_at()` | Creation time, in milliseconds. |
| | `get_started_at()` | Claim time, in milliseconds. |
| | `get_finished_at()` | Finish time, in milliseconds. |
| | `get_failed_at()` | Failure time, in milliseconds. |

See [`Task`](https://docs.rs/agentwerk/latest/agentwerk/struct.Task.html).

</details>

### Handover

Agents can pass work and results in five ways:

1. **Agent handover API**: `Agent.handover` creates a configured child task when the agent finishes.
2. **Result hook**: `on_result` creates follow-up tasks from completed work.
3. **Knowledge**: the `knowledge` tool shares durable pages between agents.
4. **Task tool**: the `task` tool reads any finished task's result by ID.
5. **Read result file**: the `read_file` tool opens a task's `result.json` in the session directory.

<details>
<summary>All ways agents pass data</summary>

#### 1. Agent handover API

Set the follow-up task on the first agent. The model can then finish with only its result:

```python
analyst = (
    Agent.from_env()
    .label("analysis")
    .role("You are a product analyst.")
    .handover(Task("Write the board report from {parent_result}.", label="report"))
)

writer = (
    Agent.from_env()
    .label("report")
    .role("You write concise board reports.")
)

werk = Werk()
werk.add_agent(analyst).add_agent(writer)
werk.add_task(Task("Rank all products by value.", label="analysis"))
```

Finishing the analysis creates the `report` task and links it to its parent. The task may use `{parent_id}`, `{parent_result}`, and `{parent_result_path}`. Calling `handover` again replaces it. Bound object results keep this handover host-owned; the legacy envelope for scalar or unbound results may override its task or schema, but not its label.

#### 2. Result hook

Use hooks to create new tasks when certain results arrive:

```python
def hand_to_report(werk, done, result):
    if done.get_label() == "research":
        werk.add_task(Task(result, label="report"))


werk.on_result(hand_to_report)
```

#### 3. Knowledge

Hand both agents one store, and either can write a page the other reads:

```python
store = Knowledge.load(".agentwerk")

analyst = Agent.from_env().label("analysis").knowledge(store)
writer = Agent.from_env().label("report").knowledge(store)

analyst.add_task("Rank the products by value, then save the ranking to your knowledge.")
```

#### 4. Task tool

Give the writer `TaskTool()`, and it reads what any finished task produced, by ID:

```python
writer = Agent.from_env().label("report").tool(TaskTool())

writer.add_task("Read the result of t-1, then write the board report.")
```

#### 5. Read result file

Give the writer `ReadFileTool()` instead, and it opens the result file named at the end of its task:

```python
writer = Agent.from_env().label("report").tool(ReadFileTool())

writer.add_task("Read .agentwerk/tasks/t-1/result.json, then write the board report.")
```

Results live in the session directory, one `result.json` per task.

</details>

### Schemas

A `Schema` defines the required shape of a task result. agentwerk fixes simple formatting errors such as quoted numbers. For other violations, it asks the model to retry up to `max_schema_retries`.

```python
from agentwerk import Schema, Task

schema = Schema(
    {
        "type": "object",
        "properties": {"title": {"type": "string"}},
        "required": ["title"],
    }
)

werk.add_task(Task("Write a report.", schema=schema))
```

For small models, use shallow, focused schemas with few required fields, clear names, and short lists of allowed values. Split large results into labeled tasks with separate schemas, then combine them in a later task. Deep nesting, long property lists, and large `anyOf` or `oneOf` branches use more context and trigger retries.

<details>
<summary>All schema methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Schema** | `Schema(document)` | Create a schema. |
| | `validate(value)` | Validate content. |
A schema for a handover child belongs on its configured `Task`. This lets a small model create the child with a result-only `finish` call:

```python
report_schema = Schema(
    {
        "type": "object",
        "properties": {"title": {"type": "string"}},
        "required": ["title"],
    }
)

analyst = Agent.from_env().handover(
    Task("Write the report from {parent_result}", label="report", schema=report_schema)
)
```

See [`Schema`](https://docs.rs/agentwerk/latest/agentwerk/schemas/struct.Schema.html).

</details>

### Configuration

A `Policy` sets limits for turns, tokens, elapsed time, retries, and compaction.

```python
werk.set_policy(Policy(max_turns=40, max_time=300.0))
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
werk.set_policy(Policy(compaction_threshold=0.7))
```

<details>
<summary>When compaction runs and what it reports</summary>

`compaction_threshold` is a fraction of the model's context window, `0.85` by default. Reaching it summarizes the older messages and the agent continues its task.

Compaction also runs after the LLM provider reports that the window was exceeded. `compaction_started`, `compaction_progress`, `compaction_finished`, and `compaction_failed` report each step, see [Events](#events).

```python
def watch(werk, event):
    if event.get_name() == Event.COMPACTION_FINISHED:
        print(f"[{event.get_task_id()}] compacted {event.get_data()['trigger']}")


werk.on_event(watch)
```

Each compaction event carries the trigger: `proactive` before a context-window error or `reactive` after one. A failure also carries a stable `kind` and human-readable `message`.

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

A `Werk` saves every task, reply, and event so you can continue the same session later.

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/sessions.gif" width="600" />
</div>

The working directory is `./.agentwerk` by default.

```python
werk = Werk.load(".agentwerk")
werk.add_agent(my_agent)
werk.start()
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

Tools give agents controlled access to files, commands, URLs, tasks, and shared knowledge.

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
| **Web** | `FetchTool()` | Fetch a URL and read its body. |
| **Events** | `EventTool()` | Publish an event; `task_finished` additionally completes the current task. |
| **Tasks** | `FinishTool()` | Write the result for the current task and mark it finished. |
| | `TaskTool()` | Read the Werk and create or edit tasks. |
| **Knowledge** | `KnowledgeTool(store)` | Write, read, remove, or list pages in a knowledge store. |

#### `FinishTool` and `KnowledgeTool`

`FinishTool()` and `KnowledgeTool(store)` are registered automatically on every agent. Agents use them to finish queued tasks or work with shared knowledge. An [interactive agent](#interactive) gets no `FinishTool()` by default, since finishing its task would end the conversation. `FinishTool()` is the compatibility wrapper around `EventTool()`'s `task_finished` event; `EventTool()` remains opt-in.

When a task carries an object schema, its displayed fields are the `finish` call:
pass them directly. Its configured handover runs automatically. Scalar and
unbound tasks retain the explicit `result` envelope, which also supports an
inline handover:

```json
{
  "result": "...",
  "handover": {
    "label": "...",
    "task": "..."
  }
}
```

#### EventTool

Give an agent `EventTool()` to let it publish application events:

```python
from agentwerk import EventTool

agent = Agent().tool(EventTool())
```

The model supplies a name and optional JSON data:

```json
{
  "name": "...",
  "data": {}
}
```

Agentwerk records the current task and agent on every event. Werk handlers can
react as events arrive, and queries can retrieve them later:

```python
werk.on_event(lambda _, event: print(event.get_name()))
events = werk.find_events("event = event_name")
```

Event names are open-ended; lowercase snake case is conventional. Publishing an
event does not change the task's status. The exception is the built-in
`task_finished` event, which has the same result and handover behavior as
`FinishTool()`:

```json
{
  "name": "task_finished",
  "data": { "result": "..." }
}
```

#### CommandTool

The `CommandTool` lets you specify exactly which commands and flags are allowed or denied.

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

#### FetchTool

The `FetchTool` fetches a URL and returns its text with the user agent `agentwerk/<version>`. `impersonate()` uses the headers and HTTP/2 settings of a browser.

```python
web = FetchTool().impersonate()
```

#### Custom Tools

Use `concurrent=True` when a custom tool has no side effects and may run in
parallel with other calls.

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

Events record what agents, tools, and LLM providers do during execution.

```python
def log(werk, event):
    if event.get_name() == Event.TASK_FINISHED:
        print(event.get_data().get("result"))


werk.on_event(log)
```

<details>
<summary>All event names and readers</summary>

| | Kind | Description |
|-|------|-------------|
| **Run** | `run_started` | Execution began. |
| | `run_finished` | Execution ended, carrying its outcome. |
| | `policy_violated` | A limit was breached and execution stopped. |
| **Task** | `task_started` | An agent claimed a task. |
| | `task_created` | A task was added to the Werk. |
| | `task_finished` | A task finished successfully, carrying its result when it has one. |
| | `task_failed` | A task failed. |
| | `turn_started` | The agent began another turn on its task. |
| | `schema_retried` | A tool call or result the model created was invalid. |
| **LLM provider** | `request_started` | A request went out to the model. |
| | `request_finished` | A request finished and reported its token usage. |
| | `request_failed` | A request failed and was not retried. |
| | `request_retried` | A temporary LLM provider error triggered a retry. |
| | `text_chunk_received` | Part of the reply arrived. |
| **Tool** | `tool_call_declined` | A tool call proposed by the model was declined. |
| | `tool_call_repaired` | A tool call or value the model created was invalid and was corrected. |
| | `tool_call_started` | A tool invocation began, carrying its registered name, call ID, and raw input. |
| | `tool_call_finished` | A tool invocation finished. |
| | `tool_call_failed` | A tool invocation failed but the task continues. |
| **Knowledge** | `knowledge_written` | A page was written. |
| | `knowledge_read` | A page was read. |
| | `knowledge_removed` | A page was removed. |
| | `knowledge_listed` | The pages were listed. |
| | `knowledge_failed` | An action against the store did not go through. |
| **Compaction** | `compaction_started` | Compaction is about to rewrite the older messages. |
| | `compaction_progress` | Compaction finished part of the work. |
| | `compaction_finished` | Compaction replaced the older messages. |
| | `compaction_failed` | Compaction could not finish. |
| **Application** | name chosen by your application | An event published with `emit_event`. |

### Publish events

Publish custom events through the Werk. Add agent or task context when relevant:

```python
from agentwerk import Event

werk.emit_event(
    Event("document_indexed")
    .data({"documents": 42})
    .task_id("t-1")
    .agent_id("indexer-1")
)

werk.emit_event(Event("index_refreshed"))
```

The first event's name, data, and context are stored as:

```json
{"name":"document_indexed","data":{"documents":42},"task_id":"t-1","agent_id":"indexer-1"}
```

`emit_event` adds the timestamp and a known task's label. Lowercase snake case is
conventional, but other names are accepted; quote names containing spaces or
punctuation in AQL. Built-in events have named constructors, such as
`Event.task_finished()` and `Event.request_started("model")`; their names are
also available as constants. Publishing one runs its hooks and updates its statistics. It is saved according to the event's normal rules, but it does not change task or execution state.

`Werk.emit_event()` never changes a task's status. To let a model finish its
current task through an event, register `EventTool()` on its agent. When the
model emits `task_finished`, the tool validates and stores `data.result`,
optionally creates a handover task, and marks the current task finished. All
other names only publish an event.

Events are saved to `.agentwerk/events.jsonl`. `text_chunk_received` events are not saved. Read events through the Werk with these methods:

| Method | Description |
|--------|-------------|
| `emit_event(event)` | Publish an event for querying and observation. |
| `find_event(query)` | Get the first matching event in query order; without `ORDER BY`, this is the earliest one. |
| `find_events(query)` | Get every matching event in query order; without `ORDER BY`, this is oldest first. |
| `get_input_tokens()` / `get_output_tokens()` | Get token counts across the run's requests. |
| `get_duration()` | Get the elapsed execution duration. |

An event handler usually checks `get_name()` and then reads its payload with
`get_data()`. Use `get_task_id()`, `get_agent_id()`, and `get_label()` to trace
where it came from, and `get_created_at()` to read its timestamp. Application
events can attach a model-facing instruction with `directive(value)` and read
it back with `get_directive()`.

</details>

<details>
<summary>All event query fields and rules</summary>

You can query events with AQL or a condition you supply.

```python
werk.find_events("tool_call_failed")
werk.find_events("event = request_finished AND agent = research-1")
werk.find_events("task = t-3 ORDER BY created DESC")
werk.find_events("payload ~ timeout AND created > -1h")
```

| | Field | Description |
|-|-------|-------------|
| **Match** | `event` | The event name, such as `run_started` or `tool_call_failed`. |
| | `agent` | The attributed agent ID, when the event has agent context. |
| | `task` | The attributed task ID, when the event has task context. |
| | `label` | The attributed task's label, when the task is known and labelled. |
| **Search** | `payload` | Search the event name and data together as text. |
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

An invalid query string raises `ValueError`. `Query(query)` checks a query without running it and raises the same error.

</details>

See [`Event`](https://docs.rs/agentwerk/latest/agentwerk/event/struct.Event.html) and [`Werk`](https://docs.rs/agentwerk/latest/agentwerk/struct.Werk.html).

### Hooks

A hook runs your function when an event, result, failure, or task state change occurs.

```python
def triage(werk, event, failed):
    if failed.get_label() == "scan":
        werk.add_task(Task(failed.get_task(), label="triage"))


werk.on_failure(triage)
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
| | `on_task_async(handler)` | Read a task state change in an async handler. |

Save replies of every finished task as a training example:

```python
def capture(werk, event, task):
    if event.get_name() == Event.TASK_FINISHED:
        model = werk.get_model_for_agent(event.get_agent_id())
        Trajectory.from_task(event.get_agent_id(), model, task).save("datasets")


werk.on_task(capture)
```

#### Async handlers

`on_result` pauses the agent until the hook finishes. Use `on_result_async` for slower work such as storing results in a database, posting them to an HTTP API, or uploading them to object storage. It takes an `async def` that runs on the same event loop waiting for results.

```python
async def store(werk, task, result):
    await database.insert(task.get_id(), result)


werk.on_result_async(store)
```

See [`Werk`](https://docs.rs/agentwerk/latest/agentwerk/struct.Werk.html).

</details>

## Knowledge

`Knowledge` provides durable memory that agents share across tasks and with other agents.

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/knowledge.gif" width="600" />
</div>

Pages use the Open Knowledge Format (OKF).

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
| `set_index_char_limit(count)` | Limit how much of the index is injected into the prompt. |
| `get_index_char_limit()` | Get the index size limit in force. |
| `get_pages()` | Get the page collection for reading and writing pages. |
| `get_pages().get_all()` | Get every page in the store. |
| `clear()` | Remove every page from the store. |

The prompt includes up to 12 000 characters of the knowledge index by default. If the index exceeds the configured limit, the agent reads the remainder from `index.md`. Pages are always saved in full.

Create entries in code:

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

- [Hello World](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/hello_world/): basic example, also available as a [Python example](https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/hello_world.py)
- [Terminal REPL](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/terminal_repl/): interactive terminal chat
- [Divide and Conquer](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/divide_and_conquer/): split an arithmetic problem across agents, also available as a [Python example](https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/divide_and_conquer.py)
- [Deep Research](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/deep_research/): research across several sources (requires `BRAVE_API_KEY`)
- [Malware Scanner](https://github.com/canvascomputing/agentwerk/tree/main/crates/use-cases/src/malware_scanner/): find signs of malware in a software package
- [Apparat Fabrik](https://github.com/canvascomputing/agentwerk/blob/main/crates/agentwerk-py/examples/apparat_fabrik.py): simulate agents inspecting and assembling factory parts

> Configure an LLM provider first (see [Environment](https://github.com/canvascomputing/agentwerk/blob/main/DEVELOPMENT.md#environment)).

```bash
python examples/divide_and_conquer.py 200 4 2
```

## Security

Report a vulnerability to security@canvascomputing.org, not in a public issue. See [SECURITY.md](https://github.com/canvascomputing/agentwerk/blob/main/SECURITY.md).

## Development

See [DEVELOPMENT.md](https://github.com/canvascomputing/agentwerk/blob/main/DEVELOPMENT.md) for build, test, and release instructions.
