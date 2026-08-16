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
