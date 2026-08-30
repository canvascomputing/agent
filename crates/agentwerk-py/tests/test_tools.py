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
    aw.TasksTool,
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


def test_fetch_url_tool_configuration_chains_on_one_object():
    tool = aw.FetchUrlTool()
    assert tool.impersonate() is tool


def test_an_agent_accepts_a_fetch_url_tool():
    agent = aw.Agent().tool(aw.FetchUrlTool().impersonate())
    assert isinstance(agent, aw.Agent)


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
    finished = aw.Event(aw.Event.TOOL_CALL_FINISHED).data({"output": "done"})
    failed = aw.Event(aw.Event.TOOL_CALL_FAILED).data({"message": "nope"})
    assert isinstance(finished, aw.Event)
    assert isinstance(failed, aw.Event)


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
