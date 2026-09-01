# Development

## Workspace

- `crates/agentwerk/`: the library.
- `crates/agentwerk-py/`: the Python bindings, built with maturin.
- `crates/use-cases/`: runnable example binaries that depend on the library.

## Build and Test

```bash
make                # build (warnings are errors)
make test           # library, doctest, and use-case tests
make fmt            # format code
make clean          # remove build artifacts
make update         # update dependencies
make hooks          # install Claude Code hooks
```

## Python Bindings

Create a virtual environment at the repository root and activate it. Maturin installs into the active environment, and the test targets use the `python3` on your `PATH`.

```bash
python3 -m venv .venv
source .venv/bin/activate

make python                    # maturin develop
make python_test               # offline pytest suite
make python_test_integration   # the tests marked live
```

## Integration Tests

> Configure an LLM provider first (see [Environment](#environment)).

```bash
make test_integration                     # run all
make test_integration name=command_usage  # run one
```

## Use Cases

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

## LiteLLM Proxy

Start a local LiteLLM proxy on port 4000 that forwards to a provider. Requires Docker.

```bash
make litellm                               # default: anthropic
make litellm LITELLM_PROVIDER=openai       # use OpenAI
make litellm LITELLM_PROVIDER=mistral      # use Mistral
```

## Local Inference Servers

agentwerk relies on server-side tool calling. Enable it through the following flags:

| Server | Flag |
|---|---|
| vLLM | `--enable-auto-tool-choice --tool-call-parser <parser>` |
| llama.cpp | `--jinja` (enables tool calling) |

## Environment

Use cases and integration tests read these environment variables from the shell. Source a `.env` file yourself before running a target.

**General**

| Variable | Description |
|----------|-------------|
| `MODEL` | Set the model returned by `Model::from_env()`. |
| `MODEL_CONTEXT_WINDOW` | Set its context window in tokens, overriding the model registry. |
| `BRAVE_API_KEY` | Authenticate the `deep-research` example. |
| `SSL_CERT_FILE` | Trust a PEM CA bundle instead of the built-in root store. |
| `SSL_CERT_DIR` | Trust PEM CA certificate files from a directory instead of the built-in root store. |

**Anthropic**

| Variable | Description |
|----------|-------------|
| `ANTHROPIC_API_KEY` | Authenticate with Anthropic. Required. |
| `ANTHROPIC_BASE_URL` | Set the API URL. Defaults to `https://api.anthropic.com`. |
| `ANTHROPIC_MODEL` | Set the model. Defaults to `claude-sonnet-4-20250514`. |

**Mistral**

| Variable | Description |
|----------|-------------|
| `MISTRAL_API_KEY` | Authenticate with Mistral. Required. |
| `MISTRAL_BASE_URL` | Set the API URL. Defaults to `https://api.mistral.ai`. |
| `MISTRAL_MODEL` | Set the model. Defaults to `mistral-medium-2508`. |

**OpenAI**

| Variable | Description |
|----------|-------------|
| `OPENAI_API_KEY` | Authenticate with OpenAI. Required. |
| `OPENAI_BASE_URL` | Set the API URL. Defaults to `https://api.openai.com`. |
| `OPENAI_MODEL` | Set the model. Defaults to `gpt-4o`. |

**LiteLLM proxy**

| Variable | Description |
|----------|-------------|
| `LITELLM_BASE_URL` | Set the proxy URL. Defaults to `http://localhost:4000`. |
| `LITELLM_API_KEY` | Authenticate with LiteLLM and select it through `Provider::from_env()`. |
| `LITELLM_MODEL` | Set the model. Defaults to `claude-sonnet-4-20250514`. |
| `LITELLM_PROVIDER` | Select `anthropic`, `mistral`, `openai`, or `litellm` before API-key detection. |
