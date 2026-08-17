Replace one or more occurrences of a string inside an existing file, preserving the rest byte-for-byte. `old_string` must match exactly, including indentation and whitespace. Returns how many occurrences were replaced.

- ALWAYS `read_file` first; otherwise `old_string` is a guess, likely absent or ambiguous.
- Default mode replaces one occurrence and fails if `old_string` is not unique: widen it with surrounding context or set `replace_all`. It also fails when `old_string` is not found at all.
- The overwrite is in place; recovery needs version control or a prior backup. `path` resolves against the working directory.

## When NOT to use

- Create a new file or fully rewrite one: use `write_file`.
