"""A minimal Python library for running many agents in parallel.

This package re-exports the compiled extension so you can import its API directly from ``agentwerk``.
"""

import collections.abc
import inspect
import math
import types
import typing

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
)


_TIMEOUT_UNSET = object()
_MAX_TIMEOUT_SECONDS = float(1 << 64)


def _annotation_schema(annotation):
    if annotation is inspect.Parameter.empty or annotation is typing.Any:
        return {}
    if annotation is None or annotation is type(None):
        return {"type": "null"}

    primitive = {
        str: "string",
        bool: "boolean",
        int: "integer",
        float: "number",
    }
    if annotation in primitive:
        return {"type": primitive[annotation]}

    origin = typing.get_origin(annotation)
    arguments = typing.get_args(annotation)
    annotated = getattr(typing, "Annotated", None)
    if annotated is not None and origin is annotated:
        return _annotation_schema(arguments[0]) if arguments else {}

    union_origins = [typing.Union]
    union_type = getattr(types, "UnionType", None)
    if union_type is not None:
        union_origins.append(union_type)
    if origin in union_origins:
        return {"anyOf": [_annotation_schema(argument) for argument in arguments]}

    literal = getattr(typing, "Literal", None)
    if literal is not None and origin is literal:
        values = [
            value
            for value in arguments
            if value is None or isinstance(value, (str, bool, int, float))
        ]
        return {"enum": values} if len(values) == len(arguments) and values else {}

    if origin is tuple and not (len(arguments) == 2 and arguments[1] is Ellipsis):
        if annotation is typing.Tuple:
            return {"type": "array", "items": {}}
        schema = {
            "type": "array",
            "minItems": len(arguments),
            "maxItems": len(arguments),
        }
        if arguments:
            schema["prefixItems"] = [
                _annotation_schema(argument) for argument in arguments
            ]
        return schema

    sequences = {
        list,
        tuple,
        set,
        frozenset,
        collections.abc.Sequence,
    }
    if origin in sequences or annotation in sequences:
        item = {}
        if arguments:
            if len(arguments) == 2 and arguments[1] is Ellipsis:
                item = _annotation_schema(arguments[0])
            elif len(arguments) == 1:
                item = _annotation_schema(arguments[0])
            else:
                item = {"anyOf": [_annotation_schema(argument) for argument in arguments]}
        return {"type": "array", "items": item}

    mappings = {dict, collections.abc.Mapping}
    if origin in mappings or annotation in mappings:
        return {"type": "object"}
    return {}


def _tool_signature(fn):
    signature = inspect.signature(fn)
    positional_only = [
        parameter.name
        for parameter in signature.parameters.values()
        if parameter.kind is inspect.Parameter.POSITIONAL_ONLY
    ]
    if positional_only:
        raise TypeError(
            "@tool cannot use positional-only parameter "
            f"{positional_only[0]!r}; agentwerk calls tools with keyword arguments"
        )
    return signature


def _signature_schema(fn, signature):
    try:
        try:
            hints = typing.get_type_hints(fn, include_extras=True)
        except TypeError:
            hints = typing.get_type_hints(fn)
    except (AttributeError, NameError, SyntaxError, TypeError):
        hints = {}
        namespace = getattr(fn, "__globals__", {})
        for parameter in signature.parameters.values():
            if not isinstance(parameter.annotation, str):
                continue
            try:
                hints[parameter.name] = eval(parameter.annotation, namespace)
            except (AttributeError, NameError, SyntaxError, TypeError):
                pass

    properties = {}
    required = []
    for parameter in signature.parameters.values():
        if parameter.kind in (
            inspect.Parameter.VAR_POSITIONAL,
            inspect.Parameter.VAR_KEYWORD,
        ):
            continue
        annotation = hints.get(parameter.name, parameter.annotation)
        properties[parameter.name] = _annotation_schema(annotation)
        if parameter.default is inspect.Parameter.empty:
            required.append(parameter.name)

    schema = {"type": "object", "properties": properties}
    if required:
        schema["required"] = required
    return schema


def _timeout_seconds(timeout):
    if timeout is None:
        raise TypeError("timeout must be a number of seconds; use 0 for no timeout")
    try:
        seconds = float(timeout)
    except OverflowError as error:
        raise ValueError("timeout is too large to represent as a duration") from error
    except (TypeError, ValueError) as error:
        raise TypeError("timeout must be a number of seconds") from error
    if not math.isfinite(seconds) or seconds < 0:
        raise ValueError("timeout must be a finite, non-negative number of seconds")
    if seconds >= _MAX_TIMEOUT_SECONDS:
        raise ValueError("timeout is too large to represent as a duration")
    return seconds


def tool(
    func=None,
    *,
    concurrent=False,
    schema=None,
    name=None,
    description=None,
    timeout=_TIMEOUT_UNSET,
):
    """Turn a Python function into a tool an agent may call.

    Write ``@tool`` or ``@tool(concurrent=True, schema={...}, timeout=30)``.
    The name defaults to the function's, the description to its docstring, and
    the schema to one inferred from its signature. The input arrives as keyword
    arguments. A timeout of zero means no timeout.
    """

    def decorate(fn):
        signature = _tool_signature(fn)
        tool_name = name or fn.__name__
        tool_description = description or (fn.__doc__ or "").strip()
        tool_schema = schema if schema is not None else _signature_schema(fn, signature)
        tool_timeout = (
            _TIMEOUT_UNSET
            if timeout is _TIMEOUT_UNSET
            else _timeout_seconds(timeout)
        )

        fn._agentwerk_tool = True
        fn._agentwerk_name = tool_name
        fn._agentwerk_description = tool_description
        fn._agentwerk_concurrent = concurrent
        fn._agentwerk_schema = tool_schema
        if tool_timeout is not _TIMEOUT_UNSET:
            fn._agentwerk_timeout = tool_timeout
        return fn

    return decorate if func is None else decorate(func)


__all__ = [
    "tool",
    "Agent",
    "Policy",
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
