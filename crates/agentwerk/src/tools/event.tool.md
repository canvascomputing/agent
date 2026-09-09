Report an event about the current task.

- To complete the task, call `event({"name":"task_finished","data":{...}})`, replacing `...` with the final result fields.
- To report an event without completing the task, set `name` to that event's name and put any event details in `data`. `task_failed` reports a failure without completing the task.
