"""Divide-and-conquer sum of squares, the Python port of the Rust use case.

Partitions ``[1, N]`` into K subranges and enqueues one ticket per subrange.
Agents share the labelled queue, call a ``python`` tool for an exact integer,
and finish with a schema-validated ``{"idx", "partial_sum"}``. The driver
aggregates once every ticket resolves and checks the total against the
closed form ``N(N+1)(2N+1)/6``.

Usage: python divide_and_conquer.py [N] [PARTITIONS] [AGENTS]
"""

import asyncio
import subprocess
import sys
from collections import Counter

from agentwerk import Agent, ManageTicketsTool, Schema, Ticket, TicketQueue, ToolResult, tool

ROLE = """
{context}

You are a precise arithmetic agent in a divide-and-conquer pipeline who computes
one partial sum exactly using the `python` tool.

If the tool fails or returns something other than a single integer, say so rather
than guess.

- Each task body gives the bounds `lo`, `hi`, and a partition index `idx`.
- MUST call `python` with `{"code": "print(sum(k*k for k in range(LO, HI + 1)))"}`,
  substituting the bounds from the task.
- Finish the ticket by calling `finish` with `idx` and `partial_sum` as its
  top-level arguments, copying `idx` verbatim from the task.
- NEVER add prose, code fences, or commentary outside the `finish` call.
"""

PARTIAL_SUM = Schema(
    {
        "type": "object",
        "properties": {
            "idx": {
                "type": "integer",
                "description": "Partition index, copied verbatim from the task",
            },
            "partial_sum": {
                "type": "integer",
                "description": "Exact integer value of the partial sum",
            },
        },
        "required": ["idx", "partial_sum"],
        "additionalProperties": False,
    }
)


@tool(
    read_only=True,
    schema={
        "type": "object",
        "properties": {
            "code": {
                "type": "string",
                "description": "Python 3 source. Must print the result to stdout.",
            }
        },
        "required": ["code"],
    },
)
def python(code: str = "") -> object:
    """Run a short Python 3 snippet and return its stdout, trimmed. Use this for
    exact integer arithmetic."""
    if not code:
        return ToolResult.error("missing required field `code`")
    done = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, timeout=30
    )
    if done.returncode != 0:
        return ToolResult.error(f"python error: {done.stderr}")
    return done.stdout.strip()


def partition(n, k):
    k = max(1, min(k, n))
    base, extra = divmod(n, k)
    bounds, lo = [], 1
    for i in range(k):
        hi = lo + base + (1 if i < extra else 0) - 1
        bounds.append((lo, hi))
        lo = hi + 1
    return bounds


def closed_form(n):
    return n * (n + 1) * (2 * n + 1) // 6


async def main(n, partitions, agents):
    bounds = partition(n, partitions)
    agents = min(agents, len(bounds))
    print(f"sum_{{k=1}}^{{{n}}} k^2 over {len(bounds)} partitions, {agents} agent(s)\n")

    tickets = TicketQueue().max_turns(20 * len(bounds))
    # The finish reason is announced once and not kept, so catch it here. The
    # per-tool counts are the same story: the queue counts the run as a whole,
    # so a breakdown is folded off the events.
    finish_reason = []
    tool_calls, tool_errors = Counter(), Counter()

    def trace(event):
        if event.kind in ("ticket_started", "ticket_finished", "ticket_failed"):
            print(f"  {event.kind:<20} {event.agent_id:<10} {event.ticket_key}")
        elif event.kind == "run_finished":
            finish_reason.append(event.data["reason"])
        elif event.kind == "tool_call_started":
            tool_calls[event.data["tool_name"]] += 1
        elif event.kind == "tool_call_failed":
            tool_errors[event.data["tool_name"]] += 1

    tickets.on_event(trace)

    for a in range(agents):
        tickets.agent(
            Agent.from_env()
            .role(ROLE.strip())
            .label("compute")
            .tools([python, ManageTicketsTool()])
            .build()
        )

    for idx, (lo, hi) in enumerate(bounds):
        tickets.ticket(
            Ticket(
                f"Compute the partial sum S = sum_{{k={lo}}}^{{{hi}}} k^2.\n"
                f"lo={lo}\nhi={hi}\nidx={idx}",
                label="compute",
                schema=PARTIAL_SUM,
            )
        )

    await tickets.finish_all()

    partials, failures = {}, []
    for ticket in tickets.tickets():
        if ticket.is_finished():
            partials[ticket.result["idx"]] = ticket.result["partial_sum"]
        else:
            failures.append((ticket.key, ticket.status))

    stats = tickets.stats()
    print(
        f"\nfinished in {stats.execution_duration():.1f}s: "
        f"{stats.event_count('ticket_finished')} done, "
        f"{stats.event_count('ticket_failed')} failed, "
        f"{stats.input_tokens()} in / {stats.output_tokens()} out tokens"
    )
    print(f"finish reason  : {finish_reason[-1]}")
    for name in sorted(tool_calls):
        errors = tool_errors[name]
        rate = errors / tool_calls[name]
        print(f"tool {name:<14}: {tool_calls[name]} calls, error rate {rate:.2f}")

    total, expected = sum(partials.values()), closed_form(n)
    print(f"\naggregated sum : {total}")
    print(f"closed form    : {expected}")
    for key, status in failures:
        print(f"x {key} {status}")
    if failures or total != expected:
        sys.exit(1)
    print("verified")


if __name__ == "__main__":
    argv = sys.argv[1:]
    asyncio.run(
        main(
            int(argv[0]) if len(argv) > 0 else 10_000,
            int(argv[1]) if len(argv) > 1 else 16,
            int(argv[2]) if len(argv) > 2 else 8,
        )
    )
