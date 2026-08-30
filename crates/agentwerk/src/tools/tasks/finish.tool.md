Record your final result and mark the current task `Finished`. Call this once, after completing the work. Without it, the task remains unfinished and you will be prompted again.

- Pass an object result directly: `{"verdict":"safe"}`.
- For any other JSON value, pass `{"result": <value>}`. Use this form also when an object result contains `result`, `handover`, or `task`, or when using the options below.
- Results must match this tool's schema. Pass native JSON values, not JSON-encoded strings. Use `{}` to finish without a result.
- `handover` finishes this task and creates a child `Todo` for the named agent or scope. It requires a result other than `null` or an empty string.
- `task` sets the child's body; otherwise, the result is used. The body ends with `Handed over from <this task>, result file: <path>` so the receiver can find the full result.

## When NOT to use

- Create or edit other tasks without finishing this one: use `tasks`.
