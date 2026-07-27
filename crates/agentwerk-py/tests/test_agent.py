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


def test_chaining_returns_the_same_agent():
    agent = aw.Agent()
    assert agent.role("r").label("x").labels(["y", "z"]).dir(".") is agent


def test_context_and_interactive_chain():
    agent = aw.Agent()
    assert agent.context("You work in a monorepo.").interactive() is agent


def test_template_variables_chain_singly_and_in_bulk():
    agent = aw.Agent()
    configured = agent.template_variable("one", "1").template_variables(
        {"two": "2", "three": "3"}
    )
    assert configured is agent


def test_build_with_explicit_provider_and_model_succeeds(offline_agent):
    assert offline_agent.task("go").startswith("TICKET-")


def test_empty_agent_builds_without_the_finish_tool():
    agent = (
        aw.Agent.empty()
        .provider(aw.AnthropicProvider("test-key"))
        .model("claude-sonnet-4-20250514")
        .build()
    )
    assert agent.task("go").startswith("TICKET-")


def test_model_accepts_a_tuned_model_object():
    agent = (
        aw.Agent()
        .provider(aw.AnthropicProvider("test-key"))
        .model(aw.Model("claude-sonnet-4-20250514").context_window(128_000))
        .build()
    )
    assert agent.task("go").startswith("TICKET-")


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
    assert agent.task("go").startswith("TICKET-")


def test_using_an_unbuilt_agent_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Agent().task("count to three")


def test_configuring_after_build_is_rejected(offline_agent):
    with pytest.raises(RuntimeError):
        offline_agent.role("too late")


def test_building_twice_is_rejected(offline_agent):
    with pytest.raises(RuntimeError):
        offline_agent.build()


def test_registering_an_unbuilt_agent_is_rejected(system):
    with pytest.raises(RuntimeError):
        system.agent(aw.Agent())


def test_agent_enqueues_a_ticket_on_its_private_system(offline_agent):
    key = offline_agent.ticket(aw.Ticket("scan the corpus", labels=["scan"]))
    assert key.startswith("TICKET-")


def test_binding_an_agent_drains_its_queue_into_the_shared_system(
    offline_agent, system
):
    offline_agent.task("count to three")
    offline_agent.ticket_system(system)

    assert [t.task for t in system.tickets()] == ["count to three"]
