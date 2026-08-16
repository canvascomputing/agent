---
name: grep
concurrent: true
---

Search file contents under the working directory. `pattern` is a regular expression by default; set `syntax: "code"` to match a call or code shape without escaping. Every file is searched, including hidden and gitignored ones, so narrow with the `path`, `glob`, or `type` arguments.

- Prefer a few narrow parallel searches over one broad one.
- `output_mode`: `files_with_matches` (default) returns file names, `content` returns `path:line:col: text` lines, `count` returns per-file counts.
- Case-sensitive unless `-i`. Results stop at `head_limit` (default 250); page with `offset`.

## Regex vs code

Regex (default), for text that varies:
- `TODO|FIXME`: either marker
- `v\d+\.\d+\.\d+`: a version like `v1.2.3`

Code (`"syntax": "code"`), for a call or code shape. No escaping, whitespace ignored; `...` is any arguments, `$NAME` captures a name, `$...NAME` a multi-token span:
- `console.log(...)`: every call to one method
- `$FN(...)` with `"constraints": [{"metavariable": "FN", "regex": "^(min|max|abs)$"}]`: a family of calls, the capture pinned to a set. A capture shows as `[$FN=value]` in content mode.

In code mode `[a-z]`, `*`, `.`, and `\` are literal, and `<word>` is plain text, not a placeholder. `-A`, `-B`, `-C`, `context`, and `multiline` are regex-only: passing one in code mode has no effect.

## When NOT to use

- Find files by name: use `glob`.
- Read a known file: use `read_file`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "minLength": 1,
      "description": "Regular expression over file contents. For a fixed call or code shape, use `syntax: \"code\"` instead of escaping."
    },
    "path": {
      "type": "string",
      "description": "File or directory to search under, relative to the working directory. Omit for the whole tree."
    },
    "glob": {
      "type": "string",
      "description": "Glob filter, e.g. `*.rs` or `*.{ts,tsx}`. A `/` makes it match paths, not just file names."
    },
    "output_mode": {
      "type": "string",
      "enum": ["content", "files_with_matches", "count"],
      "description": "`files_with_matches` (default), `content` (lines with path/line/column), or `count` (per-file counts)."
    },
    "-A": {
      "type": "integer",
      "description": "Context lines after each match (content mode)."
    },
    "-B": {
      "type": "integer",
      "description": "Context lines before each match (content mode)."
    },
    "-C": {
      "type": "integer",
      "description": "Context lines before and after each match (content mode). Alias for `context`."
    },
    "context": {
      "type": "integer",
      "description": "Context lines before and after each match (content mode)."
    },
    "-n": {
      "type": "boolean",
      "description": "Show line numbers in content mode (default true)."
    },
    "-i": {
      "type": "boolean",
      "description": "Case-insensitive (default false)."
    },
    "type": {
      "type": "string",
      "description": "File-type filter, e.g. `rust`, `py`, `js`."
    },
    "head_limit": {
      "type": "integer",
      "description": "Max results (default 250; `0` means unlimited)."
    },
    "offset": {
      "type": "integer",
      "description": "Skip the first N results (default 0), to page through more."
    },
    "multiline": {
      "type": "boolean",
      "description": "Let `.` match newlines (default false). Regex mode only."
    },
    "syntax": {
      "type": "string",
      "enum": ["regex", "code"],
      "description": "`regex` (default), or `code` for code-shape matching with `$NAME`, `$...NAME`, and `...` metavariables."
    },
    "constraints": {
      "type": "array",
      "description": "Code mode only: keep a match only when each named capture matches its regex.",
      "items": {
        "type": "object",
        "properties": {
          "metavariable": {
            "type": "string",
            "description": "Capture name to test, e.g. `FUNC` (leading `$` optional)."
          },
          "regex": {
            "type": "string",
            "description": "Regex the capture must match. Anchor with `^...$`."
          }
        },
        "required": ["metavariable", "regex"]
      }
    }
  },
  "required": [
    "pattern"
  ]
}
```
