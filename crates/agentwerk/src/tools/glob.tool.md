---
name: glob_tool
read_only: true
---

Find files in the working directory tree by glob pattern. Returns paths relative to the base, one per line, newest-modified first, capped at 200; narrow the pattern rather than paginating. Absolute paths in `path` escape the working directory.

## When NOT to use

- Search file contents: use `grep`.
- List one directory non-recursively: use `list_directory_tool`.
- Open-ended exploration over multiple rounds: delegate to `agent_tool`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "description": "Glob pattern, evaluated relative to `path` (e.g. `**/*.rs`, `src/*.toml`). Required."
    },
    "path": {
      "type": "string",
      "description": "Base directory to search under (default: `.`, i.e. the agent's working directory)."
    }
  },
  "required": [
    "pattern"
  ]
}
```
