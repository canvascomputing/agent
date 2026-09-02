import agentwerk as aw


def test_directive_is_not_a_public_namespace():
    assert not hasattr(aw, "Directive")


def test_one_override_binds_to_an_agent():
    agent = aw.Agent()
    configured = agent.directive("grep_failed", "Search failed for {path}.")

    assert configured is agent


def test_bulk_overrides_bind_to_an_agent():
    agent = aw.Agent()
    configured = agent.directives(
        {
            "tool_timed_out": "Reduce the command scope.",
            "cache_miss": "No cache entry exists for {path}.",
        }
    )

    assert configured is agent
