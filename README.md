<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/logo.png" width="200" />
</div>

<h1 align="center">agentwerk</h1>

<div align="center">
  <strong>A minimal Rust & Python library for solving hard problems with many agents.</strong>
</div>

<div align="center">
  <a href="#installation">Installation</a> •
  <a href="crates/agentwerk-py/README.md">Python</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#api">API</a> •
  <a href="#use-cases">Use Cases</a> •
  <a href="#development">Development</a>
</div>


<div align="center">agentwerk is a lightweight harness optimized for small LLMs: it splits work into tickets to keep context windows short, runs agents in parallel, validates their results and reports every step as an event.</div>

---

<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/demo.gif" width="800" />
</div>
<div align="center"><a href="crates/agentwerk-py/examples/apparat_fabrik.py">Apparat Fabrik</a></div>
<div align="center"><em>agentwerk pairs "agent" with the German "Werk", a word for both factory and artwork: machinery for building agentic systems.</em></div>

---

## Why use agentwerk?

- **Simple interface:** create agents with a few lines of code.
- **Efficient harness:** optimized for fast LLMs with low memory footprint.
- **Complex interactions:** allow agents to collaborate through queues, event hooks and shared knowledge.
- **Deep observability:** inspect every request, tool call, and failure.
- **Facilitate training:** store trajectories based on granular events for fine-tuning models.

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
        .tool(GrepTool)
        .build();

    agent.task("Find every `pub trait` defined under src/ and explain each in one sentence.");

    let work = agent.start();
    let result = work.finish_last().await.unwrap();

    println!("{}", result.as_str().unwrap_or_default());
}
```

## API

- [Agents](#agents): Define roles, behavior and actions.
- [Tickets](#tickets): Coordinate complex work across agents.
- [Tools](#tools): Define accessible tooling.
- [Events](#events): Requests, tool usage, failures and more.
- [Knowledge](#knowledge): Notes agents can share for collaboration.

## Agents

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/agents.gif" width="600" />
</div>

An `Agent` is the core entity of agentwerk. It has access to tools for solving tasks in the form of tickets.

```rust
use agentwerk::tools::ReadFileTool;

let agent = Agent::from_env()
    .role("You are a release manager who prepares release notes.")
    .tool(ReadFileTool)
    .build();

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
| | `label(label)` | Restrict the agent to tickets carrying this label. |
| | `dir(dir)` | Set the directory the agent has access to. |
| | `template(key, value)` | Inject data into prompts with template strings. |
| | `templates(pairs)` | Inject more than one entry into prompts. |
| | `knowledge(store)` | Share a knowledge store with the agent. |
| | `interactive()` | Let the agent wait for new instructions to keep a ticket in-progress. |
| | `build()` | Create the agent. |
| **Work** | `task(task)` | Submit a task and return its ticket key. |
| | `ticket(ticket)` | Submit a `Ticket` with a custom label or schema. |
| | `start()` | Begin processing tickets. |
| | `get_id()` | Get the unique identifier of an agent. |

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
    Model::from_name("my-local-model")
        .context_window(128_000)
        .reasoning_effort(ReasoningEffort::High),
);
```

See [`Provider`](https://docs.rs/agentwerk/latest/agentwerk/providers/struct.Provider.html) and [`Model`](https://docs.rs/agentwerk/latest/agentwerk/providers/struct.Model.html).

</details>

## Tickets

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/tickets.gif" width="600" />
</div>

The `TicketQueue` is the core data structure of agentwerk for coordinating complex interactions.

```rust
use agentwerk::{Agent, Ticket, TicketQueue};

let analyst = Agent::from_env()
    .label("analysis")
    .build();

let writer = Agent::from_env()
    .label("report")
    .build();

let tickets = TicketQueue::new();
tickets.agent(analyst).agent(writer);

tickets.ticket(Ticket::new("Rank all products by value.").label("analysis"));
tickets.ticket(Ticket::new("Write up the ranking.").label("report"));
```

<details>
<summary>All ticket methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Configure** | `agent(agent)` | Add an agent to this ticket queue. |
| | `schemas(store)` | Enforce schemas for ticket results. |
| | `dir(dir)` | Define where a session is stored. |
| | `get_dir()` | Get the session directory. |
| **Submit** | `task(task)` | Submit a task and return its ticket key. |
| | `ticket(ticket)` | Submit a `Ticket` with a custom label or schema, and return its key. |
| **Read** | `results()` | Get the result of every finished ticket, in creation order. |
| | `tickets()` | Get every ticket in creation order. |
| | `find_ticket(condition)` | Get the earliest ticket matching a condition. |
| | `find_tickets(condition)` | Get every ticket matching a condition. |
| | `get_ticket(key)` | Get one ticket by key. |
| **Drive** | `reply(key, content)` | Add a reply to a ticket. |
| | `edit_replies(key, editor)` | Rewrite a ticket's replies now. |
| **Resolve** | `set_finished(key, result)` | Finish a ticket with a result. |
| | `set_failed(key)` | Fail a ticket. |

See [`TicketQueue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketQueue.html).

</details>

### Execution

The ticket queue schedules the work of your agents and returns their results.

```rust
tickets.start();

if let Some(answer) = tickets.finish_last().await {
    println!("{answer}");
}
```

<details>
<summary>All execution methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Run** | `start()` | Begin processing tickets. |
| **Wait** | `finish(matches).await` | Wait for the matching tickets to be done and get their results. |
| | `finish_all().await` | Wait for every ticket to be finished and get every result. |
| | `finish_last().await` | Wait for every ticket to be finished and get the last result. |
| | `finish_reason()` | Get why the last run ended. |
| **Stop** | `cancel(matches)` | Stop work on the matching tickets. |
| | `cancel_all()` | Stop work on every ticket. |
| | `is_cancelled(ticket)` | Check whether a ticket has been cancelled. |

Ticket members:

| | Member | Description |
|-|--------|-------------|
| **Identity** | `key` | Ticket key, of the form `TICKET-N`. |
| | `task` | The work the agent is asked to do. |
| | `label` | Label carried by the ticket. |
| | `parent` | The parent ticket if a handover was performed. |
| | `reporter` | Identifier of the agent that created the ticket. |
| | `assignee` | Identifier of the agent that claimed the ticket. |
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

### Handover

Agents can share the results of their work in the following ways:

1. **Create tickets**: the `finish` tool's `handover` option opens a child ticket carrying the result.
2. **Read tickets**: the `tickets` tool allows reading any finished ticket's result, by key.
3. **Read result file**: the `read_file` tool allows reading a ticket's `result.json` in the session directory.
4. **Share knowledge**: the `knowledge` tool allows sharing knowledge with other agents.
5. **Register hooks**: the `create_ticket_on_result` and `create_tickets_on_results` hooks allow creating follow-up tickets.

<details>
<summary>All ways agents pass data</summary>

#### 1. Create tickets

A handover can be performed through a single `finish` tool call:

```json
{
  "handover": "report",
  "result": "Three products lead on value.",
  "task": "Write the board report from {parent_key}."
}
```

When `task` is not defined, the child ticket's body is the result itself. A `task` populates template variables:

- `{parent_key}`: the key of the ticket that was handed over.
- `{parent_result}`: its result.
- `{parent_result_path}`: the path of its result file.

#### 2. Read tickets

The `tickets` tool reads what any finished ticket produced, by key:

```json
{ "action": "result", "key": "TICKET-1" }
```

#### 3. Read result file

The `read_file` tool reads the original result file when its path is known:

```json
{ "path": ".agentwerk/tickets/TICKET-1/result.json" }
```

#### 4. Share knowledge

The `knowledge` tool allows sharing knowledge with other agents:

```json
{
  "action": "write",
  "slug": "value-ranking",
  "description": "How the products rank on value.",
  "content": "Three products lead on value: ..."
}
```

#### 5. Register hooks

Use hooks to create new tickets when certain results arrived:

```rust
tickets.create_ticket_on_result(|done, result| {
    done.has_label("research")
        .then(|| Ticket::new(result.clone()).label("report"))
});

tickets.create_tickets_on_results(|results| {
    match results.iter().filter(|r| r["scanned"] == true).count() == 3 {
        true => vec![Ticket::new("Write the report.").label("report")],
        false => Vec::new(),
    }
});
```

</details>

### Schemas

A `Schema` constrains the result an agent produces for a ticket. A violation triggers a retry until `max_schema_retries` is exhausted.

```rust
use agentwerk::schemas::Schema;

let schema = Schema::new(json!({
    "type": "object",
    "properties": { "title": { "type": "string" } },
    "required": ["title"]
}))?;

tickets.ticket(Ticket::new("Write a report.").schema(schema));
```

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
| | `tickets.schemas(store)` | Enforce schemas for ticket results. |

A `SchemaStore` enforces schemas for all tickets with a certain label. Registering schemas centrally spares agents from passing complex schema structures during ticket creation (see `TicketsTool`) and handovers (see `FinishTool`):

```rust
use agentwerk::SchemaStore;

let schemas = SchemaStore::new();
schemas.label("report", json!({
    "type": "object",
    "properties": { "title": { "type": "string" } },
    "required": ["title"]
}))?;

tickets.schemas(&schemas);
```

See [`Schema`](https://docs.rs/agentwerk/latest/agentwerk/schemas/struct.Schema.html) and [`SchemaStore`](https://docs.rs/agentwerk/latest/agentwerk/schemas/struct.SchemaStore.html).

</details>

### Policies

Policies allow you to define execution limits.

```rust
tickets
    .max_turns(40)
    .max_time(std::time::Duration::from_secs(300))
    .max_input_tokens(200_000)
    .max_output_tokens(50_000);
```

<details>
<summary>All limits</summary>

| Method | Description |
|--------|-------------|
| `max_turns(count)` / `get_max_turns()` | Limit the total number of turns. |
| `max_time(duration)` / `get_max_time()` | Limit the total elapsed duration. |
| `max_input_tokens(count)` / `get_max_input_tokens()` | Limit the total input tokens. |
| `max_output_tokens(count)` / `get_max_output_tokens()` | Limit the total output tokens. |
| `max_request_tokens(count)` / `get_max_request_tokens()` | Limit the output tokens of a single request. |
| `max_schema_retries(count)` / `get_max_schema_retries()` | Limit the consecutive turns without a valid tool call. |
| `max_request_retries(count)` / `get_max_request_retries()` | Limit how often a failing request is retried. |
| `request_retry_delay(duration)` / `get_request_retry_delay()` | Wait this long between retries. |
| `compact_at(fraction)` / `get_compact_at()` | Compact once the context window is this full. |

A violated limit emits `EventKind::PolicyViolated`, see [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html).

</details>

### Sessions

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/sessions.gif" width="600" />
</div>

A `TicketQueue` writes every ticket, reply, and event to its working directory (default `./.agentwerk`). You can continue a session from that directory.

```rust
let tickets = TicketQueue::load(".agentwerk")?;
tickets.agent(my_agent);
tickets.start();
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

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/tools.gif" width="600" />
</div>

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
| **Tickets** | `FinishTool` | Write the result for the current ticket and mark it finished. |
| | `TicketsTool` | Read the ticket queue and create or edit tickets. |
| **Knowledge** | `KnowledgeTool` | Write, read, remove, or list pages in a knowledge store. |

#### `FinishTool` and `KnowledgeTool`

`FinishTool` and `KnowledgeTool` are special tools, registered automatically on every agent. They are used for interacting with the `TicketQueue` or knowledge base.

#### CommandTool

The `CommandTool` allows you to granularly define what commands are allowed and what commands are denied.

```rust
let git = CommandTool::new("git")
    .allow("git status")
    .allow("git log *")
    .deny("git push*")
    .deny_flag("--force");
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

<div align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/assets/events.gif" width="600" />
</div>

Events allow you to inspect all activities of your agents.

```rust
use agentwerk::event::EventKind;

tickets.on_event(|event| {
    if let EventKind::TicketFinished = &event.kind {
        eprintln!("[{}] done {} {:?}", event.agent_id, event.ticket_key, event.label);
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
| **Ticket** | `TicketStarted` | An agent claimed a ticket. |
| | `TicketFinished` | A ticket finished successfully. |
| | `TicketFailed` | A ticket failed. |
| | `TurnStarted` | The agent began another turn on its ticket. |
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
| | `ToolCallFailed` | A tool invocation failed but the ticket continues. |
| **File** | `FileOpenFinished` | A tool opened a file. |
| | `FileOpenFailed` | A tool could not open a file. |
| **Knowledge** | `KnowledgeUsed` | A page was written, read, removed, or listed. |
| | `KnowledgeMissed` | A page the agent asked for was not there. |
| **Compaction** | `CompactionStarted` | Compaction is about to rewrite the older messages. |
| | `CompactionProgress` | Compaction finished part of the work. |
| | `CompactionFinished` | Compaction replaced the older messages. |
| | `CompactionFailed` | Compaction could not finish. |

Every event is written to the session log. You read events from the ticket queue, or from the session directory in `.agentwerk/events.jsonl`:

| Method | Description |
|--------|-------------|
| `find_event(condition)` | Get the earliest recorded event matching a condition. |
| `find_events(condition)` | Get every recorded event matching a condition, oldest first. |
| `input_tokens()` / `output_tokens()` | Get token counts across the run's requests. |
| `execution_duration()` | Get the elapsed execution duration. |

See [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html) and [`TicketQueue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketQueue.html).

</details>

### Hooks

Hooks allow you to react to events.

```rust
tickets.create_ticket_on_failure(|_, failed| {
    failed.parent.is_none().then(|| {
        Ticket::new(failed.task.clone()).parent(&failed.key)
    })
});
```

<details>
<summary>All hooks</summary>

| | Method | Description |
|-|--------|-------------|
| **Observe** | `on_event(handler)` | Read every event as it is emitted. |
| | `on_result(handler)` | Read every finished ticket together with its result. |
| | `on_result_async(handler)` | Read every finished ticket with its result, in an async handler. |
| | `on_results(handler)` | Read every result the run has produced so far, each time one lands. |
| | `on_results_async(handler)` | Read every result in an async handler. |
| | `on_failure(handler)` | Read every failure together with the ticket it happened in. |
| | `on_ticket(handler)` | Read a ticket as it starts, finishes, or fails. |
| **Add work** | `create_ticket_on_event(make)` | Enqueue a follow-up ticket from any event. |
| | `create_ticket_on_result(make)` | Enqueue a follow-up ticket from a finished ticket. |
| | `create_tickets_on_results(make)` | Enqueue follow-up tickets once a condition across every result holds. |
| | `create_ticket_on_failure(make)` | Enqueue a retry for a ticket that failed. |
| **Rewrite** | `edit_replies_on_event(editor)` | Rewrite a ticket's replies before its next request. |
| | `edit_replies_on_compaction(editor)` | Decide what compaction does with a ticket's replies. |
| | `edit_directive_on_retry(editor)` | Override the prompt that corrects an agent's behavior. |

Save replies of every finished ticket as a training example:

```rust
let queue = Arc::clone(&tickets);
tickets.on_ticket(move |event, ticket| {
    if matches!(event.kind, EventKind::TicketFinished) {
        let model = queue.model_for_agent(&event.agent_id);
        let _ = Trajectory::from_ticket(&event.agent_id, model.as_deref(), ticket)
            .save("datasets");
    }
});
```

#### Async handlers

`on_result` is blocking and prevents an agent continuing its work till the hook is finished. If you perform time-consuming operations use `on_result_async` instead: storing results in a database, posting them to an HTTP API, or uploading them to object storage.

```rust
let findings = Arc::clone(&database);
tickets.on_result_async(move |ticket, result| {
    let findings = Arc::clone(&findings);
    async move {
        let _ = findings.insert(&ticket.key, &result).await;
    }
});
```

See [`TicketQueue`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketQueue.html).

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

## Development

See [DEVELOPMENT.md](DEVELOPMENT.md).
