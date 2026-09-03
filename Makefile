.PHONY: build test test_integration bench_aql fmt clean update use_case litellm bump doc hooks skills python python_test python_test_integration check_names

CLAUDE_SKILLS_DIR := $(HOME)/.claude/skills
OPENCODE_SKILLS_DIR := $(HOME)/.config/opencode/skills
CODEX_SKILLS_DIR := $(HOME)/.agents/skills
SKILL_DESTS := $(CLAUDE_SKILLS_DIR) $(OPENCODE_SKILLS_DIR) $(CODEX_SKILLS_DIR)
SKILL_NAMES := $(notdir $(shell find $(CURDIR)/skills -mindepth 1 -maxdepth 1 -type d))

# Build the workspace with warnings as errors.
build: fmt
	RUSTFLAGS="-D warnings" cargo build

# Run offline Rust tests with warnings as errors. This includes inline test modules,
# doctests, and tests inside the use-case binaries.
test:
	RUSTFLAGS="-D warnings" cargo test --workspace --lib
	RUSTFLAGS="-D warnings" cargo test --workspace --exclude agentwerk-py --doc
	RUSTFLAGS="-D warnings" cargo test -p use-cases --bins

# Run integration tests against the LLM provider selected by the environment.
# Export the provider's variables in your shell first; nothing is read from a file.
# Usage: make test_integration              (run all)
#        make test_integration name=command_usage  (run one file)
test_integration:
ifdef name
	RUSTFLAGS="-D warnings" cargo test --test integration $(name) -- --nocapture
else
	RUSTFLAGS="-D warnings" cargo test --test integration -- --nocapture --test-threads=1
endif

# Benchmark AQL compilation and selection. Pass benchmark arguments with args.
# Usage: make bench_aql
#        make bench_aql args='joined/find_tasks --save-baseline before'
bench_aql:
	cargo bench -p agentwerk --bench aql -- $(args)

# Build and install the Python bindings into the active environment.
python:
	cd crates/agentwerk-py && maturin develop

# Run the offline Python binding tests without an LLM provider.
python_test: python
	@cd crates/agentwerk-py && python3 -m pip install -q pytest pytest-asyncio && \
	  python3 -m pytest tests -q -m "not live"

# Run the live Python binding tests against the provider selected by the environment.
python_test_integration: python
	@cd crates/agentwerk-py && python3 -m pip install -q pytest pytest-asyncio && \
	  python3 -m pytest tests -q -m live

# Build rustdoc with warnings and broken links as errors.
doc:
	RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links -D rustdoc::private-intra-doc-links" \
	  cargo doc --no-deps -p agentwerk

# Reject removed API spellings and ensure every non-test source file remains
# represented in the project inventory.
check_names:
	@sh tools/check-names.sh

# Format all code.
fmt:
	cargo fmt

# Remove build artifacts.
clean:
	cargo clean

# Update dependencies.
update:
	cargo update

# Run a use-case binary.
# Usage: make use_case name=deep-research args="Should we use Rust or Go?"
# Pass arguments with `args=`, not `--`.
# Exit 2 is the malware-scanner's malicious-verdict signal under --fail-fast,
# not a build failure, so it is tolerated; every other non-zero code still fails.
use_case:
ifdef name
	@cargo run -p use-cases --bin $(name) -- $(args) || [ $$? -eq 2 ]
else
	@echo "Available use cases:"
	@grep -A1 '^\[\[bin\]\]' crates/use-cases/Cargo.toml | grep 'name' | sed 's/.*"\(.*\)"/  \1/'
	@echo ""
	@echo "Run with: make use_case name=<name> args=\"...\""
endif

# Bump the version, test, commit, and tag a release. Use `part=patch` (default), `minor`, or `major`.
# GitHub Actions publishes to crates.io through trusted publishing after you push the tag.
part ?= patch
bump: test
	@current=$$(grep '^version' crates/agentwerk/Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'); \
	IFS='.' read -r major minor patch <<< "$$current"; \
	case "$(part)" in \
		major) major=$$((major + 1)); minor=0; patch=0;; \
		minor) minor=$$((minor + 1)); patch=0;; \
		patch) patch=$$((patch + 1));; \
		*) echo "Unknown part: $(part). Use major, minor, or patch."; exit 1;; \
	esac; \
	new="$$major.$$minor.$$patch"; \
	sed -i '' "s/^version = \"$$current\"/version = \"$$new\"/" crates/agentwerk/Cargo.toml; \
	sed -i '' "s/^version = \"$$current\"/version = \"$$new\"/" crates/agentwerk-py/Cargo.toml; \
	cargo check --workspace --quiet; \
	git add -A && git commit -m "v$$new" && \
	git tag "v$$new" && \
	echo "$$current → $$new" && \
	echo "Tagged v$$new — now run:" && \
	echo "  git push && git push --tags"

# Install Claude Code hooks into `.claude/settings.local.json`.
hooks:
	@if [ ! -f .claude/settings.local.json ]; then echo '{}' > .claude/settings.local.json; fi
	@jq -s '.[0] * .[1]' .claude/settings.local.json hooks/hooks.json > .claude/settings.local.tmp \
		&& mv .claude/settings.local.tmp .claude/settings.local.json
	@echo "Hooks installed into .claude/settings.local.json"

# Symlink every skill under `skills/` into each tool's skills directory.
# Replace any installed skill with the same name.
skills:
	@for dest in $(SKILL_DESTS); do \
		mkdir -p "$$dest"; \
		for name in $(SKILL_NAMES); do \
			rm -rf "$$dest/$$name"; \
			ln -s "$(CURDIR)/skills/$$name" "$$dest/$$name"; \
			echo "$$name → $$dest/$$name"; \
		done; \
	done

# Start a LiteLLM proxy on `localhost:4000`.
# Pass the provider API key through the environment without placing it in the command.
# Usage: make litellm                  (default: anthropic, uses ANTHROPIC_API_KEY)
#        make litellm LITELLM_PROVIDER=openai  (uses OPENAI_API_KEY)
LITELLM_PROVIDER ?= anthropic

# Map `LITELLM_PROVIDER` to its API key environment variable.
ifeq ($(LITELLM_PROVIDER),anthropic)
  LITELLM_KEY_ENV     := ANTHROPIC_API_KEY
  LITELLM_MODEL_ENV   := ANTHROPIC_MODEL
  LITELLM_DEFAULT_MDL := claude-sonnet-4-20250514
else ifeq ($(LITELLM_PROVIDER),mistral)
  LITELLM_KEY_ENV     := MISTRAL_API_KEY
  LITELLM_MODEL_ENV   := MISTRAL_MODEL
  LITELLM_DEFAULT_MDL := mistral-small-2603
else ifeq ($(LITELLM_PROVIDER),openai)
  LITELLM_KEY_ENV     := OPENAI_API_KEY
  LITELLM_MODEL_ENV   := OPENAI_MODEL
  LITELLM_DEFAULT_MDL := gpt-4o
else
  LITELLM_KEY_ENV     :=
  LITELLM_MODEL_ENV   :=
  LITELLM_DEFAULT_MDL :=
endif

LITELLM_MODEL_VAL := $(or $($(LITELLM_MODEL_ENV)),$(LITELLM_DEFAULT_MDL))

litellm:
ifndef LITELLM_KEY_ENV
	$(error Unsupported LITELLM_PROVIDER "$(LITELLM_PROVIDER)". Supported: anthropic, mistral, openai)
endif
	@printf '%s\n' \
		'model_list:' \
		'  - model_name: $(LITELLM_MODEL_VAL)' \
		'    litellm_params:' \
		'      model: $(LITELLM_PROVIDER)/$(LITELLM_MODEL_VAL)' \
		'      api_key: os.environ/$(LITELLM_KEY_ENV)' \
		> /tmp/agent_litellm_config.yaml
	docker run --rm \
		-e $(LITELLM_KEY_ENV) \
		-v /tmp/agent_litellm_config.yaml:/app/config.yaml:ro \
		-p 4000:4000 \
		docker.litellm.ai/berriai/litellm:main-stable \
		--config /app/config.yaml
