"""agentwerk: Python bindings for the agentwerk Rust agent loop.

The compiled extension (`._agentwerk`) holds the real types; this package
re-exports them so `from agentwerk import Agent, ReadFileTool` works.
"""

from ._agentwerk import (
    Agent,
    Event,
    FileStat,
    Knowledge,
    KnowledgeStat,
    Model,
    ModelStat,
    Page,
    Pages,
    Provider,
    Schema,
    Stats,
    Ticket,
    TicketSystem,
    Tool,
    ToolResult,
    ToolStat,
    Trajectory,
    AnthropicProvider,
    OpenAiProvider,
    MistralProvider,
    LiteLlmProvider,
    ReadFileTool,
    WriteFileTool,
    EditFileTool,
    GrepTool,
    GlobTool,
    ListDirectoryTool,
    FetchUrlTool,
    FindToolsTool,
    ManageKnowledgeTool,
    ReadTicketsTool,
    ManageTicketsTool,
    FinishTool,
    BashTool,
    UnrestrictedBashTool,
    provider_from_env,
    model_from_env,
    context_window_from_env,
)


def tool(
    func=None,
    *,
    read_only=False,
    defer=False,
    paths=None,
    schema=None,
    name=None,
    description=None,
):
    """Mark a Python callable as an agent tool.

    Usage: ``@tool`` or ``@tool(read_only=True, schema={...})``. The name
    defaults to the function name and the description to its docstring. The
    tool input object is passed to the callable as keyword arguments. A
    ``defer``red tool stays hidden until the agent finds it with
    ``FindToolsTool``. ``paths`` names the input fields holding a file path, so
    the files the tool opens reach ``Stats.file_stats()``.
    """

    def decorate(fn):
        fn._agentwerk_tool = True
        fn._agentwerk_name = name or fn.__name__
        fn._agentwerk_description = description or (fn.__doc__ or "").strip()
        fn._agentwerk_read_only = read_only
        fn._agentwerk_defer = defer
        fn._agentwerk_paths = list(paths or [])
        fn._agentwerk_schema = schema if schema is not None else {"type": "object"}
        return fn

    # Support both @tool and @tool(...).
    return decorate if func is None else decorate(func)


__all__ = [
    "tool",
    "Agent",
    "Event",
    "FileStat",
    "Knowledge",
    "KnowledgeStat",
    "Model",
    "ModelStat",
    "Page",
    "Pages",
    "Provider",
    "Schema",
    "Stats",
    "Ticket",
    "TicketSystem",
    "Tool",
    "ToolResult",
    "ToolStat",
    "Trajectory",
    "AnthropicProvider",
    "OpenAiProvider",
    "MistralProvider",
    "LiteLlmProvider",
    "ReadFileTool",
    "WriteFileTool",
    "EditFileTool",
    "GrepTool",
    "GlobTool",
    "ListDirectoryTool",
    "FetchUrlTool",
    "FindToolsTool",
    "ManageKnowledgeTool",
    "ReadTicketsTool",
    "ManageTicketsTool",
    "FinishTool",
    "BashTool",
    "UnrestrictedBashTool",
    "provider_from_env",
    "model_from_env",
    "context_window_from_env",
]
