"""Live tests: a small high-signal set that drives a real provider end to end.

All marked ``live`` and skipped automatically without a provider (see conftest).
Prompts are tiny to bound cost.
"""

import pytest

import agentwerk as aw

pytestmark = pytest.mark.live


async def test_runs_a_single_task_to_a_result(live_agent):
    live_agent.task("Reply with exactly the word: pong")
    # The reason is only announced, so the handler goes on before the run ends.
    work = live_agent.start()
    reasons = []
    work.on_event(
        lambda event: reasons.append(event.data["reason"])
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
    agent.task("Read secret.txt and report the exact token it contains.")
    work = agent.start()
    await work.finish_all()
    assert "THE-TOKEN-IS-42" in str(work.results()[-1])


async def test_invokes_a_python_tool_and_records_the_file_it_opened(tmp_path):
    (tmp_path / "note.txt").write_text("THE-TOKEN-IS-42\n")
    calls = []

    @aw.tool(read_only=True, paths=["path"])
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
    agent.task("Read note.txt with the slurp tool and report the token it contains.")
    work = agent.start()
    await work.finish_all()

    assert calls, "the python tool was never invoked"
    assert "note.txt" in work.stats().file_stats()


async def test_runs_two_labeled_agents_with_events_and_chaining():
    queue = aw.TicketQueue().max_turns(30)

    kinds = []
    queue.on_event(lambda event: kinds.append(event.kind))

    queue.agent(
        aw.Agent.from_env().name("A").label("a").role("Reply with one word: alpha").build()
    )
    queue.agent(
        aw.Agent.from_env().name("B").label("b").role("Reply with one word: beta").build()
    )

    def chain(ticket, result):
        if ticket.has_label("a"):
            return aw.Ticket("Reply beta", labels=["b"])
        return None

    queue.create_ticket_on_result(chain)
    queue.ticket(aw.Ticket("Reply alpha", labels=["a"]))
    await queue.finish_all()

    assert len(queue.results()) == 2
    assert "ticket_finished" in kinds


async def test_saves_the_messages_of_a_finished_ticket(tmp_path):
    queue = aw.TicketQueue().max_turns(10)
    queue.agent(
        aw.Agent.from_env().name("scribe").role("Reply with one word: pong").build()
    )

    captured = []

    def capture(event, ticket):
        if event.kind == "ticket_finished":
            model = queue.model_for_agent(event.agent_name)
            trajectory = aw.Trajectory.from_ticket(event.agent_name, model, ticket)
            trajectory.save(str(tmp_path))
            captured.append((len(trajectory.messages), trajectory.model))

    queue.on_ticket(capture)
    key = queue.task("Reply with exactly the word: pong")
    await queue.finish_all()

    written = sorted(p.name for p in (tmp_path / "trajectories").iterdir())
    assert written == [f"scribe-{key}.html", f"scribe-{key}.json"]
    (messages, model), = captured
    assert messages > 0
    assert model


async def test_an_async_compaction_editor_awaits_the_built_in_summarizer(tmp_path):
    # Two turns: the first records the token usage the trigger reads, the
    # second compacts, since a threshold of zero is always crossed. The role
    # forbids tools so the ticket cannot finish on turn one and skip it.
    queue = aw.TicketQueue().max_turns(2).dir(str(tmp_path))
    queue.compact_at(0.0)
    summaries = []

    async def summarize_the_head(compaction, replies):
        summary = await compaction.summarize(replies)
        summaries.append(summary)
        return [aw.Reply.user_text(summary)]

    queue.edit_replies_on_compaction(summarize_the_head)
    queue.agent(
        aw.Agent.from_env()
        .name("worker")
        .role("Answer in plain text. Do not call any tools.")
        .build()
    )
    queue.task("Name one colour and say why you picked it.")
    await queue.finish_all()

    assert summaries, "the editor must have run and awaited the summarizer"
    assert summaries[0].strip()
