"""Shared fixtures and the live-provider gate.

Offline tests run with no network and no `.env`. Tests marked ``live`` need a
real LLM provider and are skipped automatically when none is configured.
"""

import os

import pytest

import agentwerk as aw

PROVIDER_ENV_KEYS = (
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "MISTRAL_API_KEY",
    "LITELLM_API_KEY",
)


def has_provider() -> bool:
    """True when any supported provider is configured in the environment."""
    return any(os.environ.get(key) for key in PROVIDER_ENV_KEYS)


def pytest_collection_modifyitems(config, items):
    """Skip every ``live`` test when no provider is configured."""
    if has_provider():
        return
    skip = pytest.mark.skip(reason="no LLM provider configured")
    for item in items:
        if "live" in item.keywords:
            item.add_marker(skip)


@pytest.fixture
def queue(tmp_path):
    """A fresh, empty ticket queue with a session directory of its own, so one
    test's event log is never another's."""
    return aw.TicketQueue().dir(str(tmp_path))


@pytest.fixture
def offline_agent():
    """An agent built with a dummy provider: constructs without any network, so
    builder, enqueue, and query behavior is testable offline."""
    return (
        aw.Agent()
        .provider(aw.AnthropicProvider("test-key"))
        .model("claude-sonnet-4-20250514")
        .build()
    )


@pytest.fixture
def live_agent():
    """An agent resolved from the environment; only used by ``live`` tests."""
    return aw.Agent.from_env().role("You answer in one short word.").build()


@pytest.fixture
def knowledge_dir(tmp_path):
    """A temp directory for an Open Knowledge Format bundle."""
    return str(tmp_path / "kb")


@pytest.fixture
def dot_env(tmp_path, monkeypatch):
    """Run the test from an empty temp directory and hand it a writer for the
    `.env` there. Cleanup goes through ``os.unsetenv``, not ``os.environ``: the
    Rust side writes the process environment directly, so neither the
    ``os.environ`` snapshot nor ``monkeypatch`` ever sees those names.
    """
    monkeypatch.chdir(tmp_path)
    written = []

    def write(contents):
        (tmp_path / ".env").write_text(contents)
        for line in contents.splitlines():
            statement = line.strip().removeprefix("export ")
            if "=" in statement:
                written.append(statement.split("=", 1)[0].strip())

    yield write

    for name in written:
        os.unsetenv(name)
