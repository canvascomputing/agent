---
name: bash
read_only: false
---

Execute a shell command via `sh -c` in the working directory and return its trimmed stdout and stderr. Registration pins a glob pattern; a command that does not match is rejected and nothing runs.

- One command family per registration (e.g. `git *`). The pattern and the read-only status are the operator's choice, not yours.
- Default timeout is 120 000 ms; request up to 600 000 ms via `timeout_ms`.
- Treat as destructive by default: side effects depend on the registered pattern (`git status` is read-only, `git push` is not).

## When NOT to use

- Read a file: `read_file`. List a directory: `list_directory`. Find files by name: `glob`. Search contents: `grep`. Edit a file: `edit_file` / `write_file`.
- Never run `grep`, `rg`, `find`, `ls`, `cat`, or `sed` here: the dedicated tools are faster, structured, and cite line numbers.

## Schema

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "The bash command to execute. Must match the registered glob pattern; otherwise the call fails before execution."
    },
    "timeout_ms": {
      "type": "integer",
      "description": "Per-command timeout in milliseconds (default: 120000, max: 600000). The process is killed on timeout."
    }
  },
  "required": [
    "command"
  ]
}
```
