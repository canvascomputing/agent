# Workflow

Commands used to build, test, release, and run example agents.

## Build

**Every build MUST run with `-D warnings`.**

```bash
make        # compile the crate
make fmt    # format the code
make clean  # remove build artifacts
```

- Any warning fails the build.

## Test

**Test layout and writing rules live in [testing.md](testing.md).**

- `make test` runs three passes: `--lib` (every crate's inline `#[cfg(test)] mod tests`), `--doc` (the examples in `///` comments), and `-p use-cases --bins` (the tests inside the use-case binaries).
- `--lib` alone reaches neither of the last two.
- `make test_integration` runs the live-provider tests bundled by `tests/integration.rs`.

## Python Bindings

**`make python` builds the extension; the two test targets split on whether an LLM provider is needed.**

- `make python` runs `maturin develop` in `crates/agentwerk-py/`, building the extension into the active virtualenv.
- Create the virtualenv first with `python3 -m venv .venv` at the repo root and activate it. maturin fails without one, and the test targets resolve `python3` off the PATH, so an unactivated virtualenv imports the system interpreter instead.
- `make python_test` runs the offline pytest suite. It needs no network and no `.env`.
- `make python_test_integration` sources `.env` and runs the tests marked `live`, which call a real LLM provider.
- Both test targets depend on `make python`, so an edit to the binding crate is picked up automatically.

## Integration Environment

**Integration tests read LLM provider configuration from a `.env` file at the repo root.**

```bash
export OPENAI_API_KEY=sk-local
export OPENAI_BASE_URL=http://localhost:8095
```

- `make test_integration` sources `.env` automatically when present.
- The file holds shell `export` statements, one per variable.
- `OPENAI_BASE_URL` points at a local OpenAI-compatible proxy on port 8095.
- `.env` is gitignored: each contributor maintains their own.

## Release

**`make bump` runs the full release step in one command.**

- `make bump` runs tests, bumps the patch version, commits, and tags.
- `make bump part=minor` bumps the minor version.
- `make bump part=major` bumps the major version.
- Push the new tag with `git push --tags`.

## Hooks

**`make hooks` installs Claude Code hooks into `.claude/settings.local.json`.**

- Source files live in `hooks/` (tracked). `make hooks` copies them into `.claude/hooks/` (ignored) and merges the configuration.
- `check-conventions.sh` injects `agentdocs/style.md` and `agentdocs/architecture.md` as context after each Rust file edit.

## Use Cases

**Example agents live in a separate crate and run through `make use_case`.**

```bash
make use_case                # list available names
make use_case name=<name>    # run one
```

- Source is in `crates/use-cases/src/`.
- `terminal-repl` is a per-turn interactive chat that prints output as it arrives.
- `divide-and-conquer` partitions an arithmetic problem across agents sharing one ticket queue.
- `deep-research` is a two-phase research pipeline with web search, and requires `BRAVE_API_KEY`.
- `malware-scanner` identifies indicators of compromise in a software package.
