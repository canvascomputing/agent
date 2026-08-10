---
name: read_tickets
read_only: true
---

Read tickets from the queue: `get` by key (defaults to your current ticket), `list` by status or label, or `search` task bodies. `get` returns a markdown ticket block; `list`/`search` return a bullet summary and cap at 50 tickets, so tighten filters rather than re-running. `search` matches the task body only, not labels or results.

## When NOT to use

- Create or edit tickets, or need read AND write in one flow: use `manage_tickets`.
- Find code or files: use `grep` / `glob`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "action": {
      "type": "string",
      "description": "Read mode: `get` (one ticket), `list` (filter by status / label), `search` (free-text)."
    },
    "key": {
      "type": "string",
      "description": "Ticket key for `get` (e.g. `TICKET-3`). Defaults to the agent's current ticket. Ignored by `list` / `search`."
    },
    "status": {
      "type": "string",
      "description": "Filter for `list`: one of `Todo`, `InProgress`, `Finished`, `Failed`. Combine with `label` to narrow further."
    },
    "label": {
      "type": "string",
      "description": "Filter for `list`: only tickets carrying this label (case-sensitive). Pass a scope label to find the tickets the agents serving it work on."
    },
    "query": {
      "type": "string",
      "description": "Free-text query for `search`. Case-insensitive substring match against the task body."
    }
  },
  "required": [
    "action"
  ]
}
```
