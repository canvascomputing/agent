"""Providers, model tuning, and the knowledge store."""

import pytest

import agentwerk as aw

VENDOR_CONSTRUCTORS = [
    aw.AnthropicProvider,
    aw.OpenAiProvider,
    aw.MistralProvider,
    aw.LiteLlmProvider,
]

PROVIDER_KEYS = (
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "MISTRAL_API_KEY",
    "LITELLM_API_KEY",
    "LITELLM_PROVIDER",
)


@pytest.mark.parametrize("constructor", VENDOR_CONSTRUCTORS)
def test_per_vendor_constructors_build_a_provider(constructor):
    assert isinstance(constructor("key"), aw.Provider)
    assert isinstance(constructor("key", "https://endpoint.example/v1"), aw.Provider)


def test_model_chains_context_window_and_reasoning_effort():
    model = aw.Model("my-local-model").context_window(128_000).reasoning_effort("high")
    assert isinstance(model, aw.Model)


def test_unknown_reasoning_effort_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Model("m").reasoning_effort("bogus")


def test_provider_from_env_without_env_is_rejected(monkeypatch):
    for key in PROVIDER_KEYS:
        monkeypatch.delenv(key, raising=False)
    with pytest.raises(RuntimeError):
        aw.provider_from_env()


def test_knowledge_load_creates_an_empty_index(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    assert store.index() == ""


def test_index_char_limit_chains(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir).index_char_limit(24_000)
    assert isinstance(store, aw.Knowledge)


def test_clear_empties_the_store(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    store.clear()
    assert store.index() == ""


def test_agent_binds_a_knowledge_store(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    assert isinstance(aw.Agent().knowledge(store), aw.Agent)
