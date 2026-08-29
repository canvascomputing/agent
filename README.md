<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/logo.png" width="200" />
</div>

<h1 align="center">agentwerk</h1>

<div align="center">
  <strong>A minimal agentic loop for building efficient harnesses.</strong>
</div>

<div align="center">
  <a href="#installation">Installation</a> •
  <a href="#concepts">Concepts</a> •
  <a href="crates/agentwerk-py/README.md">Python Binding</a> •
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
<div align="center"><a href="crates/agentwerk-py/examples/apparat_fabrik.py">Apparat Fabrik</a></div>
<div align="center"><em>agentwerk pairs "agent" with the German "Werk", a word for both factory and artwork: machinery for building agentic systems.</em></div>

---

## Why use agentwerk?

- **Simple interface:** create agents with a few lines of code.
- **Efficient harness:** optimized for small and fast LLMs with low memory footprint.
- **Complex interactions:** allow agents to collaborate through queues, event hooks and shared knowledge.
- **Deep observability:** inspect every request, tool call, and failure.
- **Facilitate training:** store trajectories based on granular events for fine-tuning models.

## Concepts

| Concept | Role |
|---------|------|
| **Agent** | A model, role, tools, and optional label that claims matching tasks. |
| **Task** | A unit of work with a status, conversation, optional result schema, and result. |
| **Queue** | Runs agents and tasks concurrently and persists the session. |
| **Tool** | An action a model can call. |
| **Schema** | A JSON contract for tool arguments or a task result. |
| **Event** | A record of each step in a run. |
| **Knowledge** | Shared Markdown notes for agents. |

Agents use tools to finish tasks. Each task has its own conversation; near the model's context limit, [compaction](#compaction) summarizes older turns. Handovers create child tasks; hooks submit follow-ups.

## Installation

### Rust

```bash
cargo add agentwerk
```

Also see: [Python implementation](crates/agentwerk-py/README.md).

## Quick Start

```rust
use agentwerk::Agent;
use agentwerk::tools::{GrepTool, ReadFileTool};

#[tokio::main]
async fn main() {
    let agent = Agent::from_env()
        .role("You are a Rust developer who explores source files to answer questions.")
        .tool(ReadFileTool)
        .tool(GrepTool);

    agent.task("Find every `pub trait` defined under src/ and explain each in one sentence.");

    let work = agent.start();
    let result = work.finish_result("ORDER BY created DESC").await.unwrap();

    println!("{}", result.as_str().unwrap_or_default());
}
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

```rust
use agentwerk::tools::ReadFileTool;

let agent = Agent::from_env()
    .role("You are a release manager who prepares release notes.")
    .tool(ReadFileTool);

agent.task("Read CHANGELOG.md and summarize the entries added since the last release.");

agent.start();
```

Optionally, install the [`prompt` skill](skills/prompt/SKILL.md), which is optimized for highly efficient agents with a proven structure for effectiveness.

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
| **Work** | `task(task)` | Submit a task, or a `Task` carrying a label or schema, and return its task key. |
| | `start()` | Begin processing tasks. |
| | `id()` | Get the unique identifier of an agent. |

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

Every value is a variable of its own: `{task}`, `{date}`, `{dir}`, `{platform}`, `{os_version}`, `{turns_remaining}`, `{input_tokens_remaining}`, `{output_tokens_remaining}`, and `{time_remaining}`.

#### Interactive

An interactive agent holds one task open across many turns, so a conversation spans a whole session.

```rust
let agent = Agent::from_env().interactive();
let key = agent.task("Where does the configuration get loaded?");

let chat = agent.start();
chat.on_result(|_, task, result| println!("{}: {result}", task.key));
chat.finish_all_tasks().await;

chat.add_reply(&key, "And which environment variables override it?");
chat.finish_all_tasks().await;

chat.set_task_finished(&key, "answered")?;
```

An interactive agent never finishes its own task, because that would end the conversation. Every answer pauses the task instead: it stays `InProgress` with its agent, and each `finish_all_tasks().await` returns on the answer it waited for. `add_reply(key, content)` drives the next turn, and `set_task_finished(key, result)` ends the conversation, which is the result the hook reports. The answers in between arrive as [events](#events).

See more: [`Agent`](https://docs.rs/agentwerk/latest/agentwerk/agents/agent/struct.Agent.html).

</details>

### Providers

A `Provider` gives agents access to LLMs: Anthropic, OpenAI, Mistral, and a LiteLLM proxy.

```rust
use agentwerk::providers::Anthropic;

let agent = Agent::new()
    .provider(Anthropic::new(key))
    .model("claude-sonnet-4-20250514");
```

<details>
<summary>All provider and model settings</summary>

| Method | Description |
|--------|-------------|
| `provider(provider)` | Define the LLM provider. |
| `model(model)` | Set the model. |
| `Agent::from_env()` | Read the provider and the model from environment variables. |

You can also read the model or provider individually: `.provider(Provider::from_env()?)` or `.model(Model::from_env()?)`.

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

```rust
use agentwerk::providers::{Model, ReasoningEffort};

let agent = Agent::new().model(
    Model::new("my-local-model")
        .context_window(128_000)
        .reasoning_effort(ReasoningEffort::High),
);
```

See [`Provider`](https://docs.rs/agentwerk/latest/agentwerk/providers/struct.Provider.html) and [`Model`](https://docs.rs/agentwerk/latest/agentwerk/providers/struct.Model.html).

</details>

## Tasks

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/tasks.gif" width="600" />
</div>

The `Queue` is the core data structure of agentwerk for coordinating complex interactions.

```rust
use agentwerk::{Agent, Task, Queue};

let analyst = Agent::from_env()
    .label("analysis");

let writer = Agent::from_env()
    .label("report");

let tasks = Queue::new();
tasks.add_agent(analyst).add_agent(writer);

tasks.add_task(Task::labeled("analysis", "Rank all products by value."));
tasks.add_task(Task::labeled("report", "Write up the ranking."));
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
| **Submit and interact** | `add_task(task)` | Submit a task and return its task key. |
| | `add_reply(key, content)` | Add a reply to a task. |
| | `edit_replies(key, editor)` | Rewrite a task's replies now. |
| | `set_task_finished(key, result)` | Finish a task with a result. |
| | `set_task_failed(key)` | Fail a task. |
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
| **Inspect tasks** | `get_task(key)` | Get one task by key. |
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

Use AQL to select and sort tasks. Combine `field operator value` conditions with `AND`, `OR`, or `NOT`; put optional `ORDER BY` last.

`label IN (scan, report) AND status = Finished ORDER BY finished DESC` returns finished scans and reports, newest first.

```rust
tasks.find_tasks("scan");
tasks.find_results("t-3");
tasks.find_tasks("key IN (t-3, t-4)");
tasks.find_tasks("label IN (scan, report) AND status = Finished");
tasks.find_results("scan ORDER BY finished DESC");
```

<details>
<summary>All query terms</summary>

#### Terms

| | Term | Description |
|-|------|-------------|
| **Match** | `label = scan` | Select the tasks carrying the label `scan`. |
| | `label != scan` | Exclude that label, and every task carrying none. |
| | `label IN (scan, report)` | Select the tasks carrying either label. |
| | `label NOT IN (scan, report)` | Exclude both labels. |
| | `label IS EMPTY` | Select the tasks carrying no label. |
| | `label IS NOT EMPTY` | Select the tasks carrying one. |
| | `pending = true` | Select unfinished tasks this run may still schedule. |
| | `cancelled = true` | Select tasks this run has taken off the queue. |
| **Search** | `task ~ "retry budget"` | Search the task body, ignoring case. |
| | `task !~ draft` | Exclude the tasks the text appears in. |
| **Compare** | `failed > -1h` | Select the tasks that failed inside the last hour. |
| | `created >= 2026-08-24` | Select the tasks submitted on that date or later. |
| **Combine** | `A AND B` | Require both terms; `AND` binds tighter than `OR`. |
| | `A OR B` | Require either term. |
| | `NOT A` | Invert a term or a group. |
| | `(A OR B) AND C` | Group terms with parentheses. |
| **Shorten** | `scan` | Select the label `scan`, the short form of `label = scan`. |
| | `t-3` | Select one task by key, the short form of `key = t-3`. |
| **Sort** | `ORDER BY finished DESC` | Answer with the most recently finished first. |
| | `ORDER BY created` | Answer in creation order, which `ASC` also says. |

#### Fields

| | Field | Description |
|-|-------|-------------|
| **Match** | `key` | Match the task key, of the form `t-N`. |
| | `label` | Match the label the task carries. |
| | `status` | Match `Todo`, `InProgress`, `Finished`, or `Failed`. |
| | `pending` | Match whether the task is unfinished and not cancelled. |
| | `cancelled` | Match whether this run has taken the task off the queue. |
| | `agent` | Match the agent that claimed the task. |
| | `parent` | Match the task a handover came from. |
| **Search** | `task` | Search the work the agent was asked to do. |
| | `result` | Search the result the agent produced. |
| | `errors` | Search the failures recorded against the task. |
| **Compare** | `created` | Compare or sort by when the task was submitted. |
| | `started` | Compare or sort by when an agent claimed the task. |
| | `finished` | Compare or sort by when the task reached the `Finished` status. |
| | `failed` | Compare or sort by when the task reached the `Failed` status. |

#### Rules

Operators depend on the field type:

| Field kind | Fields | Operators |
|------------|--------|-----------|
| **Identity** | `key`, `label`, `status`, `pending`, `cancelled`, `agent`, `parent` | Exact match with `=`, `!=`, `IN (...)`, or `NOT IN (...)`. Status accepts `InProgress` and `in_progress`. |
| **Text** | `task`, `result`, `errors` | Case-insensitive contains with `~` or `!~`. |
| **Time** | `created`, `started`, `finished`, `failed` | Compare with `>`, `>=`, `<`, or `<=` against a `YYYY-MM-DD` UTC date, epoch milliseconds, or an offset such as `-30m`, `-2h`, `-7d`, or `-1w`. |
| **Presence** | Any optional field | Check with `IS EMPTY` or `IS NOT EMPTY`; `finished IS EMPTY` selects open tasks. |

Quote values containing spaces: `label = "needs review"`. Lists use parentheses: `label IN (scan, report)`.

Relative times resolve when the query compiles.

`NOT` applies to the next condition or group; `AND` binds before `OR`. Keywords ignore case, but exact labels, keys, and agent IDs do not.

`ORDER BY field` defaults to `ASC`; add `DESC` to reverse it. Without it, tasks stay in creation order. Keys sort numerically and statuses by lifecycle.

#### Examples

```rust
tasks.find_results("report AND result ~ risk");           // reports that mention risk
tasks.find_tasks("errors ~ tool_call_failed");          // saw a tool call fail
tasks.find_tasks("status = Todo AND agent IS EMPTY");   // waiting, never claimed
tasks.find_tasks("failed > -1h ORDER BY failed DESC");  // the last hour's failures
tasks.find_tasks(|t: &Task| t.replies.len() > 4);     // a closure, for what no field carries
```

</details>

### Execution

The task queue schedules the work of your agents and returns their results.

```rust
tasks.start();

if let Some(answer) = tasks.finish_result("ORDER BY created DESC").await {
    println!("{answer}");
}
```

<details>
<summary>All execution methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Run** | `start()` | Begin processing tasks. |
| | `finish_result(query).await` | Wait for matching tasks and get the first result in query order. |
| | `finish_results(query).await` | Wait for matching tasks and get their results. |
| | `finish_all_tasks().await` | Wait for every task and get every result. |
| **Cancel** | `cancel_tasks(query)` | Stop work on matching tasks. |
| | `cancel_all_tasks()` | Stop work on every task. |

Cancellation is run-local: it does not change `status` or persist with the task. `start()` clears cancellation so unfinished tasks can resume. Use `task.is_cancelled()` for a task you hold, or `cancelled = true` and `pending = true` to select by run state.

Task members:

| | Member | Description |
|-|--------|-------------|
| **Identity** | `key` | Task key, of the form `t-N`. |
| | `task` | The work the agent is asked to do. |
| | `label` | Label carried by the task. |
| | `has_label(label)` | Check whether the task carries a label. |
| | `parent` | Identifier of the parent task if a handover was performed. |
| | `reporter` | Identifier of the agent that created the task. |
| | `assignee` | Identifier of the agent that claimed the task. |
| **Outcome** | `status` | The task lifecycle status. |
| | `is_todo()` | Check whether the task is waiting to be claimed. |
| | `is_in_progress()` | Check whether an agent is working on the task. |
| | `is_finished()` | Check whether the task finished. |
| | `is_failed()` | Check whether the task failed. |
| | `is_pending()` | Check whether the task has work in this run. |
| | `is_cancelled()` | Check whether this run has taken the task off the queue. |
| | `result` | The result the agent produced. |
| | `errors` | The failures recorded against the task, as events. |
| | `replies` | Messages exchanged with the model. |
| | `schema` | Optional schema the result must satisfy. |
| **Timestamps** | `created_at` | Creation time, in milliseconds. |
| | `started_at` | Claim time, in milliseconds. |
| | `finished_at` | Finish time, in milliseconds. |
| | `failed_at` | Failure time, in milliseconds. |

See [`Task`](https://docs.rs/agentwerk/latest/agentwerk/agents/tasks/struct.Task.html).

</details>

### Handover

Agents can share the results of their work in the following ways:

1. **Create tasks**: the `finish` tool's `handover` option opens a child task carrying the result.
2. **Read tasks**: the `tasks` tool allows reading any finished task's result, by key.
3. **Read result file**: the `read_file` tool allows reading a task's `result.json` in the session directory.
4. **Share knowledge**: the `knowledge` tool allows sharing knowledge with other agents.
5. **Register hooks**: the `on_result` hook allows creating follow-up tasks.

<details>
<summary>All ways agents pass data</summary>

#### 1. Create tasks

Name the receiving label in the role or in the task, and the agent hands over as it finishes:

```rust
let analyst = Agent::from_env()
    .label("analysis")
    .role("Rank the products by value, then hand the ranking over to `report`.");

let writer = Agent::from_env()
    .label("report")
    .role("Write the board report from the ranking you were handed.");
```

The child task is filed under `report` and names the analysis task as its `parent`. Its body is the result that was handed over, unless the agent passes a task of its own, which may carry `{parent_key}`, `{parent_result}`, and `{parent_result_path}`. Either way the body ends with the parent's key and the path of its result file.

#### 2. Read tasks

Give the writer `TasksTool`, and it reads what any finished task produced, by key:

```rust
let writer = Agent::from_env()
    .label("report")
    .tool(TasksTool);

writer.task("Read the result of t-1, then write the board report.");
```

#### 3. Read result file

Give the writer `ReadFileTool` instead, and it opens the result file named at the end of its task:

```rust
let writer = Agent::from_env()
    .label("report")
    .tool(ReadFileTool);

writer.task("Read .agentwerk/tasks/t-1/result.json, then write the board report.");
```

Results live in the session directory, one `result.json` per task.

#### 4. Share knowledge

Hand both agents one store, and either can write a page the other reads:

```rust
let store = Knowledge::load(".agentwerk")?;

let analyst = Agent::from_env().label("analysis").knowledge(&store);
let writer = Agent::from_env().label("report").knowledge(&store);

analyst.task("Rank the products by value, then save the ranking to your knowledge.");
```

#### 5. Register hooks

Use hooks to create new tasks when certain results arrived:

```rust
tasks.on_result(|queue, done, result| {
    if done.has_label("research") {
        queue.add_task(Task::labeled("report", result.clone()));
    }
});
```

</details>

### Schemas

A `Schema` constrains a task result. agentwerk repairs simple representation errors such as quoted numbers; otherwise, it returns the violation to the model and retries up to `max_schema_retries`.

```rust
use agentwerk::schemas::Schema;

let schema = Schema::new(json!({
    "type": "object",
    "properties": { "title": { "type": "string" } },
    "required": ["title"]
}))?;

tasks.add_task(Task::new("Write a report.").schema(schema));
```

For small models, use shallow, focused schemas with few required fields, clear names, and simple enums. Split large results into labeled tasks with separate schemas, then combine them in a later task. Deep nesting, long property lists, and large `anyOf` or `oneOf` branches waste context and trigger retries.

<details>
<summary>All schema methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Schema** | `Schema::new(document)` | Create a schema. |
| | `validate(value)` | Validate content. |
| | `get_raw_schema()` | Read the JSON Schema document the schema was built from. |
| **SchemaStore** | `SchemaStore::new()` | Create a store of schemas bound to labels. |
| | `label(label, document)` | Bind a schema to a label. |
| | `get(label)` | Read back the schema bound to a label. |
| | `tasks.set_schemas(store)` | Enforce schemas for task results. |

A `SchemaStore` enforces schemas for all tasks with a certain label. Registering schemas centrally spares agents from passing complex schema structures during task creation (see `TasksTool`) and handovers (see `FinishTool`):

```rust
use agentwerk::SchemaStore;

let schemas = SchemaStore::new();
schemas.label("report", json!({
    "type": "object",
    "properties": { "title": { "type": "string" } },
    "required": ["title"]
}))?;

tasks.set_schemas(&schemas);
```

See [`Schema`](https://docs.rs/agentwerk/latest/agentwerk/schemas/struct.Schema.html) and [`SchemaStore`](https://docs.rs/agentwerk/latest/agentwerk/schemas/struct.SchemaStore.html).

</details>

### Configuration

A `Policy` limits the turns, tokens, and time a run may spend, and allows configuring retries and compaction.

```rust
tasks.set_policy(Policy {
    max_turns: Some(40),
    max_time: Some(std::time::Duration::from_secs(300)),
    ..Default::default()
});
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

`set_policy(policy)` replaces the whole configuration, and `get_policy()` reads it back. A violated limit emits `EventKind::PolicyViolated`, see [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html). `compaction_threshold` is the exception, see [Compaction](#compaction).

</details>

### Compaction

Compaction summarizes a task's older messages once they no longer fit the model's context window.

```rust
tasks.set_policy(Policy {
    compaction_threshold: Some(0.7),
    ..Default::default()
});
```

<details>
<summary>When compaction runs and what it reports</summary>

`compaction_threshold` is a fraction of the model's context window, `0.85` by default. Reaching it summarizes the older messages and the agent carries on.

Compaction also runs after the LLM provider reports the window exceeded. `CompactionStarted`, `CompactionProgress`, `CompactionFinished`, and `CompactionFailed` report each step, see [Events](#events).

```rust
tasks.on_event(|_, event| {
    if let EventKind::CompactionFinished { reason } = &event.kind {
        eprintln!("[{}] compacted {reason}", event.task_key);
    }
});
```

Each of the compaction events carries the reason it ran: `Proactive` ahead of the failure, `Reactive` after it. Replies that still exceed the window after a reactive compaction fail the task.

</details>

### Directives

A directive tells the model how to recover: call a tool, match a schema, correct a path, narrow a search, or choose an allowed command. You can replace the built-in wording for your model or environment.

```rust
use agentwerk::Directive;

let agent = Agent::from_env()
    .directives(|key| match key {
        Directive::GREP_FAILED => Some("The search did not run. Narrow `path`."),
        _ => None,
    });
```

<details>
<summary>All directive settings</summary>

| Method | Description |
|--------|-------------|
| `directives(compute)` | Decide every directive's text with one function. |
| `Directive::ALL` | Get every directive key, in the order the catalogue declares them. |

Return `None` to keep the default. Replacements may use template variables such as `{detail}`, `{attempt}`, and `{path}`. Include the failure and next action.

See [prompts/directives](https://github.com/canvascomputing/agentwerk/tree/main/crates/agentwerk/src/prompts/directives) for the built-in text.

</details>

### Sessions

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/sessions.gif" width="600" />
</div>

A `Queue` writes every task, reply, and event to its working directory (default `./.agentwerk`). You can continue a session from that directory.

```rust
let tasks = Queue::load(".agentwerk")?;
tasks.add_agent(my_agent);
tasks.start();
```

<details>
<summary>All session files</summary>

```
.agentwerk/
├── events.jsonl                          every event (one per line)
├── tasks/
│   └── t-1/
│       ├── task.json                   the task without its messages (key, status, label, timestamps)
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

```rust
use agentwerk::tools::{CommandTool, GrepTool, ReadFileTool};

let agent = Agent::new()
    .tool(ReadFileTool)
    .tool(GrepTool)
    .tool(CommandTool::new("git").allow("git *"));
```

<details>
<summary>All built-in and custom tools</summary>

| | Tool | Description |
|-|------|-------------|
| **File** | `ReadFileTool` | Read a file with line numbers, offset, and limit. |
| | `WriteFileTool` | Create or overwrite a file. |
| | `EditFileTool` | Replace text in a file. |
| **Search** | `GlobTool` | Find files by pattern. |
| | `GrepTool` | Search file contents by regular expression, or by code shape with `syntax: "code"`. |
| | `ListDirectoryTool` | List files and directories. |
| **Command** | `CommandTool` | Give access to specific commands. |
| **Web** | `FetchUrlTool` | Fetch a URL and read its body. |
| **Tasks** | `FinishTool` | Write the result for the current task and mark it finished. |
| | `TasksTool` | Read the task queue and create or edit tasks. |
| **Knowledge** | `KnowledgeTool` | Write, read, remove, or list pages in a knowledge store. |

#### `FinishTool` and `KnowledgeTool`

`FinishTool` and `KnowledgeTool` are special tools, registered automatically on every agent. They are used for interacting with the `Queue` or knowledge base. An [interactive agent](#interactive) gets no `FinishTool` by default, since finishing its task would end the conversation.

#### CommandTool

The `CommandTool` allows you to granularly define what commands are allowed and what commands are denied.

```rust
let git = CommandTool::new("git")
    .allow("git status")
    .allow("git log *")
    .deny("git push*")
    .deny_flag("--force");
```

With an `allow_flag` set, a command carrying any other flag is refused:

```rust
let cargo = CommandTool::new("cargo")
    .allow("cargo test*")
    .allow_flag("--all-features");
```

#### FetchUrlTool

The `FetchUrlTool` fetches a URL and returns its text, requesting it with the user agent `agentwerk/<version>`. `impersonate()` swaps in the headers and HTTP/2 settings a browser sends.

```rust
let web = FetchUrlTool::new().impersonate();
```

#### Custom Tools

You can define custom tools for specific needs with the following parameters:

| Method | Description |
|--------|-------------|
| `concurrent(true)` | If a tool has no side-effects you can run it in parallel with this option. |
| `paths(["path"])` | Name file path used for a tool call, so the files are included in statistics. |

Describe the tool, then hand it the code it runs:

```rust
use agentwerk::tools::{Tool, ToolResult};
use serde_json::Value;

let greet = Tool::new("greet")
    .description("Say hello")
    .schema(json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"]
    }))
    .concurrent(true)
    .handler(|input: Value, _context| async move {
        let name = input["name"].as_str().unwrap_or("world");
        ToolResult::success(format!("Hello, {name}!"))
    })
    .build();
```

Return `ToolResult::error(message)` for a failure the model should work around.

See [`Tool`](https://docs.rs/agentwerk/latest/agentwerk/tools/struct.Tool.html).

</details>

## Events

Events allow you to inspect all activities of your agents.

```rust
use agentwerk::event::EventKind;

tasks.on_event(|_, event| {
    if let EventKind::TaskFinished = &event.kind {
        eprintln!("[{}] done {} {:?}", event.agent_id, event.task_key, event.label);
    }
});
```

<details>
<summary>All event kinds and readers</summary>

| | Kind | Description |
|-|------|-------------|
| **Run** | `RunStarted` | Execution began. |
| | `RunFinished` | Execution ended, carrying the reason. |
| | `PolicyViolated` | A limit was breached and execution stopped. |
| **Task** | `TaskStarted` | An agent claimed a task. |
| | `TaskFinished` | A task finished successfully. |
| | `TaskFailed` | A task failed. |
| | `TurnStarted` | The agent began another turn on its task. |
| | `SchemaRetried` | A tool call or result the model created was invalid. |
| **LLM provider** | `RequestStarted` | A request went out to the model. |
| | `RequestFinished` | A request finished and reported its token usage. |
| | `RequestFailed` | A request failed and was not retried. |
| | `RequestRetried` | A transient provider error triggered a retry. |
| | `TextChunkReceived` | A piece of the reply arrived. |
| | `ResponseRepaired` | A tool call or value the model created was invalid and was corrected. |
| **Tool** | `ToolCallDeclined` | A tool call proposed by the model was declined. |
| | `ToolCallStarted` | A tool invocation began. |
| | `ToolCallFinished` | A tool invocation finished. |
| | `ToolCallFailed` | A tool invocation failed but the task continues. |
| **File** | `FileOpenFinished` | A tool opened a file. |
| | `FileOpenFailed` | A tool could not open a file. |
| **Knowledge** | `KnowledgeWritten` | A page was written. |
| | `KnowledgeRead` | A page was read. |
| | `KnowledgeRemoved` | A page was removed. |
| | `KnowledgeListed` | The pages were listed. |
| | `KnowledgeFailed` | An action against the store did not go through. |
| **Compaction** | `CompactionStarted` | Compaction is about to rewrite the older messages. |
| | `CompactionProgress` | Compaction finished part of the work. |
| | `CompactionFinished` | Compaction replaced the older messages. |
| | `CompactionFailed` | Compaction could not finish. |

Every event is written to the session log. You read events from the task queue, or from the session directory in `.agentwerk/events.jsonl`:

| Method | Description |
|--------|-------------|
| `find_event(query)` | Get the earliest recorded event matching an AQL query, or the first in the order it names. |
| `find_events(query)` | Get every recorded event matching an AQL query, oldest first. |
| `get_input_tokens()` / `get_output_tokens()` | Get token counts across the run's requests. |
| `get_duration()` | Get the elapsed execution duration. |

You can query events with AQL syntax or closures.

```rust
tasks.find_events("tool_call_failed");
tasks.find_events("event = request_finished AND agent = research-1");
tasks.find_events("task = t-3 ORDER BY created DESC");
tasks.find_events("payload ~ timeout AND created > -1h");
```

| | Field | Description |
|-|-------|-------------|
| **Match** | `event` | Match the kind, as `run_started`, `tool_call_failed`, and the rest are spelled. |
| | `agent` | Match the agent that emitted the event. |
| | `task` | Match the task the event concerns, empty on `RunStarted` and `RunFinished`. |
| | `label` | Match the label that task carries. |
| **Search** | `payload` | Search what the kind carries, its name included. |
| **Compare** | `created` | Compare or sort by when the event happened. |

- An event query takes the same operators, `AND` / `OR` / `NOT`, and `ORDER BY` a [task query](#queries) does.
- `IS EMPTY` and `IS NOT EMPTY` read `agent`, `task`, and `label`.
- A lone word is the short form of `event = <word>` when it names an event, and of `label = <word>` when it does not. A lone `t-N` is the short form of `task = t-N`.

See [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html) and [`Queue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tasks/struct.Queue.html).

</details>

### Hooks

Hooks allow you to react to events.

```rust
tasks.on_failure(|queue, _, failed| {
    if failed.has_label("scan") {
        queue.add_task(Task::labeled("triage", failed.task.clone()));
    }
});
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

```rust
tasks.on_task(|queue, event, task| {
    if matches!(event.kind, EventKind::TaskFinished) {
        let model = queue.get_model_for_agent(&event.agent_id);
        let _ = Trajectory::from_task(&event.agent_id, model.as_deref(), task)
            .save("datasets");
    }
});
```

#### Async handlers

`on_result` is blocking and prevents an agent continuing its work till the hook is finished. If you perform time-consuming operations use `on_result_async` instead: storing results in a database, posting them to an HTTP API, or uploading them to object storage.

```rust
let findings = Arc::clone(&database);
tasks.on_result_async(move |_, task, result| {
    let findings = Arc::clone(&findings);
    async move {
        let _ = findings.insert(&task.key, &result).await;
    }
});
```

See [`Queue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tasks/struct.Queue.html).

</details>

## Knowledge

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/knowledge.gif" width="600" />
</div>

`Knowledge` allows agents to share insights or learnings. Knowledge pages are created in the Open Knowledge Format (OKF).

```rust
use agentwerk::Knowledge;

let store = Knowledge::load("./notes")?;
let alice = Agent::new().knowledge(&store);
let bob = Agent::new().knowledge(&store);
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

```rust
use agentwerk::agents::knowledge::Page;

store.pages().save(Page {
    slug: "build-command".into(),
    kind: String::new(),
    description: "How the project is built.".into(),
    content: "Run `make` to compile.".into(),
    tags: vec!["build".into()],
})?;

let page = store.pages().load("build-command")?;
store.pages().remove("build-command")?;
```

See [`Knowledge`](https://docs.rs/agentwerk/latest/agentwerk/agents/knowledge/struct.Knowledge.html).

</details>

## Use Cases

Example projects built with agentwerk:

- [Hello World](crates/use-cases/src/hello_world/): basic example
- [Terminal REPL](crates/use-cases/src/terminal_repl/): minimal interactive chat
- [Divide and Conquer](crates/use-cases/src/divide_and_conquer/): arithmetic problem shared across agents
- [Deep Research](crates/use-cases/src/deep_research/): deep research pipeline (requires `BRAVE_API_KEY`)
- [Malware Scanner](crates/use-cases/src/malware_scanner/): identify indicators of compromise in a software package

> Configure an LLM provider first (see [Environment](DEVELOPMENT.md#environment)).

```bash
make use_case                # list available names
make use_case name=<name>    # run one
```

## Security

Report a vulnerability to security@canvascomputing.org, not in a public issue. See [SECURITY.md](SECURITY.md).

## Development

See [DEVELOPMENT.md](DEVELOPMENT.md).
