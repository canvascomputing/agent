---
name: manage_knowledge
read_only: false
---

Read or write pages in the shared knowledge base: durable facts injected into every ticket's system prompt. The store is an Open Knowledge Format (OKF) bundle. Existing pages' one-line descriptions appear under `## Knowledge`; no such section means the store is empty. `write` creates or replaces a whole page, `read` loads one body, `list` shows the index.

- Save a durable fact future tickets need; one topic per page, with a descriptive slug (`deployment-config`, `pkg-utils-py`).
- `description` is a one-sentence index line under 80 chars. Cross-link pages with `[text](/pages/slug.md)`.
- `type` is a short concept kind (`Overview`, `Verdict`); optional, defaults to `Knowledge`.
- `write` overwrites the page and requires `slug`, `description`, and `content`: omitting any is the most common error. Read first if you mean to append.
- Only `read` a slug already shown in `## Knowledge` or `list`; an unseen slug does not exist.

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
      "description": "The operation to perform. 'write' creates or replaces a page (requires slug + description + content). 'read' returns the full page body (requires slug; only valid for slugs in the index). 'remove' deletes a page (requires slug). 'list' returns the current index."
    },
    "slug": {
      "type": "string",
      "description": "Page identifier (lowercase, hyphens, max 60 chars). Required for write, read, and remove; omitting it returns an error. For file paths, replace dots and slashes with hyphens (e.g. pkg/utils.py \u2192 pkg-utils-py)."
    },
    "type": {
      "type": "string",
      "description": "OKF concept type: a short kind such as Overview or Verdict. Optional; defaults to Knowledge."
    },
    "description": {
      "type": "string",
      "description": "One-line index entry shown in the knowledge index. Required for write. Keep under 80 chars."
    },
    "content": {
      "type": "string",
      "description": "Full page body in markdown. Required for write. Cross-link other pages with [text](/pages/slug.md)."
    },
    "tags": {
      "type": "array",
      "items": {
        "type": "string"
      },
      "description": "Optional tags for the page."
    }
  },
  "required": [
    "action"
  ]
}
```
