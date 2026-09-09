import asyncio

import agentwerk as aw


def test_directive_is_not_a_public_namespace():
    assert not hasattr(aw, "Directive")


async def test_override_values_are_rendered_into_retry_requests(
    werk, scripted_openai
):
    scripted_openai.respond_with_text("still thinking")
    scripted_openai.respond_with_tool("finish", {"result": "done"})
    agent = (
        aw.Agent()
        .provider(scripted_openai.provider())
        .model("mock")
        .directive(
            "reply_rejected",
            "Attempt {{ attempt }} of {{ max_attempts }} must call a tool.",
        )
    )
    werk.set_policy(aw.Policy(max_schema_retries=3)).add_agent(agent)
    werk.add_task("go")

    await asyncio.wait_for(werk.finish(), timeout=5)

    retry_messages = scripted_openai.requests[1]["messages"]
    assert retry_messages[-1]["content"] == "Attempt 1 of 3 must call a tool."


def test_bulk_overrides_bind_to_an_agent():
    agent = aw.Agent()
    configured = agent.directives(
        {
            "tool_timed_out": "Reduce the command scope.",
            "cache_miss": "No cache entry exists for {{ path }}.",
        }
    )

    assert configured is agent
