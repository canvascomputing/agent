<!-- What the tool registry says about a call it could not dispatch, and what stands in for a result that does not fit. -->

## tool_not_found
No tool named `{{ name }}`. Call one of: {{ available }}. A name outside that list never resolves.

## no_tools_registered
No tool named `{{ name }}`. No tools are registered here, so no call resolves.

## tool_panicked
`{{ tool }}` did not finish: it panicked. Its work did not happen; call it again or take another route.

## tool_timed_out
Tool `{{ tool }}` timed out after {{ milliseconds }}ms.

## tool_output_empty
({{ tool }} completed with no output)

## tool_output_offloaded
Output too large ({{ size }}). Full output saved to: {{ path }}
Preview (first {{ preview_size }}):
{{ preview }}
