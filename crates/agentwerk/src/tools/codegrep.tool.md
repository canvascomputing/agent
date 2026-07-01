---
name: codegrep_tool
read_only: true
---

Search file contents by the shape of code. Write the code you are looking for and replace each identifier, value, or argument list whose exact spelling you do not know with a metavariable (`$NAME`) or an ellipsis (`...`). This is NOT a regex.

To list things whose names you do not know in advance, capture them: the pattern `fn $NAME(...)` matches any Rust function and prints each one's name as `$NAME=value`. `$NAME` stands for one identifier and captures it; `(...)` stands for any argument list. So `fn $NAME(...)`, not `fn` and not `fn [a-z]*`.

Each match prints `<path>:<line>:<col>: <matched_text>` plus `[$NAME=value]` for captures (newlines shown as `\n`, long matches truncated); skips `.git`, `target`, `node_modules`, `vendor`. A pattern that matches nothing reports `No matches` with a reminder of the syntax.

- Use when the match must ignore identifier names, whitespace, or argument shape; use `grep_tool` for a byte-for-byte literal. Restrict to a file or folder with `path`, or to a name pattern with `glob` (`*.rs`, `*.ts`); never put a file name in `pattern`.
- Do NOT use regex syntax. Character classes (`[a-z]`), quantifiers (`*`, `+`, `?`), and the wildcard `.` are plain characters here, not operators.
- Pass the pattern exactly as written, with no backslash escaping: send `fn $NAME(...)`, never `fn \$NAME\(\)`; send `eval(`, never `eval\(`.
- `$NAME` captures one word, `$...NAME` a span; reusing a name back-references it exactly. `...` spans tokens at one bracket level and stops at newlines; `....` crosses them.
- `constraints` keep a match only when a named capture matches a regex (anchor with `^...$`); this is the one place regex applies, the pattern itself never does.

## When NOT to use

- Match a literal substring: use `grep_tool`.
- Find files by name: use `glob_tool`.
- Regex features (`\d`, `[a-z]`, `?`, `*`): not supported; an empty `pattern` is an error, not match-all.

## Examples

- `fn $NAME(...)` (Rust): every function, capturing its name; `def $NAME(...):` does the same in Python, `func $NAME(...) {...}` in Go.
- `$X.unwrap()` (Rust) or `console.log(...)` (JavaScript): every call of a given shape, ignoring the receiver and the argument list.
- `($...ARGS) => {...}` (JavaScript): arrow functions, capturing the whole argument span in `$ARGS`.
- `func ($R $T) $NAME(...) {...}` (Go): methods, capturing receiver, type, and name in one pattern.
- `catch ($E) {}` (Java): empty catch blocks, to audit for swallowed errors.
- `$X === $X` (JavaScript): a self-comparison bug; reusing `$X` forces both sides to be the same text.
- `SELECT *` with `caseless: true` (SQL): a keyword search regardless of letter case.
- `$X` with `constraints: [{ "metavariable": "X", "regex": "^AKIA[0-9A-Z]{16}$" }]`: an AWS access-key id, the whole token pinned to its shape.

## Schema

```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "description": "The code shape to match, written literally with metavariables for the parts that vary: `$NAME` for one word (captured), `$...NAME` for a span, `...` for any token sequence, `....` for one that crosses newlines. Not a regex, and never backslash-escaped: send `fn $NAME(...)` verbatim. Must be non-empty."
    },
    "path": {
      "type": "string",
      "description": "Directory or file to search under (default: `.`)."
    },
    "glob": {
      "type": "string",
      "description": "File-name filter (e.g. `*.rs`, `*.{ts,tsx}`). Applied before content search."
    },
    "mode": {
      "type": "string",
      "description": "`multiline` (default) treats newlines as whitespace, like a formatter would. `singleline` makes `...` stop at newlines so a pattern stays within one line; use `....` to span lines."
    },
    "caseless": {
      "type": "boolean",
      "description": "Match identifiers without regard to letter case (default: false)."
    },
    "max_results": {
      "type": "integer",
      "description": "Maximum number of matches to return (default: 100)."
    },
    "constraints": {
      "type": "array",
      "description": "Keep a match only when each named capture exists and its text matches the regex.",
      "items": {
        "type": "object",
        "properties": {
          "metavariable": {
            "type": "string",
            "description": "Capture name to test, e.g. `REST` (leading `$` optional)."
          },
          "regex": {
            "type": "string",
            "description": "Regex the capture's text must match. Anchor with `^...$`. No backreferences or lookaround."
          }
        },
        "required": ["metavariable", "regex"]
      }
    }
  },
  "required": [
    "pattern"
  ]
}
```
