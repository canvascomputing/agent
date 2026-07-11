---
name: grep_tool
read_only: true
---

Search file contents for a literal substring (not a regex) under the working directory. Skips `.git`, `target`, `node_modules`, `vendor`. ALWAYS use this for content search; never run `grep`/`rg` via `bash_tool`.

- Scope with `glob` (`*.rs`, `src/*.py`) to search fewer files.
- `output_mode`: `content` (default) gives `<path>:<line>:<col>: <line>`; `files` lists matching paths.
- In `content` mode long lines truncate to ~200 bytes around the match; read full context with `read_file_tool` and `column`/`length`.
- Results cap at 100; narrow `pattern` or `glob` rather than re-running.

## When NOT to use

- Find files by name, not contents: use `glob_tool`.
- Open-ended searches needing multiple rounds: delegate to `agent_tool`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "description": "Substring to search for. Literal match; not a regex."
    },
    "glob": {
      "type": "string",
      "description": "File filter supporting `*` and `?`. A bare pattern matches file names (`*.rs`); one with a `/` matches paths relative to the working directory (`src/*.rs`, with `*` crossing directories)."
    },
    "output_mode": {
      "type": "string",
      "description": "What to return: `content` (default) gives matching lines with file path, line number, and column; `files` gives distinct paths that contain the match."
    },
    "case_insensitive": {
      "type": "boolean",
      "description": "Match without regard to case (default: false)."
    }
  },
  "required": [
    "pattern"
  ]
}
```
