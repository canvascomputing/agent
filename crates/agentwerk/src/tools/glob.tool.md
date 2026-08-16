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
