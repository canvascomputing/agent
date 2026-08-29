"""The module surface and agent configuration."""

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


def test_an_explicit_provider_and_model_let_the_agent_take_a_task(offline_agent):
    assert offline_agent.task("go").startswith("t-")


def test_model_accepts_a_tuned_model_object():
    agent = (
        aw.Agent()
        .provider(aw.Anthropic("test-key"))
        .model(aw.Model("claude-sonnet-4-20250514").context_window(128_000))
    )
    assert agent.task("go").startswith("t-")


def test_starting_without_a_provider_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Agent().model("claude-sonnet-4-20250514").start()


def test_starting_without_a_model_is_rejected():
    with pytest.raises(RuntimeError):
        aw.Agent().provider(aw.Anthropic("test-key")).start()


def test_from_env_without_provider_env_is_rejected(monkeypatch):
    for key in PROVIDER_KEYS:
        monkeypatch.delenv(key, raising=False)
    with pytest.raises(RuntimeError):
        aw.Agent.from_env()


def test_id_is_taken_from_the_label():
    agent = (
        aw.Agent()
        .label("id_from_label")
        .provider(aw.Anthropic("test-key"))
        .model("claude-sonnet-4-20250514")
    )
    assert agent.id == "id_from_label-1"


def test_registering_an_agent_without_a_provider_is_rejected(queue):
    with pytest.raises(RuntimeError):
        queue.agent(aw.Agent())


def test_agent_enqueues_a_task_on_its_private_queue(offline_agent):
    key = offline_agent.task(aw.Task("scan the corpus", label="scan"))
    assert key.startswith("t-")


def test_binding_an_agent_drains_its_queue_into_the_shared_queue(
    offline_agent, queue
):
    offline_agent.task("count to three")
    queue.agent(offline_agent)

    assert [t.task for t in queue.tasks()] == ["count to three"]
