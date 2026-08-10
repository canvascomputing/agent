# Development

## Workspace

- `crates/agentwerk/`: the library.
- `crates/agentwerk-py/`: the Python bindings, built with maturin.
- `crates/use-cases/`: runnable example binaries that depend on the library.

## Building and testing

```bash
make                # build (warnings are errors)
make test           # unit tests bundled by tests/unit (workspace --lib)
make fmt            # format code
make clean          # remove build artifacts
make update         # update dependencies
make hooks          # install Claude Code hooks
```

## Python bindings

Create a virtualenv at the repo root and activate it; maturin installs into it,
and the test targets call the `python3` on your PATH.

```bash
python3 -m venv .venv
source .venv/bin/activate

make python                    # maturin develop
make python_test               # offline pytest suite
make python_test_integration   # the tests marked live
```

## Integration tests

> Configure an LLM provider first (see [Environment](#environment)).

```bash
make test_integration                     # run all
make test_integration name=bash_usage     # run one
```

## Use cases

```bash
make use_case                                                 # list available
make use_case name=terminal-repl                              # run one
make use_case name=deep-research args="What is a good life?"  # with arguments
```

## Publishing

```bash
make bump                  # bump patch version, run tests, commit, tag
make bump part=minor       # bump minor version
make bump part=major       # bump major version
```

GitHub Actions handles the crates.io publish via trusted publishing once the new tag is pushed (`git push --tags`).

## Documentation

```bash
make doc                   # cargo doc --no-deps -p agentwerk (strict rustdoc)
```

## LiteLLM proxy

Start a local LiteLLM proxy on port 4000 that forwards to a provider. Requires Docker.

```bash
make litellm                               # default: anthropic
make litellm LITELLM_PROVIDER=openai       # use OpenAI
make litellm LITELLM_PROVIDER=mistral      # use Mistral
```

## Local inference servers

agentwerk relies on server-side tool calling. Enable it through the following flags:

| Server | Flag |
|---|---|
| vLLM | `--enable-auto-tool-choice --tool-call-parser <parser>` |
| llama.cpp | `--jinja` (enables tool calling) |

## Environment

Use cases and integration tests use the following environment variables, or a
`.env` file read by the `from_dot_env()` constructors.

**General**

| Variable | Description |
|----------|-------------|
| `MODEL` | Generic model override for `Model::from_env()`. |
| `MODEL_CONTEXT_WINDOW` | Context window size in tokens, applied by `Model::from_env()` over the registry's value for the name. |
| `BRAVE_API_KEY` | Required by the `deep-research` example. |
| `SSL_CERT_FILE` | PEM bundle of CA certificates to trust instead of the built-in root store. |
| `SSL_CERT_DIR` | Directory of PEM CA certificate files to trust instead of the built-in root store. |

**Anthropic**

| Variable | Description |
|----------|-------------|
| `ANTHROPIC_API_KEY` | API key (required) |
| `ANTHROPIC_BASE_URL` | API URL (default: `https://api.anthropic.com`) |
| `ANTHROPIC_MODEL` | Model (default: `claude-sonnet-4-20250514`) |

**Mistral**

| Variable | Description |
|----------|-------------|
| `MISTRAL_API_KEY` | API key (required) |
| `MISTRAL_BASE_URL` | API URL (default: `https://api.mistral.ai`) |
| `MISTRAL_MODEL` | Model (default: `mistral-medium-2508`) |

**OpenAI**

| Variable | Description |
|----------|-------------|
| `OPENAI_API_KEY` | API key (required) |
| `OPENAI_BASE_URL` | API URL (default: `https://api.openai.com`) |
| `OPENAI_MODEL` | Model (default: `gpt-4o`) |

**LiteLLM proxy**

| Variable | Description |
|----------|-------------|
| `LITELLM_BASE_URL` | Proxy URL (default: `http://localhost:4000`) |
| `LITELLM_API_KEY` | Auth key (required to select via `Provider::from_env()`) |
| `LITELLM_MODEL` | Model (default: `claude-sonnet-4-20250514`) |
| `LITELLM_PROVIDER` | LLM provider (`anthropic`, `mistral`, `openai`, `litellm`): explicit selection that overrides API-key auto-detection. |
