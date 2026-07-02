---
name: finish_ticket
read_only: false
---

Record your final answer and mark your current ticket `Finished` (terminal). Call it once, as the last action that completes the work: without it the ticket stays `InProgress` and the loop re-runs it.

- Pass your answer as this tool's arguments: an object `schema` takes its fields directly; otherwise use `result`, omitted when there is nothing to report.

## When NOT to use

- Create or edit other tickets: use `manage_tickets_tool`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "result": {
      "description": "Your final answer as a JSON value; omit when there is nothing to report."
    }
  }
}
```
