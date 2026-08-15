---
name: glob
concurrent: true
---

Find files in the working directory tree by glob pattern. Returns paths relative to the base, one per line, newest-modified first, capped at 200.

- Narrow the pattern rather than paginating: results past the 200th are dropped, not queued.
- An absolute path in `path` escapes the working directory.

## When NOT to use

- Search file contents: use `grep`.
- List one directory non-recursively: use `list_directory`.

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
