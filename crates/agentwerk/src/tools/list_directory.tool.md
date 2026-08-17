List a directory's entries to survey an unfamiliar layout. Output is one entry per line, sorted alphabetically: a directory ends in `/`, a symlink ends in `@`, a file shows its size as `<name>  <size_bytes> bytes`.

- The suffix marks the type and is not part of the name: listing or reading `foo/` as a path fails.
- In recursive mode `<name>` is relative to `path`. The path resolves against the working directory.

## When NOT to use

- Find files by pattern across the tree: use `glob`.
- Search file contents: use `grep`.
- Read one file: use `read_file`.
