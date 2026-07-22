"""Smoke tests for the agentwerk Python bindings.

The offline tests need no provider: they check the module surface, the fluent
builder, and the error paths. The live test is skipped unless a provider is
configured in the environment (mirrors the Rust integration tests reading .env).
"""

import os

import pytest

import agentwerk as aw


def test_exports_present():
    for name in ("Agent", "BuiltAgent", "TicketSystem", "Tool", "tool"):
        assert hasattr(aw, name), name


def test_build_without_provider_raises():
    builder = aw.Agent().role("tester").tool(aw.ReadFileTool())
    with pytest.raises(RuntimeError):
        builder.build()


def test_tool_rejects_non_tool():
    with pytest.raises(TypeError):
        aw.Agent().tool("not a tool")


def test_fluent_chaining_returns_builder():
    builder = aw.Agent().role("r").label("x").labels(["y", "z"]).dir(".")
    assert isinstance(builder, aw.Agent)


def test_tool_decorator_attaches_metadata():
    @aw.tool(read_only=True)
    def sample(path: str) -> str:
        """Describe the sample."""
        return path

    assert sample._agentwerk_tool is True
    assert sample._agentwerk_name == "sample"
    assert sample._agentwerk_description == "Describe the sample."
    assert sample._agentwerk_read_only is True


def test_ticket_builder_and_schema():
    schema = aw.Schema({"type": "object", "properties": {"n": {"type": "integer"}}})
    ticket = aw.Ticket({"ask": "x"}).label("scan").labels(["b"]).schema(schema)
    assert isinstance(ticket, aw.Ticket)


def test_schema_rejects_invalid_document():
    with pytest.raises(Exception):
        aw.Schema({"type": "not-a-real-type"})


def test_ticket_system_chaining_and_queries():
    sys = aw.TicketSystem().max_turns(5).max_time(30.0).dir(".")
    assert isinstance(sys, aw.TicketSystem)
    assert sys.tickets() == []
    assert sys.last_result() is None


def test_ticket_queries_filter_by_predicate():
    sys = aw.TicketSystem()
    sys.ticket(aw.Ticket("alpha").label("a"))
    sys.ticket(aw.Ticket("beta").label("b"))
    assert len(sys.tickets()) == 2
    a_only = sys.find_tickets(lambda t: "a" in t["labels"])
    assert [t["task"] for t in a_only] == ["alpha"]
    todo = sys.find_ticket(lambda t: t["status"] == "Todo")
    assert todo is not None


def _has_provider() -> bool:
    return any(
        os.environ.get(key)
        for key in ("ANTHROPIC_API_KEY", "OPENAI_API_KEY", "MISTRAL_API_KEY", "LITELLM_API_KEY")
    )


@pytest.mark.skipif(not _has_provider(), reason="no LLM provider configured")
@pytest.mark.asyncio
async def test_live_single_task():
    agent = aw.Agent().from_env().role("You answer in one short word.").build()
    agent.task("Reply with exactly the word: pong")
    work = await agent.finish()
    assert work.last_result() is not None
    assert work.finish_reason() == "Drained"
