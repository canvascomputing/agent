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
    aw.CodegrepTool,
    aw.FetchUrlTool,
    aw.ReadTicketsTool,
    aw.ManageTicketsTool,
    aw.UnrestrictedBashTool,
]


@pytest.mark.parametrize("factory", BUILTIN_FACTORIES)
def test_builtin_factories_return_a_tool(factory):
    assert isinstance(factory(), aw.Tool)


def test_bash_tool_takes_a_name_and_pattern():
    assert isinstance(aw.BashTool("git", "git *"), aw.Tool)


def test_tool_decorator_records_name_doc_and_read_only():
    @aw.tool(read_only=True)
    def sample(path: str) -> str:
        """Describe the sample."""
        return path

    assert sample._agentwerk_name == "sample"
    assert sample._agentwerk_description == "Describe the sample."
    assert sample._agentwerk_read_only is True


def test_agent_accepts_a_builtin_tool():
    assert isinstance(aw.Agent().tool(aw.ReadFileTool()), aw.Agent)


def test_agent_accepts_a_decorated_function():
    @aw.tool
    def noop(x: str) -> str:
        return x

    assert isinstance(aw.Agent().tool(noop), aw.Agent)


def test_agent_rejects_a_non_tool_with_type_error():
    with pytest.raises(TypeError):
        aw.Agent().tool("not a tool")
