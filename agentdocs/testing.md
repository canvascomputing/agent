# Testing

How Rust, Python, documentation, and live-provider tests demonstrate behavior.

## Offline Layers

**Put each deterministic test at the narrowest public boundary that proves the contract.**

- Keep Rust unit tests in inline `#[cfg(test)] mod tests` blocks beside the implementation.
- Keep rustdoc examples runnable as part of the offline suite.
- Keep binary-specific tests inside `crates/use-cases/src/` so the `--bins` pass reaches them.
- Keep offline Python behavior under `crates/agentwerk-py/tests/` without the `live` marker.

## Live Providers

**Separate provider-dependent behavior from the offline suite.**

- Put Rust live tests under `crates/agentwerk/tests/integration/` and register them through `tests/integration.rs`.
- Put shared live helpers in `tests/integration/common.rs`.
- Mark provider-dependent Python tests with `@pytest.mark.live`.
- Require explicit provider environment variables; do not silently skip a requested live suite.

## Test Shape

**Make one test demonstrate one observable behavior.**

- Name the behavior and expected outcome, such as `add_reply_appends_one_line_to_replies_jsonl`.
- Exercise the public entry point when the contract is public; use private access only for a private algorithm.
- Assert state through readers such as `Task::get_status`, `Task::get_result`, or `Werk::find_events`.
- Cover rejected transitions and verify the original state remains unchanged.
- Merge sibling cases when one parameterized or table-driven test expresses the same rule more clearly.

## Boundaries

**Use real in-process state and isolate only external trust boundaries.**

- Prefer temporary directories, real stores, and real serialization over mocks of agentwerk internals.
- Fake network responses, elapsed time, or provider output only at the boundary that owns them.
- Keep the action under test visible; move repeated setup into `test_util` helpers.
- Comment only when a test protects an architectural invariant that its name cannot express.

## Surface Parity

**Treat tests as executable API documentation.**

- Add a happy path, failure path, and meaningful boundary for each public operation.
- Update Rust doctests when public examples change.
- Update `crates/agentwerk-py/tests/test_parity.py` when Rust or Python exports change.
- Follow [workflow.md](workflow.md) for the exact commands and live-test prerequisites.
