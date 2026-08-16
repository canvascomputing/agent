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

## Schema

```json
{
  "type": "object",
  "properties": {
    "action": {
      "type": "string",
      "enum": [
        "ticket",
        "result",
        "list",
        "search",
        "create",
        "edit"
      ],
      "description": "Read: `ticket`, `result`, `list`, `search`. Write: `create`, `edit`."
    },
    "key": {
      "type": "string",
      "description": "Ticket key (e.g. `TICKET-3`). Used by `ticket`, `result`, `edit`. Defaults to the agent's current ticket. Ignored by `create`, `list`, `search`."
    },
    "status": {
      "type": "string",
      "description": "For `list`: filter by status. One of `Todo`, `InProgress`, `Finished`, `Failed`."
    },
    "label": {
      "type": "string",
      "description": "For `list`: filter to tickets carrying this label (case-sensitive). For `create` or `edit` (optional): the ticket's label scope, which decides who picks it up: every agent serving that label may claim it. A ticket carries at most one label, and on `edit` the new one replaces the current one."
    },
    "query": {
      "type": "string",
      "description": "For `search`: case-insensitive substring matched against the task body."
    },
    "task": {
      "type": ["string", "object", "array", "number", "boolean"],
      "description": "For `create` (required) or `edit` (optional): the task body, any JSON value (string, object, array, scalar)."
    }
  },
  "required": [
    "action"
  ],
  "allOf": [
    {
      "if": {
        "required": ["action"],
        "properties": { "action": { "const": "search" } }
      },
      "then": { "required": ["query"] }
    },
    {
      "if": {
        "required": ["action"],
        "properties": { "action": { "const": "create" } }
      },
      "then": { "required": ["task"] }
    }
  ],
  "examples": [
    { "action": "ticket" },
    { "action": "result", "key": "TICKET-3" },
    { "action": "list", "status": "Todo", "label": "review" },
    { "action": "search", "query": "retry budget" },
    {
      "action": "create",
      "task": "Check the retry budget on the upload path.",
      "label": "review"
    },
    { "action": "edit", "key": "TICKET-3", "label": "review" }
  ]
}
```
