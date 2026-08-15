---
name: finish
concurrent: false
---

Record your final answer and mark your current ticket `Finished`. Call it once, as the last action of the work: until you do, the ticket is unfinished and you'll be asked for it again.

- Pass your answer as the arguments: when the ticket's schema is an object, its fields go at the top level; otherwise use `result`. Omit `result` when there is nothing to report.
- Add `handover` to finish and pass work to another agent in one call: a new `Todo` ticket goes to that agent or scope with this ticket as `parent`.
- The new ticket's body defaults to your `result`; pass `task` to describe the work instead. Either way it gains a last line, `Handed over from <this ticket>, result file: <path>`, so the receiver can read your whole result.

## When NOT to use

- Create or edit other tickets without finishing this one: use `tickets`.

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
      "description": "Who picks up the follow-up ticket: a scope label assigns it to the agents serving that label. Becomes the new ticket's label. Omit to finish without passing work on to anyone."
    },
    "task": {
      "type": "string",
      "description": "Body of the follow-up ticket, for a `handover` only; when omitted it is your `result`. Pass it to tell the receiving agent something beyond the result. `{parent_key}`, `{parent_result}`, and `{parent_result_path}` are substituted with this ticket's key, result, and result file; unknown `{name}` placeholders pass through verbatim."
    }
  }
}
```
