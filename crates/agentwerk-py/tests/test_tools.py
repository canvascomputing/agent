"""Test built-in tools and the ``@tool`` decorator through ``Agent.tool``."""

import asyncio
import time
from typing import Annotated, Any, Dict, List, Literal, Mapping, Optional, Sequence, Tuple, Union

import pytest

import agentwerk as aw

BUILTIN_FACTORIES = [
    aw.ReadFileTool,
    aw.WriteFileTool,
    aw.EditFileTool,
    aw.GrepTool,
    aw.GlobTool,
    aw.ListDirectoryTool,
    aw.TaskTool,
    aw.EventTool,
    aw.FinishTool,
]

TIMEOUT_TOOL_FACTORIES = [
    pytest.param(aw.GrepTool, id="tool"),
    pytest.param(aw.FetchTool, id="fetch"),
    pytest.param(lambda: aw.CommandTool("echo"), id="command"),
]


async def run_scripted_agent(scripted_openai, tmp_path, tool):
    agent = (
        aw.Agent()
        .provider(scripted_openai.provider())
        .model("mock")
        .role("Follow the scripted tool calls.")
        .tool(tool)
    )
    werk = (
        aw.Werk()
        .set_dir(str(tmp_path))
        .set_policy(aw.Policy(max_request_retries=0, max_time=2.0))
        .add_agent(agent)
    )
    werk.add_task("Run the tool, then finish.")
    results = await werk.finish()
    return werk, results


async def run_scripted_tool(scripted_openai, tmp_path, tool, name, arguments):
    scripted_openai.respond_with_tool(name, arguments)
    scripted_openai.respond_with_tool("finish", {"answer": "done"})
    return await run_scripted_agent(scripted_openai, tmp_path, tool)


@pytest.mark.parametrize("factory", BUILTIN_FACTORIES)
def test_builtin_factories_return_a_tool(factory):
    assert isinstance(factory(), aw.Tool)


@pytest.mark.parametrize("factory", TIMEOUT_TOOL_FACTORIES)
def test_timeout_configuration_is_fluent(factory):
    tool = factory()
    assert tool.timeout(30.0) is tool
    assert tool.timeout(0) is tool


def test_command_tool_configuration_chains_on_one_object():
    tool = aw.CommandTool("git")
    assert tool.allow("git *") is tool
    assert tool.allow_flag("--oneline") is tool
    assert tool.deny("git push*") is tool
    assert tool.deny_flag("--force") is tool
    assert tool.concurrent(True) is tool
    assert tool.description("Run git commands.") is tool


def test_command_tool_description_accepts_file_contents_read_by_the_caller(tmp_path):
    description = tmp_path / "git.tool.md"
    description.write_text("Run git commands.\n")
    tool = aw.CommandTool("git")
    assert tool.description(description.read_text()) is tool


def test_tool_decorator_accepts_file_contents_read_by_the_caller(tmp_path):
    description = tmp_path / "sample.tool.md"
    description.write_text("Describe the sample.\n")

    @aw.tool(description=description.read_text())
    def sample(path: str) -> str:
        return path

    agent = aw.Agent().tool(sample)
    assert isinstance(agent, aw.Agent)


def test_an_agent_accepts_a_command_tool():
    agent = aw.Agent().tool(aw.CommandTool("git").allow("git *"))
    assert isinstance(agent, aw.Agent)


def test_fetch_tool_configuration_chains_on_one_object():
    tool = aw.FetchTool()
    assert tool.impersonate() is tool


@pytest.mark.parametrize("factory", TIMEOUT_TOOL_FACTORIES)
@pytest.mark.parametrize(
    ("timeout", "error"),
    [
        pytest.param(None, TypeError, id="none"),
        pytest.param(-1, ValueError, id="negative"),
        pytest.param(float("nan"), ValueError, id="nan"),
        pytest.param(float("inf"), ValueError, id="infinite"),
        pytest.param(1e300, ValueError, id="too-large"),
    ],
)
def test_tool_timeout_rejects_invalid_values(factory, timeout, error):
    with pytest.raises(error):
        factory().timeout(timeout)


def test_an_agent_accepts_a_fetch_tool():
    agent = aw.Agent().tool(aw.FetchTool().impersonate())
    assert isinstance(agent, aw.Agent)


def test_fetch_tool_is_not_a_compatibility_alias():
    assert not hasattr(aw, "FetchUrlTool")


def test_task_tool_is_not_exposed_under_the_old_plural_name():
    assert not hasattr(aw, "TasksTool")


def test_tool_decorator_records_name_doc_and_concurrent():
    @aw.tool(concurrent=True)
    def sample(path: str) -> str:
        """Describe the sample."""
        return path

    assert sample._agentwerk_name == "sample"
    assert sample._agentwerk_description == "Describe the sample."
    assert sample._agentwerk_concurrent is True


def test_inferred_schema_marks_only_parameters_without_defaults_as_required():
    @aw.tool
    def sample(required: str, defaulted: int = 1, unannotated=None):
        return required

    schema = sample._agentwerk_schema
    assert schema["required"] == ["required"]
    assert set(schema["properties"]) == {"required", "defaulted", "unannotated"}


@pytest.mark.parametrize(
    ("annotation", "expected"),
    [
        pytest.param(str, {"type": "string"}, id="string"),
        pytest.param(bool, {"type": "boolean"}, id="boolean"),
        pytest.param(int, {"type": "integer"}, id="integer"),
        pytest.param(float, {"type": "number"}, id="number"),
        pytest.param(
            Optional[List[str]],
            {
                "anyOf": [
                    {"type": "array", "items": {"type": "string"}},
                    {"type": "null"},
                ]
            },
            id="optional-array",
        ),
        pytest.param(
            Literal["fast", "safe"], {"enum": ["fast", "safe"]}, id="literal"
        ),
        pytest.param(
            Union[str, int],
            {"anyOf": [{"type": "string"}, {"type": "integer"}]},
            id="union",
        ),
        pytest.param(
            Annotated[str, "metadata"], {"type": "string"}, id="annotated"
        ),
        pytest.param(Any, {}, id="any"),
        pytest.param(
            List[Tuple[int, ...]],
            {
                "type": "array",
                "items": {"type": "array", "items": {"type": "integer"}},
            },
            id="nested-variable-tuple",
        ),
        pytest.param(
            Tuple[int, str],
            {
                "type": "array",
                "prefixItems": [{"type": "integer"}, {"type": "string"}],
                "minItems": 2,
                "maxItems": 2,
            },
            id="fixed-tuple",
        ),
        pytest.param(Dict[str, str], {"type": "object"}, id="dictionary"),
        pytest.param(
            Sequence[str],
            {"type": "array", "items": {"type": "string"}},
            id="sequence",
        ),
        pytest.param(Mapping[str, int], {"type": "object"}, id="mapping"),
    ],
)
def test_inferred_schema_metadata_maps_supported_annotations(annotation, expected):
    def sample(value):
        return value

    sample.__annotations__["value"] = annotation
    decorated = aw.tool(sample)

    assert decorated._agentwerk_schema["properties"]["value"] == expected


def test_fixed_tuple_schema_enforces_length_and_position():
    @aw.tool
    def sample(value: Tuple[int, str]):
        return value

    schema = aw.Schema(sample._agentwerk_schema)

    assert schema.validate({"value": [1, "one"]})[0] == {"value": [1, "one"]}
    with pytest.raises(RuntimeError):
        schema.validate({"value": ["one", 1]})
    with pytest.raises(RuntimeError):
        schema.validate({"value": [1]})


async def test_inferred_schema_is_sent_to_the_model(scripted_openai, tmp_path):
    @aw.tool
    def lookup(path: str, limit: int = 20):
        return path

    scripted_openai.respond_with_tool("finish", {"answer": "done"})
    await run_scripted_agent(scripted_openai, tmp_path, lookup)

    tools = scripted_openai.requests[0]["tools"]
    lookup_schema = next(
        tool["function"]["parameters"]
        for tool in tools
        if tool["function"]["name"] == "lookup"
    )
    assert lookup_schema == {
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "limit": {"type": "integer"},
        },
        "required": ["path"],
    }


def test_explicit_tool_schema_bypasses_signature_inference():
    explicit = {"type": "object", "properties": {"raw": {"type": "string"}}}

    @aw.tool(schema=explicit)
    def sample(value: int) -> str:
        return str(value)

    assert sample._agentwerk_schema is explicit


def test_inferred_schema_metadata_leaves_unknown_annotations_unconstrained():
    def sample(known, plain, unresolved: "MissingType" = None):
        return plain

    sample.__annotations__["known"] = "str"
    sample = aw.tool(sample)

    assert sample._agentwerk_schema == {
        "type": "object",
        "properties": {
            "known": {"type": "string"},
            "plain": {},
            "unresolved": {},
        },
        "required": ["known", "plain"],
    }


def test_inferred_schema_omits_variadic_parameters():
    @aw.tool
    def sample(path: str, *args, **kwargs):
        return path

    assert sample._agentwerk_schema["properties"] == {"path": {"type": "string"}}


def test_inferred_schema_rejects_positional_only_parameters():
    with pytest.raises(TypeError, match="positional-only"):

        @aw.tool
        def sample(path: str, /):
            return path


def test_explicit_schema_still_rejects_positional_only_parameters():
    with pytest.raises(TypeError, match="positional-only"):

        @aw.tool(schema={"type": "object"})
        def explicit(path: str, /):
            return path


@pytest.mark.parametrize(
    ("timeout", "error"),
    [
        pytest.param(None, TypeError, id="none"),
        pytest.param(-1, ValueError, id="negative"),
        pytest.param(float("nan"), ValueError, id="nan"),
        pytest.param(float("inf"), ValueError, id="infinite"),
        pytest.param(1e300, ValueError, id="too-large"),
        pytest.param(10**1000, ValueError, id="integer-overflow"),
    ],
)
def test_tool_decorator_rejects_invalid_timeout(timeout, error):
    with pytest.raises(error):

        @aw.tool(timeout=timeout)
        def sample():
            return ""


def test_failed_timeout_decoration_does_not_modify_the_function():
    def sample():
        return ""

    with pytest.raises(ValueError, match="too large"):
        aw.tool(timeout=1e300)(sample)

    assert not any(
        attribute.startswith("_agentwerk_") for attribute in vars(sample)
    )


async def test_positive_python_timeout_fails_the_call_and_the_agent_continues(
    scripted_openai, tmp_path
):
    completed = []

    @aw.tool(timeout=0.01)
    def wait(seconds: float):
        time.sleep(seconds)
        completed.append(True)
        return "late"

    werk, results = await run_scripted_tool(
        scripted_openai, tmp_path, wait, "wait", {"seconds": 1.0}
    )

    failure = werk.find_event(
        lambda event: event.get_name() == aw.Event.TOOL_CALL_FAILED
        and event.get_data().get("tool_name") == "wait"
    )
    assert failure.get_directive() == "tool_timed_out"
    assert results == [{"answer": "done"}]
    assert completed == []


async def test_zero_python_timeout_allows_the_call_to_finish(scripted_openai, tmp_path):
    @aw.tool(timeout=0)
    def wait(seconds: float):
        time.sleep(seconds)
        return "completed"

    werk, _ = await run_scripted_tool(
        scripted_openai, tmp_path, wait, "wait", {"seconds": 0.02}
    )

    finished = werk.find_event(
        lambda event: event.get_name() == aw.Event.TOOL_CALL_FINISHED
        and event.get_data().get("tool_name") == "wait"
    )
    assert finished.get_data()["output"] == "completed"


async def test_sync_python_tool_receives_keyword_arguments(scripted_openai, tmp_path):
    received = []

    @aw.tool
    def combine(left: str, right: str):
        received.append((left, right))
        return left + right

    await run_scripted_tool(
        scripted_openai,
        tmp_path,
        combine,
        "combine",
        {"left": "agent", "right": "werk"},
    )

    assert received == [("agent", "werk")]


async def test_async_python_tool_receives_keyword_arguments(scripted_openai, tmp_path):
    received = []

    @aw.tool
    async def combine(left: str, right: str):
        await asyncio.sleep(0)
        received.append((left, right))
        return left + right

    await run_scripted_tool(
        scripted_openai,
        tmp_path,
        combine,
        "combine",
        {"left": "agent", "right": "werk"},
    )

    assert received == [("agent", "werk")]


def test_tool_decorator_has_no_path_configuration():
    with pytest.raises(TypeError):
        aw.tool(paths=["path"])


def test_knowledge_tool_binds_a_store(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    assert isinstance(aw.KnowledgeTool(store), aw.Tool)


def test_terminal_tool_events_are_events():
    finished = aw.Event.tool_call_finished("done")
    failed = aw.Event.tool_call_failed("nope")
    assert isinstance(finished, aw.Event)
    assert isinstance(failed, aw.Event)
    assert finished.get_data() == {"output": "done"}
    assert failed.get_data() == {"kind": "execution_failed", "message": "nope"}


def test_named_event_constructors_cover_every_builtin_payload():
    empty = {}
    cases = [
        (aw.Event.run_started(), aw.Event.RUN_STARTED, empty),
        (aw.Event.run_finished("drained"), aw.Event.RUN_FINISHED, {"outcome": "drained"}),
        (aw.Event.task_created(), aw.Event.TASK_CREATED, empty),
        (aw.Event.task_started(), aw.Event.TASK_STARTED, empty),
        (aw.Event.task_finished(), aw.Event.TASK_FINISHED, empty),
        (aw.Event.task_failed(), aw.Event.TASK_FAILED, empty),
        (aw.Event.turn_started(), aw.Event.TURN_STARTED, empty),
        (aw.Event.request_started("model"), aw.Event.REQUEST_STARTED, {"model": "model"}),
        (
            aw.Event.request_finished("model", {"input_tokens": 3, "output_tokens": 5}),
            aw.Event.REQUEST_FINISHED,
            {"model": "model", "usage": {"input_tokens": 3, "output_tokens": 5}},
        ),
        (
            aw.Event.request_failed("model", "connection_failed", "offline"),
            aw.Event.REQUEST_FAILED,
            {"model": "model", "kind": "connection_failed", "message": "offline"},
        ),
        (
            aw.Event.request_retried("model", 2, 4, "rate_limited", "later"),
            aw.Event.REQUEST_RETRIED,
            {
                "model": "model",
                "attempt": 2,
                "max_attempts": 4,
                "kind": "rate_limited",
                "message": "later",
            },
        ),
        (aw.Event.text_chunk_received("hello"), aw.Event.TEXT_CHUNK_RECEIVED, {"content": "hello"}),
        (
            aw.Event.tool_call_repaired("grep", "c-1", "value_mistyped", "fixed"),
            aw.Event.TOOL_CALL_REPAIRED,
            {"tool_name": "grep", "call_id": "c-1", "kind": "value_mistyped", "message": "fixed"},
        ),
        (
            aw.Event.tool_call_declined("grep", "already_delivered"),
            aw.Event.TOOL_CALL_DECLINED,
            {"tool_name": "grep", "kind": "already_delivered"},
        ),
        (
            aw.Event.tool_call_started("grep", "c-1", {"q": "x"}),
            aw.Event.TOOL_CALL_STARTED,
            {"tool_name": "grep", "call_id": "c-1", "input": {"q": "x"}},
        ),
        (aw.Event.tool_call_finished("done"), aw.Event.TOOL_CALL_FINISHED, {"output": "done"}),
        (
            aw.Event.tool_call_failed("nope"),
            aw.Event.TOOL_CALL_FAILED,
            {"kind": "execution_failed", "message": "nope"},
        ),
        (aw.Event.knowledge_written("notes"), aw.Event.KNOWLEDGE_WRITTEN, {"slug": "notes"}),
        (aw.Event.knowledge_read("notes"), aw.Event.KNOWLEDGE_READ, {"slug": "notes"}),
        (aw.Event.knowledge_removed("notes"), aw.Event.KNOWLEDGE_REMOVED, {"slug": "notes"}),
        (aw.Event.knowledge_listed(), aw.Event.KNOWLEDGE_LISTED, empty),
        (
            aw.Event.knowledge_failed("read", "notes", "not_found", "missing"),
            aw.Event.KNOWLEDGE_FAILED,
            {"action": "read", "slug": "notes", "kind": "not_found", "message": "missing"},
        ),
        (
            aw.Event.policy_violated("turns", 10),
            aw.Event.POLICY_VIOLATED,
            {"policy": "turns", "limit": 10},
        ),
        (
            aw.Event.schema_retried(2, 4, "schema_failed", "invalid"),
            aw.Event.SCHEMA_RETRIED,
            {"attempt": 2, "max_attempts": 4, "kind": "schema_failed", "message": "invalid"},
        ),
        (
            aw.Event.compaction_started("proactive", 5),
            aw.Event.COMPACTION_STARTED,
            {"trigger": "proactive", "total": 5},
        ),
        (
            aw.Event.compaction_progress("proactive", 2, 5),
            aw.Event.COMPACTION_PROGRESS,
            {"trigger": "proactive", "completed": 2, "total": 5},
        ),
        (
            aw.Event.compaction_finished("proactive"),
            aw.Event.COMPACTION_FINISHED,
            {"trigger": "proactive"},
        ),
        (
            aw.Event.compaction_failed("reactive", "summarization_failed", "bad reply"),
            aw.Event.COMPACTION_FAILED,
            {"trigger": "reactive", "kind": "summarization_failed", "message": "bad reply"},
        ),
    ]

    assert len(cases) == 28
    for event, name, data in cases:
        assert event.get_name() == name
        assert event.get_data() == data


def test_agent_accepts_a_builtin_tool():
    agent = aw.Agent()
    assert agent.tool(aw.ReadFileTool()) is agent


def test_agent_accepts_a_decorated_function():
    @aw.tool
    def noop(x: str) -> str:
        return x

    agent = aw.Agent()
    assert agent.tool(noop) is agent


def test_agent_accepts_several_tools_at_once():
    agent = aw.Agent()
    assert agent.tools([aw.ReadFileTool(), aw.GrepTool()]) is agent


def test_agent_rejects_a_non_tool_with_type_error():
    with pytest.raises(TypeError):
        aw.Agent().tool("not a tool")
