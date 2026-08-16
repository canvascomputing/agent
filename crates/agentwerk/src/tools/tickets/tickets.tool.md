---
name: tickets
concurrent: false
---

Read and mutate the ticket queue from one tool: `ticket` / `result` / `list` / `search` to read, `create` / `edit` to write. One `action` per call; a `key` defaults to your current ticket, and `create` stamps `reporter` from the calling agent. `ticket` answers with a markdown ticket block, `result` with a finished ticket's result and the file holding it, which is how you pick up what another agent produced. `list` and `search` answer with a bullet summary and cap at 50 tickets, so tighten filters rather than re-running; `search` matches the task body only, not the label or the result.

- This tool cannot transition status: finish with `finish` (`Failed` is reserved for system outcomes like a schema-retry trip or policy violation).
- ALWAYS finish your current ticket with `finish` before the response ends, or it stays `InProgress` and the loop re-picks it.

## When NOT to use

- Finish your current ticket: call `finish`.
- Find code or files: use `grep` / `glob`.
