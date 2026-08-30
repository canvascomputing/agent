"""Built-in tools and the @tool decorator, at the Agent.tool interface."""

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


@pytest.mark.parametrize("factory", BUILTIN_FACTORIES)
def test_builtin_factories_return_a_tool(factory):
    assert isinstance(factory(), aw.Tool)


def test_command_tool_configuration_chains_on_one_object():
    tool = aw.CommandTool("git")
    assert tool.allow("git *") is tool
    assert tool.allow_flag("--oneline") is tool
    assert tool.deny("git push*") is tool
    assert tool.deny_flag("--force") is tool
    assert tool.concurrent(True) is tool
    assert tool.description("Run git commands.") is tool


def test_command_tool_description_reads_a_path_as_the_file_holding_it(tmp_path):
    description = tmp_path / "git.tool.md"
    description.write_text("Run git commands.\n")
    tool = aw.CommandTool("git")
    assert tool.description(description) is tool


def test_tool_decorator_reads_a_description_from_a_path(tmp_path):
    description = tmp_path / "sample.tool.md"
    description.write_text("Describe the sample.\n")

    @aw.tool(description=description)
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


def test_an_agent_accepts_a_fetch_tool():
    agent = aw.Agent().tool(aw.FetchTool().impersonate())
    assert isinstance(agent, aw.Agent)


def test_fetch_url_tool_is_not_a_compatibility_alias():
    assert not hasattr(aw, "FetchUrlTool")


def test_tool_decorator_records_name_doc_and_concurrent():
    @aw.tool(concurrent=True)
    def sample(path: str) -> str:
        """Describe the sample."""
        return path

    assert sample._agentwerk_name == "sample"
    assert sample._agentwerk_description == "Describe the sample."
    assert sample._agentwerk_concurrent is True


def test_tool_decorator_records_path_fields():
    @aw.tool(concurrent=True, paths=["path"])
    def cat(path: str) -> str:
        """Read a file."""
        return path

    assert cat._agentwerk_paths == ["path"]
    agent = aw.Agent()
    assert agent.tool(cat) is agent


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
        (
            aw.Event.file_open_finished("src/lib.rs", "read_file", "c-1"),
            aw.Event.FILE_OPEN_FINISHED,
            {"path": "src/lib.rs", "tool_name": "read_file", "call_id": "c-1"},
        ),
        (
            aw.Event.file_open_failed("missing.rs", "read_file", "c-2", "not_found", "missing"),
            aw.Event.FILE_OPEN_FAILED,
            {
                "path": "missing.rs",
                "tool_name": "read_file",
                "call_id": "c-2",
                "kind": "not_found",
                "message": "missing",
            },
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

    assert len(cases) == 30
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
