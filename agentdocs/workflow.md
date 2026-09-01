# Workflow

Commands for building, testing, documenting, running, and releasing the workspace.

## Build

**Use the Make targets so warnings and documentation checks stay consistent.**

```bash
make                 # format and build with warnings denied
make fmt             # format Rust code
make doc             # build strict rustdoc for agentwerk
make check_names     # reject removed names and missing inventory files
make clean           # remove build artifacts
make update          # update dependencies
```

- Run `make` after Rust changes.
- Run `make doc` after public API or rustdoc changes.
- Update `INVENTORY.md` with every added, removed, renamed, or retyped declaration.

## Rust Tests

**Run the offline suite before any live-provider suite.**

```bash
make test
make test_integration
make test_integration name=command_usage
```

- `make test` runs workspace library tests, rustdoc examples, and the `use-cases` binary tests.
- `make test_integration` runs `crates/agentwerk/tests/integration.rs` against the configured LLM provider.
- Set `name=<test name>` to filter the Rust integration binary.
- Export provider variables in the shell before live tests; the target does not load a `.env` file.

## Python Bindings

**Build and test the extension inside an activated virtual environment.**

```bash
python3 -m venv .venv
source .venv/bin/activate
make python
make python_test
make python_test_integration
```

- `make python` runs `maturin develop` in `crates/agentwerk-py/`.
- `make python_test` runs tests not marked `live`.
- `make python_test_integration` runs only tests marked `live` against the configured provider.
- Keep the virtual environment active because both maturin and pytest use `python3` from `PATH`.

## Use Cases

**Run examples through `make use_case`.**

```bash
make use_case
make use_case name=terminal-repl
make use_case name=deep-research args="What is a good life?"
```

- Use the empty target to list names from `crates/use-cases/Cargo.toml`.
- Pass program arguments through `args=`, not after `--`.
- Set `BRAVE_API_KEY` before running `deep-research`.

## Local Tooling

**Treat setup targets as changes outside the repository.**

- `make hooks` merges `hooks/hooks.json` into `.claude/settings.local.json`.
- `make skills` replaces same-named skills under `~/.claude/skills` and `~/.config/opencode/skills` with repository symlinks.
- `make litellm` starts a Docker proxy on port 4000; set `LITELLM_PROVIDER` to `anthropic`, `openai`, or `mistral`.

## Release

**Use `make bump` only when a versioned release is intended.**

```bash
make bump
make bump part=minor
make bump part=major
git push && git push --tags
```

- The target runs tests, updates both crate versions, commits, and creates a `v<version>` tag.
- Omit `part` for a patch release; use only `patch`, `minor`, or `major`.
- GitHub Actions publishes after the tag is pushed.
