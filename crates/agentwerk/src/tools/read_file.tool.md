---
name: read_file
read_only: true
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

## Schema

```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "Path to the file to read."
    },
    "offset": {
      "type": "integer",
      "description": "1-based line number to start reading from (default: 1)."
    },
    "limit": {
      "type": "integer",
      "description": "Number of lines to read (default: to end of file). Combine with `offset` to read a slice."
    },
    "column": {
      "type": "integer",
      "description": "1-based byte offset within each line to start reading from (default: full line). Compatible with the column reported by `grep` in content mode. To see context around a match, subtract a margin (e.g. `column = hit_column - 50`)."
    },
    "length": {
      "type": "integer",
      "description": "Max bytes to read per line from `column` (default: to end of line)."
    }
  },
  "required": [
    "path"
  ]
}
```
