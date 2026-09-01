"""A minimal Python library for running many agents in parallel.

This package re-exports the compiled extension so you can import its API directly from ``agentwerk``.
"""

from ._agentwerk import (
    Agent,
    Policy,
    Event,
    Knowledge,
    Model,
    Page,
    Pages,
    Provider,
    Query,
    Schema,
    Reply,
    ReplyContent,
    Task,
    Werk,
    Tool,
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
    FetchTool,
    KnowledgeTool,
    TaskTool,
    EventTool,
    FinishTool,
    CommandTool,
    Directive,
)


def tool(
    func=None,
    *,
    concurrent=False,
    schema=None,
    name=None,
    description=None,
):
    """Turn a Python function into a tool an agent may call.

    Write ``@tool`` or ``@tool(concurrent=True, schema={...})``. The name
    defaults to the function's, and the description to its docstring. The input
    arrives as keyword arguments.
    """

    def decorate(fn):
        fn._agentwerk_tool = True
        fn._agentwerk_name = name or fn.__name__
        fn._agentwerk_description = description or (fn.__doc__ or "").strip()
        fn._agentwerk_concurrent = concurrent
        fn._agentwerk_schema = schema if schema is not None else {"type": "object"}
        return fn

    return decorate if func is None else decorate(func)


__all__ = [
    "tool",
    "Agent",
    "Policy",
    "Directive",
    "Event",
    "Knowledge",
    "Model",
    "Page",
    "Pages",
    "Provider",
    "Query",
    "Schema",
    "Reply",
    "ReplyContent",
    "Task",
    "Werk",
    "Tool",
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
    "FetchTool",
    "KnowledgeTool",
    "TaskTool",
    "EventTool",
    "FinishTool",
    "CommandTool",
]
