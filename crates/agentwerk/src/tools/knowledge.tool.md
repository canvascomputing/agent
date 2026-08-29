Read or write pages in your knowledge: durable facts shared across tasks and agents, injected into every task's system prompt. The store is an Open Knowledge Format (OKF) bundle. `write` creates or replaces a whole page, `read` loads one body, `list` shows every page with its one-line description.

- Save a durable fact later tasks need; one topic per page, with a descriptive slug (`deployment-config`, `pkg-utils-py`).
- `description` is a one-sentence index line under 80 chars. Cross-link pages with `[text](/pages/slug.md)`.
- `write` overwrites the page and requires `slug`, `description`, and `content`: omitting any is the most common error. Read first if you mean to append.
- Only `read` a slug you have seen listed, whether in your knowledge or in the index file it points at: an unseen slug does not exist and the call fails.
- `remove` is a rarely needed cleanup for a page that turned out wrong. Prefer `write` to correct it in place, since removing it takes the page from every other agent too.

## When NOT to use

- Task progress or TODOs: those belong on tasks.
- A new page per tiny fact: consolidate related facts.
