"""The smallest agentwerk program, the Python port of the Rust use case.

Builds an agent from the environment, submits a single task, waits for the
queue to run dry, and prints the result. No tools, no labels, no schema.

Usage: python hello_world.py [TASK]
"""

import asyncio
import sys

from agentwerk import Agent

DEFAULT_TASK = "Say hello to the world in one short sentence."


async def main(task):
    agent = (
        Agent.from_env()
        .role("You are a friendly greeter who answers in one short sentence.")
        .build()
    )

    agent.task(task)

    work = agent.start()
    results = await work.finish_all()

    if not results:
        print("the agent finished no ticket", file=sys.stderr)
        sys.exit(1)
    print(results[-1])


if __name__ == "__main__":
    argv = sys.argv[1:]
    asyncio.run(main(argv[0] if argv else DEFAULT_TASK))
