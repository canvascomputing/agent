Read and mutate the ticket queue from one tool: `ticket` / `result` / `list` to read, `create` / `edit` to write. One `action` per call; a `key` defaults to your current ticket, and `create` stamps `reporter` from the calling agent. `ticket` answers with a markdown ticket block, `result` with a finished ticket's result and the file holding it, which is how you pick up what another agent produced. `list` answers with a bullet summary and caps at 50 tickets, so narrow the `aql` rather than re-running it.

- This tool cannot transition status: finish with `finish` (`Failed` is reserved for system outcomes like a schema-retry trip or policy violation).
- ALWAYS finish your current ticket with `finish` before the response ends, or it stays `InProgress` and the loop re-picks it.

## When NOT to use

- Finish your current ticket: call `finish`.
- Find code or files: use `grep` / `glob`.
