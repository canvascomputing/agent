Read and write the ticket queue. One `action` per call.

- `ticket` answers with a markdown ticket block.
- `result` answers with a finished ticket's result and the file holding it: how you pick up what another agent produced.
- `list` answers with a bullet summary, capped at 50 tickets. Narrow the `aql` or order it with `ORDER BY` rather than re-running it.
- `create` opens a ticket and stamps you as its `reporter`. `edit` replaces the task or label of one that exists.
- This tool cannot transition status: finish with `finish` (`Failed` is reserved for system outcomes like a schema-retry trip or policy violation).
- ALWAYS finish your current ticket with `finish` before the response ends, or it stays `InProgress` and the loop re-picks it.

## When NOT to use

- Finish your current ticket: call `finish`.
- Find code or files: use `grep` / `glob`.
