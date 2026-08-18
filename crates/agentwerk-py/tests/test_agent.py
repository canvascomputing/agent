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
    assert agent.role("r").label("x").dir(".") is agent


def test_role_reads_a_path_as_the_file_holding_it(tmp_path):
    role = tmp_path / "reviewer.md"
    role.write_text("You review code.\n")
    agent = aw.Agent()
    assert agent.role(role) is agent


def test_role_naming_a_missing_file_is_rejected(tmp_path):
    with pytest.raises(RuntimeError):
        aw.Agent().role(tmp_path / "absent.md")


def test_interactive_chains():
    agent = aw.Agent()
    assert agent.interactive() is agent


def test_templates_chain_singly_and_in_bulk():
    agent = aw.Agent()
    configured = agent.template("one", "1").templates({"two": "2", "three": "3"})
    assert configured is agent


def test_build_with_explicit_provider_and_model_succeeds(offline_agent):
    assert offline_agent.ticket("go").startswith("TICKET-")


def test_model_accepts_a_tuned_model_object():
    agent = (
        aw.Agent()
        .provider(aw.Anthropic("test-key"))
        .model(aw.Model("claude-sonnet-4-20250514").context_window(128_000))
        .build()
    )
    assert agent.ticket("go").startswith("TICKET-")


def test_build_without_a_provider_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Agent().model("claude-sonnet-4-20250514").build()


def test_build_without_a_model_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Agent().provider(aw.Anthropic("test-key")).build()


def test_from_env_without_provider_env_is_rejected(monkeypatch):
    for key in PROVIDER_KEYS:
        monkeypatch.delenv(key, raising=False)
    with pytest.raises(RuntimeError):
        aw.Agent.from_env()


def test_using_an_unbuilt_agent_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Agent().ticket("count to three")


def test_id_is_built_from_the_label():
    agent = (
        aw.Agent()
        .label("id_from_label")
        .provider(aw.Anthropic("test-key"))
        .model("claude-sonnet-4-20250514")
        .build()
    )
    assert agent.id == "id_from_label-1"


def test_reading_the_id_of_an_unbuilt_agent_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Agent().id


def test_configuring_after_build_is_rejected(offline_agent):
    with pytest.raises(RuntimeError):
        offline_agent.role("too late")


def test_building_twice_is_rejected(offline_agent):
    with pytest.raises(RuntimeError):
        offline_agent.build()


def test_registering_an_unbuilt_agent_is_rejected(queue):
    with pytest.raises(RuntimeError):
        queue.agent(aw.Agent())


def test_agent_enqueues_a_ticket_on_its_private_queue(offline_agent):
    key = offline_agent.ticket(aw.Ticket("scan the corpus", label="scan"))
    assert key.startswith("TICKET-")


def test_binding_an_agent_drains_its_queue_into_the_shared_queue(
    offline_agent, queue
):
    offline_agent.ticket("count to three")
    queue.agent(offline_agent)

    assert [t.task for t in queue.tickets()] == ["count to three"]
