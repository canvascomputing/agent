"""Provide shared fixtures and skip live tests without a provider.

Offline tests make no external requests. Tests marked ``live`` need a real
LLM provider and are skipped automatically when none is configured.
"""

import json
import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

import agentwerk as aw

PROVIDER_ENV_KEYS = (
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "MISTRAL_API_KEY",
    "LITELLM_API_KEY",
)


class ScriptedOpenAi:
    """Serve queued tool calls through the public OpenAI provider binding."""

    def __init__(self):
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                owner.requests.append(json.loads(self.rfile.read(length)))
                if not owner.responses:
                    self.send_error(500, "scripted responses exhausted")
                    return
                body = owner.responses.pop(0)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, _format, *_args):
                pass

        self.requests = []
        self.responses = []
        self._next_call = 1
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(
            target=lambda: self._server.serve_forever(poll_interval=0.01),
            daemon=True,
        )
        self._thread.start()

    def respond_with_tool(self, name, arguments):
        call_id = f"call-{self._next_call}"
        self._next_call += 1
        chunk = {
            "model": "mock",
            "choices": [
                {
                    "index": 0,
                    "delta": {
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": json.dumps(arguments),
                                },
                            }
                        ]
                    },
                    "finish_reason": "tool_calls",
                }
            ],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1},
        }
        self.responses.append(
            f"data: {json.dumps(chunk)}\n\ndata: [DONE]\n\n".encode()
        )

    def provider(self):
        host, port = self._server.server_address
        return aw.OpenAi("test-key", base_url=f"http://{host}:{port}")

    def close(self):
        self._server.shutdown()
        self._server.server_close()
        self._thread.join()


def has_provider() -> bool:
    """Return whether the environment configures a supported provider."""
    return any(os.environ.get(key) for key in PROVIDER_ENV_KEYS)


def pytest_collection_modifyitems(config, items):
    """Skip every ``live`` test when no provider is configured."""
    if has_provider():
        return
    skip = pytest.mark.skip(reason="no LLM provider configured")
    for item in items:
        if "live" in item.keywords:
            item.add_marker(skip)


@pytest.fixture
def werk(tmp_path):
    """Provide an empty Werk with an isolated session directory."""
    return aw.Werk().set_dir(str(tmp_path))


@pytest.fixture
def offline_agent():
    """Provide an agent that supports offline configuration and query tests."""
    return (
        aw.Agent()
        .provider(aw.Anthropic("test-key"))
        .model("claude-sonnet-4-20250514")
    )


@pytest.fixture
def scripted_openai():
    """Provide deterministic model tool calls over a loopback HTTP server."""
    server = ScriptedOpenAi()
    try:
        yield server
    finally:
        server.close()


@pytest.fixture
def live_agent():
    """Provide an agent from the environment for ``live`` tests."""
    return aw.Agent.from_env().role("You answer in one short word.")


@pytest.fixture
def knowledge_dir(tmp_path):
    """Provide a temporary directory for an Open Knowledge Format bundle."""
    return str(tmp_path / "kb")
