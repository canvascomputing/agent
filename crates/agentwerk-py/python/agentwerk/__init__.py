"""agentwerk: a minimal Python library for running many agents in parallel.

The compiled extension (`._agentwerk`) holds the real types, and this package
re-exports them so `from agentwerk import Agent, ReadFileTool` works.
"""

from ._agentwerk import (
    Agent,
    Compaction,
    Event,
    Knowledge,
    Model,
    Page,
    Pages,
    Provider,
    Schema,
    SchemaStore,
    Reply,
    ReplyContent,
    Ticket,
    TicketQueue,
    Tool,
    ToolResult,
    Trajectory,
    Anthropic,
    OpenAi,
    Mistral,
    LiteLlm,
    ReadFileTool,
    WriteFileTool,
    EditFileTool,
    GrepTool,
    GlobTool,
    ListDirectoryTool,
    FetchUrlTool,
    KnowledgeTool,
    TicketsTool,
    FinishTool,
    CommandTool,
    event_names,
    Directive,
)

# The names an `Event.kind` reports, as
# constants rather than literals. Built from the crate's list so the two
# cannot end up spelling a kind differently.
EventName = type(
    "EventName",
    (),
    {name.upper(): name for name in event_names()},
)
EventName.__doc__ = "Every event kind's name, the spelling `Event.kind` reports."


def tool(
    func=None,
    *,
    concurrent=False,
    paths=None,
    schema=None,
    name=None,
    description=None,
):
    """Turn a Python function into a tool an agent may call.

    Write ``@tool`` or ``@tool(concurrent=True, schema={...})``. The name
    defaults to the function's, and the description to its docstring. The input
    arrives as keyword arguments. ``paths`` names the input fields holding a
    file path, so the files a call opens are included in statistics.
    """

    def decorate(fn):
        fn._agentwerk_tool = True
        fn._agentwerk_name = name or fn.__name__
        fn._agentwerk_description = description or (fn.__doc__ or "").strip()
        fn._agentwerk_concurrent = concurrent
        fn._agentwerk_paths = list(paths or [])
        fn._agentwerk_schema = schema if schema is not None else {"type": "object"}
        return fn

    # Support both @tool and @tool(...).
    return decorate if func is None else decorate(func)


__all__ = [
    "tool",
    "Agent",
    "Compaction",
    "Directive",
    "Event",
    "EventName",
    "Knowledge",
    "Model",
    "Page",
    "Pages",
    "Provider",
    "Schema",
    "SchemaStore",
    "Reply",
    "ReplyContent",
    "Ticket",
    "TicketQueue",
    "Tool",
    "ToolResult",
    "Trajectory",
    "Anthropic",
    "OpenAi",
    "Mistral",
    "LiteLlm",
    "ReadFileTool",
    "WriteFileTool",
    "EditFileTool",
    "GrepTool",
    "GlobTool",
    "ListDirectoryTool",
    "FetchUrlTool",
    "KnowledgeTool",
    "TicketsTool",
    "FinishTool",
    "CommandTool",
]
