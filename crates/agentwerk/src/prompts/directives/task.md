<!-- What task operations say when an ID, a result, or a handover is missing. -->

## werk_unavailable
Task actions are unavailable here, so no task can be read or changed.

## task_id_missing
No task was selected. Provide its `id` and retry.

## task_not_assigned
No task is assigned to you. Provide a task `id` and retry.

## task_not_found
No task with `id` {id} exists. Use `list` to see the available tasks.

## task_result_missing
Task {id} is {status} and has no result yet. Read it again after it finishes.

## task_query_invalid
{error}

## task_edit_incomplete
An edit requires `task`, `label`, or both. Provide the fields to change and retry.

## task_transition_rejected
{error}

## handover_result_missing
A follow-up requires a non-null, non-empty result. Retry the completion call with a value that meets those requirements.

## handover_schema_invalid
`handover.schema` is not a valid JSON Schema: {error}. Correct that field or omit it.
