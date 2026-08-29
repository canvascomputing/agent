Record your final answer and mark your current task `Finished`. Call it once, as the last action of the work: until you do, the task is unfinished and you'll be asked for it again.

- Pass your answer as `result`, matching the schema this tool declares for it. Emit it as its native JSON type, never as a JSON-encoded string. A handover needs a real `result`: `null` or an empty string is rejected. Omit `result` only when the schema allows it and there is nothing to report.
- Add `handover` to finish and pass work to another agent in one call: a new `Todo` task goes to that agent or scope with this task as `parent`.
- The new task's body defaults to your `result`; pass `task` to describe the work instead. Either way it gains a last line, `Handed over from <this task>, result file: <path>`, so the receiver can read your whole result.

## When NOT to use

- Create or edit other tasks without finishing this one: use `tasks`.
