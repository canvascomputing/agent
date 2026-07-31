# Project

agentwerk is a Rust crate for building LLM agents. An agent reads input, calls an LLM provider, optionally invokes tools, and returns an output. The six sections below list the design principles the rest of the crate is measured against.

## Library, Not Framework

**The crate provides building blocks. The caller composes them.**

- No runtime to boot.
- No traits the caller must implement to get started.
- No required structure for the consuming application.
- Every feature is optional.

## Minimal Surface

**Each abstraction must remove more complexity than it adds.**

- Dependencies are limited to tokio, serde, serde_json, reqwest, and futures-util.
- No transport abstractions, no plugin registries.
- Providers own a `reqwest::Client` directly.
- Indirection without a concrete benefit is not added.

## Parallel by Default

**Many agents share one `TicketSystem` and pick up tickets concurrently.**

```rust
tickets.agent(Agent::new().name("scout_0").label("scan").from_env().build());
tickets.ticket(Ticket::new("Audit src/db.").label("scan"));
```

- Each agent runs on its own tokio task; the shared queue claims a ticket exactly once.
- A label assigns work to every agent carrying it. An agent's own name is one of its labels, so naming a label pins the ticket to that one agent.
- Agents are cloned and modified, then bound to a `TicketSystem`. No global registration, no implicit state.
- A ticket carries a `Schema`; the loop validates the agent's result against it.

## Provider-Agnostic

**The same agent code runs against any supported LLM provider.**

- Anthropic, OpenAI, Mistral, and LiteLLM are supported.
- Switching providers changes only the `.model(...)` call.
- All providers share one retry policy.
- `from_env()` and `model_from_env()` (in `providers::environment`) select a provider and model from environment variables.

## Observe, Do Not Prescribe

**The loop emits events. The caller decides what to do with them.**

- No built-in UI.
- No required logging.
- The event handler receives `Event { kind, ... }` at every lifecycle boundary.
- The handler may log, forward, store, or discard each event.

## Correctness Over Convenience

**Zero warnings, typed errors, no silent fallbacks.**

- The build MUST pass with `RUSTFLAGS="-D warnings"`: any warning fails it.
- Schema validation retries on mismatch, then fails explicitly.
- IMPORTANT: no blanket `From<io::Error>` or `From<serde_json::Error>`. Every conversion is an explicit mapping into a typed variant.
- Misconfigured builders panic at build time.
