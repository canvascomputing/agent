---
name: manage_tickets
read_only: false
---

Read and mutate the ticket queue from one tool: `get` / `list` / `search` to read, `create` / `edit` to write. One `action` per call; a `key` defaults to your current ticket when omitted, and `create` stamps `reporter` from the calling agent. `list` and `search` cap at 50 tickets.

- This tool cannot transition status: finish with `finish` (`Failed` is reserved for system outcomes like a schema-retry trip or policy violation).
- ALWAYS finish your current ticket with `finish` before the response ends, or it stays `InProgress` and the loop re-picks it.

## When NOT to use

- Reads only: register `read_tickets` (smaller surface, fewer mistakes).
- Finish your current ticket: call `finish`.
- Find code or files: use `grep` / `glob`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "action": {
      "type": "string",
      "description": "Read: `get`, `list`, `search`. Write: `create`, `edit`."
    },
    "key": {
      "type": "string",
      "description": "Ticket key (e.g. `TICKET-3`). Used by `get`, `edit`. Defaults to the agent's current ticket. Ignored by `create`, `list`, `search`."
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
      "description": "For `create` (required) or `edit` (optional): the task body, any JSON value (string, object, array, scalar)."
    }
  },
  "required": [
    "action"
  ]
}
```
