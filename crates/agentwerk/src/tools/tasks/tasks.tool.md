Read and write the task queue. One `action` per call.

- `task` answers with a markdown task block.
- `result` answers with a finished task's result and the file holding it: how you pick up what another agent produced.
- `list` answers with a bullet summary, capped at 50 tasks. Narrow the `aql` or order it with `ORDER BY` rather than re-running it.
- `create` opens a task and stamps you as its `reporter`. `edit` replaces the task or label of one that exists.
- This tool cannot transition status: finish with `finish` (`Failed` is reserved for system outcomes like an exhausted schema-retry budget or a breached limit).
- ALWAYS finish your current task with `finish` before the response ends, or it stays `InProgress` and the loop re-picks it.

## When NOT to use

- Finish your current task: call `finish`.
- Find code or files: use `grep` / `glob`.
