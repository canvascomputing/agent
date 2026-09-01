"""Test LLM providers, model settings, and the knowledge store."""

import pytest

import agentwerk as aw

VENDOR_CONSTRUCTORS = [
    aw.Anthropic,
    aw.OpenAi,
    aw.Mistral,
    aw.LiteLlm,
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


@pytest.mark.parametrize("constructor", VENDOR_CONSTRUCTORS)
def test_provider_accepts_a_request_timeout_in_seconds(constructor):
    assert isinstance(constructor("key", timeout=30.0), aw.Provider)


def test_model_chains_context_window_and_reasoning_effort():
    model = aw.Model("my-local-model").context_window(128_000).reasoning_effort("high")
    assert model.get_context_window() == 128_000
    assert model.get_reasoning_effort() == "high"


def test_model_reasoning_effort_is_off_by_default():
    assert aw.Model("my-local-model").get_reasoning_effort() == "off"


def test_unknown_reasoning_effort_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Model("m").reasoning_effort("bogus")


def test_provider_from_env_builds_a_provider(monkeypatch):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")
    assert isinstance(aw.Provider.from_env(), aw.Provider)


def test_provider_from_env_without_env_is_rejected(monkeypatch):
    for key in PROVIDER_KEYS:
        monkeypatch.delenv(key, raising=False)
    with pytest.raises(RuntimeError):
        aw.Provider.from_env()


def test_model_from_env_reads_the_model_variable(monkeypatch):
    monkeypatch.setenv("MODEL", "my-local-model")
    assert aw.Model.from_env().get_name() == "my-local-model"


def test_model_from_env_applies_the_window_override(monkeypatch):
    monkeypatch.setenv("MODEL", "my-local-model")
    monkeypatch.setenv("MODEL_CONTEXT_WINDOW", "64000")
    assert aw.Model.from_env().get_context_window() == 64_000


def test_model_from_env_leaves_the_window_unset_without_the_override(monkeypatch):
    monkeypatch.setenv("MODEL", "my-local-model")
    monkeypatch.delenv("MODEL_CONTEXT_WINDOW", raising=False)
    assert aw.Model.from_env().get_context_window() is None


def test_model_from_env_without_provider_env_is_rejected(monkeypatch):
    for key in (*PROVIDER_KEYS, "MODEL"):
        monkeypatch.delenv(key, raising=False)
    with pytest.raises(RuntimeError):
        aw.Model.from_env()


def test_knowledge_load_creates_an_empty_index(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    assert store.get_index() == ""


def test_set_index_char_limit_chains(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir).set_index_char_limit(24_000)
    assert isinstance(store, aw.Knowledge)


def test_saved_page_is_readable_and_indexed(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)

    store.get_pages().save(
        aw.Page(
            "build-command",
            "How the project is built.",
            "Run `make` to compile with warnings denied.",
            tags=["build"],
        )
    )

    page = store.get_pages().get_page("build-command")
    assert page.get_description() == "How the project is built."
    assert page.get_tags() == ["build"]
    assert "build-command" in store.get_index()


def test_page_kind_defaults_to_the_store_default(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    page = aw.Page("scratch", "A note.", "Some content.")

    store.get_pages().save(page)

    assert page.get_kind() == "Knowledge"
    assert store.get_pages().get_page("scratch").get_kind() == "Knowledge"


def test_list_returns_every_saved_page_in_index_order(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    store.get_pages().save(aw.Page("build", "How to build.", "Run make."))
    store.get_pages().save(aw.Page("deploy", "How to deploy.", "Push the tag."))

    pages = store.get_pages().get_all()

    assert [page.get_slug() for page in pages] == ["build", "deploy"]
    assert pages[0].get_description() == "How to build."


def test_get_index_char_limit_returns_the_default_until_it_is_set(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    assert store.get_index_char_limit() == 12_000
    store.set_index_char_limit(80)
    assert store.get_index_char_limit() == 80


def test_removed_page_leaves_the_index_empty(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    store.get_pages().save(aw.Page("scratch", "A note.", "Some content."))

    store.get_pages().remove("scratch")

    assert store.get_index() == ""


def test_loading_an_unknown_page_is_rejected(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    with pytest.raises(RuntimeError):
        store.get_pages().get_page("does-not-exist")


def test_clear_empties_the_store(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    store.get_pages().save(aw.Page("scratch", "A note.", "Some content."))

    store.clear()

    assert store.get_index() == ""


def test_agent_binds_a_knowledge_store(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    agent = aw.Agent()
    assert agent.knowledge(store) is agent
