"""Prompt rendering stays inside Werk while callers pass strings."""

import asyncio

import pytest

import agentwerk as aw


async def test_shared_template_values_are_inserted_literally(werk, scripted_openai):
    scripted_openai.respond_with_tool("finish", {"answer": "done"})
    werk.set_templates(
        {
            "company": "Acme",
            "brief": "For {{ company }}: {{ result: missing }}",
        }
    )
    werk.add_agent(
        aw.Agent()
        .provider(scripted_openai.provider())
        .model("mock")
        .role("{{ brief }}")
    )
    werk.add_task("{{ brief }} {{ unknown }}")
    await asyncio.wait_for(werk.finish(), timeout=5)
    messages = scripted_openai.requests[0]["messages"]
    assert messages[0]["content"] == "For {{ company }}: {{ result: missing }}"
    assert messages[1]["content"] == "For {{ company }}: {{ result: missing }} {{ unknown }}"


async def test_direct_aql_expressions_resolve_results_and_paths(
    werk, tmp_path, scripted_openai
):
    first = werk.add_task(aw.Task("first", label="research"))
    second = werk.add_task(aw.Task("second", label="research"))
    werk.set_task_finished(first, {"research": "one {{ company }}"})
    werk.set_task_finished(second, {"answer": 42})
    second_path = (tmp_path / "tasks" / second / "result.json").resolve()
    role = f"{{{{ result: research }}}} | {{{{ result_path: {second} }}}}"
    scripted_openai.respond_with_tool("finish", {"answer": "done"})
    werk.add_agent(
        aw.Agent()
        .provider(scripted_openai.provider())
        .model("mock")
        .role(role)
    )
    werk.add_task("{{ results: research ORDER BY task.id DESC }}")
    await asyncio.wait_for(werk.finish(), timeout=5)
    messages = scripted_openai.requests[0]["messages"]
    assert messages[0]["content"] == (
        f'{{"research":"one {{{{ company }}}}"}} | {second_path}'
    )
    assert messages[1]["content"] == (
        '[{"answer":42},{"research":"one {{ company }}"}]'
    )


async def test_result_json_paths_navigate_selected_json(werk, scripted_openai):
    first = werk.add_task(aw.Task("first", label="research"))
    second = werk.add_task(aw.Task("second", label="research"))
    werk.set_task_finished(first, {"company": {"name": "Acme"}})
    werk.set_task_finished(second, {"company": {"name": "Canvas"}})
    scripted_openai.respond_with_tool("finish", {"answer": "done"})
    werk.add_agent(
        aw.Agent()
        .provider(scripted_openai.provider())
        .model("mock")
        .role("{{ result: research | company.name }}")
    )
    werk.add_task("{{ results: research | [*].company.name }}")

    await asyncio.wait_for(werk.finish(), timeout=5)

    messages = scripted_openai.requests[0]["messages"]
    assert messages[0]["content"] == "Acme"
    assert messages[1]["content"] == '["Acme","Canvas"]'


async def test_nested_query_values_and_readable_results(
    werk, scripted_openai
):
    first = werk.add_task(aw.Task("first", label="research"))
    second = werk.add_task(aw.Task("second", label="research"))
    werk.set_task_finished(first, {"title": "Market", "empty": None})
    werk.set_task_finished(second, ["one", None, "two"])
    werk.set_templates({"selection": "task.label = research"})
    scripted_openai.respond_with_tool("finish", {"answer": "done"})
    werk.add_agent(
        aw.Agent()
        .provider(scripted_openai.provider())
        .model("mock")
        .role("{{ readable(results: {{ selection }}) }}")
    )
    werk.add_task("go")
    await asyncio.wait_for(werk.finish(), timeout=5)
    assert scripted_openai.requests[0]["messages"][0]["content"] == (
        "- title: Market\n-\n  - one\n  - two"
    )


async def test_task_prompts_stay_fixed_after_the_first_request(werk, scripted_openai):
    scripted_openai.respond_with_tool("step", {})
    scripted_openai.respond_with_tool("finish", {"answer": "done"})

    @aw.tool
    def step():
        """Advance to the next request."""
        return "continue"

    agent = (
        aw.Agent()
        .provider(scripted_openai.provider())
        .model("mock")
        .role("{{ company }}")
    )
    werk.add_agent(agent.tool(step))
    task = werk.add_task("Write for {{ company }}")
    werk.set_template("company", "Old")

    def update(current, event):
        if event.get_name() == aw.Event.REQUEST_FINISHED:
            current.set_template("company", "New")

    werk.on_event(update)
    await asyncio.wait_for(werk.finish(), timeout=5)
    first, second = [request["messages"] for request in scripted_openai.requests]
    assert first[0]["content"] == "Old"
    assert second[0]["content"] == "Old"
    assert first[1] == second[1]
    assert first[1]["content"] == "Write for Old"
    assert werk.get_task(task).get_task() == "Write for {{ company }}"


async def test_prompt_render_failure_reaches_hooks_without_a_provider_request(
    werk, scripted_openai
):
    werk.add_agent(
        aw.Agent()
        .provider(scripted_openai.provider())
        .model("mock")
        .role("{{ result: missing }}")
    )
    task = werk.add_task("go")
    failures = []
    werk.on_failure(lambda _werk, event, _task: failures.append(event.get_name()))
    await asyncio.wait_for(werk.finish(), timeout=5)
    assert scripted_openai.requests == []
    assert failures == [aw.Event.PROMPT_RENDER_FAILED, aw.Event.TASK_FAILED]
    assert werk.get_task(task).get_errors()[0].get_data()["expression"] == "result: missing"


async def test_template_cycles_are_inserted_literally(werk, scripted_openai):
    werk.set_templates({"a": "{{ b }}", "b": "{{ a }}"})
    scripted_openai.respond_with_tool("finish", {"answer": "done"})
    werk.add_agent(
        aw.Agent().provider(scripted_openai.provider()).model("mock").role("{{ a }}")
    )
    task = werk.add_task("go")
    await asyncio.wait_for(werk.finish(), timeout=5)
    assert scripted_openai.requests[0]["messages"][0]["content"] == "{{ b }}"
    assert werk.get_task(task).is_finished()


async def test_reload_uses_only_templates_restored_by_the_caller(
    werk, tmp_path, scripted_openai
):
    werk.set_template("company", "Old")
    werk.add_task("{{ company }}")
    loaded = aw.Werk.load(str(tmp_path))
    loaded.set_template("company", "New")
    loaded.add_agent(aw.Agent().provider(scripted_openai.provider()).model("mock"))
    scripted_openai.respond_with_tool("finish", {"answer": "done"})
    await asyncio.wait_for(loaded.finish(), timeout=5)
    user = [
        message
        for message in scripted_openai.requests[0]["messages"]
        if message["role"] == "user"
    ]
    assert user == [{"role": "user", "content": "New"}]


async def test_bulk_string_conversion_failure_is_atomic(
    werk, scripted_openai, tmp_path
):
    agent = (
        aw.Agent()
        .provider(scripted_openai.provider())
        .model("mock")
        .role("{{ company }}")
    )
    werk.add_agent(agent)
    werk.set_template("company", "Old")
    for update in [werk.set_templates, agent.templates]:
        with pytest.raises(TypeError):
            update({"company": "New", "missing": tmp_path / "absent.md"})
    scripted_openai.respond_with_tool("finish", {"answer": "done"})
    werk.add_task("go")
    await asyncio.wait_for(werk.finish(), timeout=5)
    assert scripted_openai.requests[0]["messages"][0]["content"] == "Old"


def test_prompt_api_and_path_inputs_are_not_public(werk, tmp_path):
    path = tmp_path / "prompt.md"
    path.write_text("text")
    assert not hasattr(aw, "Prompt")
    assert not hasattr(aw.Werk, "render_prompt")
    for call in [
        lambda: aw.Agent().role(path),
        lambda: aw.Agent().template("name", path),
        lambda: werk.set_template("name", path),
        lambda: aw.Task(path),
        lambda: werk.add_task(path),
        lambda: aw.CommandTool("git").description(path),
    ]:
        with pytest.raises((TypeError, ValueError)):
            call()
