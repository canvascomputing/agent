---
name: list_directory
concurrent: true
---

List a directory's entries to survey an unfamiliar layout. Output is one entry per line, sorted alphabetically: a directory ends in `/`, a symlink ends in `@`, a file shows its size as `<name>  <size_bytes> bytes`.

- The suffix marks the type and is not part of the name: listing or reading `foo/` as a path fails.
- In recursive mode `<name>` is relative to `path`. The path resolves against the working directory.

## When NOT to use

- Find files by pattern across the tree: use `glob`.
- Search file contents: use `grep`.
- Read one file: use `read_file`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "Directory to list (default: `.`)."
    },
    "recursive": {
      "type": "boolean",
      "description": "Walk subdirectories and list every entry beneath `path` (default: false). Use sparingly: on a large tree `glob` with a pattern returns far less."
    }
  },
  "examples": [
    { "path": "src" },
    { "path": "src", "recursive": true }
  ]
}
```
