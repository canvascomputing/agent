---
name: read_file
concurrent: true
---

Read a file's contents, returning line-numbered text so you can cite `file:line` and target edits. Output is `<line_no>\t<line>` from line 1; the path resolves against the working directory.

- For large files, pass `offset` and `limit` to read only the slice you need.
- To read around a `grep` hit, set `column`/`length` (e.g. `column = hit_col - 50`); output then becomes `<line_no>:<col>\t<slice>`.
- A directory `path` returns its entries instead of file lines: read one of them next.
- ALWAYS read a file before editing it; `edit_file` refuses otherwise.

## When NOT to use

- List a directory: use `list_directory`.
- Find files by name: use `glob`.
- Search many files: use `grep`.
