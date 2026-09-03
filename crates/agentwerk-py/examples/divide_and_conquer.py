"""Divide-and-conquer sum of squares, the Python port of the Rust use case.

Partitions ``[1, N]`` into K subranges and enqueues one task per subrange.
Agents share the labelled Werk, call a ``python`` tool for an exact integer,
and finish with a schema-validated ``{"idx", "partial_sum"}``. The program
aggregates once every task resolves and checks the total against the
closed form ``N(N+1)(2N+1)/6``.

Usage: python divide_and_conquer.py [N] [PARTITIONS] [AGENTS]
"""

import asyncio
import subprocess
import sys
from collections import Counter

from agentwerk import (
    Agent,
    Policy,
    Schema,
    Task,
    Werk,
    TaskTool,
    Event,
    tool,
)

ROLE = """
{context}

You compute one partial sum exactly with the `python` tool.

If the tool fails or returns something other than a single integer, do not invent
a partial sum or call `finish` with an unverified value.

- Each task body gives the bounds `lo`, `hi`, and a partition index `idx`.
- MUST call `python` with `{"code": "print(sum(k*k for k in range(LO, HI + 1)))"}`,
  substituting the bounds from the task.
- Finish the task with `idx` and `partial_sum` as top-level arguments:
  `finish({"idx": IDX, "partial_sum": N})`, copying `idx` verbatim from the task
  and using the integer the tool printed for `N`.
- NEVER add prose, code fences, or commentary outside the `finish` call, because
  text outside it is not returned.
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
    concurrent=True,
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
        return Event(Event.TOOL_CALL_FAILED).data({"message": "missing required field `code`"})
    done = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, timeout=30
    )
    if done.returncode != 0:
        return Event(Event.TOOL_CALL_FAILED).data({"message": f"python error: {done.stderr}"})
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

    werk = Werk().set_policy(Policy(max_turns=20 * len(bounds)))
    # The finish reason is announced once and not kept, so catch it here. The
    # per-tool counts are the same story: the Werk counts the run as a whole,
    # so a breakdown is folded off the events.
    finish_reason = []
    tool_calls, tool_errors = Counter(), Counter()

    def trace(event):
        if event.get_name() in ("task_started", "task_finished", "task_failed"):
            print(f"  {event.get_name():<20} {event.get_agent_id():<10} {event.get_task_id()}")
        elif event.get_name() == "run_finished":
            finish_reason.append(event.get_data()["outcome"])
        elif event.get_name() == "tool_call_started":
            tool_calls[event.get_data()["tool_name"]] += 1
        elif event.get_name() == "tool_call_failed":
            tool_errors[event.get_data()["tool_name"]] += 1

    werk.on_event(trace)

    for a in range(agents):
        werk.add_agent(
            Agent.from_env()
            .role(ROLE.strip())
            .label("compute")
            .tools([python, TaskTool()])
        )

    for idx, (lo, hi) in enumerate(bounds):
        werk.add_task(
            Task(
                f"Compute the partial sum S = sum_{{k={lo}}}^{{{hi}}} k^2.\n"
                f"lo={lo}\nhi={hi}\nidx={idx}",
                label="compute",
                schema=PARTIAL_SUM,
            )
        )

    await werk.finish_all_tasks()

    partials, failures = {}, []
    for task in werk.get_tasks():
        if task.is_finished():
            partials[task.get_result()["idx"]] = task.get_result()["partial_sum"]
        else:
            failures.append((task.get_id(), task.get_status()))

    event_counts = Counter(
        event.get_name() for event in werk.find_events("ORDER BY event.created")
    )
    duration = werk.get_duration() or 0.0
    print(
        f"\nfinished in {duration:.1f}s: "
        f"{event_counts['task_finished']} done, "
        f"{event_counts['task_failed']} failed, "
        f"{werk.get_input_tokens()} in / {werk.get_output_tokens()} out tokens"
    )
    print(f"finish reason  : {finish_reason[-1]}")
    for name in sorted(tool_calls):
        errors = tool_errors[name]
        rate = errors / tool_calls[name]
        print(f"tool {name:<14}: {tool_calls[name]} calls, error rate {rate:.2f}")

    total, expected = sum(partials.values()), closed_form(n)
    print(f"\naggregated sum : {total}")
    print(f"closed form    : {expected}")
    for id, status in failures:
        print(f"x {id} {status}")
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
