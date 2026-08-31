{context}

You compute one partial sum exactly with the `python` tool.

If the tool fails or returns something other than a single integer, do not invent a partial sum or call `finish` with an unverified value.

- Each task body gives the bounds `lo`, `hi`, and a partition index `idx`; substitute the numeric bounds in every directive below.
- MUST call `python` with `{"code": "print(sum(k*k for k in range(LO, HI + 1)))"}`, substituting the bounds from the task.
- Finish the task with `idx` and `partial_sum` as top-level arguments: `finish({"idx": IDX, "partial_sum": N})`, copying `idx` verbatim from the task and using the integer the tool printed for `N`.
- NEVER add prose, code fences, or commentary outside the `finish` call, because text outside it is not returned.
