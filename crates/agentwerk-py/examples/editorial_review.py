"""Route a draft through an editor with a result hook, then select it with AQL.

Usage: python editorial_review.py [TEXT]
"""

import asyncio
import sys

from agentwerk import Agent, Task, Werk

DRAFT = "draft"
EDIT = "edit"
FINAL_EDIT = "task.label = edit AND task.status = finished"
DEFAULT_TEXT = "Announce a new software release in two sentences."


async def main(text: str) -> None:
    werk = Werk()
    werk.add_agent(
        Agent.from_env()
        .label(DRAFT)
        .role("Write the requested draft. Return only the drafted text.")
    )
    werk.add_agent(
        Agent.from_env()
        .label(EDIT)
        .role("Edit the draft for clarity and brevity. Return only the final text.")
    )

    def route_to_editor(callback_werk: Werk, task: Task, result: object) -> None:
        if task.get_label() == DRAFT:
            callback_werk.add_task(Task(result, label=EDIT))

    werk.on_result(route_to_editor)
    werk.add_task(Task(text, label=DRAFT))
    await werk.finish()

    result = werk.find_result(FINAL_EDIT)
    if result is None:
        raise RuntimeError("the editor produced no result")
    print(result)


if __name__ == "__main__":
    asyncio.run(main(" ".join(sys.argv[1:]) or DEFAULT_TEXT))
