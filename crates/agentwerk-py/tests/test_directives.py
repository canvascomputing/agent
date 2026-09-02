import agentwerk as aw
from agentwerk import Directive


def test_every_key_is_a_constant_on_the_directive():
    assert Directive.GREP_FAILED == "grep_failed"
    assert Directive.REPLY_REJECTED == "reply_rejected"


def test_tool_timeout_is_the_only_timeout_directive():
    assert Directive.TOOL_TIMED_OUT == "tool_timed_out"
    assert not hasattr(Directive, "COMMAND_TIMED_OUT")
    assert not hasattr(Directive, "GREP_TIMED_OUT")


def test_a_function_binds_to_an_agent():
    configured = aw.Agent().directives(lambda key: None)

    assert isinstance(configured, aw.Agent)


def test_two_agents_take_functions_of_their_own():
    assert isinstance(aw.Agent().directives(lambda key: "one"), aw.Agent)
    assert isinstance(aw.Agent().directives(lambda key: "other"), aw.Agent)
