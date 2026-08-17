Execute one command in the working directory and return its output. A command the patterns below do not permit is rejected before it runs.

- No shell: `&`, `|`, `;`, `<`, `>`, `(`, `)`, `$(...)` and backticks are rejected. Make one call per command.
- No expansion: `$HOME`, `~` and `*.txt` reach the program as written.
- Quote an argument containing spaces; use single quotes around text holding a double quote.

## When NOT to use

Reach for the dedicated tool, which is faster, structured, and cites line numbers: `read_file` over `cat`, `list_directory` over `ls`, `glob` over `find`, `grep` over the `grep` and `rg` programs, `edit_file` or `write_file` over `sed`.
