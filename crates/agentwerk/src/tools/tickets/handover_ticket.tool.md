---
name: handover_ticket
read_only: false
---

Finish the current ticket and hand follow-up work to another agent in one atomic call: it writes this ticket's `result`, marks it `Finished` (terminal), and creates a `Todo` child pinned to `to` with this ticket as its `parent`. This is the only way to finish-and-chain in one turn.

- `to` labels the child (an agent name pins it, a scope label routes it); `task` is its plain-text instruction; an optional `schema` rides along to validate the child's result.
- Pass your own answer alongside `to`/`task`: an object `schema` takes its fields directly, otherwise use `result`.

## When NOT to use

- Record a result with no follow-up: call `finish_ticket`.
- Create or edit tickets without finishing this one: call `manage_tickets_tool`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "to": {
      "type": "string",
      "description": "Target for the child ticket: either an agent's name (pins the ticket to that agent) or a scope label (routes it to any agent in that scope). Becomes a label on the child."
    },
    "task": {
      "type": "string",
      "description": "Body of the child ticket as a plain-text instruction. MUST be a non-empty string. Describe what the receiving agent should do next. Reserved placeholders `{parent_key}` and `{parent_result}` are substituted at handover time with the finishing ticket's key and result string; unknown `{name}` placeholders pass through verbatim."
    },
    "result": {
      "description": "Final answer for the current ticket before the handover: any JSON value, validated against this ticket's own schema. A plain-text summary when this ticket has no schema. `null` or an empty string is rejected."
    },
    "schema": {
      "type": "object",
      "description": "Optional JSON Schema document attached to the child ticket. When set, the receiving agent's final result must validate against it; failures count toward `max_schema_retries`."
    }
  },
  "required": [
    "to",
    "task",
    "result"
  ]
}
```
