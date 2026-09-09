Searches the public web and returns result titles, URLs, and descriptions.

Use the results to find pages worth opening with `fetch`. A result description is a lead, not evidence.

Usage:
- `query`: the exact search query
- `count`: optional result count from 1 to 20; defaults to 5

# Instructions
- Write a focused query for the assigned research angle
- Use another query only when the first results leave a specific gap
- NEVER cite the returned description, because it may omit or distort the source page

Example usage:

<example>
brave_search({"query":"software API compatibility maintenance case study","count":5})
</example>
