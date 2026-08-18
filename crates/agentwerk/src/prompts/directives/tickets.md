<!-- What the ticket tools say when a key, a result, or a handover is missing. -->

## ticket_queue_unavailable
No ticket queue is available here, so no ticket can be read or changed.

## ticket_key_missing
`key` is missing and this call carries no agent, so there is no ticket to act on. Name the ticket with `key`.

## ticket_not_assigned
`key` is missing and you hold no ticket, so there is nothing to act on. Name the ticket with `key`.

## ticket_not_found
No ticket {key}. The `list` action shows every ticket that exists.

## ticket_result_missing
Ticket {key} has no result yet, it is {status}. Read it again once it is finished.

## ticket_query_invalid
{error}

## ticket_edit_incomplete
An edit needs at least one of `task` or `label`. Give the one you want changed.

## ticket_transition_rejected
{error}

## handover_result_missing
A handover needs a result to pass on. Call `finish` again with a `result`, or without `handover` to finish without passing work on.

## finish_argument_blank
`{argument}` must be a non-blank string when given, got {value}. Give text, or leave it out.
