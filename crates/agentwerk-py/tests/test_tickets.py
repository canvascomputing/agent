"""Tickets, schemas, and ticket-queue state, exercised through the public API."""

import asyncio
import sqlite3
from collections import Counter

import pytest

import agentwerk as aw


def test_enqueued_ticket_appears_with_its_status_and_label(queue):
    assert queue.tickets() == []

    queue.ticket(aw.Ticket("scan the corpus", label="scan"))

    (ticket,) = queue.tickets()
    assert ticket.task == "scan the corpus"
    assert ticket.status == "todo"
    assert ticket.has_label("scan")


def test_unstarted_ticket_carries_its_key_and_no_messages(queue):
    key = queue.ticket(aw.Ticket("scan the corpus", label="scan"))

    ticket = queue.get_ticket(key)
    assert ticket.key == key
    assert ticket.result is None
    assert ticket.started_at is None
    assert ticket.replies == []


def test_status_predicates_agree_with_the_status_string(queue):
    key = queue.ticket(aw.Ticket("scan the corpus"))

    ticket = queue.get_ticket(key)
    assert ticket.is_todo()
    assert ticket.is_pending()
    assert not ticket.is_in_progress()
    assert not ticket.is_finished()
    assert not ticket.is_failed()


def test_parent_records_the_handover_trail(queue):
    parent = queue.ticket(aw.Ticket("survey the corpus"))
    child = queue.ticket(aw.Ticket("scan one file", parent=parent))

    assert queue.get_ticket(child).parent == parent


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


def test_policy_setters_chain(queue):
    configured = (
        queue.max_turns(5)
        .max_time(30.0)
        .max_input_tokens(1000)
        .max_output_tokens(500)
        .max_request_tokens(8000)
        .max_schema_retries(3)
        .max_request_retries(2)
        .request_retry_delay(0.25)
    )
    assert isinstance(configured, aw.TicketQueue)


def test_find_tickets_filters_by_predicate(queue):
    queue.ticket(aw.Ticket("alpha", label="a"))
    queue.ticket(aw.Ticket("beta", label="b"))

    matches = queue.find_tickets(lambda t: t.has_label("a"))
    assert [t.task for t in matches] == ["alpha"]


def test_find_ticket_returns_the_first_match(queue):
    queue.ticket(aw.Ticket("alpha", label="a"))
    queue.ticket(aw.Ticket("beta", label="b"))

    found = queue.find_ticket(lambda t: t.is_todo())
    assert found.task == "alpha"


def test_get_ticket_returns_none_for_unknown_key(queue):
    assert queue.get_ticket("TICKET-does-not-exist") is None


def test_set_failed_resolves_a_ticket_from_outside_the_run(queue):
    key = queue.ticket(aw.Ticket("scan the corpus"))

    queue.set_failed(key)

    assert queue.get_ticket(key).status == "failed"


def test_set_finished_resolves_a_ticket_with_its_result(queue):
    key = queue.ticket(aw.Ticket("scan the corpus"))

    queue.set_finished(key, {"verdict": "clean"})

    assert queue.get_ticket(key).status == "finished"
    assert queue.results()[-1] == {"verdict": "clean"}


def test_set_finished_rejects_a_result_that_misses_the_schema(queue):
    schema = aw.Schema(
        {
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
        }
    )
    key = queue.ticket(aw.Ticket("write a report", schema=schema))

    with pytest.raises(RuntimeError):
        queue.set_finished(key, {"body": "no title"})

    assert queue.get_ticket(key).status == "todo"


def test_set_failed_rejects_an_unknown_key(queue):
    with pytest.raises(RuntimeError):
        queue.set_failed("TICKET-does-not-exist")


def test_reply_chains(queue):
    key = queue.ticket(aw.Ticket("scan the corpus"))
    assert isinstance(queue.reply(key, "keep going"), aw.TicketQueue)


def test_results_are_empty_before_a_run(queue):
    queue.ticket(aw.Ticket("alpha", label="a"))

    assert queue.results() == []


def test_find_tickets_returns_every_status_not_just_finished(queue):
    queue.ticket(aw.Ticket("alpha", label="a"))
    queue.ticket(aw.Ticket("beta", label="b"))

    tasks = [ticket.task for ticket in queue.find_tickets(lambda t: t.has_label("a"))]
    assert tasks == ["alpha"]


def test_policy_readers_return_the_limits_that_were_set(queue):
    queue.max_turns(40).max_time(300.0)

    assert queue.get_max_turns() == 40
    assert queue.get_max_time() == 300.0
    assert queue.get_max_input_tokens() is None
    assert queue.get_max_request_retries() == 10


def test_cancel_takes_the_matching_tickets_off_the_queue(queue):
    scan = aw.Ticket("scan the corpus", label="scan")
    report = aw.Ticket("write it up", label="report")
    assert queue.is_cancelled(scan) is False

    assert isinstance(queue.cancel(lambda t: t.has_label("scan")), aw.TicketQueue)

    assert queue.is_cancelled(scan) is True
    assert queue.is_cancelled(report) is False


def test_stats_reports_zero_counts_before_a_run(queue):
    stats = queue.stats()
    assert stats.event_count(aw.EventName.REQUEST_FINISHED) == 0
    assert stats.event_count(aw.EventName.TICKET_CREATED) == 0
    assert stats.execution_duration() is None


def test_event_count_rejects_a_name_no_event_carries(queue):
    with pytest.raises(RuntimeError):
        queue.stats().event_count("request_finishd")


def test_event_name_spells_the_kind_an_event_reports(queue):
    seen = []
    queue.on_event(lambda event: seen.append(event.kind))

    queue.task("seed")

    assert aw.EventName.TICKET_CREATED in seen
    assert queue.stats().event_count(aw.EventName.TICKET_CREATED) == 1


def test_stats_to_dict_keeps_the_on_disk_shape(queue):
    assert queue.stats().to_dict()["input_tokens"] == 0


def test_an_event_carries_the_label_of_the_ticket_it_concerns(queue):
    created = Counter()

    def count_per_label(event):
        if event.kind == aw.EventName.TICKET_CREATED:
            created[event.label] += 1

    queue.on_event(count_per_label)

    queue.ticket(aw.Ticket("scan the tree", label="scan"))
    queue.ticket(aw.Ticket("scan the lockfile", label="scan"))
    queue.ticket(aw.Ticket("write the report", label="report"))

    assert created == Counter({"scan": 2, "report": 1})


def test_model_for_agent_is_none_when_no_agent_is_bound(queue):
    assert queue.model_for_agent("scribe") is None


def test_on_result_receives_the_finished_ticket_and_its_result(queue):
    seen = []
    queue.on_result(lambda ticket, result: seen.append((ticket.key, result)))
    key = queue.ticket(aw.Ticket("scan the corpus"))

    queue.set_finished(key, {"verdict": "clean"})

    assert seen == [(key, {"verdict": "clean"})]


def test_on_results_hands_over_every_result_so_far(queue):
    seen = []
    queue.on_results(lambda results: seen.append(results))
    first = queue.ticket(aw.Ticket("scan a.py"))
    second = queue.ticket(aw.Ticket("scan b.py"))

    queue.set_finished(first, "clean")
    queue.set_finished(second, "malicious")

    assert seen == [["clean"], ["clean", "malicious"]]


def test_create_tickets_on_results_waits_until_the_results_call_for_the_work(queue):
    queue.create_tickets_on_results(
        lambda results: [aw.Ticket(r, label="review") for r in results]
        if len(results) == 2
        else None
    )
    first = queue.ticket(aw.Ticket("scan a.py"))
    second = queue.ticket(aw.Ticket("scan b.py"))

    queue.set_finished(first, "clean")
    assert queue.find_tickets(lambda t: t.label == "review") == []

    queue.set_finished(second, "malicious")
    filed = [t.task for t in queue.find_tickets(lambda t: t.label == "review")]
    assert filed == ["clean", "malicious"]


def test_on_failure_receives_the_failed_ticket(queue):
    seen = []
    queue.on_failure(lambda event, ticket: seen.append((event.kind, ticket.key)))
    key = queue.ticket(aw.Ticket("scan the corpus"))

    queue.set_failed(key)

    assert seen == [("ticket_failed", key)]


def test_create_ticket_on_failure_enqueues_a_retry(queue):
    queue.create_ticket_on_failure(
        lambda event, failed: None
        if failed.parent
        else aw.Ticket(failed.task, parent=failed.key)
    )
    key = queue.ticket(aw.Ticket("scan the corpus"))

    queue.set_failed(key)

    retry = queue.find_ticket(lambda ticket: ticket.parent == key)
    assert retry.task == "scan the corpus"


def test_create_ticket_on_event_enqueues_a_follow_up(queue):
    queue.create_ticket_on_event(
        lambda event: aw.Ticket("report", label="report")
        if event.kind == "ticket_finished"
        else None
    )
    key = queue.ticket(aw.Ticket("scan the corpus"))

    queue.set_finished(key, {"verdict": "clean"})

    filed = queue.find_tickets(lambda t: t.has_label("report"))
    assert [t.task for t in filed] == ["report"]


def test_edit_replies_on_event_chains(queue):
    configured = queue.edit_replies_on_event(lambda events, replies: replies)
    assert isinstance(configured, aw.TicketQueue)


def test_edit_directive_on_retry_chains(queue):
    configured = queue.edit_directive_on_retry(lambda event, directive: "replacement")
    assert isinstance(configured, aw.TicketQueue)


def test_edit_replies_on_compaction_chains(queue):
    async def keep_the_tail(compaction, replies):
        return replies[-2:]

    configured = queue.edit_replies_on_compaction(keep_the_tail)
    assert isinstance(configured, aw.TicketQueue)


def test_compact_at_round_trips_through_get_compact_at(queue):
    assert queue.get_compact_at() is None

    queue.compact_at(0.8)

    assert queue.get_compact_at() == 0.8


def test_compact_at_clamps_a_fraction_above_one(queue):
    queue.compact_at(1.5)

    assert queue.get_compact_at() == 1.0


def test_edit_replies_on_an_unstarted_ticket_is_a_no_op(queue):
    key = queue.ticket(aw.Ticket("scan the corpus"))

    queue.edit_replies(key, lambda replies: replies)

    assert queue.get_ticket(key).replies == []


def test_edit_replies_drops_a_reply_from_a_non_empty_list(queue):
    key = queue.task("scan the corpus")
    queue.reply(key, "keep me")
    queue.reply(key, "drop me")

    queue.edit_replies(
        key, lambda replies: [r for r in replies if r.content[0].data["text"] != "drop me"]
    )

    remaining = [r.content[0].data["text"] for r in queue.get_ticket(key).replies]
    assert remaining == ["keep me"]


def test_edit_replies_appends_a_reply_built_in_python(queue):
    key = queue.task("scan the corpus")
    queue.reply(key, "first")

    queue.edit_replies(key, lambda replies: replies + [aw.Reply.user_text("second")])

    texts = [r.content[0].data["text"] for r in queue.get_ticket(key).replies]
    assert texts == ["first", "second"]


def test_edit_replies_raises_when_the_editor_raises(queue):
    key = queue.task("scan the corpus")
    queue.reply(key, "first")

    def editor(replies):
        raise ValueError("no good")

    with pytest.raises(ValueError, match="no good"):
        queue.edit_replies(key, editor)


def test_edit_replies_raises_when_the_editor_returns_dicts(queue):
    key = queue.task("scan the corpus")
    queue.reply(key, "first")

    with pytest.raises(RuntimeError, match="list of Reply objects"):
        queue.edit_replies(key, lambda replies: [{"author": "user", "content": []}])


async def test_run_finished_announces_why_execution_ended(queue):
    reasons = []
    queue.on_event(
        lambda event: reasons.append(event.data["reason"])
        if event.kind == "run_finished"
        else None
    )
    await queue.finish_all()
    assert queue.get_finish_reason() == "drained"
    assert reasons == ["drained"]


async def test_on_result_async_awaits_the_handler_before_finish_all_returns(queue):
    seen = []

    async def persist(ticket, result):
        await asyncio.sleep(0)
        seen.append((ticket.key, result))

    queue.on_result_async(persist)
    key = queue.task("scan the corpus")
    queue.set_finished(key, {"verdict": "clean"})

    await queue.finish_all()

    assert seen == [(key, {"verdict": "clean"})]


async def test_on_result_async_finishes_one_handler_before_starting_the_next(queue):
    seen = []

    async def persist(ticket, result):
        seen.append(f"start {ticket.key}")
        # A scheduled-only coroutine would let the next one start here.
        await asyncio.sleep(0.01)
        seen.append(f"end {ticket.key}")

    queue.on_result_async(persist)
    first = queue.task("scan a.py")
    second = queue.task("scan b.py")
    queue.set_finished(first, "clean")
    queue.set_finished(second, "clean")

    await queue.finish_all()

    assert seen == [f"start {first}", f"end {first}", f"start {second}", f"end {second}"]


async def test_on_result_async_writes_every_result_to_a_database(queue, tmp_path):
    # `check_same_thread` off because `to_thread` runs the insert on a worker.
    database = sqlite3.connect(tmp_path / "verdicts.db", check_same_thread=False)
    database.execute("CREATE TABLE verdicts (ticket TEXT, verdict TEXT)")

    def insert(key, verdict):
        database.execute("INSERT INTO verdicts VALUES (?, ?)", (key, verdict))
        database.commit()

    async def persist(ticket, result):
        await asyncio.to_thread(insert, ticket.key, result["verdict"])

    queue.on_result_async(persist)
    first = queue.task("scan a.py")
    second = queue.task("scan b.py")
    queue.set_finished(first, {"verdict": "clean"})
    queue.set_finished(second, {"verdict": "malicious"})

    await queue.finish_all()

    # `finish_all` waited, so no write is still in flight here.
    rows = database.execute("SELECT ticket, verdict FROM verdicts").fetchall()
    assert rows == [(first, "clean"), (second, "malicious")]


async def test_on_results_async_hands_over_every_result_so_far(queue):
    seen = []

    async def note(results):
        await asyncio.sleep(0)
        seen.append(results)

    queue.on_results_async(note)
    first = queue.task("scan a.py")
    second = queue.task("scan b.py")
    queue.set_finished(first, "clean")
    queue.set_finished(second, "malicious")

    await queue.finish_all()

    assert seen == [["clean"], ["clean", "malicious"]]


async def test_on_result_async_runs_the_handler_on_the_callers_event_loop(queue):
    loops = []

    async def persist(ticket, result):
        loops.append(asyncio.get_running_loop())

    queue.on_result_async(persist)
    key = queue.task("scan the corpus")
    queue.set_finished(key, "clean")

    await queue.finish_all()

    # The whole point: a commit here can be serialized against the caller's own.
    assert loops == [asyncio.get_running_loop()]


async def test_finish_hands_back_the_results_its_filter_named(queue):
    key = queue.task("work")
    queue.set_finished(key, {"verdict": "clean"})
    assert await queue.finish(lambda t: t.key == key) == [{"verdict": "clean"}]


async def test_finish_all_hands_back_the_results_of_every_pool(queue):
    scan = queue.ticket(aw.Ticket("scan the corpus", label="scan"))
    report = queue.ticket(aw.Ticket("write it up", label="report"))
    queue.set_finished(scan, {"verdict": "clean"})
    queue.set_finished(report, {"pages": 2})

    assert await queue.finish_all() == [{"verdict": "clean"}, {"pages": 2}]


async def test_a_cancelled_run_reports_its_reason(queue):
    queue.start()
    queue.task("work")
    queue.cancel_all()
    await queue.finish_all()
    assert queue.get_finish_reason() == "cancelled"


def test_assignee_is_unset_until_an_agent_claims_the_ticket(queue):
    key = queue.task("work")
    assert queue.get_ticket(key).assignee is None
    assert queue.find_tickets(lambda t: t.assignee == "scout") == []


def test_load_reopens_a_session_directory(queue, tmp_path):
    queue.dir(str(tmp_path))
    key = queue.ticket(aw.Ticket("scan the corpus", label="scan"))

    reopened = aw.TicketQueue.load(str(tmp_path))

    assert reopened.get_ticket(key).task == "scan the corpus"


def test_a_schema_is_read_back_by_the_label_it_was_bound_to():
    schemas = aw.SchemaStore()
    schemas.label("analysis", {"type": "object", "required": ["verdict"]})

    assert schemas.get("analysis").validate({"verdict": "clean"}) == {"verdict": "clean"}
    assert schemas.get("discovery") is None


def test_label_raises_on_a_document_that_is_not_a_schema():
    schemas = aw.SchemaStore()
    with pytest.raises(RuntimeError):
        schemas.label("analysis", {"uniqueItems": True})
    assert schemas.get("analysis") is None


def test_a_queue_accepts_a_schema_store(queue):
    schemas = aw.SchemaStore()
    schemas.label("analysis", {"type": "string"})

    assert queue.schemas(schemas) is queue
