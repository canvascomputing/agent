<div align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/logo.png" width="200" />
</div>

<h1 align="center">agentwerk</h1>

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
<div align="center"><a href="crates/agentwerk-py/examples/apparat_fabrik.py">Apparat Fabrik</a></div>
<div align="center"><em>agentwerk pairs "agent" with the German "Werk", a word for both factory and artwork: machinery for building agentic systems.</em></div>

---

## Why use agentwerk?

- **Minimal interface:** create agents with a few lines of code.
- **Complex workflows:** allow agents to interact through shared knowledge and tickets.
- **Deep observability:** inspect every request, message and failure.
- **Ease of integration:** apply agents as simple as HTTP calls.
- **Facilitate training:** collect trajectories for fine-tuning models.

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
    let mut results = work.finish(|_| true).await;

    let result = results.pop().unwrap();
    println!("{}", result.as_str().unwrap_or_default());
}
```

## Use Cases

Example projects built with agentwerk:

- [Terminal REPL](crates/use-cases/src/terminal_repl/): minimal interactive chat
- [Divide and Conquer](crates/use-cases/src/divide_and_conquer/): arithmetic problem shared across agents
- [Deep Research](crates/use-cases/src/deep_research/): deep research pipeline (requires `BRAVE_API_KEY`)
- [Malware Scanner](crates/use-cases/src/malware_scanner/): identify indicators of compromise in a software package

> Configure an LLM provider first (see [Environment](DEVELOPMENT.md#environment)).

```bash
make use_case                # list available names
make use_case name=<name>    # run one
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

```rust
use agentwerk::tools::ReadFileTool;

let agent = Agent::from_env()
    .name("agent_0")
    .label("math")
    .role("You are an arithmetic agent. Compute step by step and show your work.")
    .tool(ReadFileTool)
    .build();

tickets.agent(agent);
tickets.task("Compute (47 * 92) / 8, then round to the nearest integer.");
```

<details>
<summary>All builder methods</summary>

| Method | Description |
|--------|-------------|
| `Agent::empty()` | Create an agent with no tools pre-registered. |
| `name(name)` | Set a name or identifier for assigning tickets. |
| `role(role)` | Define who the agent is and how it should work. |
| `label(label)` / `labels(labels)` | Restrict the agent to tickets carrying a matching label. |
| `tool(tool)` / `tools(tools)` | Register a tool the agent may call. |
| `template(key, value)` | Inject data into prompts with template strings. |
| `templates(pairs)` | Inject more than one entry into prompts. |
| `dir(dir)` | Set the directory the agent has access to. |
| `interactive()` | Let the agent wait for new instructions to keep a ticket in-progress. |
| `build()` | Create the agent. |
| `ticket_queue(queue)` | Attach a built agent to a ticket queue. |

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

```rust
use agentwerk::providers::AnthropicProvider;

let agent = Agent::new()
    .provider(AnthropicProvider::new(key))
    .model("claude-sonnet-4-20250514");

// Or read both from the environment.
let agent = Agent::from_env();
```

<details>
<summary>All provider settings</summary>

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

</details>

### Models

You can configure models to set a custom context window size or the applied reasoning:

```rust
use agentwerk::providers::{Model, ReasoningEffort};

let agent = Agent::new().model(
    Model::from_name("my-local-model")
        .context_window(128_000)
        .reasoning_effort(ReasoningEffort::High),
);
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
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/tickets.jpg" width="600" />
</div>

The `TicketQueue` is the core data structure of agentwerk allowing to coordinate complex interactions.

```rust
let analyst = Agent::from_env()
    .name("analyst")
    .label("analysis")
    .build();

tickets.agent(analyst);
tickets.ticket(Ticket::new("Rank all products by value.").label("analysis"));
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

```rust
tickets.start();
let answer = tickets.finish(|_| true).await.pop();
```

<details>
<summary>All execution methods</summary>

| | Method | Description |
|-|--------|-------------|
| **Run** | `start()` | Begin processing tickets. |
| **Wait** | `finish(matches).await` | Wait for the matching tickets to be done and get their results. |
| | `get_finish_reason()` | Get why the last run ended. |
| **Stop** | `cancel(matches)` | Stop work on the matching tickets. |
| | `is_cancelled(ticket)` | Check whether a ticket has been cancelled. |

</details>

### Results

Access the results of the agents' work:

```rust
if let Some(answer) = tickets.finish(|_| true).await.pop() {
    println!("{answer}");
}

for ticket in tickets.tickets() {
    println!("{}: {}", ticket.key, ticket.status);
}
```

Each `Ticket` carries a result as free text or JSON validated by schemas:

```rust
#[derive(serde::Deserialize)]
struct Report { title: String }

let ticket = tickets.find_ticket(|t| t.has_label("analysis")).unwrap();
let report: Report = serde_json::from_value(ticket.result.clone().unwrap())?;
```

<details>
<summary>All result and ticket accessors</summary>

| Method | Description |
|--------|-------------|
| `results()` | Get the result of every finished ticket, in creation order. |
| `results_for_label(label)` | Get the result of every finished ticket carrying a label. |
| `results_for_agent(name)` | Get the result of every finished ticket claimed by an agent. |
| `result_for_ticket(key)` | Get one ticket's result by key. |
| `tickets()` | Get every ticket in creation order. |
| `tickets_for_label(label)` | Get every ticket carrying a label, in any status. |
| `tickets_for_agent(name)` | Get every ticket claimed by an agent, in any status. |
| `find_ticket(condition)` | Get the earliest ticket matching a condition. |
| `find_tickets(condition)` | Get every ticket matching a condition. |
| `get_ticket(key)` | Get one ticket by key. |

Ticket members:

| | Members |
|-|---------|
| **Identity** | `key`, `task`, `labels`, `parent`, `reporter`, `assignee` |
| **Outcome** | `status`, `result`, `replies`, `schema` |
| **Timestamps** | `created_at`, `started_at`, `finished_at`, `failed_at` |
| **Checks** | `has_label(label)`, `is_todo()`, `is_in_progress()`, `is_finished()`, `is_failed()`, `is_pending()`, `is_resolved()` |

See [`Ticket`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.Ticket.html).

</details>

### Schemas

A `Schema` constrains the result an agent produces for a ticket. A violation triggers a retry until `max_schema_retries` is exhausted.

```rust
use agentwerk::schemas::Schema;

let schema = Schema::parse(json!({
    "type": "object",
    "properties": { "title": { "type": "string" } },
    "required": ["title"]
}))?;

tickets.ticket(Ticket::new("Write a report.").schema(schema));
```

<details>
<summary>All schema methods</summary>

| Method | Description |
|--------|-------------|
| `Schema::parse(document)` | Create a schema. |
| `Schema::validate(value)` | Validate content. |
| `tickets.schema_for_label(label, schema)` | Register a schema for all tickets with a certain label. |

</details>

### Policies

Policies allow you to define execution limits:

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
| `max_schema_retries(count)` / `get_max_schema_retries()` | Limit how often a result may fail its schema before the ticket fails. |
| `max_request_retries(count)` / `get_max_request_retries()` | Limit how often a failing request is retried. |
| `request_retry_delay(duration)` / `get_request_retry_delay()` | Wait this long between retries. |
| `compact_at(fraction)` / `get_compact_at()` | Compact once the context window is this full. |

A violated limit emits `EventKind::PolicyViolated`, see [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html). `compact_at` is the exception: reaching it compacts the ticket and execution continues.

</details>

## Tools

Tools allow agents to perform their work.

```rust
use agentwerk::tools::{BashTool, GrepTool, ReadFileTool};

let agent = Agent::new()
    .tool(ReadFileTool)
    .tool(GrepTool)
    .tool(BashTool::new("git", "git *"));
```

`FinishTool` and `ManageKnowledgeTool` are special tools, registered automatically on every agent. They are used for interacting with the `TicketQueue`.

<details>
<summary>All built-in tools</summary>

| | Tool | Description |
|-|------|-------------|
| **File** | `ReadFileTool` | Read a file with line numbers, offset, and limit. |
| | `WriteFileTool` | Create or overwrite a file. |
| | `EditFileTool` | Replace text in a file. |
| **Search** | `GlobTool` | Find files by pattern. |
| | `GrepTool` | Search file contents by regular expression, or by code shape with `syntax: "code"`. |
| | `ListDirectoryTool` | List files and directories. |
| **Shell** | `BashTool` | Run a shell command matching an allowed pattern. |
| **Web** | `FetchUrlTool` | Fetch a URL and read its body. |
| **Tickets** | `FinishTool` | Write the result for the current ticket and mark it finished. |
| | `ManageTicketsTool` | Read the ticket queue and create or edit tickets. |
| | `ReadTicketsTool` | Read the ticket queue. |
| **Knowledge** | `ManageKnowledgeTool` | Write, read, remove, or list pages in a knowledge store. |
| **Discovery** | `FindToolsTool` | Look up the tools held back until they are needed. |

</details>

### Custom tools

You can define custom tools for specific needs:

```rust
use agentwerk::tools::{Tool, ToolResult};

let greet = Tool::new("greet", "Say hello")
    .schema(json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"]
    }))
    .read_only(true)
    .handler(|input, _context| async move {
        let name = input["name"].as_str().unwrap_or("world");
        Ok(ToolResult::success(format!("Hello, {name}!")))
    })
    .build();
```

<details>
<summary>All tool options</summary>

| Method | Description |
|--------|-------------|
| `read_only(true)` | Let the agent run this tool concurrently with other read-only calls in the same turn. |
| `defer(true)` | Hold the tool back until the agent looks it up with `FindToolsTool`. |
| `paths(["path"])` | Name file path used for a tool call, so the files are included in statistics. |

Return `ToolResult::error(message)` for a failure the model should work around.

</details>

## Events

Events give you insights to the lifecycle and activities of your agents' work.

```rust
use agentwerk::event::EventKind;

tickets.on_event(|event| {
    if let EventKind::TicketFinished = &event.kind {
        eprintln!("[{}] done {}", event.agent_name, event.ticket_key);
    }
});
```

<details>
<summary>All event kinds</summary>

| | Kind | Description |
|-|------|-------------|
| **Run** | `RunStarted` | Execution began. |
| | `RunFinished` | Execution ended, carrying the reason. |
| | `PolicyViolated` | A limit was breached and execution stopped. |
| **Ticket** | `TicketStarted` | An agent claimed a ticket. |
| | `TicketFinished` | A ticket finished successfully. |
| | `TicketFailed` | A ticket failed. |
| | `TurnStarted` | The agent began another turn on its ticket. |
| | `SchemaRetried` | A result missed its schema and the agent was asked again. |
| **LLM provider** | `RequestStarted` | A request went out to the model. |
| | `RequestFinished` | A request finished and reported its token usage. |
| | `RequestFailed` | A request failed and was not retried. |
| | `RequestRetried` | A transient provider error triggered a retry. |
| | `TextChunkReceived` | A piece of the reply arrived. |
| **Tool** | `ToolCallStarted` | A tool invocation began. |
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

See [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html).

</details>

### Hooks

Hooks allow you to react to events:

```rust
tickets.create_ticket_on_failure(|_, ticket| {
    Some(Ticket::new(ticket.task.clone()).label("retry"))
});
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

```rust
let queue = Arc::clone(&tickets);
tickets.on_ticket(move |event, ticket| {
    if matches!(event.kind, EventKind::TicketFinished) {
        let model = queue.model_for_agent(&event.agent_name);
        let _ = Trajectory::from_ticket(&event.agent_name, model.as_deref(), ticket)
            .save("datasets");
    }
});
```

</details>

## Stats

Statistics give you deep insights into behavior of your agents: working time, tickets, failure rates, bottlenecks etc.

```rust
use agentwerk::event::EventName;

let stats = tickets.stats();
println!(
    "{} requests, {} input tokens",
    stats.event_count(EventName::RequestFinished),
    stats.input_tokens(),
);

for (name, stat) in stats.tool_stats() {
    println!("{name}: {} calls", stat.calls);
}
```

<details>
<summary>All statistics</summary>

| Method | Description |
|--------|-------------|
| `execution_duration()` | Get the elapsed execution duration. |
| `ticket_duration()` | Get the time from creation to resolution, summed and averaged over resolved tickets. |
| `work_duration()` | Get the time agents spent working, summed across every agent and averaged per ticket. |
| `event_count(event)` | Get how many events of one kind were recorded, such as `EventName::TurnStarted`. |
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

</details>

## Sessions

A `TicketQueue` writes every ticket, reply, statistic, and lifecycle event to its working directory (default `./.agentwerk`). You can continue a session from that directory.

```rust
let tickets = TicketQueue::load(".agentwerk")?;
tickets.agent(my_agent);
tickets.start();
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

See [DEVELOPMENT.md](DEVELOPMENT.md).
