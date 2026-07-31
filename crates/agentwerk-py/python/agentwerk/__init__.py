"""agentwerk: a minimal Python library for running many agents in parallel.

The compiled extension (`._agentwerk`) holds the real types, and this package
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
    Reply,
    ReplyContent,
    Ticket,
    TicketQueue,
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
    """Turn a Python function into a tool an agent may call.

    Write ``@tool`` or ``@tool(read_only=True, schema={...})``. The name
    defaults to the function's, and the description to its docstring. The input
    arrives as keyword arguments. ``defer`` holds the tool back until the agent
    looks it up with ``FindToolsTool``. ``paths`` names the input fields holding
    a file path, so the files a call opens are included in statistics.
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
    "Reply",
    "ReplyContent",
    "Ticket",
    "TicketQueue",
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
