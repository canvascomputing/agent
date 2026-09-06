"""Test the module surface and agent configuration."""

import asyncio

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


def test_queue_is_not_a_compatibility_alias():
    assert not hasattr(aw, "Queue")


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
    assert offline_agent.add_task("go").startswith("t-")


def test_model_accepts_a_tuned_model_object():
    agent = (
        aw.Agent()
        .provider(aw.Anthropic("test-key"))
        .model(aw.Model("claude-sonnet-4-20250514").context_window(128_000))
    )
    assert agent.add_task("go").startswith("t-")


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
    assert agent.get_id() == "id_from_label-1"


def test_registering_an_agent_without_a_provider_is_rejected(werk):
    with pytest.raises(RuntimeError):
        werk.add_agent(aw.Agent())


def test_agent_enqueues_a_task_on_its_private_werk(offline_agent):
    id = offline_agent.add_task(aw.Task("scan the corpus", label="scan"))
    assert id.startswith("t-")


def test_binding_an_agent_drains_its_private_werk_into_the_shared_werk(
    offline_agent, werk
):
    offline_agent.add_task("count to three")
    werk.add_agent(offline_agent)

    assert [t.get_task() for t in werk.get_tasks()] == ["count to three"]


def test_add_task_uses_the_shared_werk_after_binding(offline_agent, werk):
    offline_agent.template("topic", "parity")
    werk.add_agent(offline_agent)

    id = offline_agent.add_task("check {topic}")

    assert id.startswith("t-")
    assert werk.get_task(id).get_task() == "check parity"


def test_agent_task_is_not_a_compatibility_alias():
    assert not hasattr(aw.Agent, "task")


@pytest.mark.parametrize("method", ["finish_task", "finish_tasks", "finish"])
async def test_finish_methods_start_execution_and_convert_results(
    method, scripted_openai, tmp_path
):
    expected = {"answer": [42, True, None]}
    scripted_openai.respond_with_tool("finish", {"result": expected})
    agent = (
        aw.Agent().provider(scripted_openai.provider()).model("mock").dir(str(tmp_path))
    )
    task = agent.add_task("answer")

    args = () if method == "finish" else (task,)
    result = await asyncio.wait_for(getattr(agent, method)(*args), timeout=5)

    assert result == (expected if method == "finish_task" else [expected])
    assert len(scripted_openai.requests) == 1


@pytest.mark.parametrize("method", ["finish_task", "finish_tasks", "finish"])
@pytest.mark.parametrize("missing", ["provider", "model"])
def test_finish_methods_reject_incomplete_configuration(method, missing):
    agent = aw.Agent()
    if missing == "provider":
        agent.model("mock")
    else:
        agent.provider(aw.Anthropic("test-key"))
    args = () if method == "finish" else ("missing",)
    with pytest.raises(RuntimeError, match=f"{missing} not set"):
        getattr(agent, method)(*args)


async def test_finish_methods_select_across_the_shared_werk(offline_agent, werk):
    offline_agent.label("scan")
    werk.add_agent(offline_agent)
    scan = offline_agent.add_task(aw.Task("scan", label="scan"))
    report = werk.add_task(aw.Task("report", label="report"))
    werk.set_task_finished(report, {"pages": 2})
    werk.set_task_finished(scan, {"verdict": "clean"})

    assert await offline_agent.finish_task("ORDER BY task.id DESC") == {"pages": 2}
    assert await offline_agent.finish_tasks(aw.Query("report")) == [{"pages": 2}]
    assert await offline_agent.finish_tasks(lambda task: task.get_id() == report) == [
        {"pages": 2}
    ]
    assert await offline_agent.finish() == [{"verdict": "clean"}, {"pages": 2}]


async def test_finish_methods_return_no_results_for_failed_tasks(offline_agent, werk):
    werk.add_agent(offline_agent)
    task = offline_agent.add_task("failed")
    werk.set_task_failed(task)

    assert await offline_agent.finish_task(task) is None
    assert await offline_agent.finish_task("missing") is None
    assert await offline_agent.finish_tasks(task) == []
    assert await offline_agent.finish() == []
