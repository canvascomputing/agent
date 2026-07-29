---
name: find_tools
read_only: true
---

Discover tools withheld from the initial system prompt to keep context small. Returns matching names, descriptions, and input schemas; a surfaced tool becomes callable by name for the rest of the session.

- Use when a tool you expect is missing from your definitions block: the framework defers some tools and shows only their names until searched.
- Match is by name and keyword (`pdf`, `database query`, `calendar event`). A match grows the system-prompt context, so search broadly once rather than narrowly many times.

## When NOT to use

- A tool already in your definitions: call it directly.
- Search file contents: use `grep`. Find files by name: use `glob_tool`.

## Schema

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Search terms, matched against tool names and descriptions. Keywords work better than full sentences (e.g. `csv parse`, not `how do I parse a csv file`)."
    }
  },
  "required": [
    "query"
  ]
}
```
