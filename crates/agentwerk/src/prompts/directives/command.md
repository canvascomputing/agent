<!-- What the command tool says when a line is not one runnable program, or the rules refuse it. -->

## command_cancelled
Command cancelled: the run is ending.

## command_timed_out
`{program}` was stopped after {milliseconds}ms. Narrow the work before calling it again, because the same command times out again.

## command_not_started
`{program}` could not be started: {error}. Check the program name before calling again.

## command_missing
`command` is empty. Give the one program to run.

## command_shell_operator_found
Command '{command}' holds the shell operator `{operator}`. This tool runs one program directly, with no shell, so make one call per command.

## command_quote_unterminated
Command '{command}' ends inside a quote or an escape. Close it before calling again.

## command_control_character_found
Command '{command}' holds a control character. Remove it before calling again.

## command_assignment_found
Command '{command}' sets an environment variable. This tool runs one program with the environment it was started in, so drop the assignment.

## command_flag_denied
Command '{command}' carries the denied flag '{flag}'. Call it without that flag.

## command_pattern_denied
Command '{command}' matches the denied pattern '{pattern}'. Call something this tool permits.

## command_not_allowed
Command '{command}' is not allowed by tool '{tool}'. {allowed}

## command_flag_not_allowed
Command '{command}' carries the flag '{flag}', which tool '{tool}' does not allow. Allowed flags: {allowed}, and no other.
