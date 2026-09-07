<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/logo.png" width="200" />
</div>

<h1 align="center">agentwerk</h1>

<div align="center">
  <strong>A minimal agentic loop for building efficient harnesses.</strong>
</div>

<div align="center">
  <a href="#installation">Installation</a> •
  <a href="crates/agentwerk-py/README.md">Python Binding</a> •
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
<div align="center"><a href="crates/agentwerk-py/examples/apparat_fabrik.py">Apparat Fabrik</a></div>
<div align="center"><em>“Werk” is German for both a factory and a work of art.</em></div>

---

## Why use agentwerk?

- **Simple interface:** create agents with a few lines of code.
- **Efficient harness:** optimized for small and fast LLMs with low memory footprint.
- **Complex interactions:** let agents collaborate through a shared Werk, event hooks, and knowledge.
- **Deep observability:** inspect every request, tool call, and failure.
- **Facilitate training:** store trajectories based on granular events for fine-tuning models.

## Installation

Install the Rust crate from crates.io or follow the [separate guide](crates/agentwerk-py/README.md) for its Python bindings.

### Rust

```bash
cargo add agentwerk
```

## Quick Start

This example gives one agent read-only tools to search Rust source files, then waits for one result.

```rust
use agentwerk::Agent;
use agentwerk::tools::{GrepTool, ReadFileTool};

#[tokio::main]
async fn main() {
    let agent = Agent::from_env()
        .role("You are a Rust developer who explores source files to answer questions.")
        .tool(ReadFileTool)
        .tool(GrepTool);

    let task = agent.add_task("Find every `pub trait` defined under src/ and explain each in one sentence.");

    let result = agent.finish_task(task).await.unwrap();

    println!("{}", result.as_str().unwrap_or_default());
}
```

## API

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

```rust
use agentwerk::tools::ReadFileTool;

let agent = Agent::from_env()
    .role("You are a release manager who prepares release notes.")
    .tool(ReadFileTool);

agent.add_task("Read CHANGELOG.md and summarize the entries added since the last release.");

let results = agent.finish().await;
```

The [prompt skill](skills/prompt/SKILL.md) provides a compact template for writing agent roles.

<details>
<summary>Agent reference</summary>

| | Method | Description |
|-|--------|-------------|
| **Configure** | `role(role)` | Define who the agent is and how it should work. |
| | `tool(tool)` | Register a tool the agent may call. |
| | `tools(tools)` | Register several tools the agent may call. |
| | `label(label)` | Restrict the agent to tasks carrying this label. |
| | `dir(dir)` | Set the directory the agent has access to. |
| | `template(key, value)` | Inject data into prompts with template strings. |
| | `templates(variables)` | Inject more than one entry into prompts. |
| | `directive(key, template)` | Set a directive template by key. |
| | `directives(overrides)` | Set more than one directive template. |
| | `knowledge(store)` | Share a knowledge store with the agent. |
| | `interactive()` | Let the agent wait for new instructions to keep a task in-progress. |
| **Work** | `add_task(task)` | Submit a task, or a `Task` carrying a label or schema, and return its task ID. |
| | `start()` | Keep processing tasks in the background. |
| | `finish_task(query)` | Wait for all matches and return the first result in query order. |
| | `finish_tasks(query)` | Wait for matching tasks and get their results. |
| | `finish()` | Run tasks and return their results. |
| | `get_id()` | Get the unique identifier of an agent. |

Use `{context}` in a role to include the current task and execution limits:

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

An interactive agent keeps a task open across replies. It has no completion tool by default.

```rust
let agent = Agent::from_env().interactive();
let id = agent.add_task("Where does the configuration get loaded?");

let werk = agent.start();
werk.on_result(|_, task, result| println!("{}: {result}", task.get_id()));
werk.finish().await;

werk.add_reply(&id, "And which environment variables override it?");
werk.finish().await;

werk.set_task_finished(&id, "answered")?;
```

Replies pause the task in `in_progress`, and completion methods return when it pauses. Use `add_reply(id, content)` to resume and `set_task_finished(id, result)` to end the conversation. Intermediate replies arrive as [events](#events); `on_result` receives the final result.

See [`Agent`](https://docs.rs/agentwerk/latest/agentwerk/agents/agent/struct.Agent.html).

</details>

### Providers

An LLM provider connects agents to Anthropic, OpenAI, Mistral, or a LiteLLM proxy.

```rust
use agentwerk::providers::Anthropic;

let agent = Agent::new()
    .provider(Anthropic::new(key))
    .model("claude-sonnet-4-20250514");
```

<details>
<summary>Provider and model reference</summary>

| Method | Description |
|--------|-------------|
| `provider(provider)` | Define the LLM provider. |
| `model(model)` | Set the model. |
| `Agent::from_env()` | Read the provider and the model from environment variables. |
| `verify(model)` | Verify that the provider can answer with a model. |
| `Anthropic::new(key).base_url(url).timeout(duration)` | Configure an Anthropic endpoint. The OpenAI, Mistral, and LiteLLM types expose the same methods. |

You can also read the model or provider individually: `.provider(Provider::from_env()?)` or `.model(Model::from_env()?)`.

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
| `MODEL` | Set the model name. |
| `ANTHROPIC_MODEL`, `OPENAI_MODEL`, `MISTRAL_MODEL`, `LITELLM_MODEL` | Set the model for the detected provider when `MODEL` is unset. |
| `MODEL_CONTEXT_WINDOW` | Set the context window size in tokens. |

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

## Werk

A `Werk` stores tasks, assigns them to matching agents, and records their results.

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/werk.gif" width="600" />
</div>

```rust
use agentwerk::{Agent, Task, Werk};

let analyst = Agent::from_env()
    .label("analysis");

let writer = Agent::from_env()
    .label("report");

let werk = Werk::new();
werk.add_agent(analyst).add_agent(writer);

werk.add_task(Task::labeled("analysis", "Rank all products by value."));
werk.add_task(Task::labeled("report", "Write up the ranking."));
```

<details>
<summary>Werk reference</summary>

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
| **Run** | `start()` | Keep processing tasks in the background. |
| | `finish_task(query)` | Wait for all matches and return the first result in query order. |
| | `finish_tasks(query)` | Wait for matching tasks and get their results. |
| | `finish()` | Run tasks and return their results. |
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

Agent Query Language (AQL) filters tasks and events. Pass an AQL string
directly, or compile it with `Query::new` to reuse it.

```rust
// Find tasks labeled `scan`.
werk.find_tasks("scan");

// Find failed tool calls from scan tasks.
werk.find_events("scan AND event.name = tool_call_failed");

// Find the result produced by task `t-3`.
werk.find_results("t-3");
```

<details>
<summary>Query reference</summary>

#### Terms

| | Syntax | Meaning |
|-|--------|---------|
| **Match** | `field = value`, `field != value` | Include or exclude one exact value. |
| | `field IN (a, b)`, `field NOT IN (a, b)` | Include or exclude a list. |
| **Presence** | `field IS EMPTY`, `field IS NOT EMPTY` | Test whether an optional field has a value. |
| **Search** | `field ~ text`, `field !~ text` | Include or exclude case-insensitive text. |
| **Compare** | `field > value`, `>=`, `<`, `<=` | Compare a time field. |
| **Combine** | `A AND B`, `A OR B`, `NOT A`, `(A OR B)` | Combine or group conditions. |
| **Task label** | `scan`, `"needs review"` | Short for `task.label = scan`; quote labels containing spaces or query words. |
| **Task ID** | `t-3` | Short for `task.id = t-3`; IDs take precedence over labels. |
| **Sort** | `ORDER BY field DESC` | Sort matches; `ASC` is the default. |

#### Fields

| Origin | Fields |
|--------|--------|
| **Task** | `task.id`, `task.label`, `task.status`, `task.pending`, `task.cancelled`, `task.assignee`, `task.input`, `task.result`, `task.errors`, `task.created`, `task.started`, `task.finished`, `task.failed` |
| **Event** | `event.name`, `event.agent_id`, `event.task_id`, `event.label`, `event.created`, `event.data` |

Queries using both namespaces match events with their referenced tasks. Events without an existing task do not match. Joined matches default to event-log order; `ORDER BY` accepts task or event fields.

Result finders return raw results where `task.result` is present. They select finished tasks unless the query specifies another status.

Completion methods and `cancel_tasks` also accept AQL. Event and joined queries snapshot matching task IDs when the operation starts. Task-only cancellation also applies to later matching tasks.

#### Rules

Missing values do not match `!=`. Include unlabeled tasks with `task.label IS EMPTY OR task.label != scan`.

Qualify fields in full expressions. Use parentheses when mixing `AND` and `OR`; `NOT` applies to the next condition or group. Query keywords ignore case; labels and IDs do not.

Times accept UTC dates such as `2026-08-30`, epoch milliseconds, or offsets such as `-30m`, `-2h`, `-7d`, and `-1w`. Offsets are resolved when a query is compiled; reusing a compiled query keeps its original cutoff.

Missing sort values come last in either direction. Tasks and results selected through events follow matching event order, with each task returned once. Events selected through tasks follow task order, then log order within each task.

#### Examples

```rust
// Find tasks referenced by failed tool-call events.
werk.find_tasks("event.name = tool_call_failed");

// Find events attached to tasks labeled `scan`.
werk.find_events("scan");

// Find results produced by tasks with finished events.
werk.find_results("event.name = task_finished");

werk.find_results("report AND task.result ~ risk");
werk.find_tasks("task.errors ~ tool_call_failed");
werk.find_tasks("task.status = todo AND task.assignee IS EMPTY");
werk.find_tasks("task.failed > -1h ORDER BY task.failed DESC");
werk.find_tasks(|t: &Task| t.get_replies().len() > 4);   // a closure, for what no field carries
```

</details>

### Execution

`start()` keeps processing tasks in the background. `finish()` runs tasks and waits for results.

```rust
let task = werk.add_task("Write a report.");

if let Some(answer) = werk.finish_task(task).await {
    println!("{answer}");
}
```

<details>
<summary>Execution and task reference</summary>

| | Method | Description |
|-|--------|-------------|
| **Run** | `start()` | Keep processing tasks in the background. |
| | `finish_task(query)` | Wait for all matches and return the first result in query order. |
| | `finish_tasks(query)` | Wait for matching tasks and get their results. |
| | `finish()` | Run tasks and return their results. |
| **Cancel** | `cancel_tasks(query)` | Stop work on matching tasks. |
| | `cancel_all_tasks()` | Stop work on every task. |

Cancellation affects only the current execution, not persisted task status. Starting a new run with `start()` clears cancellation. Inspect it with `task.is_cancelled()`, or query execution state with `task.cancelled = true` and `task.pending = true`.

Task members:

| | Member | Description |
|-|--------|-------------|
| **Identity** | `get_id()` | Get the task ID, of the form `t-N`. |
| | `get_task()` | Get the work the agent is asked to do. |
| | `get_label()` | Get the label carried by the task. |
| | `get_reporter()` | Get the ID of the agent that created the task. |
| | `get_assignee()` | Get the ID of the agent that claimed the task. |
| **Outcome** | `get_status()` | Get the task's current status. |
| | `is_todo()` | Check whether the task is waiting to be claimed. |
| | `is_in_progress()` | Check whether an agent is working on the task. |
| | `is_finished()` | Check whether the task finished. |
| | `is_failed()` | Check whether the task failed. |
| | `is_pending()` | Check whether the task has work in this run. |
| | `is_cancelled()` | Check whether this run has excluded the task from scheduling. |
| | `get_result()` | Get the result the agent produced. |
| | `get_errors()` | Get failures recorded against the task as events. |
| | `get_replies()` | Get messages exchanged with the model. |
| | `get_schema()` | Get the optional schema the result must satisfy. |
| **Timestamps** | `get_created_at()` | Get the creation time in milliseconds. |
| | `get_started_at()` | Get the claim time in milliseconds. |
| | `get_finished_at()` | Get the finish time in milliseconds. |
| | `get_failed_at()` | Get the failure time in milliseconds. |

See [`Task`](https://docs.rs/agentwerk/latest/agentwerk/struct.Task.html).

</details>

### Sharing results

Agents can pass work and results in five ways:

1. **Result hook**: `on_result` creates follow-up tasks from completed work.
2. **Template variables**: `template` injects a result into another agent's prompt.
3. **Knowledge**: the `knowledge` tool shares durable pages between agents.
4. **Task tool**: the `task` tool reads any finished task's result by ID.
5. **Read result file**: the `read_file` tool opens a task's `result.json` in the session directory.

<details>
<summary>Result-sharing examples</summary>

#### 1. Result hook

Use hooks to create new tasks when certain results arrive:

```rust
werk.on_result(|werk, done, result| {
    if done.get_label() == Some("research") {
        werk.add_task(Task::labeled("report", result.clone()));
    }
});
```

#### 2. Template variables

Inject a result into the next agent's role or text task:

```rust
let writer = Agent::from_env()
    .role("Use this research:\n{research}")
    .template("research", research);

writer.add_task("Write the board report.");
```

#### 3. Knowledge

Hand both agents one store, and either can write a page the other reads:

```rust
let store = Knowledge::load(".agentwerk/knowledge")?;

let analyst = Agent::from_env().label("analysis").knowledge(&store);
let writer = Agent::from_env().label("report").knowledge(&store);

analyst.add_task("Rank the products by value, then save the ranking to your knowledge.");
```

#### 4. Task tool

Give the writer `TaskTool`, and it reads what any finished task produced, by ID:

```rust
let writer = Agent::from_env()
    .label("report")
    .tool(TaskTool);

writer.add_task("Read the result of t-1, then write the board report.");
```

#### 5. Read result file

Give the writer `ReadFileTool` instead, and it opens the result file named at the end of its task:

```rust
let writer = Agent::from_env()
    .label("report")
    .tool(ReadFileTool);

writer.add_task("Read .agentwerk/tasks/t-1/result.json, then write the board report.");
```

Results live in the session directory, one `result.json` per task.

</details>

### Schemas

A `Schema` defines the required shape of a task result.

```rust
use agentwerk::schemas::Schema;

let schema = Schema::new(json!({
    "type": "object",
    "properties": { "title": { "type": "string" } },
    "required": ["title"]
}))?;

werk.add_task(Task::new("Write a report.").schema(schema));
```

<details>
<summary>Schema reference</summary>

Quoted JSON numbers, booleans, objects, and arrays are converted to the schema's expected type. String enums allow case and outer-whitespace corrections when they identify one candidate; enum correction never changes JSON type. Other result violations trigger a retry, subject to `max_schema_retries`.

Use shallow, focused schemas for small models. Split complex work into tasks with separate schemas.

| | Method | Description |
|-|--------|-------------|
| **Schema** | `Schema::new(document)` | Create a schema. |
| | `validate(value)` | Return the validated value and JSON pointers to repaired values, or report violations. |
| | `get_raw_schema()` | Read the JSON Schema document the schema was built from. |

See [`Schema`](https://docs.rs/agentwerk/latest/agentwerk/schemas/struct.Schema.html).

</details>

### Configuration

A `Policy` sets limits for turns, tokens, elapsed time, retries, and compaction.

```rust
werk.set_policy(Policy {
    max_turns: Some(40),
    max_time: Some(std::time::Duration::from_secs(300)),
    ..Default::default()
});
```

<details>
<summary>Configuration reference</summary>

| Field | Description |
|-------|-------------|
| `max_turns` | Limit the total number of turns. |
| `max_time` | Limit the total elapsed duration. |
| `max_input_tokens` | Limit the total input tokens. |
| `max_output_tokens` | Limit the total output tokens. |
| `max_request_tokens` | Limit the output tokens of a single request. |
| `max_schema_retries` | Limit consecutive failed tool calls or silent replies; a successful call resets the count. |
| `max_request_retries` | Limit how often a failing request is retried. |
| `request_retry_delay` | Set the base delay for exponential backoff between request retries. |
| `compaction_threshold` | Compact once the next request would fill this share of the window. |

`set_policy(policy)` replaces the whole configuration, and `get_policy()` reads it back. A violated limit emits `Event::POLICY_VIOLATED`. `compaction_threshold` is the exception, see [Compaction](#compaction).

</details>

### Compaction

Compaction summarizes older messages as a task approaches the model's context limit or after the provider reports an overflow.

```rust
werk.set_policy(Policy {
    compaction_threshold: Some(0.7),
    ..Default::default()
});
```

<details>
<summary>Compaction reference</summary>

`compaction_threshold` is a fraction of the model's context window, `0.85` by default. Reaching it summarizes the older messages and the agent continues its task.

Compaction also runs after the LLM provider reports that the window was exceeded. `compaction_started`, `compaction_progress`, `compaction_finished`, and `compaction_failed` report each step, see [Events](#events).

```rust
werk.on_event(|_, event| {
    if event.get_name() == Event::COMPACTION_FINISHED {
        eprintln!("[{}] compacted {}", event.get_task_id(), event.get_data()["trigger"]);
    }
});
```

Each compaction event carries the trigger: `proactive` before a context-window error or `reactive` after one. A failure also carries a stable `kind` and human-readable `message`.

</details>

### Directives

Directives tell the model how to recover from failures. Override their wording for your model or environment.

```rust
let agent = Agent::from_env()
    .directive("grep_failed", "The search did not run. Narrow `path`.")
    .directives([
        ("tool_timed_out", "Reduce the command scope."),
        ("cache_miss", "No cache entry exists for {path}."),
    ]);
```

<details>
<summary>Directive reference</summary>

Built-in keys override recovery text; keys without overrides retain their defaults. Templates accept runtime values such as `{detail}`, `{attempt}`, and `{path}`. Placeholders without a value remain unchanged.

For non-terminal `EventTool` events, use the event name as the key to replace the acknowledgement sent to the model. The template can use `{data}` for the JSON payload or a top-level field such as `{path}`. The event and tool result record the directive key.

See [prompts/directives](https://github.com/canvascomputing/agentwerk/tree/main/crates/agentwerk/src/prompts/directives) for the built-in text.

</details>

### Sessions

A `Werk` saves tasks, replies, and recorded events so you can resume a session.

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/sessions.gif" width="600" />
</div>

The session directory is `./.agentwerk` by default.

```rust
let werk = Werk::load(".agentwerk")?;
werk.add_agent(my_agent);
werk.start();
```

<details>
<summary>Session files</summary>

```
.agentwerk/
├── events.jsonl                        recorded events, one per line
├── tasks/
│   └── t-1/
│       ├── task.json                   task metadata and input
│       ├── result.json                 task result
│       ├── replies.jsonl               model messages, one per line
│       └── outputs/<tool_use_id>.txt   full tool outputs
└── knowledge/
    ├── pages/<slug>.md                 knowledge pages
    └── index.md                        knowledge index
```

</details>

## Tools

Tools give agents controlled access to files, commands, URLs, tasks, and shared knowledge.

```rust
use agentwerk::tools::{CommandTool, GrepTool, ReadFileTool};

let agent = Agent::new()
    .tool(ReadFileTool)
    .tool(GrepTool)
    .tool(CommandTool::new("git").allow("git *"));
```

<details>
<summary>Tool reference</summary>

| | Tool | Description |
|-|------|-------------|
| **File** | `ReadFileTool` | Read a file with line numbers, offset, and limit. |
| | `WriteFileTool` | Create or overwrite a file. |
| | `EditFileTool` | Replace text in a file. |
| **Search** | `GlobTool` | Find files by pattern. |
| | `GrepTool` | Search file contents by regular expression, or by code shape with `syntax: "code"`. |
| | `ListDirectoryTool` | List files and directories. |
| **Command** | `CommandTool` | Give access to specific commands. |
| **Web** | `FetchTool` | Fetch a URL and read its body. |
| **Events** | `EventTool` | Publish an event. `task_finished` also completes the current task. |
| **Tasks** | `FinishTool` | Write the result for the current task and mark it finished. |
| | `TaskTool` | Read the Werk and create or edit tasks. |
| **Knowledge** | `KnowledgeTool` | Write, read, remove, or list pages in a knowledge store. |

#### Timeouts

Override a tool's limit with `timeout(duration)`; zero disables it.

```rust
use std::time::Duration;
use agentwerk::tools::FetchTool;

let quick_fetch = FetchTool::new().timeout(Duration::from_secs(15));
let patient_fetch = FetchTool::new().timeout(Duration::ZERO);
```

| Tool | Default timeout |
|------|-----------------|
| Custom tools | None |
| `FetchTool` | 60 seconds |
| `GrepTool` | 180 seconds |
| `CommandTool` | The call's `timeout_ms`, or 120 seconds if omitted |

#### `FinishTool` and `KnowledgeTool`

`KnowledgeTool` is registered automatically. Non-interactive agents also get `FinishTool`; [interactive agents](#interactive) receive it only if you add it. Add `EventTool` explicitly to enable custom events.

When a task carries an object schema, its displayed fields are the `finish` call: pass them directly. Scalar and unbound tasks retain the explicit `result` envelope:

```json
{
  "result": "..."
}
```

#### EventTool

Give an agent `EventTool` to let it publish custom events:

```rust
use agentwerk::tools::EventTool;

let agent = Agent::new().tool(EventTool);
```

The model supplies a name and optional JSON data:

```json
{
  "name": "...",
  "data": {}
}
```

Events carry the current task and agent context; see [Events](#events) for handlers and queries. Names are unrestricted; lowercase snake case is conventional.

Only `task_finished` completes the current task. Its result goes in `data.result`, including when the result is an object:

```json
{
  "name": "task_finished",
  "data": { "result": "..." }
}
```

Use [Directives](#directives) to customize the acknowledgement sent to the model.

#### CommandTool

The `CommandTool` lets you specify exactly which commands and flags are allowed or denied.

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

#### FetchTool

The `FetchTool` fetches a URL and returns its text with the user agent `agentwerk/<version>`. `impersonate()` uses the headers and HTTP/2 settings of a browser.

```rust
let web = FetchTool::new().impersonate();
```

#### Custom Tools

Use `concurrent(true)` when a custom tool has no side effects and may run in parallel with other calls.

Describe the tool, then hand it the code it runs:

```rust
use agentwerk::{Event, tools::Tool};
use serde_json::Value;

let greet = Tool::new("greet")
    .description("Say hello")
    .schema(json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"]
    }))
    .concurrent(true)
    .timeout(std::time::Duration::from_secs(5))
    .handler(|input: Value| async move {
        let name = input["name"].as_str().unwrap_or("world");
        Event::tool_call_finished(format!("Hello, {name}!"))
    });
```

Return a `tool_call_failed` event with a string `message` for a failure the model should work around.

See [`Tool`](https://docs.rs/agentwerk/latest/agentwerk/tools/struct.Tool.html).

</details>

## Events

Events record what agents, tools, and LLM providers do during execution.

```rust
use agentwerk::Event;

werk.on_event(|_, event| {
    if event.get_name() == Event::TASK_FINISHED {
        eprintln!("{}", event.get_data()["result"]);
    }
});
```

<details>
<summary>Event reference</summary>

#### Event names

| | Name | Description |
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
| **Custom** | name chosen by your application | An event published with `emit_event`. |

#### Publish events

Publish custom events through the Werk. Add agent or task context when relevant:

```rust
use agentwerk::Event;
use serde_json::json;

werk.emit_event(
    Event::new("document_indexed")
        .data(json!({ "documents": 42 }))
        .task_id("t-1")
        .agent_id("indexer-1"),
);

werk.emit_event(Event::new("index_refreshed"));
```

`Werk::emit_event` does not change task status. Use [EventTool](#eventtool) for model-driven completion through `task_finished`.

#### Read events

Events are saved to `.agentwerk/events.jsonl`, except `text_chunk_received`. These streamed fragments reach handlers but are not available to event queries.

| Method | Description |
|--------|-------------|
| `emit_event(event)` | Publish an event for querying and observation. |
| `find_event(query)` | Get the first event selected directly or through a matching task. |
| `find_events(query)` | Get events selected directly or through matching tasks, in query order. |
| `get_input_tokens()` | Get input tokens across the run's requests. |
| `get_output_tokens()` | Get output tokens across the run's requests. |
| `get_duration()` | Get the elapsed execution duration. |

| Event method | Description |
|--------------|-------------|
| `get_name()` | Read the event name. |
| `get_data()` | Read the event payload. |
| `get_task_id()` | Read the associated task ID. |
| `get_agent_id()` | Read the associated agent ID. |
| `get_label()` | Read the associated task's label. |
| `get_created_at()` | Read the timestamp in epoch milliseconds. |
| `directive(value)` | Set directive metadata; this does not send an instruction to the model. |
| `get_directive()` | Read the directive metadata. |

When no event handler is installed, `event::default_logger()` logs events.

#### Query events

Query events with AQL or a predicate; see [Queries](#queries) for shared syntax.

```rust
werk.find_events("event.name = tool_call_failed");
werk.find_events("event.name = request_finished AND event.agent_id = research-1");
werk.find_events("event.task_id = t-3 ORDER BY event.created DESC");
werk.find_events("event.data ~ timeout AND event.created > -1h");
```

| | Field | Description |
|-|-------|-------------|
| **Match** | `event.name` | The event name, such as `run_started` or `tool_call_failed`. |
| | `event.agent_id` | The attributed agent ID, when the event has agent context. |
| | `event.task_id` | The attributed task ID, when the event has task context. |
| | `event.label` | The attributed task's label, when the task is known and labelled. |
| **Search** | `event.data` | Search only the serialized raw event data as text. |
| **Compare** | `event.created` | When the event was recorded. |

Use `IS EMPTY` to find events without agent, task, or label context. `event.data` searches only the payload, not the name. Events default to oldest-first order.

See [`Event`](https://docs.rs/agentwerk/latest/agentwerk/event/struct.Event.html) and [`Werk`](https://docs.rs/agentwerk/latest/agentwerk/struct.Werk.html).

</details>

### Hooks

A hook runs your function when an event, result, failure, or task state change occurs.

```rust
werk.on_failure(|werk, _, failed| {
    if failed.get_label() == Some("scan") {
        werk.add_task(Task::labeled("triage", failed.get_task().clone()));
    }
});
```

<details>
<summary>Hook reference</summary>

| | Method | Description |
|-|--------|-------------|
| **Observe** | `on_event(handler)` | Read every event as it is emitted. |
| | `on_event_async(handler)` | Read every event in an async handler. |
| | `on_result(handler)` | Read every finished task together with its result. |
| | `on_result_async(handler)` | Read every finished task and result in an async handler. |
| | `on_failure(handler)` | Read every failure together with its task. |
| | `on_failure_async(handler)` | Read every failure and task in an async handler. |
| | `on_task(handler)` | Read task state changes. |
| | `on_task_async(handler)` | Read task state changes in an async handler. |

Save replies of every finished task as a training example:

```rust
werk.on_task(|werk, event, task| {
    if event.get_name() == Event::TASK_FINISHED {
        let model = werk.get_model_for_agent(event.get_agent_id());
        let _ = Trajectory::from_task(event.get_agent_id(), model.as_deref(), task)
            .save("datasets");
    }
});
```

#### Async handlers

`on_result` runs synchronously on the agent; keep it brief. Use `on_result_async` for work that needs to await.

Async hooks run while a completion method is waiting, and finish before it returns. `start()` alone does not run them. Do not call `finish`, `finish_task`, or `finish_tasks` inside an async hook: it can deadlock.

```rust
let findings = Arc::clone(&database);
werk.on_result_async(move |_, task, result| {
    let findings = Arc::clone(&findings);
    async move {
        let _ = findings.insert(task.get_id(), &result).await;
    }
});
```

See [`Werk`](https://docs.rs/agentwerk/latest/agentwerk/struct.Werk.html).

</details>

## Knowledge

`Knowledge` provides durable memory that agents share across tasks and with other agents.

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/knowledge.gif" width="600" />
</div>

```rust
use agentwerk::Knowledge;

let store = Knowledge::load("./notes")?;
let alice = Agent::new().knowledge(&store);
let bob = Agent::new().knowledge(&store);
```

<details>
<summary>Knowledge reference</summary>

Pages use the Open Knowledge Format (OKF) and are stored at `./notes/pages/<slug>.md`. Each has an entry in `./notes/index.md`, which is included in the prompts of agents sharing the store.

| Method | Description |
|--------|-------------|
| `get_index()` | Get the index, which is injected into the agent prompt. |
| `set_index_char_limit(count)` | Limit how much of the index is injected into the prompt. |
| `get_index_char_limit()` | Get the index size limit in force. |
| `get_pages()` | Get the page collection for reading and writing pages. |
| `get_pages().get_all()` | Get every page in the store. |
| `clear()` | Remove every page from the store. |

By default, prompts include up to 12,000 characters of the index; agents can read the rest from `index.md`. Pages are always saved in full.

Create entries in code:

```rust
use agentwerk::agents::knowledge::Page;

store.get_pages().save(Page {
    slug: "build-command".into(),
    kind: String::new(),
    description: "How the project is built.".into(),
    content: "Run `make` to compile.".into(),
    tags: vec!["build".into()],
})?;

let page = store.get_pages().get_page("build-command")?;
store.get_pages().remove("build-command")?;
```

See [`Knowledge`](https://docs.rs/agentwerk/latest/agentwerk/agents/knowledge/struct.Knowledge.html).

</details>

## Use Cases

Example projects built with agentwerk:

- [Hello World](crates/use-cases/src/hello_world/): basic example
- [Terminal REPL](crates/use-cases/src/terminal_repl/): interactive terminal chat
- [Divide and Conquer](crates/use-cases/src/divide_and_conquer/): split an arithmetic problem across agents
- [Deep Research](crates/use-cases/src/deep_research/): research across several sources (requires `BRAVE_API_KEY`)
- [Malware Scanner](crates/use-cases/src/malware_scanner/): find signs of malware in a software package

> Configure an LLM provider first (see [Environment](DEVELOPMENT.md#environment)).

```bash
make use_case                # list available names
make use_case name=<name>    # run one
```

## Security

Report a vulnerability to security@canvascomputing.org, not in a public issue. See [SECURITY.md](SECURITY.md).

## Development

See [DEVELOPMENT.md](DEVELOPMENT.md) for build, test, and release instructions.
