Finish the current task and return its final result. Call it once, after the work is complete.

- Follow the arguments shown for the current task. Pass final-answer fields at the top level when they appear there; when `result` appears, put the final answer in `result`.
- Pass native JSON values, NEVER JSON written inside a string, because encoding a value as text makes the call invalid.
- Pass `handover` only to supply or change the follow-up fields shown for this call. Omit it otherwise.
