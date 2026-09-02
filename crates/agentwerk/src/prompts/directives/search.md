<!-- What `grep` and the code search say about a pattern, a glob, or a search that did not finish. -->

## grep_cancelled
Search cancelled: the run is ending.

## grep_failed
The search did not run. Narrow `path` and call `grep` once more; an identical retry fails the same way.

## grep_glob_rejected
`glob` is not a valid glob: {error}. Write it as a path pattern such as `**/*.rs`.

## grep_file_type_unknown
No file type named `{file_type}`: {error}. Drop `file_type` and narrow with `glob` instead.

## grep_pattern_rejected
Search failed: {error}. `pattern` is a regular expression. To find a call or code shape, use `syntax: "code"` (`Name(...)`); otherwise escape the metacharacters.

## code_pattern_rejected
`pattern` is not a valid code pattern: {error}. Write it as source, with `$NAME` where the code varies.

## code_constraint_incomplete
Each entry in `constraints` needs a `metavariable` and a `regex`. Give both, or drop the entry.

## code_constraint_metavariable_unknown
`constraints` names ${name}, which `pattern` does not declare. Constrain a metavariable the pattern writes.

## code_constraint_regex_rejected
The `regex` for ${name} is not a valid regular expression: {error}. Escape the metacharacters and call again.
