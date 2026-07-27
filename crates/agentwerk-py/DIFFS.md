# Differences from the Rust API

These bindings wrap the `agentwerk` crate, and almost every name carries over unchanged.
This file lists the places where the two APIs do not line up, so a reader of the
[Python README](README.md) or the [Rust one](../../README.md) can predict the other.
`agentdocs/style.md` holds the rule that keeps this list short; the list is what the rule
allows.

## Everywhere

These apply across the whole surface, so the tables below do not repeat them.

- A `Duration` becomes float seconds. The parameter keeps its name: `max_time(30.0)`,
  `request_retry_delay(0.25)`, `Stats.run_duration()`.
- An enum becomes its lowercase string: `ticket.status`, `event.kind`, `finish_reason()`,
  and the `kind`, `reason`, and `op` fields of `event.data`.
- Every error type collapses to `RuntimeError`. There is no typed exception surface.
- An `Arc<T>` becomes a plain object. `Knowledge` and `TicketSystem` are shared by passing
  the same object to several agents.
- An `async fn` becomes awaitable: `await agent.finish()`.
- A `Serialize` argument becomes any JSON-serializable Python value, and a `Deserialize`
  return becomes a dict or list.

## Agent

| Rust | Python | Why |
|------|--------|-----|
| `Agent::new() -> AgentBuilder` | `Agent()` | Python constructs with `__init__`, and PyO3 cannot return another class from it. |
| `AgentBuilder<P, M>` | Folded into `Agent` | The type changes as the provider and model slots fill, which Python cannot hold across calls. |
| `AgentBuilder::build(self) -> Agent` | `Agent.build() -> Agent` | Returns the same object, armed. Configuring after it, or building twice, raises. |
| `Agent::empty()` | `Agent.empty()` | Same meaning. Returns an unbuilt agent rather than a builder. |

Rust gets the one-way transition for free: `build(self)` consumes the builder. Python has
to reject the second call, because the agent object owns its private ticket system and
rebuilding would orphan the queue.

## Tickets

| Rust | Python | Why |
|------|--------|-----|
| `Ticket::new(t).labels(..).schema(..).parent(..)` | `Ticket(task, labels=.., schema=.., parent=..)` | A Python class cannot carry a `labels` method and a `labels` attribute. |
| `Reply` | A dict | `ticket.replies` converts on access, so a callback that never asks never pays. |
| `Status` | A string | See the enum rule above. `is_todo()` and its five siblings read better than comparing it. |

## Tools

| Rust | Python | Why |
|------|--------|-----|
| `Tool::new(..).schema(..).handler(..).build()` | The `@tool` decorator | A decorated function carries the name, description, and schema a `ToolBuilder` collects. |
| `ToolLike` | A `@tool`-decorated callable | Python cannot implement a Rust trait. |
| `BashTool::unrestricted()` | `UnrestrictedBashTool()` | A second constructor on one class becomes a second function. |
| `ToolBuilder::paths([..])` | `@tool(paths=[..])` | Same for `read_only` and `defer`. |

## Knowledge

| Rust | Python | Why |
|------|--------|-----|
| `Page { slug, kind, description, content, tags }` | `Page(slug, description, content, kind=.., tags=..)` | A struct literal becomes a constructor, so the optional fields move last. |

## Statistics

| Rust | Python | Why |
|------|--------|-----|
| `serde_json::to_value(stats)` | `Stats.to_dict()` | Python cannot call `serde`, so reaching the `stats.json` shape needs a method. |

## Not bound

Rust items with no Python counterpart, and what to use instead.

- `AgentBuilder`, `ToolBuilder`: folded into the class they build, as above.
- `Status`, `EventKind`, `FinishReason`: strings, as above.
- `ToolError`, `ProviderError`, `RequestErrorKind`: `RuntimeError`, as above.
- `ToolContext`: a `@tool` function receives its input as keyword arguments only.
- `ModelRequest`, `ProviderToolDefinition`, `ToolChoice`, `ReasoningEffort`, `Message`,
  `AsUserMessage`, `ContentBlock`, `ModelResponse`, `ResponseStatus`, `StreamEvent`,
  `TokenUsage`: the shapes an LLM provider is built from. Python binds the four providers,
  not what they are built out of. Write a new one in Rust.
- `default_logger`: `TicketSystem.on_event(handler)` replaces it, in both languages.
- `Provider`: a trait in Rust, an opaque handle in Python.
