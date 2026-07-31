"""Tickets, schemas, and ticket-system state, exercised through the public API."""

import asyncio

import pytest

import agentwerk as aw


def test_enqueued_ticket_appears_with_its_status_and_labels(system):
    assert system.tickets() == []

    system.ticket(aw.Ticket("scan the corpus", labels=["scan"]))

    (ticket,) = system.tickets()
    assert ticket.task == "scan the corpus"
    assert ticket.status == "todo"
    assert ticket.has_label("scan")


def test_unstarted_ticket_carries_its_key_and_no_messages(system):
    key = system.ticket(aw.Ticket("scan the corpus", labels=["scan"]))

    ticket = system.get_ticket(key)
    assert ticket.key == key
    assert ticket.result is None
    assert ticket.started_at is None
    assert ticket.replies == []


def test_status_predicates_agree_with_the_status_string(system):
    key = system.ticket(aw.Ticket("scan the corpus"))

    ticket = system.get_ticket(key)
    assert ticket.is_todo()
    assert ticket.is_pending()
    assert not ticket.is_in_progress()
    assert not ticket.is_finished()
    assert not ticket.is_failed()
    assert not ticket.is_resolved()


def test_parent_records_the_handover_trail(system):
    parent = system.ticket(aw.Ticket("survey the corpus"))
    child = system.ticket(aw.Ticket("scan one file", parent=parent))

    assert system.get_ticket(child).parent == parent


def test_valid_schema_parses_and_attaches_to_a_ticket():
    schema = aw.Schema({"type": "object", "properties": {"n": {"type": "integer"}}})
    ticket = aw.Ticket("write a report", schema=schema)
    assert isinstance(ticket.schema, aw.Schema)


def test_schema_validate_returns_the_value_to_keep():
    schema = aw.Schema({"type": "object", "required": ["status"]})
    assert schema.validate({"status": "done"}) == {"status": "done"}


def test_schema_validate_decodes_a_double_encoded_value():
    schema = aw.Schema({"type": "object", "required": ["status"]})
    assert schema.validate('{"status": "done"}') == {"status": "done"}


def test_schema_validate_rejects_a_violating_value():
    schema = aw.Schema({"type": "object", "required": ["status"]})
    with pytest.raises(RuntimeError):
        schema.validate({})


def test_invalid_schema_document_is_rejected_with_runtime_error():
    with pytest.raises(RuntimeError):
        aw.Schema({"type": "not-a-real-type"})


def test_schema_for_label_chains(system):
    configured = system.schema_for_label("scan", aw.Schema({"type": "object"}))
    assert isinstance(configured, aw.TicketSystem)


def test_policy_setters_chain(system):
    configured = (
        system.max_turns(5)
        .max_time(30.0)
        .max_input_tokens(1000)
        .max_output_tokens(500)
        .max_request_tokens(8000)
        .max_schema_retries(3)
        .max_request_retries(2)
        .request_retry_delay(0.25)
    )
    assert isinstance(configured, aw.TicketSystem)


def test_find_tickets_filters_by_predicate(system):
    system.ticket(aw.Ticket("alpha", labels=["a"]))
    system.ticket(aw.Ticket("beta", labels=["b"]))

    matches = system.find_tickets(lambda t: t.has_label("a"))
    assert [t.task for t in matches] == ["alpha"]


def test_find_ticket_returns_the_first_match(system):
    system.ticket(aw.Ticket("alpha", labels=["a"]))
    system.ticket(aw.Ticket("beta", labels=["b"]))

    found = system.find_ticket(lambda t: t.is_todo())
    assert found.task == "alpha"


def test_get_ticket_returns_none_for_unknown_key(system):
    assert system.get_ticket("TICKET-does-not-exist") is None


def test_set_failed_resolves_a_ticket_from_outside_the_run(system):
    key = system.ticket(aw.Ticket("scan the corpus"))

    system.set_failed(key)

    assert system.get_ticket(key).status == "failed"


def test_set_finished_resolves_a_ticket_with_its_result(system):
    key = system.ticket(aw.Ticket("scan the corpus"))

    system.set_finished(key, {"verdict": "clean"})

    assert system.get_ticket(key).status == "finished"
    assert system.last_result() == {"verdict": "clean"}


def test_set_finished_rejects_a_result_that_misses_the_schema(system):
    schema = aw.Schema(
        {
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
        }
    )
    key = system.ticket(aw.Ticket("write a report", schema=schema))

    with pytest.raises(RuntimeError):
        system.set_finished(key, {"body": "no title"})

    assert system.get_ticket(key).status == "todo"


def test_set_failed_rejects_an_unknown_key(system):
    with pytest.raises(RuntimeError):
        system.set_failed("TICKET-does-not-exist")


def test_reply_chains(system):
    key = system.ticket(aw.Ticket("scan the corpus"))
    assert isinstance(system.reply(key, "keep going"), aw.TicketSystem)


def test_results_are_empty_before_a_run(system):
    system.ticket(aw.Ticket("alpha", labels=["a"]))

    assert system.results() == []
    assert system.results_for_label("a") == []
    assert system.last_result() is None


def test_tickets_for_label_returns_every_status_not_just_finished(system):
    system.ticket(aw.Ticket("alpha", labels=["a"]))
    system.ticket(aw.Ticket("beta", labels=["b"]))

    tasks = [ticket.task for ticket in system.tickets_for_label("a")]
    assert tasks == ["alpha"]


def test_cancel_label_on_result_chains(system):
    assert isinstance(
        system.cancel_label_on_result("scan", lambda ticket, result: False), aw.TicketSystem
    )


def test_policy_readers_return_the_limits_that_were_set(system):
    system.max_turns(40).max_time(300.0)

    assert system.get_max_turns() == 40
    assert system.get_max_time() == 300.0
    assert system.get_max_input_tokens() is None
    assert system.get_max_request_retries() == 10


def test_cancel_marks_the_system_cancelled(system):
    assert system.is_cancelled() is False
    system.cancel()
    assert system.is_cancelled() is True


def test_cancel_label_chains(system):
    assert isinstance(system.cancel_label("scan"), aw.TicketSystem)


def test_finish_reason_is_none_before_a_run(system):
    assert system.finish_reason() is None


def test_stats_reports_zero_counts_before_a_run(system):
    stats = system.stats()
    assert stats.requests() == 0
    assert stats.tickets_created() == 0
    assert stats.tool_stats() == {}
    assert stats.run_duration() is None


def test_stats_to_dict_keeps_the_on_disk_shape(system):
    assert system.stats().to_dict()["requests"] == 0


def test_stats_for_label_slices_the_run(system):
    assert system.stats().stats_for_label("scan").requests() == 0


def test_model_for_agent_is_none_when_no_agent_is_bound(system):
    assert system.model_for_agent("scribe") is None


async def test_cancel_on_accepts_an_awaitable_and_chains(system):
    configured = system.cancel_on(asyncio.sleep(0))
    assert isinstance(configured, aw.TicketSystem)


def test_cancel_on_event_and_cancel_on_result_chain(system):
    configured = system.cancel_on_event(lambda event: False).cancel_on_result(
        lambda ticket, result: False
    )
    assert isinstance(configured, aw.TicketSystem)


def test_cancel_label_on_event_chains(system):
    configured = system.cancel_label_on_event("scan", lambda event: True)
    assert isinstance(configured, aw.TicketSystem)


def test_on_result_receives_the_finished_ticket_and_its_result(system):
    seen = []
    system.on_result(lambda ticket, result: seen.append((ticket.key, result)))
    key = system.ticket(aw.Ticket("scan the corpus"))

    system.set_finished(key, {"verdict": "clean"})

    assert seen == [(key, {"verdict": "clean"})]


def test_on_failure_receives_the_failed_ticket(system):
    seen = []
    system.on_failure(lambda event, ticket: seen.append((event.kind, ticket.key)))
    key = system.ticket(aw.Ticket("scan the corpus"))

    system.set_failed(key)

    assert seen == [("ticket_failed", key)]


def test_cancel_on_failure_stops_the_run(system):
    system.cancel_on_failure(lambda event, ticket: True)
    key = system.ticket(aw.Ticket("scan the corpus"))
    assert not system.is_cancelled()

    system.set_failed(key)

    assert system.is_cancelled()


def test_cancel_label_on_failure_calls_off_one_pool_only(system):
    system.cancel_label_on_failure("scan", lambda event, ticket: True)
    key = system.ticket(aw.Ticket("scan the corpus", labels=["scan"]))

    system.set_failed(key)

    assert system.label_cancelled("scan")
    assert not system.is_cancelled()


def test_create_ticket_on_failure_enqueues_a_retry(system):
    system.create_ticket_on_failure(
        lambda event, failed: None
        if failed.parent
        else aw.Ticket(failed.task, parent=failed.key)
    )
    key = system.ticket(aw.Ticket("scan the corpus"))

    system.set_failed(key)

    retry = system.find_ticket(lambda ticket: ticket.parent == key)
    assert retry.task == "scan the corpus"


def test_create_ticket_on_event_enqueues_a_follow_up(system):
    system.create_ticket_on_event(
        lambda event: aw.Ticket("report", labels=["report"])
        if event.kind == "ticket_finished"
        else None
    )
    key = system.ticket(aw.Ticket("scan the corpus"))

    system.set_finished(key, {"verdict": "clean"})

    assert [t.task for t in system.tickets_for_label("report")] == ["report"]


def test_edit_replies_on_event_chains(system):
    configured = system.edit_replies_on_event(lambda events, replies: replies)
    assert isinstance(configured, aw.TicketSystem)


def test_edit_replies_on_an_unstarted_ticket_is_a_no_op(system):
    key = system.ticket(aw.Ticket("scan the corpus"))

    system.edit_replies(key, lambda replies: replies)

    assert system.get_ticket(key).replies == []


def test_edit_replies_drops_a_reply_from_a_non_empty_list(system):
    key = system.task("scan the corpus")
    system.reply(key, "keep me")
    system.reply(key, "drop me")

    system.edit_replies(
        key, lambda replies: [r for r in replies if r.content[0].data["text"] != "drop me"]
    )

    remaining = [r.content[0].data["text"] for r in system.get_ticket(key).replies]
    assert remaining == ["keep me"]


def test_edit_replies_appends_a_reply_built_in_python(system):
    key = system.task("scan the corpus")
    system.reply(key, "first")

    system.edit_replies(key, lambda replies: replies + [aw.Reply.user_text("second")])

    texts = [r.content[0].data["text"] for r in system.get_ticket(key).replies]
    assert texts == ["first", "second"]


def test_edit_replies_raises_when_the_editor_raises(system):
    key = system.task("scan the corpus")
    system.reply(key, "first")

    def editor(replies):
        raise ValueError("no good")

    with pytest.raises(ValueError, match="no good"):
        system.edit_replies(key, editor)


def test_edit_replies_raises_when_the_editor_returns_dicts(system):
    key = system.task("scan the corpus")
    system.reply(key, "first")

    with pytest.raises(RuntimeError, match="list of Reply objects"):
        system.edit_replies(key, lambda replies: [{"author": "user", "content": []}])


async def test_wait_for_ticket_returns_none_when_nothing_matches(system):
    system.cancel()
    assert await system.wait_for_ticket(lambda t: t.is_finished()) is None


def test_load_reopens_a_session_directory(system, tmp_path):
    system.dir(str(tmp_path))
    key = system.ticket(aw.Ticket("scan the corpus", labels=["scan"]))

    reopened = aw.TicketSystem.load(str(tmp_path))

    assert reopened.get_ticket(key).task == "scan the corpus"
