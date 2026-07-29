---
name: finish
read_only: false
---

Record your final answer and mark your current ticket `Finished`. Call it once, as the last action of the work: until you do, the ticket is unfinished and you will be asked for it again.

- Pass your answer as the arguments: when the ticket's schema is an object, its fields go at the top level; otherwise use `result`. Omit `result` when there is nothing to report.
- Add `handover` to finish and pass work on to another agent in one call: a new `Todo` ticket goes to that agent or scope with this ticket as its `parent`.
- The new ticket's body defaults to your `result`; pass `task` only to tell the receiving agent something beyond it.

## When NOT to use

- Create or edit other tickets without finishing this one: use `manage_tickets`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "result": {
      "description": "Your final answer as a JSON value; omit when there is nothing to report. With `handover` it is required: `null` or an empty string is rejected."
    },
    "handover": {
      "type": "string",
      "description": "Who picks up the follow-up ticket: an agent's name pins it to that agent, a scope label assigns it to any agent in that scope. Becomes a label on the new ticket. Omit to finish without passing work on to anyone."
    },
    "task": {
      "type": "string",
      "description": "Body of the follow-up ticket, for a `handover` only; when omitted it is your `result`. Pass it to tell the receiving agent something beyond the result. `{parent_key}` and `{parent_result}` are substituted with this ticket's key and result; unknown `{name}` placeholders pass through verbatim."
    }
  }
}
```
