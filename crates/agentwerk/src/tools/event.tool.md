Publish an event for your current task and agent. The event reaches queue handlers, queries, statistics, and the event log.

- Pass its `name` and optional JSON `data`; omitted data becomes an empty object. Lowercase snake case is conventional, but every custom or built-in name is accepted.
- `task_finished` is terminal: put your final answer in `data.result`, matching the schema declared there. Add `data.handover` and optional `data.task` to create a child task exactly as `finish` does.
- Every other name only reports what happened. Even `task_failed` does not change the task's status.

Call `task_finished` once, as the last action of completed work. Use other names whenever an observer needs a durable application event.
