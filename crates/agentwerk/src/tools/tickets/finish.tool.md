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
