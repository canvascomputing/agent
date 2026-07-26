"""Tickets, schemas, and ticket-system state, exercised through the public API."""

import asyncio

import pytest

import agentwerk as aw


def test_enqueued_ticket_appears_with_its_status_and_labels(system):
    assert system.tickets() == []

    system.ticket(aw.Ticket("scan the corpus").label("scan"))

    (ticket,) = system.tickets()
    assert ticket["task"] == "scan the corpus"
    assert ticket["status"] == "Todo"
    assert "scan" in ticket["labels"]


def test_valid_schema_parses():
    schema = aw.Schema({"type": "object", "properties": {"n": {"type": "integer"}}})
    ticket = aw.Ticket("write a report").schema(schema)
    assert isinstance(ticket, aw.Ticket)


def test_invalid_schema_document_is_rejected_with_runtime_error():
    with pytest.raises(RuntimeError):
        aw.Schema({"type": "not-a-real-type"})


def test_policy_setters_chain(system):
    configured = (
        system.max_turns(5)
        .max_time(30.0)
        .max_input_tokens(1000)
        .max_output_tokens(500)
    )
    assert isinstance(configured, aw.TicketSystem)


def test_find_tickets_filters_by_predicate(system):
    system.ticket(aw.Ticket("alpha").label("a"))
    system.ticket(aw.Ticket("beta").label("b"))

    matches = system.find_tickets(lambda t: "a" in t["labels"])
    assert [t["task"] for t in matches] == ["alpha"]


def test_find_ticket_returns_the_first_match(system):
    system.ticket(aw.Ticket("alpha").label("a"))
    system.ticket(aw.Ticket("beta").label("b"))

    found = system.find_ticket(lambda t: t["status"] == "Todo")
    assert found["task"] == "alpha"


def test_get_ticket_returns_none_for_unknown_key(system):
    assert system.get_ticket("TICKET-does-not-exist") is None


def test_cancel_marks_the_system_cancelled(system):
    assert system.is_cancelled() is False
    system.cancel()
    assert system.is_cancelled() is True


def test_finish_reason_is_none_before_a_run(system):
    assert system.finish_reason() is None


def test_stats_reports_zero_counts_before_a_run(system):
    stats = system.stats()
    assert stats["requests"] == 0
    assert stats["tickets_created"] == 0


async def test_cancel_on_accepts_an_awaitable_and_chains(system):
    configured = system.cancel_on(asyncio.sleep(0))
    assert isinstance(configured, aw.TicketSystem)


def test_cancel_label_on_event_chains(system):
    configured = system.cancel_label_on_event("scan", lambda event: True)
    assert isinstance(configured, aw.TicketSystem)


def test_edit_messages_on_event_chains(system):
    configured = system.edit_messages_on_event(lambda events, messages: messages)
    assert isinstance(configured, aw.TicketSystem)
