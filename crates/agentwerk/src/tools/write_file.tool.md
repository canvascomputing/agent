---
name: write_file
concurrent: false
---

Create a new file or overwrite an existing one in one shot (no append mode); parent directories are created automatically. `path` resolves against the working directory. Returns a one-line confirmation.

- ALWAYS `read_file` an existing file before overwriting it, or you are guessing what you destroy. The overwrite is recoverable only from version control or a backup.
- Prefer `edit_file` for changes to an existing file: it sends only the diff and is much cheaper. Reach here only to create or fully rewrite.

## When NOT to use

- Modify part of an existing file: use `edit_file`.
