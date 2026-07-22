# agentwerk (Python)

Python bindings for [agentwerk](https://github.com/canvascomputing/agentwerk), a
minimal Rust crate for building LLM agents. The Rust crate runs the agent loop;
this package is a thin veneer over it.

```python
import asyncio
from agentwerk import Agent, ReadFileTool, GrepTool


async def main():
    agent = (
        Agent()
        .from_env()
        .role("You are a Rust developer who explores source files to answer questions.")
        .tool(ReadFileTool())
        .tool(GrepTool())
        .build()
    )

    agent.task("Find every `pub trait` defined under src/ and explain each in one sentence.")
    work = await agent.finish()

    print(work.last_result())


asyncio.run(main())
```

Set a provider via environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`,
`MISTRAL_API_KEY`, or `LITELLM_API_KEY`), the same as the Rust crate.
