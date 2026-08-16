---
name: command
concurrent: false
---

Execute one command in the working directory and return its output. A command the patterns below do not permit is rejected before it runs.

- No shell: `&`, `|`, `;`, `<`, `>`, `(`, `)`, `$(...)` and backticks are rejected. Make one call per command.
- No expansion: `$HOME`, `~` and `*.txt` reach the program as written.
- Quote an argument containing spaces; use single quotes around text holding a double quote.

## When NOT to use

Reach for the dedicated tool, which is faster, structured, and cites line numbers: `read_file` over `cat`, `list_directory` over `ls`, `glob` over `find`, `grep` over the `grep` and `rg` programs, `edit_file` or `write_file` over `sed`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "One program and its arguments. Must match an allowed pattern and no denied one."
    },
    "timeout_ms": {
      "type": "integer",
      "description": "Per-command timeout in milliseconds (default: 120000, max: 600000). The process is killed on timeout."
    }
  },
  "required": [
    "command"
  ],
  "examples": [
    { "command": "git status --short" },
    { "command": "git log -5 --format=%s" },
    { "command": "cargo test --lib", "timeout_ms": 300000 }
  ]
}
```
