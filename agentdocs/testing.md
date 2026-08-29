# Testing

How tests are organized and written. Commands used to run them live in [workflow.md](workflow.md).

## Layers

**Two layers: integration and inline.**

- `tests/integration/` uses a real LLM provider; bundled by `tests/integration.rs`, with shared helpers in `tests/integration/common.rs`.
- Inline `#[cfg(test)] mod tests` lives next to the code it covers and runs without a network.

## Purpose

**One test, one observable behavior.**

- A test exists because a single contract would otherwise go undemonstrated.
- A failure points to one cause: no grab-bag assertions across unrelated concerns.
- A sibling covering the same branch with trivial input changes is merged or removed.
- Behavior is tested at the layer where it lives: unit, integration, or inline.

## Naming

**The name states the behavior, not the method called.**

```rust
add_reply_appends_one_line_to_replies_jsonl        // accepted
an_explicit_task_schema_overrides_the_label_default
test_add_reply                                     // rejected
test_schema_works
```

- The body verifies what the name claims, with no surprise assertions.
- The name is the first line of the documentation the test provides.

## API Focus

**Tests exercise the public surface the way callers hold it.**

- Call the public entry point; do not poke at private fields, patched internals, or field assignments.
- Mock at trust boundaries (network, clock, disk), never at the subject under test.
- Assert observable outcomes, not call logs or the order internal methods ran in.
- The arrange, act, and assert shape mirrors how a real caller would use the API.

## State Transitions

**Actions and the resulting state MUST be visible through the public API.**

- Build the starting state by calling real actions, not by field assignment that bypasses invariants.
- Read the resulting state back through a public query, not by peeking at private fields.
- Assert both the starting and the final state so the transition is shown, not implied.
- Cover illegal transitions and verify state is unchanged after a rejection.
- One transition per test, so a failure locates the exact broken action.

## Clarity

**Setup is hidden. Intent is highlighted.**

- Push scaffolding into factories, builders, and fixtures so the body reads as a short story.
- Name literals that carry meaning: `EXPIRED_COUPON`, not `42`.
- Keep the act step a single visible line; do not bury it in setup.
- Comments are justified only to pin an architectural invariant the test guards.

## Coverage Shape

**Every public operation has a test that demonstrates intended usage.**

- Error cases, edge conditions, and boundaries sit at the same interface level as the happy path.
- A missing behavior is added before a duplicate case is kept for symmetry.
- IMPORTANT: a public method with no test is a documentation gap, not just a coverage gap.
