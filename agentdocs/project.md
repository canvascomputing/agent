# Project

agentwerk is a Rust crate for building LLM agents. An agent reads input, calls an LLM provider, optionally invokes tools, and returns an output.

## Library, Not Framework

**The crate provides building blocks. The caller composes them.**

- No runtime to boot, and no traits the caller must implement to get started.
- No required structure for the consuming application.
- Every feature is optional.

## Minimal Surface

**Each abstraction must remove more complexity than it adds.**

- Dependencies are limited to tokio, serde, serde_json, reqwest, and futures-util.
- No transport abstractions and no plugin registries: providers own a `reqwest::Client` directly.
- Indirection without a concrete benefit is not added.

## Parallel by Default

**Many agents share one `TicketQueue` and pick up tickets concurrently.**

```rust
tickets.agent(Agent::from_env().label("scan"));
tickets.ticket(Ticket::new("Audit src/db.").label("scan"));
```

- Each agent runs on its own tokio task; the shared queue claims a ticket exactly once.
- An agent serves one label and a ticket carries one, so a label only one agent serves pins the ticket to that agent.
- Agents are cloned and modified, then bound to a `TicketQueue`. No global registration, no implicit state.
- A ticket carries a `Schema`; the loop validates the agent's result against it.

## Provider-Agnostic

**The same agent code runs against any supported LLM provider.**

- Anthropic, OpenAI, Mistral, and LiteLLM are supported, and share one retry policy.
- Switching providers changes only the `.model(...)` call.
- `Provider::from_env()` and `Model::from_env()` select a provider and model from environment variables.

## Observe, Do Not Prescribe

**The loop emits events. The caller decides what to do with them.**

- No built-in UI, no required logging.
- The event handler receives `Event { kind, ... }` at every lifecycle boundary.
- The handler may log, forward, store, or discard each event.

## Correctness Over Convenience

**Zero warnings, typed errors, no silent fallbacks.**

- The build MUST pass with `RUSTFLAGS="-D warnings"`: any warning fails it.
- Schema validation retries on mismatch, then fails explicitly.
- IMPORTANT: no blanket `From<io::Error>` or `From<serde_json::Error>`. Every conversion is an explicit mapping into a typed variant.
- Misconfigured builders panic at build time.
