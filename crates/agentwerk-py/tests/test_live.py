"""Live tests: a small high-signal set that drives a real provider end to end.

All marked ``live`` and skipped automatically without a provider (see conftest).
Prompts are tiny to bound cost.
"""

import pytest

import agentwerk as aw

pytestmark = pytest.mark.live


async def test_runs_a_single_task_to_a_result(live_agent):
    live_agent.task("Reply with exactly the word: pong")
    work = await live_agent.finish()
    assert work.last_result() is not None
    assert work.finish_reason() == "drained"


async def test_invokes_a_builtin_tool(tmp_path):
    (tmp_path / "secret.txt").write_text("THE-TOKEN-IS-42\n")
    agent = (
        aw.Agent()
        .from_env()
        .role("You read files to answer. Use the read_file tool.")
        .dir(str(tmp_path))
        .tool(aw.ReadFileTool())
        .build()
    )
    agent.task("Read secret.txt and report the exact token it contains.")
    work = await agent.finish()
    assert "THE-TOKEN-IS-42" in str(work.last_result())


async def test_invokes_a_python_tool_and_records_the_file_it_opened(tmp_path):
    (tmp_path / "note.txt").write_text("THE-TOKEN-IS-42\n")
    calls = []

    @aw.tool(read_only=True, paths=["path"])
    def slurp(path: str) -> str:
        """Return the contents of the file at `path`."""
        calls.append(path)
        return (tmp_path / path).read_text()

    agent = (
        aw.Agent()
        .from_env()
        .role("Call the slurp tool on the given file, then finish.")
        .tool(slurp)
        .build()
    )
    agent.task("Read note.txt with the slurp tool and report the token it contains.")
    work = await agent.finish()

    assert calls, "the python tool was never invoked"
    assert "note.txt" in work.stats().file_stats()


async def test_runs_two_labeled_agents_with_events_and_chaining():
    system = aw.TicketSystem().max_turns(30)

    kinds = []
    system.on_event(lambda event: kinds.append(event.kind))

    system.agent(
        aw.Agent().name("A").label("a").from_env().role("Reply with one word: alpha").build()
    )
    system.agent(
        aw.Agent().name("B").label("b").from_env().role("Reply with one word: beta").build()
    )

    def chain(ticket):
        if ticket.has_label("a"):
            return aw.Ticket("Reply beta", labels=["b"])
        return None

    system.create_ticket_on_result(chain)
    system.ticket(aw.Ticket("Reply alpha", labels=["a"]))
    await system.finish()

    assert len(system.results()) == 2
    assert "ticket_finished" in kinds


async def test_saves_the_messages_of_a_finished_ticket(tmp_path):
    system = aw.TicketSystem().max_turns(10)
    system.agent(
        aw.Agent().name("scribe").from_env().role("Reply with one word: pong").build()
    )

    captured = []

    def capture(event, ticket):
        if event.kind == "ticket_finished":
            model = system.model_for_agent(event.agent_name)
            trajectory = aw.Trajectory.from_ticket(event.agent_name, model, ticket)
            trajectory.save(str(tmp_path))
            captured.append((len(trajectory.messages), trajectory.model))

    system.on_ticket(capture)
    key = system.task("Reply with exactly the word: pong")
    await system.finish()

    written = sorted(p.name for p in (tmp_path / "trajectories").iterdir())
    assert written == [f"scribe-{key}.html", f"scribe-{key}.json"]
    (messages, model), = captured
    assert messages > 0
    assert model
