"""Test core behavior end to end against a real provider.

Every test is marked ``live`` and skipped when no provider is configured. Short prompts limit cost.
"""

import asyncio

import pytest

import agentwerk as aw

pytestmark = pytest.mark.live


async def test_runs_a_single_task_to_a_result(live_agent):
    live_agent.add_task("Reply with exactly the word: pong")
    # The reason is only announced, so the handler goes on before the run ends.
    werk = live_agent.start()
    reasons = []
    werk.on_event(
        lambda _, event: reasons.append(event.get_data()["outcome"])
        if event.get_name() == "run_finished"
        else None
    )
    await werk.finish()
    assert werk.get_results()
    assert reasons == ["drained"]


async def test_invokes_a_builtin_tool(tmp_path):
    (tmp_path / "secret.txt").write_text("THE-TOKEN-IS-42\n")
    agent = (
        aw.Agent.from_env()
        .role("You read files to answer. Use the read_file tool.")
        .dir(str(tmp_path))
        .tool(aw.ReadFileTool())
    )
    agent.add_task("Read secret.txt and report the exact token it contains.")
    werk = agent.start()
    assert "THE-TOKEN-IS-42" in str(
        await werk.finish_task("ORDER BY task.created DESC")
    )


async def test_invokes_a_python_tool(tmp_path):
    (tmp_path / "note.txt").write_text("THE-TOKEN-IS-42\n")
    calls = []

    @aw.tool(concurrent=True)
    def slurp(path: str) -> str:
        """Return the contents of the file at `path`."""
        calls.append(path)
        return (tmp_path / path).read_text()

    agent = (
        aw.Agent.from_env()
        .role("Call the slurp tool on the given file, then finish.")
        .tool(slurp)
    )
    agent.add_task("Read note.txt with the slurp tool and report the token it contains.")
    werk = agent.start()
    await werk.finish()

    assert calls, "the python tool was never invoked"


async def test_runs_two_labeled_agents_with_events_and_chaining():
    werk = aw.Werk().set_policy(aw.Policy(max_turns=30))

    kinds = []
    werk.on_event(lambda _, event: kinds.append(event.get_name()))

    werk.add_agent(
        aw.Agent.from_env().label("a").role("Reply with one word: alpha")
    )
    werk.add_agent(
        aw.Agent.from_env().label("b").role("Reply with one word: beta")
    )

    def chain(callback_werk, task, result):
        if task.get_label() == "a":
            callback_werk.add_task(aw.Task("Reply beta", label="b"))

    werk.on_result(chain)
    werk.add_task(aw.Task("Reply alpha", label="a"))
    await werk.finish()

    assert len(werk.get_results()) == 2
    assert "task_finished" in kinds


async def test_saves_the_messages_of_a_finished_task(tmp_path):
    werk = aw.Werk().set_policy(aw.Policy(max_turns=10))
    werk.add_agent(
        aw.Agent.from_env().role("Reply with one word: pong")
    )

    captured = []

    def capture(_, event, task):
        if event.get_name() == "task_finished":
            model = werk.get_model_for_agent(event.get_agent_id())
            trajectory = aw.Trajectory.from_task(event.get_agent_id(), model, task)
            trajectory.save(str(tmp_path))
            captured.append((event.get_agent_id(), len(trajectory.get_replies()), trajectory.get_model()))

    werk.on_task(capture)
    id = werk.add_task("Reply with exactly the word: pong")
    await werk.finish()

    (agent_id, replies, model), = captured
    written = sorted(p.name for p in (tmp_path / "trajectories").iterdir())
    assert written == [f"{agent_id}-{id}.html", f"{agent_id}-{id}.json"]
    assert replies > 0
    assert model


async def test_compaction_summarizes_the_replies_against_the_live_model(tmp_path):
    # An interactive agent carries no finish tool, so the task cannot end on
    # turn one and skip compaction. The follow-up reply starts the request that
    # a threshold of zero compacts before.
    task = "Name one colour and say why you picked it."
    werk = aw.Werk().set_dir(str(tmp_path))
    werk.set_policy(aw.Policy(compaction_threshold=0.0))
    kinds = []
    werk.on_event(
        lambda _, event: kinds.append(event.get_name())
        if event.get_name().startswith("compaction_")
        else None
    )
    werk.add_agent(
        aw.Agent.from_env().role("Answer in plain text.").interactive()
    )
    werk.start()
    id = werk.add_task(task)
    await _until(lambda: _answered(werk, id))
    werk.add_reply(id, "Now name a second colour.")
    await _until(lambda: "compaction_finished" in kinds)
    werk.cancel_all_tasks()
    await werk.finish()

    assert "compaction_failed" not in kinds
    texts = [b.get_data().get("text", "") for r in werk.get_task(id).get_replies() for b in r.get_content()]
    assert task not in texts, "the summary must have replaced the task reply"
    assert any(text.strip() for text in texts), "the summary must carry text"


def _answered(werk, id):
    replies = werk.get_task(id).get_replies()
    return bool(replies) and replies[-1].get_author() == "assistant"


async def _until(condition, timeout=120.0):
    """Wait for `condition`, which a live turn takes seconds to satisfy."""
    for _ in range(int(timeout / 0.5)):
        if condition():
            return
        await asyncio.sleep(0.5)
    raise AssertionError("the run did not reach the awaited state")
