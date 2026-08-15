---
name: knowledge
concurrent: false
---

Read or write pages in your knowledge: durable facts shared across tickets and agents, injected into every ticket's system prompt. The store is an Open Knowledge Format (OKF) bundle. `write` creates or replaces a whole page, `read` loads one body, `list` shows every page with its one-line description.

- Save a durable fact later tickets need; one topic per page, with a descriptive slug (`deployment-config`, `pkg-utils-py`).
- `description` is a one-sentence index line under 80 chars. Cross-link pages with `[text](/pages/slug.md)`.
- `write` overwrites the page and requires `slug`, `description`, and `content`: omitting any is the most common error. Read first if you mean to append.
- Only `read` a slug you have seen listed, whether in your knowledge or in the index file it points at: an unseen slug does not exist and the call fails.
- `remove` is a rarely needed cleanup for a page that turned out wrong. Prefer `write` to correct it in place, since removing it takes the page from every other agent too.

## When NOT to use

- Task progress or TODOs: those belong on tickets.
- A new page per tiny fact: consolidate related facts.

## Schema

```json
{
  "type": "object",
  "properties": {
    "action": {
      "type": "string",
      "enum": [
        "write",
        "read",
        "remove",
        "list"
      ],
      "description": "The operation to perform. 'write' creates or replaces a page (requires slug + description + content). 'read' returns the full page body (requires slug; only valid for slugs in the index). 'list' returns the current index. 'remove' deletes a page (requires slug); rarely needed, prefer 'write' to correct a page."
    },
    "slug": {
      "type": "string",
      "description": "Page identifier (lowercase, hyphens, max 60 chars). Required for write, read, and remove; omitting it returns an error. For file paths, replace dots and slashes with hyphens (e.g. pkg/utils.py \u2192 pkg-utils-py)."
    },
    "description": {
      "type": "string",
      "description": "One-line index entry shown in the knowledge index. Required for write. Keep under 80 chars."
    },
    "content": {
      "type": "string",
      "description": "Full page body in markdown. Required for write. Cross-link other pages with [text](/pages/slug.md)."
    }
  },
  "required": [
    "action"
  ]
}
```
