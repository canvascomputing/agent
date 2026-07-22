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
    assert work.finish_reason() == "Drained"


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


async def test_invokes_a_python_tool():
    calls = []

    @aw.tool(read_only=True)
    def echo(text: str) -> str:
        """Return the text unchanged."""
        calls.append(text)
        return text

    agent = (
        aw.Agent()
        .from_env()
        .role("Call the echo tool with the word 'ping', then finish.")
        .tool(echo)
        .build()
    )
    agent.task("Echo the word ping using the tool.")
    await agent.finish()
    assert calls, "the python tool was never invoked"


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
        if "a" in ticket["labels"]:
            return aw.Ticket("Reply beta").label("b")
        return None

    system.create_ticket_on_result(chain)
    system.ticket(aw.Ticket("Reply alpha").label("a"))
    await system.finish()

    assert len(system.results()) == 2
    assert "ticket_finished" in kinds
