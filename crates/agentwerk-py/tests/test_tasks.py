"""Test tasks, schemas, and Werk state through the public API."""

import asyncio
import json
import sqlite3
from collections import Counter

import pytest

import agentwerk as aw


def test_enqueued_task_appears_with_its_status_and_label(werk):
    assert werk.get_tasks() == []

    werk.add_task(aw.Task("scan the corpus", label="scan"))

    (task,) = werk.get_tasks()
    assert task.get_task() == "scan the corpus"
    assert task.get_status() == "todo"
    assert task.get_label() == "scan"


def test_a_path_task_is_read_from_the_file(werk, tmp_path):
    task = tmp_path / "task.md"
    task.write_text("scan the corpus\n")

    werk.add_task(task)

    (task,) = werk.get_tasks()
    assert task.get_task() == "scan the corpus"


def test_a_string_task_stays_the_string_even_when_it_names_a_file(werk, tmp_path):
    task_path = tmp_path / "task.md"
    task_path.write_text("scan the corpus\n")

    werk.add_task(str(task_path))

    (task,) = werk.get_tasks()
    assert task.get_task() == str(task_path)


def test_unstarted_task_carries_its_id_and_no_messages(werk):
    id = werk.add_task(aw.Task("scan the corpus", label="scan"))

    task = werk.get_task(id)
    assert task.get_id() == id
    assert task.get_result() is None
    assert task.get_started_at() is None
    assert task.get_replies() == []


def test_task_selection_uses_aql_status_and_pending_fields(werk):
    id = werk.add_task(aw.Task("scan the corpus"))

    assert [task.get_id() for task in werk.find_tasks("status = todo")] == [id]
    assert [task.get_id() for task in werk.find_tasks("pending = true")] == [id]
    assert werk.find_tasks("status = in_progress") == []
    assert werk.find_tasks("status = finished") == []
    assert werk.find_tasks("status = failed") == []


def test_task_predicates_follow_label_status_and_cancellation(werk):
    todo_key = werk.add_task(aw.Task("scan", label="scan"))
    todo = werk.get_task(todo_key)
    assert todo.get_label() == "scan"
    assert todo.is_todo()
    assert todo.is_pending()
    assert not todo.is_in_progress()
    assert not todo.is_finished()
    assert not todo.is_failed()
    assert not todo.is_cancelled()

    unlabeled_key = werk.add_task(aw.Task("unscoped"))
    assert werk.get_task(unlabeled_key).get_label() is None

    werk.cancel_tasks("label = scan")
    cancelled = werk.get_task(todo_key)
    assert cancelled.is_cancelled()
    assert not cancelled.is_pending()

    finished_key = werk.add_task("finish")
    werk.set_task_finished(finished_key, "done")
    assert werk.get_task(finished_key).is_finished()

    failed_key = werk.add_task("fail")
    werk.set_task_failed(failed_key)
    assert werk.get_task(failed_key).is_failed()


def test_removed_werk_method_names_are_not_compatibility_aliases(werk):
    for name in (
        "agent",
        "policy",
        "dir",
        "schemas",
        "task",
        "reply",
        "set_finished",
        "set_failed",
        "cancel",
        "cancel_all",
        "finish",
        "finish_result",
        "finish_results",
        "finish_all",
        "finish_last",
        "finish_reason",
        "model_for_agent",
        "tasks",
        "results",
        "input_tokens",
        "output_tokens",
        "execution_duration",
        "is_cancelled",
    ):
        assert not hasattr(werk, name)


def test_parent_records_the_handover_trail(werk):
    parent = werk.add_task(aw.Task("survey the corpus"))
    child = werk.add_task(aw.Task("scan one file", parent=parent))

    assert werk.get_task(child).get_parent() == parent


def test_valid_schema_parses_and_attaches_to_a_task():
    schema = aw.Schema({"type": "object", "properties": {"n": {"type": "integer"}}})
    task = aw.Task("write a report", schema=schema)
    assert isinstance(task.get_schema(), aw.Schema)


def test_schema_validate_returns_the_value_to_keep_and_no_repair():
    schema = aw.Schema({"type": "object", "required": ["status"]})
    assert schema.validate({"status": "done"}) == ({"status": "done"}, [])


def test_schema_validate_decodes_a_double_encoded_value():
    schema = aw.Schema({"type": "object", "required": ["status"]})
    kept, repaired = schema.validate('{"status": "done"}')
    assert kept == {"status": "done"}
    assert repaired == [""]


def test_schema_validate_preserves_a_quoted_large_integer():
    schema = aw.Schema({"type": "number"})
    kept, repaired = schema.validate("9007199254740993")
    assert kept == 9007199254740993
    assert repaired == [""]


def test_schema_validate_does_not_fold_a_string_into_a_non_string_enum():
    schema = aw.Schema({"enum": [None]})
    with pytest.raises(RuntimeError):
        schema.validate("null")


def test_schema_validate_rejects_a_violating_value():
    schema = aw.Schema({"type": "object", "required": ["status"]})
    with pytest.raises(RuntimeError):
        schema.validate({})


def test_invalid_schema_document_is_rejected_with_runtime_error():
    with pytest.raises(RuntimeError):
        aw.Schema({"type": "not-a-real-type"})


def test_config_returns_the_werk_so_calls_chain(werk):
    configured = werk.set_policy(aw.Policy(max_turns=5, max_time=30.0)).set_dir("/tmp")
    assert isinstance(configured, aw.Werk)


def test_find_tasks_accepts_a_callable_for_dynamic_conditions(werk):
    werk.add_task(aw.Task("alpha", label="a"))
    werk.add_task(aw.Task("beta", label="b"))

    wanted = "a"
    matches = werk.find_tasks(lambda task: task.get_label() == wanted)
    assert [t.get_task() for t in matches] == ["alpha"]


def test_find_task_returns_the_first_match(werk):
    werk.add_task(aw.Task("alpha", label="a"))
    werk.add_task(aw.Task("beta", label="b"))

    found = werk.find_task("status = todo")
    assert found.get_task() == "alpha"


def test_find_tasks_filters_by_query(werk):
    werk.add_task(aw.Task("alpha", label="a"))
    werk.add_task(aw.Task("beta", label="b"))

    matches = werk.find_tasks(aw.Query("label = a"))
    assert [t.get_task() for t in matches] == ["alpha"]


def test_find_tasks_compiles_the_string_as_a_query(werk):
    werk.add_task(aw.Task("alpha", label="a"))
    werk.add_task(aw.Task("beta", label="b"))

    assert [t.get_task() for t in werk.find_tasks("b")] == ["beta"]
    assert [t.get_task() for t in werk.find_tasks("label = a")] == ["alpha"]


def test_a_malformed_query_string_raises_value_error(werk):
    with pytest.raises(ValueError):
        werk.find_tasks("assignee = alice")


def test_a_query_compiles_its_string_on_construction():
    with pytest.raises(ValueError):
        aw.Query("label =")


def test_an_event_query_raises_where_tasks_are_selected(werk):
    with pytest.raises(ValueError):
        werk.find_tasks(aw.Query("event = task_finished"))


def test_task_takes_a_bare_task_without_a_task_object(werk):
    id = werk.add_task("scan the corpus")

    assert werk.get_task(id).get_task() == "scan the corpus"


def test_find_results_selects_by_label(werk):
    scan = werk.add_task(aw.Task("scan the corpus", label="scan"))
    report = werk.add_task(aw.Task("write the report", label="report"))
    werk.set_task_finished(scan, {"verdict": "clean"})
    werk.set_task_finished(report, {"summary": "nothing found"})

    assert werk.find_results("scan") == [{"verdict": "clean"}]
    assert werk.find_result(aw.Query("label = report")) == {"summary": "nothing found"}


def test_find_results_takes_a_callable(werk):
    scan = werk.add_task(aw.Task("scan the corpus", label="scan"))
    report = werk.add_task(aw.Task("write the report", label="report"))
    werk.set_task_finished(scan, {"verdict": "clean"})
    werk.set_task_finished(report, {"summary": "nothing found"})

    assert werk.find_results(lambda task: task.get_label() == "scan") == [{"verdict": "clean"}]


def test_get_task_returns_none_for_unknown_id(werk):
    assert werk.get_task("t-does-not-exist") is None


def test_set_failed_resolves_a_task_from_outside_the_run(werk):
    id = werk.add_task(aw.Task("scan the corpus"))

    werk.set_task_failed(id)

    assert werk.get_task(id).get_status() == "failed"


def test_errors_is_a_list_and_excludes_the_terminal_failure(werk):
    id = werk.add_task(aw.Task("scan the corpus"))

    werk.set_task_failed(id)

    # A host fail is the terminal marker, not a recorded cause: the errors
    # list holds the failure events (failed requests, tool calls) the run saw.
    assert werk.get_task(id).get_errors() == []


def test_set_finished_resolves_a_task_with_its_result(werk):
    id = werk.add_task(aw.Task("scan the corpus"))

    werk.set_task_finished(id, {"verdict": "clean"})

    assert werk.get_task(id).get_status() == "finished"
    assert werk.get_results()[-1] == {"verdict": "clean"}


def test_set_finished_rejects_a_result_that_misses_the_schema(werk):
    schema = aw.Schema(
        {
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
        }
    )
    id = werk.add_task(aw.Task("write a report", schema=schema))

    with pytest.raises(RuntimeError):
        werk.set_task_finished(id, {"body": "no title"})

    assert werk.get_task(id).get_status() == "todo"


def test_set_failed_rejects_an_unknown_key(werk):
    with pytest.raises(RuntimeError):
        werk.set_task_failed("t-does-not-exist")


def test_reply_chains(werk):
    id = werk.add_task(aw.Task("scan the corpus"))
    assert isinstance(werk.add_reply(id, "keep going"), aw.Werk)


def test_results_are_empty_before_a_run(werk):
    werk.add_task(aw.Task("alpha", label="a"))

    assert werk.get_results() == []


def test_find_tasks_returns_every_status_not_just_finished(werk):
    werk.add_task(aw.Task("alpha", label="a"))
    werk.add_task(aw.Task("beta", label="b"))

    tasks = [task.get_task() for task in werk.find_tasks("label = a")]
    assert tasks == ["alpha"]


def test_policy_round_trips_through_get_policy(werk):
    werk.set_policy(aw.Policy(max_turns=40, max_time=300.0))

    config = werk.get_policy()
    assert config.max_turns == 40
    assert config.max_time == 300.0
    assert config.max_input_tokens is None
    assert config.max_request_retries == 10


def test_cancel_takes_the_matching_tasks_off_the_queue(werk):
    scan = werk.add_task(aw.Task("scan the corpus", label="scan"))
    werk.add_task(aw.Task("write it up", label="report"))

    assert isinstance(werk.cancel_tasks("label = scan"), aw.Werk)

    assert [task.get_id() for task in werk.find_tasks("cancelled = true")] == [scan]
    assert [task.get_label() for task in werk.find_tasks("cancelled = false")] == ["report"]
    assert werk.get_task(scan).is_cancelled()


def test_cancel_applies_to_matching_tasks_inserted_later(werk):
    werk.cancel_tasks("label = scan")

    scan = werk.add_task(aw.Task("scan the corpus", label="scan"))
    werk.add_task(aw.Task("write it up", label="report"))

    assert werk.find_task("cancelled = true").get_id() == scan


async def test_start_clears_cancellation_flags_and_filters(werk):
    werk.add_task(aw.Task("first", label="scan"))
    werk.cancel_tasks("label = scan")
    assert len(werk.find_tasks("cancelled = true")) == 1

    werk.start()
    werk.add_task(aw.Task("second", label="scan"))

    assert werk.find_tasks("cancelled = true") == []
    assert len(werk.find_tasks("pending = true")) == 2
    werk.cancel_all_tasks()
    await werk.finish_all_tasks()


def test_task_json_does_not_persist_cancellation(werk, tmp_path):
    id = werk.add_task(aw.Task("scan", label="scan"))
    werk.cancel_tasks("label = scan")
    werk.set_task_failed(id)

    record = json.loads((tmp_path / "tasks" / id / "task.json").read_text())
    assert record["id"] == id
    assert "key" not in record
    assert "cancelled" not in record

    reopened = aw.Werk.load(str(tmp_path))
    assert reopened.find_tasks("cancelled = true") == []
    assert len(reopened.find_tasks("cancelled = false")) == 1


def test_a_werk_that_has_not_run_records_nothing(werk):
    assert werk.find_events(lambda e: True) == []
    assert werk.get_input_tokens() == 0
    assert werk.get_duration() is None


def test_a_condition_that_raises_reads_as_no_match(werk):
    def broken(event):
        raise ValueError("boom")

    werk.add_task("seed")

    assert werk.find_events(broken) == []
    assert werk.find_event(broken) is None


def test_event_constants_spell_the_name_an_event_reports(werk):
    seen = []
    werk.on_event(lambda _, event: seen.append(event.get_name()))

    werk.add_task("seed")

    assert aw.Event.TASK_CREATED in seen
    assert len(werk.find_events(lambda e: e.get_name() == aw.Event.TASK_CREATED)) == 1


def test_find_event_returns_the_earliest_match(werk):
    werk.add_task("one")
    werk.add_task("two")

    first = werk.find_event(lambda e: e.get_name() == aw.Event.TASK_CREATED)
    assert first.get_task_id() == "t-1"
    assert werk.find_event(lambda e: e.get_name() == aw.Event.TASK_FAILED) is None


def test_find_events_takes_an_aql_string(werk, tmp_path):
    werk.add_task(aw.Task("scan", label="scout"))
    werk.add_task("two")

    assert len(werk.find_events("task_created")) == 2
    assert len(werk.find_events("event = task_created AND label = scout")) == 1
    assert len(werk.find_events("t-2")) == 1
    assert werk.find_events("run_finished") == []

    events_path = tmp_path / "events.jsonl"
    records = [json.loads(line) for line in events_path.read_text().splitlines()]
    for record, created_at in zip(records, (100, 200)):
        record["created_at"] = created_at
    events_path.write_text("".join(f"{json.dumps(record)}\n" for record in records))

    newest = werk.find_event("task_created ORDER BY created DESC")
    assert newest.get_task_id() == "t-2"


def test_find_events_takes_a_compiled_query(werk):
    werk.add_task("seed")

    assert len(werk.find_events(aw.Query("task_created"))) == 1
    assert werk.find_events(aw.Query("event = task_exploded")) == []
    with pytest.raises(ValueError):
        werk.find_events("event = ")


def test_emit_event_publishes_named_data_with_optional_context(werk, tmp_path):
    werk.set_dir(str(tmp_path))
    id = werk.add_task(aw.Task("scan", label="scout"))
    seen = []
    werk.on_event(lambda _, event: seen.append(event))

    emitted = werk.emit_event(
        aw.Event("document_indexed")
        .data({"documents": 42})
        .task_id(id)
        .agent_id("scout-1")
    )

    assert emitted.get_name() == "document_indexed"
    assert emitted.get_data() == {"documents": 42}
    assert emitted.get_task_id() == id
    assert emitted.get_agent_id() == "scout-1"
    assert emitted.get_label() == "scout"
    assert emitted.get_created_at() > 0
    assert len(seen) == 1
    assert seen[0].get_name() == "document_indexed"
    assert seen[0].get_data() == {"documents": 42}
    assert werk.find_event("event = document_indexed").get_data() == {"documents": 42}

    records = [json.loads(line) for line in (tmp_path / "events.jsonl").read_text().splitlines()]
    record = next(record for record in records if record["name"] == "document_indexed")
    assert record["task_id"] == id
    assert "task_key" not in record

    reopened = aw.Werk.load(str(tmp_path))
    restored = reopened.find_event("event = document_indexed")
    assert restored.get_data() == {"documents": 42}
    assert restored.get_label() == "scout"


def test_emit_event_accepts_global_events(werk):
    empty = werk.emit_event(aw.Event("cache_checked"))
    emitted = werk.emit_event(aw.Event("index_refreshed").data([1, 2, 3]))

    assert empty.get_data() == {}
    assert emitted.get_agent_id() == ""
    assert emitted.get_task_id() == ""
    assert emitted.get_label() is None
    assert emitted.get_data() == [1, 2, 3]


def test_event_builders_replace_their_values(werk):
    emitted = werk.emit_event(
        aw.Event("document_indexed")
        .data({"documents": 1})
        .data({"documents": 42})
        .task_id("t-1")
        .task_id("t-2")
        .agent_id("old")
        .agent_id("indexer-1")
    )

    assert emitted.get_data() == {"documents": 42}
    assert emitted.get_task_id() == "t-2"
    assert emitted.get_agent_id() == "indexer-1"


def test_emitting_a_builtin_name_activates_name_based_hooks_without_changing_state(werk):
    id = werk.add_task("work")
    seen = []
    werk.on_task(lambda *args: seen.append("task"))
    werk.on_result(lambda *args: seen.append("result"))
    werk.on_failure(lambda *args: seen.append("failure"))

    werk.emit_event(aw.Event(aw.Event.TASK_FINISHED).task_id(id))

    assert werk.get_task(id).get_status() == "todo"
    assert seen == ["task"]


@pytest.mark.parametrize("name", ["Document Indexed", "document__indexed", "TaskFinished"])
def test_emit_event_accepts_arbitrary_names(werk, name):
    emitted = werk.emit_event(aw.Event(name))

    assert emitted.get_name() == name
    assert werk.find_event(f'event = "{name}"').get_name() == name


def test_a_query_neither_field_set_accepts_raises_on_construction():
    with pytest.raises(ValueError):
        aw.Query("assignee = alice")


def test_a_task_query_raises_where_events_are_selected(werk):
    werk.add_task("seed")
    tasks_only = aw.Query("status = finished")

    assert werk.find_tasks(tasks_only) == []
    with pytest.raises(ValueError):
        werk.find_events(tasks_only)


def test_an_event_carries_the_label_of_the_task_it_concerns(werk):
    created = Counter()

    def count_per_label(_, event):
        if event.get_name() == aw.Event.TASK_CREATED:
            created[event.get_label()] += 1

    werk.on_event(count_per_label)

    werk.add_task(aw.Task("scan the tree", label="scan"))
    werk.add_task(aw.Task("scan the lockfile", label="scan"))
    werk.add_task(aw.Task("write the report", label="report"))

    assert created == Counter({"scan": 2, "report": 1})


def test_model_for_agent_is_none_when_no_agent_is_bound(werk):
    assert werk.get_model_for_agent("scribe") is None


def test_on_result_receives_the_finished_task_and_its_result(werk):
    seen = []
    werk.on_result(lambda _, task, result: seen.append((task.get_id(), result)))
    id = werk.add_task(aw.Task("scan the corpus"))

    werk.set_task_finished(id, {"verdict": "clean"})

    assert seen == [(id, {"verdict": "clean"})]


def test_a_hook_reads_the_results_that_landed_before_it(werk):
    seen = []
    werk.on_result(lambda callback_werk, _, __: seen.append(callback_werk.get_results()))
    first = werk.add_task(aw.Task("scan a.py"))
    second = werk.add_task(aw.Task("scan b.py"))

    werk.set_task_finished(first, "clean")
    werk.set_task_finished(second, "malicious")

    assert seen == [["clean"], ["clean", "malicious"]]


def test_a_hook_waits_for_the_results_it_needs_before_filing_the_next_step(werk):
    def review_once_both_landed(callback_werk, _, __):
        results = callback_werk.get_results()
        if len(results) == 2:
            for result in results:
                callback_werk.add_task(aw.Task(result, label="review"))

    werk.on_result(review_once_both_landed)
    first = werk.add_task(aw.Task("scan a.py"))
    second = werk.add_task(aw.Task("scan b.py"))

    werk.set_task_finished(first, "clean")
    assert werk.find_tasks(lambda t: t.get_label() == "review") == []

    werk.set_task_finished(second, "malicious")
    filed = [t.get_task() for t in werk.find_tasks(lambda t: t.get_label() == "review")]
    assert filed == ["clean", "malicious"]


def test_on_failure_receives_the_failed_task(werk):
    seen = []
    werk.on_failure(lambda _, event, task: seen.append((event.get_name(), task.get_id())))
    id = werk.add_task(aw.Task("scan the corpus"))

    werk.set_task_failed(id)

    assert seen == [("task_failed", id)]


def test_on_failure_files_a_retry_through_the_werk_it_is_handed(werk):
    def retry_once(callback_werk, _, failed):
        if not failed.get_parent():
            callback_werk.add_task(aw.Task(failed.get_task(), parent=failed.get_id()))

    werk.on_failure(retry_once)
    id = werk.add_task(aw.Task("scan the corpus"))

    werk.set_task_failed(id)

    retry = werk.find_task(lambda task: task.get_parent() == id)
    assert retry.get_task() == "scan the corpus"


def test_on_event_files_a_follow_up_for_any_kind(werk):
    def report_when_done(callback_werk, event):
        if event.get_name() == aw.Event.TASK_FINISHED:
            callback_werk.add_task(aw.Task("report", label="report"))

    werk.on_event(report_when_done)
    id = werk.add_task(aw.Task("scan the corpus"))

    werk.set_task_finished(id, {"verdict": "clean"})

    filed = werk.find_tasks("label = report")
    assert [t.get_task() for t in filed] == ["report"]


def test_an_event_handler_rewrites_replies_through_the_werk(werk):
    def redact_when_done(callback_werk, event):
        if event.get_name() == aw.Event.TASK_FINISHED:
            callback_werk.edit_replies(event.get_task_id(), lambda replies: [aw.Reply.user_text("[redacted]")])

    werk.on_event(redact_when_done)
    id = werk.add_task(aw.Task("scan the corpus"))
    werk.add_reply(id, "secret")

    werk.set_task_finished(id, {"verdict": "clean"})

    texts = [r.get_content()[0].get_data()["text"] for r in werk.get_task(id).get_replies()]
    assert texts == ["[redacted]"]


def test_compaction_threshold_round_trips_through_get_policy(werk):
    assert werk.get_policy().compaction_threshold is None

    werk.set_policy(aw.Policy(compaction_threshold=0.8))

    assert werk.get_policy().compaction_threshold == 0.8


def test_compaction_threshold_clamps_a_fraction_above_one(werk):
    werk.set_policy(aw.Policy(compaction_threshold=1.5))

    assert werk.get_policy().compaction_threshold == 1.0


def test_edit_replies_on_an_unstarted_task_is_a_no_op(werk):
    id = werk.add_task(aw.Task("scan the corpus"))

    werk.edit_replies(id, lambda replies: replies)

    assert werk.get_task(id).get_replies() == []


def test_edit_replies_drops_a_reply_from_a_non_empty_list(werk):
    id = werk.add_task("scan the corpus")
    werk.add_reply(id, "keep me")
    werk.add_reply(id, "drop me")

    werk.edit_replies(
        id, lambda replies: [r for r in replies if r.get_content()[0].get_data()["text"] != "drop me"]
    )

    remaining = [r.get_content()[0].get_data()["text"] for r in werk.get_task(id).get_replies()]
    assert remaining == ["keep me"]


def test_edit_replies_appends_a_reply_built_in_python(werk):
    id = werk.add_task("scan the corpus")
    werk.add_reply(id, "first")

    werk.edit_replies(id, lambda replies: replies + [aw.Reply.user_text("second")])

    texts = [r.get_content()[0].get_data()["text"] for r in werk.get_task(id).get_replies()]
    assert texts == ["first", "second"]


def test_edit_replies_raises_when_the_editor_raises(werk):
    id = werk.add_task("scan the corpus")
    werk.add_reply(id, "first")

    def editor(replies):
        raise ValueError("no good")

    with pytest.raises(ValueError, match="no good"):
        werk.edit_replies(id, editor)


def test_edit_replies_raises_when_the_editor_returns_dicts(werk):
    id = werk.add_task("scan the corpus")
    werk.add_reply(id, "first")

    with pytest.raises(RuntimeError, match="list of Reply objects"):
        werk.edit_replies(id, lambda replies: [{"author": "user", "content": []}])


async def test_run_finished_announces_why_execution_ended(werk):
    reasons = []
    werk.on_event(
        lambda _, event: reasons.append(event.get_data()["outcome"])
        if event.get_name() == aw.Event.RUN_FINISHED
        else None
    )
    await werk.finish_all_tasks()
    assert werk.get_finish_reason() == "drained"
    assert reasons == ["drained"]


async def test_on_result_async_awaits_the_handler_before_finish_all_returns(werk):
    seen = []

    async def persist(_, task, result):
        await asyncio.sleep(0)
        seen.append((task.get_id(), result))

    werk.on_result_async(persist)
    id = werk.add_task("scan the corpus")
    werk.set_task_finished(id, {"verdict": "clean"})

    await werk.finish_all_tasks()

    assert seen == [(id, {"verdict": "clean"})]


async def test_on_result_async_finishes_one_handler_before_starting_the_next(werk):
    seen = []

    async def persist(_, task, result):
        seen.append(f"start {task.get_id()}")
        # A scheduled-only coroutine would let the next one start here.
        await asyncio.sleep(0.01)
        seen.append(f"end {task.get_id()}")

    werk.on_result_async(persist)
    first = werk.add_task("scan a.py")
    second = werk.add_task("scan b.py")
    werk.set_task_finished(first, "clean")
    werk.set_task_finished(second, "clean")

    await werk.finish_all_tasks()

    assert seen == [f"start {first}", f"end {first}", f"start {second}", f"end {second}"]


async def test_on_result_async_writes_every_result_to_a_database(werk, tmp_path):
    # `check_same_thread` off because `to_thread` runs the insert on a worker.
    database = sqlite3.connect(tmp_path / "verdicts.db", check_same_thread=False)
    database.execute("CREATE TABLE verdicts (task TEXT, verdict TEXT)")

    def insert(id, verdict):
        database.execute("INSERT INTO verdicts VALUES (?, ?)", (id, verdict))
        database.commit()

    async def persist(_, task, result):
        await asyncio.to_thread(insert, task.get_id(), result["verdict"])

    werk.on_result_async(persist)
    first = werk.add_task("scan a.py")
    second = werk.add_task("scan b.py")
    werk.set_task_finished(first, {"verdict": "clean"})
    werk.set_task_finished(second, {"verdict": "malicious"})

    await werk.finish_all_tasks()

    # `finish_all` waited, so no write is still in flight here.
    rows = database.execute("SELECT task, verdict FROM verdicts").fetchall()
    assert rows == [(first, "clean"), (second, "malicious")]


async def test_on_task_async_awaits_the_handler_before_finish_all_returns(werk):
    seen = []

    async def note(_, event, task):
        await asyncio.sleep(0)
        seen.append((event.get_name(), task.get_id()))

    werk.on_task_async(note)
    id = werk.add_task("scan the corpus")
    werk.set_task_finished(id, "clean")

    await werk.finish_all_tasks()

    assert seen == [("task_finished", id)]


async def test_on_failure_async_awaits_the_handler_before_finish_all_returns(werk):
    seen = []

    async def note(_, event, task):
        await asyncio.sleep(0)
        seen.append((event.get_name(), task.get_id()))

    werk.on_failure_async(note)
    id = werk.add_task("scan the corpus")
    werk.set_task_failed(id)

    await werk.finish_all_tasks()

    assert seen == [("task_failed", id)]


async def test_on_event_async_sees_the_kinds_no_task_hook_accepts(werk):
    seen = []

    async def note(_, event):
        await asyncio.sleep(0)
        seen.append(event.get_name())

    werk.on_event_async(note)
    id = werk.add_task("scan the corpus")
    werk.set_task_finished(id, "clean")

    await werk.finish_all_tasks()

    assert "task_created" in seen


async def test_on_event_async_receives_named_events(werk):
    seen = []

    async def note(_, event):
        await asyncio.sleep(0)
        if event.get_name() == "document_indexed":
            seen.append(event.get_name())

    werk.on_event_async(note)
    werk.emit_event(aw.Event("document_indexed"))

    await werk.finish_all_tasks()

    assert seen == ["document_indexed"]


async def test_on_result_async_runs_the_handler_on_the_callers_event_loop(werk):
    loops = []

    async def persist(_, task, result):
        loops.append(asyncio.get_running_loop())

    werk.on_result_async(persist)
    id = werk.add_task("scan the corpus")
    werk.set_task_finished(id, "clean")

    await werk.finish_all_tasks()

    # Running on the caller's loop lets the caller serialize its own database work with the handler.
    assert loops == [asyncio.get_running_loop()]


async def test_finish_hands_back_the_results_its_filter_named(werk):
    id = werk.add_task("work")
    werk.set_task_finished(id, {"verdict": "clean"})
    assert await werk.finish_tasks(lambda t: t.get_id() == id) == [{"verdict": "clean"}]


async def test_finish_all_hands_back_the_results_of_every_pool(werk):
    scan = werk.add_task(aw.Task("scan the corpus", label="scan"))
    report = werk.add_task(aw.Task("write it up", label="report"))
    werk.set_task_finished(scan, {"verdict": "clean"})
    werk.set_task_finished(report, {"pages": 2})

    assert await werk.finish_all_tasks() == [{"verdict": "clean"}, {"pages": 2}]


async def test_finish_task_hands_back_the_first_result_in_query_order(werk):
    scan = werk.add_task(aw.Task("scan the corpus", label="scan"))
    report = werk.add_task(aw.Task("write it up", label="report"))
    # Resolve back to front so the answer distinguishes creation order from completion order.
    werk.set_task_finished(report, {"pages": 2})
    werk.set_task_finished(scan, {"verdict": "clean"})

    assert await werk.finish_task("ORDER BY id DESC") == {"pages": 2}


async def test_finish_task_is_none_when_nothing_finished(werk):
    assert await werk.finish_task("status = finished") is None


async def test_a_cancelled_run_reports_its_reason(werk):
    werk.start()
    werk.add_task("work")
    werk.cancel_all_tasks()
    await werk.finish_all_tasks()
    assert werk.get_finish_reason() == "cancelled"


def test_assignee_is_unset_until_an_agent_claims_the_task(werk):
    id = werk.add_task("work")
    assert werk.get_task(id).get_assignee() is None
    assert werk.find_tasks(lambda t: t.get_assignee() == "scout") == []


def test_load_reopens_a_session_directory(werk, tmp_path):
    werk.set_dir(str(tmp_path))
    id = werk.add_task(aw.Task("scan the corpus", label="scan"))

    reopened = aw.Werk.load(str(tmp_path))

    assert reopened.get_task(id).get_task() == "scan the corpus"
