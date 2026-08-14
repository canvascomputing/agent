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
    aw.FetchUrlTool,
    aw.FindToolsTool,
    aw.TicketsTool,
    aw.FinishTool,
]


@pytest.mark.parametrize("factory", BUILTIN_FACTORIES)
def test_builtin_factories_return_a_tool(factory):
    assert isinstance(factory(), aw.Tool)


def test_the_unrestricted_bash_tool_is_configurable_like_any_other():
    assert isinstance(aw.UnrestrictedBashTool().read_only(True), aw.BashTool)


def test_bash_tool_configuration_chains_on_one_object():
    tool = aw.BashTool("git")
    assert tool.allow("git *") is tool
    assert tool.deny("git push*") is tool
    assert tool.read_only(True) is tool
    assert tool.description("Run git commands.") is tool


def test_an_agent_accepts_a_bash_tool():
    agent = aw.Agent().tool(aw.BashTool("git").allow("git *"))
    assert isinstance(agent, aw.Agent)


def test_tool_decorator_records_name_doc_and_read_only():
    @aw.tool(read_only=True)
    def sample(path: str) -> str:
        """Describe the sample."""
        return path

    assert sample._agentwerk_name == "sample"
    assert sample._agentwerk_description == "Describe the sample."
    assert sample._agentwerk_read_only is True
    assert sample._agentwerk_defer is False


def test_tool_decorator_records_defer():
    @aw.tool(defer=True)
    def deep(query: str) -> str:
        """Search deeply."""
        return query

    assert deep._agentwerk_defer is True


def test_tool_decorator_records_path_fields():
    @aw.tool(read_only=True, paths=["path"])
    def cat(path: str) -> str:
        """Read a file."""
        return path

    assert cat._agentwerk_paths == ["path"]
    agent = aw.Agent()
    assert agent.tool(cat) is agent


def test_knowledge_tool_binds_a_store(knowledge_dir):
    store = aw.Knowledge.load(knowledge_dir)
    assert isinstance(aw.KnowledgeTool(store), aw.Tool)


def test_tool_result_constructors_produce_a_tool_result():
    for result in (
        aw.ToolResult.success("done"),
        aw.ToolResult.error("nope"),
        aw.ToolResult.schema_error("bad input"),
    ):
        assert isinstance(result, aw.ToolResult)


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
