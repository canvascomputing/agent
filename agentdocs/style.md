# Style

Naming, API, comment, binding, and README conventions for this workspace.

## Public API

**Keep the root API small and domain types in their modules.**

- Re-export only headline concepts and types required by root-level signatures from `lib.rs`.
- Keep concrete providers under `providers::`, tools under `tools::`, and domain errors beside their domain.
- Keep implementation types `pub(crate)` unless callers implementing a public trait must name them.
- Follow [workflow.md](workflow.md) for inventory and verification steps after API changes.

## Type and Variant Names

**Choose names that describe the domain without redundant prefixes.**

- Name concrete LLM providers for the vendor: `Anthropic`, `OpenAi`, `Mistral`, `LiteLlm`.
- Give the public handle the bare noun and its extension trait the `Like` suffix: `Provider` and `ProviderLike`.
- Name failure variants as concrete states: `AuthenticationFailed`, `ContextWindowExceeded`, `PolicyViolated`.
- Use a tuple variant for one self-explanatory payload and a struct variant when field names carry meaning.
- When a fieldless discriminant needs a stable external spelling, expose `get_name()` and reuse it for `Display` and serde.

## Fields

**Use one vocabulary for common payloads and units.**

- Name human-readable failure text `message`, wrapped errors `source`, and stable categories `kind`.
- Use `Duration` in public Rust APIs; use a float named `seconds` in Python bindings.
- Suffix directory paths with `_dir`, file paths with `_file`, and ambiguous paths with `_path`.
- Name scalar counts with a plural noun such as `turns` or `input_tokens`; reserve `_counts` for grouped maps.
- Return `Option` when an empty population has no value, but return zero for an empty sum.

## Methods

**Let the receiver and operation determine the method name.**

- Use bare nouns for builders: `model`, `tool`, `label`, `concurrent`; do not add `with_`.
- Pair singular and bulk builders such as `directive` / `directives`, `template` / `templates`, and `tool` / `tools`.
- Use `get_` for public readers, `set_` for mutation, and `is_` or `has_` for boolean questions.
- Use `new` for the primary constructor and a semantic name such as `load`, `from_env`, `success`, or `error` for another path.
- Use `save` and `load` for whole values, and `append` for append-only logs.
- Keep free functions for module entry points, foreign-type construction, shared algorithms, or ambient state; otherwise use a method.

## Werk Operations

**Name orchestration methods by action and selection scope.**

- Use `finish_task(matches)`, `finish_tasks(matches)`, and `finish_all_tasks()` for waiting and results.
- Use `cancel_tasks(matches)` and `cancel_all_tasks()` for cancellation; do not add label-specific variants.
- Use `find_*` for AQL or closure selection and `get_*` for direct access such as `get_task(id)`.
- Name observers `on_<trigger>` and async twins `on_<trigger>_async`; name immediate mutation `edit_<noun>`.
- Publish built-in and application events only through `Werk::emit_event`.

## Rust Documentation

**Document public purpose and non-obvious contracts, not implementation narration.**

- Begin each module with `//!` stating what it contributes.
- Begin `///` comments with a noun phrase for a type or an imperative verb for a method.
- Keep rustdoc examples small and runnable; let strict rustdoc checks catch broken links and doctest drift.
- Describe caller-visible behavior in public docs; keep locks, `Arc`, private helpers, and file algorithms in `architecture.md` when they form an invariant.
- Keep a type's member documentation consistently complete; do not document only arbitrary members.

## Line Comments

**Keep comments only when the code cannot state the reason.**

- Explain ordering, crash safety, protocol quirks, workarounds, or non-obvious constraints.
- Use a plain section label only to divide a long function.
- Delete comments that restate the next line, narrate setup, or preserve task history.
- Do not leave `TODO`, `FIXME`, commented-out code, or decorative banners.

## Python Bindings

**Mirror every supported Rust item with the same public name unless Python requires a documented transform.**

- Keep binding modules aligned with their Rust domains and centralize conversion in `convert.rs`.
- Collapse Rust configuration types into the Python class they configure when Python cannot expose both builder and reader forms.
- Represent fieldless Rust enums by their snake_case name and data-carrying enums by a `kind` plus `data` object.
- Convert `&mut` editors into callables returning a replacement or `None` to keep the current value.
- Update `_agentwerk`, `__init__.py`, `__init__.pyi`, and parity tests together when the surface changes.

## Prose

**Write direct, concrete sentences at the reader's abstraction level.**

- Put the action or constraint first and keep one idea per sentence.
- Use second person for caller guidance and imperative voice for agent-facing text.
- Avoid marketing adjectives, borrowed metaphors, hedging, and unexplained internal jargon.
- Use a colon or a new sentence instead of an em dash.
- Preserve negations, exceptions, units, identifiers, and ordering constraints when shortening prose.

## README

**Keep both READMEs example-first and synchronized by concept.**

- Lead each API section with the smallest useful example and move exhaustive lists into one trailing `<details>` block.
- Use tables for catalogues inside folds and short prose above them.
- Describe public behavior, not private types or control flow.
- Mirror root examples in `crates/agentwerk-py/README.md` with the same names, values, and operation order.
- Update examples, methods, events, tools, and environment variables in the same change as the public API.
