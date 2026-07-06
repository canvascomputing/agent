<p align="center">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/logo.png" width="200" />
</p>

<h1 align="center">agentwerk</h1>

<p align="center">
  <strong>A minimal Rust crate for running many agents in parallel on a shared ticket queue.</strong>
</p>

<p align="center">
  <a href="#installation">Installation</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#demo">Demo</a> •
  <a href="#use-cases">Use Cases</a> •
  <a href="#api">API</a> •
  <a href="#development">Development</a>
</p>

<p align="center">agentwerk runs many agents in parallel on a shared ticket queue. Tickets carry the work, labels assign it to matching agents, and agentwerk handles concurrency, built-in tools, a knowledge store, schema validation, retries, limits, and multi-provider support.</p>

<p align="center"><em>agentwerk pairs "agent" with the German "Werk", a word for both factory and artwork: machinery for building agentic systems.</em></p>

---

## Installation

```bash
cargo add agentwerk
```

## Quick Start

```rust
use agentwerk::Agent;
use agentwerk::tools::{GrepTool, ReadFileTool};

#[tokio::main]
async fn main() {
    let agent = Agent::new()
        .from_env()
        .role("You are a Rust developer who explores source files to answer questions.")
        .tool(ReadFileTool)
        .tool(GrepTool)
        .build();

    let work = agent
        .task("Find every `pub trait` defined under src/ and explain each in one sentence.")
        .finish()
        .await;

    println!("{}", work.last_result().unwrap());
}
```

## Demo (Malware Scan)

<p align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/demo.gif" width="600" />
</p>

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

# API

- [Agents](#agents): Pick up tickets and produce results.
- [Tickets](#tickets): Ticket system allowing to orchestrate complex work.
- [Prompting](#prompting): Role, context, and task shaping the work of an agent.
- [Tools](#tools): Capabilities agents use to solve a ticket.
- [Knowledge](#knowledge): Knowledge base an agent creates during a run.
- [Sessions](#sessions): Working directory layout and how to reopen a run.
- [Events](#events): Lifecycle events emitted while agents work.
- [Stats](#stats): Metrics about tickets, tokens and time.

## Agents

An `Agent` picks up **tickets**, uses assigned tools to solve them, and writes the result back onto each ticket.

```rust
use agentwerk::tools::ReadFileTool;

let agent = Agent::new()
    .name("agent_0")
    .label("math")
    .tool(ReadFileTool)
```

| Method | Description |
|--------|-------------|
| `name(s)` | Set an identifier for assigning tickets. |
| `label(l)` / `labels([..])` | Restrict the agent to tickets carrying matching labels. |
| `tool(t)` / `tools([..])` | Register a tool the agent may call. |

### Providers

A `Provider` connects the agent to an LLM service. agentwerk ships providers for Anthropic, OpenAI, Mistral, and a LiteLLM proxy.

```rust
use agentwerk::providers::AnthropicProvider;

let agent = Agent::new()
    .provider(AnthropicProvider::new(key))
    .model("claude-sonnet-4-20250514");

// Or pick from environment variables (see DEVELOPMENT.md).
let agent = Agent::new().from_env();
```

Each provider exposes `.base_url(url)` and `.timeout(duration)` to override the endpoint and request timeout.

| Method | Description |
|--------|-------------|
| `provider(p)` | Set the LLM provider. |
| `model(m)` | Set the model the provider runs. |
| `from_env()` | Detect provider and model in one call. |

To read only the provider from the environment (and set the model explicitly), or only the model (and set the provider explicitly), use `provider_from_env()` or `model_from_env()` (see [`AgentBuilder`](https://docs.rs/agentwerk/latest/agentwerk/agents/agent/struct.AgentBuilder.html)).

### Models

`.model(m)` accepts a model name or a `Model`. Names registered with a known provider resolve to a context window automatically. For private or proxied models, build a `Model` and pass an explicit window so automatic compaction stays active:

```rust
use agentwerk::providers::Model;

let agent = Agent::new()
    .model(Model::from_name("my-local-model").context_window(128_000));
```

## Tickets

<p align="left">
  <img src="https://raw.githubusercontent.com/canvascomputing/agentwerk/main/tickets.jpg" width="400" />
</p>

The `TicketSystem` is the core data structure of agentwerk to orchestrate complex collaboration between agents. A `task` is the work itself, a `ticket` wraps it with additional metadata, like labels and schemas. Labels assign work to matching agents.

```rust
use agentwerk::{Agent, Ticket, TicketSystem};
use agentwerk::tools::FetchUrlTool;

let tickets = TicketSystem::new();

for i in 0..4 {
    tickets.agent(
        Agent::new()
            .name(format!("researcher_{i}"))
            .label("research")
            .from_env()
            .tool(FetchUrlTool)
            .build(),
    );
}

tickets.agent(
    Agent::new()
        .name("analyst")
        .label("analysis")
        .from_env()
        .build()
);

for url in pricing_pages {
    tickets.task_labeled(
        format!("Fetch {url} and extract pricing tiers, limits, and features."),
        "research",
    );
}

tickets.ticket(
    Ticket::new("Rank all products by value for a 10-person engineering team.")
        .label("analysis")
        .schema(comparison_schema)
);
```

| Method | Description |
|--------|-------------|
| `agent(agent)` | Add an agent to this ticket system. |
| `task(t)` | Submit a task and return its ticket key. |
| `task_labeled(t, l)` | Submit a task tagged with `l` for label-scoped assignment. Shorthand for `ticket(Ticket::new(t).label(l))`. |
| `ticket(t)` | Submit a `Ticket` with custom labels, a schema, or a parent link. |

Also on [`TicketSystem`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketSystem.html): `dir(d)` to relocate persisted state, `reply(key, c)` to continue a multi-turn conversation on one ticket.

### Execution

Start, wait, and cancel a run:

```rust
tickets.start();
tickets.finish().await;
let answer = tickets.last_result();
```

| Method | Description |
|--------|-------------|
| `start()` | Begin processing tickets in the background. |
| `finish().await` | Process every queued ticket and return. |
| `cancel()` | Cancel the run. |
| `finish_reason()` | Return why the most recent `finish()` returned: `Drained`, `PolicyViolated(kind)`, or `Cancelled`. |

To cancel when another task finishes, use `cancel_on(trigger)`. To cancel when an event matches a condition you supply, use `cancel_on_event(p)`. To block until one ticket matches a predicate rather than draining the whole queue, use `wait_for_ticket(p).await`. `is_cancelled()` reports external cancel only; a clean drain or a policy stop leaves it false. The same outcome is announced as `EventKind::RunFinished { reason }` for subscribers attached via `on_event`. See [`TicketSystem`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketSystem.html).

### Reading results

Query the system after `finish().await` returns:

```rust
tickets.finish().await;

if let Some(answer) = tickets.last_result() {
    println!("{answer}");
}

for ticket in tickets.tickets() {
    println!("{}: {}", ticket.key, ticket.status);
}
```

| Method | Description |
|--------|-------------|
| `last_result()` | Return the most recent finished ticket's payload as a string. |
| `results()` | Return every finished ticket's payload as a string. |
| `tickets()` | Return every ticket in creation order, with status, payload, and metadata. |
| `find_ticket(predicate)` | Return the earliest ticket matching the predicate. |

More query methods on [`TicketSystem`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketSystem.html): `get_ticket`, `first_ticket`, `last_ticket`, `search_tickets`, `find_tickets`, `count_tickets`, `collect_results_by_label`, `result_by_label`, `is_cancelled`.

### Inspecting tickets

Each `Ticket` carries the recorded result, its transcript, and lifecycle timestamps as `pub` fields. Reach in directly; deserialize structured results with `serde_json::from_value`:

```rust
#[derive(serde::Deserialize)]
struct Report { title: String }

let ticket = tickets.find_ticket(|t| t.has_label("analysis")).unwrap();
let report: Report = serde_json::from_value(ticket.result.clone().unwrap()).unwrap();
```

See [`Ticket`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.Ticket.html) for the full field list (`key`, `status`, `result`, `replies`, `labels`, `parent`, and the four lifecycle timestamps) and the status predicates that read better than comparing `status` directly: `is_finished`, `is_in_progress`, `is_pending`, `is_resolved`, `has_label`.

### Policies

Configure execution policies on a ticket system. A breach fires `EventKind::PolicyViolated` and halts execution.

```rust
let tickets = TicketSystem::new();
tickets
    .max_turns(40)
    .max_time(std::time::Duration::from_secs(300))
    .max_input_tokens(200_000)
    .max_output_tokens(50_000);
```

| Method | Description |
|--------|-------------|
| `max_turns(n)` | Limit the total number of turns. |
| `max_time(d)` | Limit the total elapsed duration. |
| `max_input_tokens(n)` | Limit the total input tokens. |
| `max_output_tokens(n)` | Limit the total output tokens. |

See [`TicketSystem`](https://docs.rs/agentwerk/latest/agentwerk/agents/tickets/struct.TicketSystem.html) for the retry and per-request limits: `max_schema_retries`, `max_request_retries`, `request_retry_delay`, `max_request_tokens`.

### Schemas

A `Schema` constrains the result an agent must produce for a ticket. A violation triggers a retry until `max_schema_retries` is exhausted.

```rust
use agentwerk::schemas::Schema;
use agentwerk::Ticket;

let schema = Schema::parse(json!({
    "type": "object",
    "properties": { "title": { "type": "string" } },
    "required": ["title"]
}))?;

tickets.ticket(Ticket::new("Write a report.").schema(schema));
```

### Compaction

agentwerk compacts the transcript automatically when the model's context window is near full; observe progress via the `Compaction*` variants on [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html).

## Prompting

Every prompt has three parts: `role` (who the agent is), `context` (the situation it operates in), and `task` (work it should perform). `role` and `context` are set on the agent; the task body arrives per ticket via `tickets.task()`. The structure follows the [prompting guide](https://github.com/canvascomputing/prompting).

```rust
let agent = Agent::new()
    .role("You are an arithmetic agent. Compute step by step and show your work.")
    .context("- Stage 2 of a math-tutor pipeline.\n- Attempts remaining: 2.")
    .template_variable("divisor", "8")
    .from_env()
    .build();

tickets.agent(agent);
tickets.task("Compute (47 * 92) / {divisor}, then round to the nearest integer.");
```

When `context(...)` is not set, agentwerk supplies a default block. When the agent processes a ticket, the ticket key is prepended automatically:

```markdown
You work within a ticket system. Each task arrives as a ticket; you process one at a time. Each reply you generate is one turn.

- Ticket: TICKET-7
- Date: 2026-05-06
- Directory: /Users/caro
- Platform: darwin 25.1.0
- Turns remaining: 8
- Input tokens remaining: 95000
- Output tokens remaining: 12000
- Time remaining: 240s
```

## Tools

Give agents access to tools helping them to solve a given task. Each tool exposes an action the agent can choose to take. agentwerk provides minimal baseline tools:

| | Tool | Description |
|-|------|-------------|
| **File** | `ReadFileTool` | Read a file with line numbers, offset, and limit. |
| | `WriteFileTool` | Create or overwrite a file. |
| | `EditFileTool` | Replace text in a file. |
| **Search** | `GlobTool` | Find files by pattern. |
| | `GrepTool` | Search file contents. |
| | `ListDirectoryTool` | List files and directories. |
| **Shell** | `BashTool` | Run a shell command matching an allowed pattern. |
| **Web** | `FetchUrlTool` | Fetch a URL and read its body. |
| **Tickets** | `FinishTicketTool` | Write the result for the current ticket and mark it finished. |
| | `HandoverTicketTool` | Write the result, mark the ticket finished, and hand follow-up work to another agent. |
| | `ManageTicketsTool` | Read the ticket queue and create or edit tickets. |
| | `ReadTicketsTool` | Read the ticket queue. |
| **Knowledge** | `ManageKnowledgeTool` | Write, read, remove, or list pages in the agent's knowledge store. |

### Bash

`BashTool` restricts execution to commands matching a glob pattern. The first argument names the tool the model sees; the second is the allowed pattern.

```rust
use agentwerk::tools::BashTool;

let agent = Agent::new()
    .tool(BashTool::new("git", "git *"))
    .tool(BashTool::unrestricted());
```

`BashTool::unrestricted()` removes the pattern check.

### Custom tools

Define custom tools for specific needs. Each tool declares a JSON-Schema for its inputs:

```rust
use agentwerk::tools::{Tool, ToolResult};
use serde_json::json;

let greet = Tool::new("greet", "Say hello")
    .schema(json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"]
    }))
    .read_only(true)
    .handler(|input, _ctx| async move {
        let name = input["name"].as_str().unwrap_or("world");
        Ok(ToolResult::success(format!("Hello, {name}!")))
    })
    .build();
```

`.read_only(true)` allows the agent to run a tool concurrently with other read-only calls in the same turn.

## Knowledge

A `Knowledge` store is the agent's long-term memory. It is written to disk, can be shared across multiple agents, and is curated by the agent through `ManageKnowledgeTool`.

Each page is an Open Knowledge Format (OKF) v0.1 concept file with `type`, `description`, and `timestamp` frontmatter. A compact index of one-line descriptions goes into the system prompt, so the agent picks which pages to read. Because the store is a plain OKF bundle, `Knowledge::load` can open one authored elsewhere to seed an agent.

```rust
use agentwerk::Knowledge;

// Open a store and share it across agents:
let store = Knowledge::load("./.agentwerk")?;
let alice = Agent::new().knowledge(&store);
let bob = Agent::new().knowledge(&store);

// Raise the rendered-index char budget (default 4000):
let store = Knowledge::load("./.agentwerk")?.index_char_limit(12_000);
let agent = Agent::new().knowledge(&store);
```

## Sessions

A `TicketSystem` writes every ticket, transcript, statistic, and lifecycle event to its working directory (default `./.agentwerk`). That directory is the session: stop the process, and `TicketSystem::load(dir)` reopens it from disk and continues from where it stopped.

```rust
let tickets = TicketSystem::load(".agentwerk")?;
tickets.agent(my_agent);
tickets.start();
```

Layout:

```
.agentwerk/
├── stats.json                            run statistics
├── tickets.jsonl                         lifecycle events (one per line)
├── results.jsonl                         finished results (one per line)
├── tickets/
│   └── TICKET-1/
│       ├── ticket.json                   the ticket without its transcript (key, status, labels, timestamps, result)
│       ├── ticket.<ts>.json              the ticket saved at each compaction; the timestamp matches `replies.<ts>.jsonl`
│       ├── replies.jsonl                 pre-compaction transcript
│       ├── replies.<ts>.jsonl            post-compaction transcript
│       └── outputs/<tool_use_id>.txt     full tool outputs spilled out of the transcript
├── pages/<slug>.md                       knowledge pages
└── index.md                              knowledge index
```

## Events

Events report everything that happens while your agents work and give you deep insights into behavior or failures.

```rust
use agentwerk::event::{Event, EventKind};

tickets.on_event(|event: Event| {
    if let EventKind::TicketFinished { key } = &event.kind {
        eprintln!("[{}] done {key}", event.agent_name);
    }
});
```

| | Kind | Description |
|-|------|-------------|
| **Ticket** | `TicketStarted` | An agent claimed a ticket. |
| | `TicketFinished` | A ticket finished successfully. |
| | `TicketFailed` | A ticket failed. |
| **Provider** | `RequestFinished` | A provider request finished and reported its token usage. |
| | `RequestRetried` | A transient provider error triggered a retry. |
| **Tool** | `ToolCallFinished` | A tool invocation finished. |
| | `ToolCallFailed` | A tool invocation failed but the ticket continues. |
| **Compaction** | `CompactionStarted` | Compaction is about to summarize the conversation tail. |
| | `CompactionFinished` | Compaction finished and replaced the tail with a summary. |
| **Run** | `PolicyViolated` | A policy limit was breached and execution stopped. |

Also: `RequestStarted`, `RequestFailed`, `TextChunkReceived`, `ToolCallStarted`, `SchemaRetried`, `CompactionProgress`, `CompactionFailed`. Full enum on [`EventKind`](https://docs.rs/agentwerk/latest/agentwerk/event/enum.EventKind.html).

## Stats

`Stats` contain metrics about the progress of your agents' work, allowing to optimize your agentic system and identify bottlenecks.

```rust
let s = tickets.stats();
let scan = s.stats_for_label("scan");
```

| Method | Description |
|--------|-------------|
| `run_duration()` | Return the run's elapsed duration. |
| `tickets_success_rate()` | Return `finished / (finished + failed)`. |
| `input_tokens()` / `output_tokens()` | Return token totals across responses. |
| `tool_stats()` | Return per-tool call and failure counts, broken down by failure kind. |
| `stats_for_label(label)` | Return a stats slice scoped to one label. |

More statistics on [`Stats`](https://docs.rs/agentwerk/latest/agentwerk/agents/stats/struct.Stats.html): work and ticket durations, per-ticket counts, turns, requests, tool calls, provider errors, and per-tool errors.

# Development

See [DEVELOPMENT.md](DEVELOPMENT.md) for the workspace layout, build commands, integration tests, publishing flow, the LiteLLM proxy setup, and environment variables.
