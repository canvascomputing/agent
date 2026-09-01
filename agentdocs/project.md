# Project

agentwerk is a Rust library for composing LLM agents, tools, tasks, and shared execution state.

## Library Boundary

**Provide building blocks that a caller composes inside its own application.**

- Keep startup, process structure, logging, and UI in the consuming application.
- Let callers begin with `Agent::from_env()` without implementing framework traits.
- Add an abstraction only when it removes more caller complexity than it introduces.

## Minimal Surface

**Make every public concept earn its maintenance cost.**

- Prefer extending `Agent`, `Werk`, `Task`, `Tool`, `Event`, or `Knowledge` over adding a parallel concept.
- Keep features optional unless correctness requires them.
- Reject registries, adapters, and aliases that only rename existing behavior.
- Put exhaustive API detail in rustdoc and `README.md`, not in convention files.

## Parallel Composition

**Use one `Werk` to coordinate agents over shared tasks.**

- Assign tasks through one optional label on `Agent` and `Task`.
- Claim each task once while allowing agents with the same label to work concurrently.
- Attach a `Schema` when a task result needs a machine-checked shape.
- Keep agent configuration local: no global agent or tool registration.

## Provider Independence

**Keep orchestration independent of the selected LLM provider.**

- Support Anthropic, OpenAI, Mistral, and LiteLLM through `ProviderLike` and `Provider`.
- Keep tasks, tools, schemas, events, and policies unchanged when provider configuration changes.
- Isolate vendor request and response formats under `providers/`.

## Observable State

**Expose behavior through tasks, results, events, and optional durable state.**

- Publish lifecycle and failure information as `Event` records without requiring a logger.
- Persist sessions under the directory configured by `Werk::set_dir`.
- Share durable facts through `Knowledge` only when the caller opts in.
- Report invalid configuration and exhausted retries explicitly; do not silently fall back.
