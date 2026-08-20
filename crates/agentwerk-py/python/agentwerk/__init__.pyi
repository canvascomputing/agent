"""Type stubs for the agentwerk Python bindings."""

import os
from typing import Any, Awaitable, Callable, Optional, overload

class Provider:
    """An LLM provider, passed to ``Agent.provider(...)``."""

    @staticmethod
    def from_env() -> "Provider": ...

class Model:
    """A model name, with an optional context window size and reasoning level."""

    name: str

    def __init__(self, name: str) -> None: ...
    @staticmethod
    def from_env() -> "Model": ...
    def context_window(self, size: int) -> "Model": ...
    def reasoning_effort(self, effort: str) -> "Model": ...
    def get_context_window(self) -> Optional[int]: ...
    def get_reasoning_effort(self) -> str: ...

def Anthropic(
    api_key: str, base_url: Optional[str] = ..., timeout: Optional[float] = ...
) -> Provider: ...
def OpenAi(
    api_key: str, base_url: Optional[str] = ..., timeout: Optional[float] = ...
) -> Provider: ...
def Mistral(
    api_key: str, base_url: Optional[str] = ..., timeout: Optional[float] = ...
) -> Provider: ...
def LiteLlm(
    api_key: str, base_url: Optional[str] = ..., timeout: Optional[float] = ...
) -> Provider: ...

class Tool:
    """A tool an agent may call, passed to ``Agent.tool(...)``."""

class ToolResult:
    """What a tool reports back when a bare return value is not enough."""

    @staticmethod
    def success(content: str) -> "ToolResult": ...
    @staticmethod
    def error(content: str) -> "ToolResult": ...

def ReadFileTool() -> Tool: ...
def WriteFileTool() -> Tool: ...
def EditFileTool() -> Tool: ...
def GrepTool() -> Tool:
    """Search file contents by regular expression, or by code shape with ``syntax="code"``."""
    ...
def GlobTool() -> Tool: ...
def ListDirectoryTool() -> Tool: ...
def KnowledgeTool(store: "Knowledge") -> Tool: ...
def TicketsTool() -> Tool: ...
def FinishTool() -> Tool: ...

class FetchUrlTool:
    """Fetch a URL and read its body, passed to ``Agent.tool(...)``."""

    def __init__(self) -> None: ...
    def impersonate(self) -> "FetchUrlTool":
        """Send the headers and HTTP/2 settings a browser sends. The TLS
        handshake is unchanged, so a site reading the ClientHello rather than
        the headers refuses the request either way."""
        ...

class CommandTool:
    """A command an agent may call, passed to ``Agent.tool(...)``."""

    def __init__(self, name: str) -> None: ...
    def allow(self, pattern: str) -> "CommandTool": ...
    def allow_flag(self, flag: str) -> "CommandTool": ...
    def deny(self, pattern: str) -> "CommandTool": ...
    def deny_flag(self, flag: str) -> "CommandTool": ...
    def description(self, description: "str | os.PathLike[str]") -> "CommandTool": ...
    def concurrent(self, concurrent: bool) -> "CommandTool": ...

@overload
def tool(func: Callable[..., Any]) -> Callable[..., Any]: ...
@overload
def tool(
    *,
    concurrent: bool = ...,
    paths: Optional[list[str]] = ...,
    schema: Optional[dict] = ...,
    name: Optional[str] = ...,
    description: Optional["str | os.PathLike[str]"] = ...,
) -> Callable[[Callable[..., Any]], Callable[..., Any]]: ...

class Schema:
    def __init__(self, document: Any) -> None: ...
    def validate(self, value: Any) -> tuple[Any, list[str]]: ...

class SchemaStore:
    def __init__(self) -> None: ...
    def label(self, label: str, document: Any) -> "SchemaStore": ...
    def get(self, label: str) -> Optional[Schema]: ...

class ReplyContent:
    """One block inside a reply: text, a tool call, a tool result, or reasoning."""

    kind: str
    data: dict

    @staticmethod
    def text(text: str) -> "ReplyContent": ...
    @staticmethod
    def tool_use(id: str, name: str, input: Any) -> "ReplyContent": ...
    @staticmethod
    def tool_result(
        tool_use_id: str,
        content: str,
        succeeded: bool = ...,
        path: Optional[str] = ...,
    ) -> "ReplyContent": ...
    @staticmethod
    def thinking(thinking: str, signature: str) -> "ReplyContent": ...
    @staticmethod
    def redacted_thinking(data: str) -> "ReplyContent": ...
    def __repr__(self) -> str: ...

class Reply:
    """One entry in a ticket's replies."""

    author: str
    content: list[ReplyContent]
    created_at: int

    @staticmethod
    def user_text(text: str) -> "Reply": ...
    def __repr__(self) -> str: ...

class Query:
    """Selects tickets by field values, compiled from AQL.

    The string is the same syntax a query argument carries, `ORDER BY <field>
    ASC | DESC` included, and one that does not compile raises `ValueError`.
    """

    def __init__(self, query: str) -> None: ...
    def __repr__(self) -> str: ...

class Ticket:
    key: str
    status: str
    task: Any
    result: Optional[Any]
    label: Optional[str]
    schema: Optional[Schema]
    parent: Optional[str]
    reporter: str
    assignee: Optional[str]
    created_at: int
    started_at: Optional[int]
    finished_at: Optional[int]
    failed_at: Optional[int]
    replies: list[Reply]

    def __init__(
        self,
        task: Any,
        *,
        label: Optional[str] = ...,
        schema: Optional[Schema] = ...,
        parent: Optional[str] = ...,
    ) -> None: ...
    def has_label(self, label: str) -> bool: ...
    def is_todo(self) -> bool: ...
    def is_finished(self) -> bool: ...
    def is_failed(self) -> bool: ...
    def is_in_progress(self) -> bool: ...
    def is_pending(self) -> bool: ...
    def __repr__(self) -> str: ...

class Trajectory:
    key: str
    model: Optional[str]
    replies: list[Reply]

    @staticmethod
    def from_ticket(
        agent_id: str, model: Optional[str], ticket: Ticket
    ) -> "Trajectory": ...
    def save(self, dir: str) -> None: ...
    def __repr__(self) -> str: ...

class Page:
    slug: str
    kind: str
    description: str
    content: str
    tags: list[str]

    def __init__(
        self,
        slug: str,
        description: str,
        content: str,
        kind: str = ...,
        tags: Optional[list[str]] = ...,
    ) -> None: ...
    def __repr__(self) -> str: ...

class Pages:
    def save(self, page: Page) -> None: ...
    def load(self, slug: str) -> Page: ...
    def list(self) -> list[Page]: ...
    def remove(self, slug: str) -> None: ...

class Knowledge:
    @staticmethod
    def load(store_dir: str) -> "Knowledge": ...
    def index_char_limit(self, count: int) -> "Knowledge": ...
    def get_index_char_limit(self) -> int: ...
    def index(self) -> str: ...
    def pages(self) -> Pages: ...
    def clear(self) -> None: ...

class Event:
    kind: str
    created_at: int
    agent_id: str
    ticket_key: str
    label: Optional[str]
    @property
    def data(self) -> dict: ...
    def __repr__(self) -> str: ...

class EventName:
    """Every event kind's name, the spelling `Event.kind` reports."""

    RUN_STARTED: str
    RUN_FINISHED: str
    TICKET_CREATED: str
    TICKET_STARTED: str
    TICKET_FINISHED: str
    TICKET_FAILED: str
    TURN_STARTED: str
    REQUEST_STARTED: str
    REQUEST_FINISHED: str
    REQUEST_FAILED: str
    REQUEST_RETRIED: str
    TEXT_CHUNK_RECEIVED: str
    RESPONSE_REPAIRED: str
    TOOL_CALL_DECLINED: str
    TOOL_CALL_STARTED: str
    TOOL_CALL_FINISHED: str
    TOOL_CALL_FAILED: str
    FILE_OPEN_FINISHED: str
    FILE_OPEN_FAILED: str
    KNOWLEDGE_WRITTEN: str
    KNOWLEDGE_READ: str
    KNOWLEDGE_REMOVED: str
    KNOWLEDGE_LISTED: str
    KNOWLEDGE_FAILED: str
    POLICY_VIOLATED: str
    SCHEMA_RETRIED: str
    COMPACTION_STARTED: str
    COMPACTION_PROGRESS: str
    COMPACTION_FINISHED: str
    COMPACTION_FAILED: str

class Directive:
    """Every directive agentwerk can send, one constant per key."""

    REPLY_REJECTED: str
    NO_TOOL_CALLED: str
    ARGUMENTS_REJECTED: str
    ARGUMENTS_EXPECTED: str
    RESULT_SCHEMA_REQUIRED: str
    SUMMARY_REQUESTED: str
    KNOWLEDGE_INDEX_TRUNCATED: str
    TOOL_NOT_FOUND: str
    NO_TOOLS_REGISTERED: str
    TOOL_PANICKED: str
    TOOL_OUTPUT_EMPTY: str
    TOOL_OUTPUT_OFFLOADED: str
    EDIT_FILE_READ_FAILED: str
    EDIT_FILE_OLD_STRING_NOT_FOUND: str
    EDIT_FILE_OLD_STRING_NOT_UNIQUE: str
    EDIT_FILE_WRITE_FAILED: str
    WRITE_FILE_PARENT_NOT_CREATED: str
    WRITE_FILE_FAILED: str
    READ_FILE_PATH_IS_DIRECTORY: str
    READ_FILE_PATH_IS_DIRECTORY_WITH_ENTRIES: str
    READ_FILE_IS_BINARY: str
    READ_FILE_NOT_FOUND: str
    READ_FILE_FAILED: str
    LIST_DIRECTORY_PATH_IS_FILE: str
    LIST_DIRECTORY_NOT_FOUND: str
    LIST_DIRECTORY_FAILED: str
    PATH_HINT_DIRECTORY_LISTED: str
    PATH_HINT_SUGGESTION: str
    PATH_HINT_WORKING_DIRECTORY: str
    COMMAND_CANCELLED: str
    COMMAND_TIMED_OUT: str
    COMMAND_NOT_STARTED: str
    COMMAND_MISSING: str
    COMMAND_SHELL_OPERATOR_FOUND: str
    COMMAND_QUOTE_UNTERMINATED: str
    COMMAND_CONTROL_CHARACTER_FOUND: str
    COMMAND_ASSIGNMENT_FOUND: str
    COMMAND_FLAG_DENIED: str
    COMMAND_PATTERN_DENIED: str
    COMMAND_NOT_ALLOWED: str
    COMMAND_FLAG_NOT_ALLOWED: str
    GREP_CANCELLED: str
    GREP_TIMED_OUT: str
    GREP_FAILED: str
    GREP_GLOB_REJECTED: str
    GREP_FILE_TYPE_UNKNOWN: str
    GREP_PATTERN_REJECTED: str
    CODE_PATTERN_REJECTED: str
    CODE_CONSTRAINT_INCOMPLETE: str
    CODE_CONSTRAINT_METAVARIABLE_UNKNOWN: str
    CODE_CONSTRAINT_REGEX_REJECTED: str
    FETCH_URL_TOO_LONG: str
    FETCH_URL_SCHEME_MISSING: str
    FETCH_URL_SCHEME_UNSUPPORTED: str
    FETCH_URL_CREDENTIALS_PRESENT: str
    FETCH_URL_HOST_MISSING: str
    FETCH_URL_HOST_NOT_RESOLVABLE: str
    FETCH_URL_TOO_MANY_REDIRECTS: str
    FETCH_URL_REQUEST_FAILED: str
    FETCH_URL_BODY_NOT_READ: str
    FETCH_URL_RESPONSE_TOO_LARGE: str
    FETCH_URL_REDIRECT_LOCATION_MISSING: str
    KNOWLEDGE_PAGE_NOT_FOUND: str
    KNOWLEDGE_WRITE_FAILED: str
    KNOWLEDGE_REMOVE_FAILED: str
    TICKET_QUEUE_UNAVAILABLE: str
    TICKET_KEY_MISSING: str
    TICKET_NOT_ASSIGNED: str
    TICKET_NOT_FOUND: str
    TICKET_RESULT_MISSING: str
    TICKET_QUERY_INVALID: str
    TICKET_EDIT_INCOMPLETE: str
    TICKET_TRANSITION_REJECTED: str
    HANDOVER_RESULT_MISSING: str
    FINISH_ARGUMENT_BLANK: str
    SCHEMA_FALSE_REJECTED: str
    SCHEMA_TYPE_MISMATCHED: str
    SCHEMA_CONST_MISMATCHED: str
    SCHEMA_ENUM_MISMATCHED: str
    SCHEMA_ANY_OF_UNMATCHED: str
    SCHEMA_ONE_OF_AMBIGUOUS: str
    SCHEMA_NOT_MATCHED: str
    SCHEMA_PROPERTY_MISSING: str
    SCHEMA_PROPERTY_UNEXPECTED: str
    SCHEMA_ARRAY_TOO_SHORT: str
    SCHEMA_ARRAY_TOO_LONG: str
    SCHEMA_STRING_TOO_SHORT: str
    SCHEMA_STRING_TOO_LONG: str
    SCHEMA_PATTERN_UNMATCHED: str
    SCHEMA_NUMBER_TOO_SMALL: str
    SCHEMA_NUMBER_TOO_LARGE: str
    SCHEMA_HINT_UNQUOTE: str
    SCHEMA_HINT_JSON: str
    SCHEMA_HINT_QUOTE: str

class Agent:
    """The core entity of agentwerk. It has access to tools for solving tasks in
    the form of tickets."""

    def __init__(self) -> None: ...
    @staticmethod
    def from_env() -> "Agent": ...
    def provider(self, provider: Provider) -> "Agent": ...
    def model(self, model: "str | Model") -> "Agent": ...
    def role(self, role: "str | os.PathLike[str]") -> "Agent": ...
    def label(self, label: str) -> "Agent": ...
    def interactive(self) -> "Agent": ...
    def template(self, key: str, value: str) -> "Agent": ...
    def templates(self, variables: dict[str, str]) -> "Agent": ...
    def dir(self, dir: str) -> "Agent": ...
    def knowledge(self, store: Knowledge) -> "Agent": ...
    def directives(self, compute: Callable[[str], Optional[str]]) -> "Agent": ...
    def tool(self, tool: Any) -> "Agent": ...
    def tools(self, tools: list) -> "Agent": ...
    def build(self) -> "Agent": ...
    @property
    def id(self) -> str: ...
    def ticket(self, ticket: "Ticket | Any") -> str: ...
    def start(self) -> "TicketQueue": ...

class Policy:
    max_turns: Optional[int]
    max_input_tokens: Optional[int]
    max_output_tokens: Optional[int]
    max_request_tokens: Optional[int]
    max_schema_retries: Optional[int]
    max_request_retries: int
    request_retry_delay: float
    max_time: Optional[float]
    compaction_threshold: Optional[float]

    def __init__(
        self,
        *,
        max_turns: Optional[int] = ...,
        max_input_tokens: Optional[int] = ...,
        max_output_tokens: Optional[int] = ...,
        max_request_tokens: Optional[int] = ...,
        max_schema_retries: Optional[int] = ...,
        max_request_retries: Optional[int] = ...,
        request_retry_delay: Optional[float] = ...,
        max_time: Optional[float] = ...,
        compaction_threshold: Optional[float] = ...,
    ) -> None: ...

class TicketQueue:
    def __init__(self) -> None: ...
    @staticmethod
    def load(tickets_dir: str) -> "TicketQueue": ...
    def agent(self, agent: Agent) -> "TicketQueue": ...
    def ticket(self, ticket: "Ticket | Any") -> str: ...
    def reply(self, key: str, content: str) -> "TicketQueue": ...
    def set_finished(self, key: str, result: Any) -> None: ...
    def set_failed(self, key: str) -> None: ...
    def policy(self, policy: "Policy") -> "TicketQueue": ...
    def get_policy(self) -> "Policy": ...
    def dir(self, dir: str) -> "TicketQueue": ...
    def get_dir(self) -> str: ...
    def schemas(self, store: SchemaStore) -> "TicketQueue": ...
    def on_event(
        self, handler: Callable[["TicketQueue", Event], Any]
    ) -> "TicketQueue": ...
    def on_event_async(
        self, handler: Callable[["TicketQueue", Event], Awaitable[Any]]
    ) -> "TicketQueue": ...
    def on_result(
        self, handler: Callable[["TicketQueue", Ticket, Any], Any]
    ) -> "TicketQueue": ...
    def on_result_async(
        self, handler: Callable[["TicketQueue", Ticket, Any], Awaitable[Any]]
    ) -> "TicketQueue": ...
    def on_failure(
        self, handler: Callable[["TicketQueue", Event, Ticket], Any]
    ) -> "TicketQueue": ...
    def on_failure_async(
        self, handler: Callable[["TicketQueue", Event, Ticket], Awaitable[Any]]
    ) -> "TicketQueue": ...
    def on_ticket(
        self, handler: Callable[["TicketQueue", Event, Ticket], Any]
    ) -> "TicketQueue": ...
    def on_ticket_async(
        self, handler: Callable[["TicketQueue", Event, Ticket], Awaitable[Any]]
    ) -> "TicketQueue": ...
    def edit_replies(
        self, key: str, editor: Callable[[list[Reply]], Optional[list[Reply]]]
    ) -> "TicketQueue": ...
    def model_for_agent(self, agent_id: str) -> Optional[str]: ...
    def get_ticket(self, key: str) -> Optional[Ticket]: ...
    def tickets(self) -> list[Ticket]: ...
    def find_tickets(
        self, predicate: "Query | str | Callable[[Ticket], bool]"
    ) -> list[Ticket]: ...
    def find_ticket(
        self, predicate: "Query | str | Callable[[Ticket], bool]"
    ) -> Optional[Ticket]: ...
    def find_events(self, predicate: Callable[[Event], bool]) -> list[Event]: ...
    def find_event(self, predicate: Callable[[Event], bool]) -> Optional[Event]: ...
    def input_tokens(self) -> int: ...
    def output_tokens(self) -> int: ...
    def execution_duration(self) -> Optional[float]: ...
    def start(self) -> "TicketQueue": ...
    async def finish(
        self, matches: "Query | str | Callable[[Ticket], bool]"
    ) -> list[Any]: ...
    async def finish_all(self) -> list[Any]: ...
    async def finish_last(self) -> Optional[Any]: ...
    def finish_reason(self) -> Optional[str]: ...
    def cancel(
        self, matches: "Query | str | Callable[[Ticket], bool]"
    ) -> "TicketQueue": ...
    def cancel_all(self) -> "TicketQueue": ...
    def is_cancelled(self, ticket: Ticket) -> bool: ...
    def results(self) -> list[Any]: ...
    def find_results(
        self, query: "Query | str | Callable[[Ticket], bool]"
    ) -> list[Any]: ...
    def find_result(
        self, query: "Query | str | Callable[[Ticket], bool]"
    ) -> Optional[Any]: ...
