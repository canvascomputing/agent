---
name: fetch_url
read_only: true
---

Fetch a URL over HTTPS and return its content as text: HTML becomes readable plain text, while JSON, text, and markdown pass through. HTTP is upgraded to HTTPS. Output is truncated to `max_length` chars (default 100 000). Limits: 60 s timeout, 10 MB body cap, 10 same-host redirect hops.

- A cross-host redirect is surfaced, not followed: the tool returns a `REDIRECT DETECTED` message with the new URL; re-call `fetch_url` with it.
- Authenticated or private URLs fail; do not retry, fall back to a specialized tool or `bash_tool` with credentials.

## When NOT to use

- Private/authenticated URLs (Nextcloud, GitLab, Confluence, Jira, dashboards): check for a specialized tool first.
- GitHub URLs: prefer `gh pr view` / `gh issue view` / `gh api` via `bash_tool`.
- Custom headers, methods, or bodies: use `bash_tool` with `curl`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "url": {
      "type": "string",
      "description": "The URL to fetch. HTTP is upgraded to HTTPS. Max length 2000 characters."
    },
    "max_length": {
      "type": "integer",
      "description": "Max response length in characters (default: 100000). Increase only if the content is known to exceed the default."
    }
  },
  "required": [
    "url"
  ]
}
```
