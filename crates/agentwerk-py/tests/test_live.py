"""Live tests: a small high-signal set that drives a real provider end to end.

All marked ``live`` and skipped automatically without a provider (see conftest).
Prompts are tiny to bound cost.
"""

import asyncio

import pytest

import agentwerk as aw

pytestmark = pytest.mark.live


async def test_runs_a_single_task_to_a_result(live_agent):
    live_agent.ticket("Reply with exactly the word: pong")
    # The reason is only announced, so the handler goes on before the run ends.
    work = live_agent.start()
    reasons = []
    work.on_event(
        lambda _, event: reasons.append(event.data["reason"])
        if event.kind == "run_finished"
        else None
    )
    await work.finish_all()
    assert work.results()
    assert reasons == ["drained"]


async def test_invokes_a_builtin_tool(tmp_path):
    (tmp_path / "secret.txt").write_text("THE-TOKEN-IS-42\n")
    agent = (
        aw.Agent.from_env()
        .role("You read files to answer. Use the read_file tool.")
        .dir(str(tmp_path))
        .tool(aw.ReadFileTool())
        .build()
    )
    agent.ticket("Read secret.txt and report the exact token it contains.")
    work = agent.start()
    assert "THE-TOKEN-IS-42" in str(await work.finish_last())


async def test_invokes_a_python_tool_and_records_the_file_it_opened(tmp_path):
    (tmp_path / "note.txt").write_text("THE-TOKEN-IS-42\n")
    calls = []

    @aw.tool(concurrent=True, paths=["path"])
    def slurp(path: str) -> str:
        """Return the contents of the file at `path`."""
        calls.append(path)
        return (tmp_path / path).read_text()

    agent = (
        aw.Agent.from_env()
        .role("Call the slurp tool on the given file, then finish.")
        .tool(slurp)
        .build()
    )
    agent.ticket("Read note.txt with the slurp tool and report the token it contains.")
    work = agent.start()
    opened = []
    work.on_event(
        lambda _, event: opened.append(event.data["path"])
        if event.kind == aw.EventName.FILE_OPEN_FINISHED
        else None
    )
    await work.finish_all()

    assert calls, "the python tool was never invoked"
    assert any("note.txt" in path for path in opened)


async def test_runs_two_labeled_agents_with_events_and_chaining():
    queue = aw.TicketQueue().config(aw.Config(max_turns=30))

    kinds = []
    queue.on_event(lambda _, event: kinds.append(event.kind))

    queue.agent(
        aw.Agent.from_env().label("a").role("Reply with one word: alpha").build()
    )
    queue.agent(
        aw.Agent.from_env().label("b").role("Reply with one word: beta").build()
    )

    def chain(work, ticket, result):
        if ticket.has_label("a"):
            work.ticket(aw.Ticket("Reply beta", label="b"))

    queue.on_result(chain)
    queue.ticket(aw.Ticket("Reply alpha", label="a"))
    await queue.finish_all()

    assert len(queue.results()) == 2
    assert "ticket_finished" in kinds


async def test_saves_the_messages_of_a_finished_ticket(tmp_path):
    queue = aw.TicketQueue().config(aw.Config(max_turns=10))
    queue.agent(
        aw.Agent.from_env().role("Reply with one word: pong").build()
    )

    captured = []

    def capture(_, event, ticket):
        if event.kind == "ticket_finished":
            model = queue.model_for_agent(event.agent_id)
            trajectory = aw.Trajectory.from_ticket(event.agent_id, model, ticket)
            trajectory.save(str(tmp_path))
            captured.append((event.agent_id, len(trajectory.replies), trajectory.model))

    queue.on_ticket(capture)
    key = queue.ticket("Reply with exactly the word: pong")
    await queue.finish_all()

    (agent_id, replies, model), = captured
    written = sorted(p.name for p in (tmp_path / "trajectories").iterdir())
    assert written == [f"{agent_id}-{key}.html", f"{agent_id}-{key}.json"]
    assert replies > 0
    assert model


async def test_compaction_summarizes_the_replies_against_the_live_model(tmp_path):
    # An interactive agent carries no finish tool, so the ticket cannot end on
    # turn one and skip compaction. The follow-up reply drives the request that
    # a threshold of zero compacts before.
    task = "Name one colour and say why you picked it."
    queue = aw.TicketQueue().dir(str(tmp_path))
    queue.config(aw.Config(compaction_threshold=0.0))
    kinds = []
    queue.on_event(
        lambda _, event: kinds.append(event.kind)
        if event.kind.startswith("compaction_")
        else None
    )
    queue.agent(
        aw.Agent.from_env().role("Answer in plain text.").interactive().build()
    )
    queue.start()
    key = queue.ticket(task)
    await _until(lambda: _answered(queue, key))
    queue.reply(key, "Now name a second colour.")
    await _until(lambda: "compaction_finished" in kinds)
    queue.cancel_all()
    await queue.finish_all()

    assert "compaction_failed" not in kinds
    texts = [b.data.get("text", "") for r in queue.get_ticket(key).replies for b in r.content]
    assert task not in texts, "the summary must have replaced the task reply"
    assert any(text.strip() for text in texts), "the summary must carry text"


def _answered(queue, key):
    replies = queue.get_ticket(key).replies
    return bool(replies) and replies[-1].author == "assistant"


async def _until(condition, timeout=120.0):
    """Wait for `condition`, which a live turn takes seconds to satisfy."""
    for _ in range(int(timeout / 0.5)):
        if condition():
            return
        await asyncio.sleep(0.5)
    raise AssertionError("the run did not reach the awaited state")
