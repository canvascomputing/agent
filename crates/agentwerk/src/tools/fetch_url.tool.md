---
name: fetch_url
concurrent: true
---

Fetch a URL over HTTPS and return its content as text: HTML becomes readable plain text, while JSON, text, and markdown pass through. HTTP is upgraded to HTTPS. Output is truncated to `max_length` chars (default 100 000). Limits: 60 s timeout, 10 MB body cap, 10 same-host redirect hops.

- A cross-host redirect is surfaced, not followed: the tool returns a `REDIRECT DETECTED` message with the new URL; re-call `fetch_url` with it.
- Authenticated or private URLs fail; do not retry. Fall back to a specialized tool if one is registered.

## When NOT to use

- Private/authenticated URLs (Nextcloud, GitLab, Confluence, Jira, dashboards): check for a specialized tool first.
- GitHub URLs: prefer `gh pr view` / `gh issue view` / `gh api`, if a tool is registered to run them.
- Custom headers, methods, or bodies: use `curl`, if a tool is registered to run it.
