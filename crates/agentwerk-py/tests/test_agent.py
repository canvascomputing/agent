"""The module surface and the agent builder."""

import pytest

import agentwerk as aw

PROVIDER_KEYS = (
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "MISTRAL_API_KEY",
    "LITELLM_API_KEY",
    "LITELLM_PROVIDER",
)


def test_re_exports_every_name_in_all():
    for name in aw.__all__:
        assert hasattr(aw, name), name


def test_chaining_returns_the_builder():
    builder = aw.Agent().role("r").label("x").labels(["y", "z"]).dir(".")
    assert isinstance(builder, aw.Agent)


def test_build_with_explicit_provider_and_model_succeeds(offline_agent):
    assert isinstance(offline_agent, aw.BuiltAgent)


def test_build_without_a_provider_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Agent().model("claude-sonnet-4-20250514").build()


def test_build_without_a_model_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Agent().provider(aw.AnthropicProvider("test-key")).build()


def test_from_env_without_provider_env_is_rejected(monkeypatch):
    for key in PROVIDER_KEYS:
        monkeypatch.delenv(key, raising=False)
    with pytest.raises(RuntimeError):
        aw.Agent().from_env().build()


def test_on_failure_hook_is_accepted_and_builds():
    agent = (
        aw.Agent()
        .provider(aw.AnthropicProvider("test-key"))
        .model("claude-sonnet-4-20250514")
        .on_failure(lambda detail: "replacement")
        .build()
    )
    assert isinstance(agent, aw.BuiltAgent)
