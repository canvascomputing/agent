<!-- What the task tools say when a key, a result, or a handover is missing. -->

## queue_unavailable
No task queue is available here, so no task can be read or changed.

## task_key_missing
`key` is missing and this call carries no agent, so there is no task to act on. Name the task with `key`.

## task_not_assigned
`key` is missing and you hold no task, so there is nothing to act on. Name the task with `key`.

## task_not_found
No task {key}. The `list` action shows every task that exists.

## task_result_missing
Task {key} has no result yet, it is {status}. Read it again once it is finished.

## task_query_invalid
{error}

## task_edit_incomplete
An edit needs at least one of `task` or `label`. Give the one you want changed.

## task_transition_rejected
{error}

## handover_result_missing
A handover needs a result to pass on. Call `finish` again with a `result`, or without `handover` to finish without passing work on.

## finish_argument_blank
`{argument}` must be a non-blank string when given, got {value}. Give text, or leave it out.
