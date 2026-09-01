"""Provide shared fixtures and skip live tests without a provider.

Offline tests run with no network. Tests marked ``live`` need a real LLM
provider and are skipped automatically when none is configured.
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
    """Return whether the environment configures a supported provider."""
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
def werk(tmp_path):
    """Provide an empty Werk with an isolated session directory."""
    return aw.Werk().set_dir(str(tmp_path))


@pytest.fixture
def offline_agent():
    """Provide an agent that supports offline configuration and query tests."""
    return (
        aw.Agent()
        .provider(aw.Anthropic("test-key"))
        .model("claude-sonnet-4-20250514")
    )


@pytest.fixture
def live_agent():
    """Provide an agent from the environment for ``live`` tests."""
    return aw.Agent.from_env().role("You answer in one short word.")


@pytest.fixture
def knowledge_dir(tmp_path):
    """Provide a temporary directory for an Open Knowledge Format bundle."""
    return str(tmp_path / "kb")
