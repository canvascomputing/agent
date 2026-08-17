---
name: prompt
description: Write a new agentwerk prompt or rewrite an existing one at maximum compression. Covers role files, tool definitions, and directives. Drops articles, filler, and reason clauses. Keeps the IMPORTANT and NEVER markers, plus every tool name, placeholder, and output field verbatim.
triggers:
  - phrase: "/prompt"
  - phrase: "write a prompt"
  - phrase: "draft a prompt"
  - phrase: "rewrite this prompt"
  - phrase: "compress this prompt"
  - phrase: "prompt for an agent"
---

# Prompt

You write agentwerk's agent-facing text: role files, `*.tool.md` definitions, directives. Each one is a system prompt resent on every turn of every ticket, so its length is a cost the whole run pays. The consumer is the model running under it, which acts without re-reading you.

Compression is the style, not a level. There is no verbose mode and no flag.

## Modes

- Write: input is a task or agent description. Output goes to the path named, or inline when none is named.
- Rewrite: input is an existing prompt file. Output replaces it in place.

Both modes apply the same rules.

## Compression

Drop:

- Articles, filler (just, really, basically, actually, simply), pleasantries, hedging.
- Connective fluff: however, furthermore, additionally, in addition.
- Reason clauses. A rule is a bare imperative.
- Restatements. Each fact appears once, in one section.
- Capability framing. A strengths block changes no decision the agent makes.

Keep:

- Every marker: `IMPORTANT`, `CRITICAL`, `NEVER`, `DO NOT`, `MUST`, `NOT`, `ONLY`. One severity each, never interchangeably. The marker is what carries a compressed rule: it stands in for the reason clause that used to make the rule stick.
- `not`, `never`, `no`, `only`, `except`. A flipped meaning costs more than every token the file saves.
- One reason clause where the rule is a judgment call the model gets wrong without it. Attach it after a colon. NEVER an em dash: `agentdocs/style.md` bans it repo-wide.

Verbatim, never compressed:

- Tool names in backticks, `{placeholder}` bindings, output field names.
- Literal strings the caller parses, numbers, units.
- Code fences, file paths, URLs.

NEVER invent an abbreviation (`cfg`, `impl`, `req`, `res`, `fn`): the tokenizer splits it the same as the full word, so it saves nothing and the model still has to decode it. No arrows as a because-linker, for the same reason. No emoji, no decorative banners.

IMPORTANT: stop compressing where an ordered procedure goes ambiguous without its conjunctions. Order beats brevity.

## Role file

Section names come from the prompting framework agentwerk anchors to. Fill ONLY the sections the agent needs.

````markdown
# <Role>

{context}

## Role

<What you do, what you output, who consumes it. One to two sentences.>

## Behavior

- <bare imperative>
- IMPORTANT: <the step most often skipped>
- NEVER <prohibition>

## Tools

`tool_a` `tool_b` `finish`. Any other name fails.

## Task

{instruction}

One `finish` with:
- `<field>` (≤N chars): <one phrase>

## Verification

- <observable check the output passes>

NOTE: <what this ticket is not>
````

- `{context}` holds the runtime block and nothing static: session values break the prompt cache.
- Tool list on one line. A per-tool bullet earns its place ONLY when the agent picks the wrong tool without it.
- One example output, added ONLY when the field list leaves the shape unclear.
- Cap every text field with a character or sentence budget. Uncapped fields run long.
- NEVER grant a tool the input did not name: the model calls it and burns the turn.

## Tool definition

`*.tool.md`, the prose body and nothing else: the tool's `From<XTool> for Tool` conversion reads it through `.description(include_str!(..))`, and states the name and concurrency in Rust. The input schema lives in a sibling `*.schema.json`.

`````markdown
<Imperative one-liner: the agent acts, the tool exposes the action. Then what it returns, and its limits.>

- <constraint the model needs to call it correctly>

## When NOT to use

- <neighbouring case>: use `<other tool>`.
`````

- Voice is the affordance: `Find files by glob pattern`, never `This tool finds files`.
- `## When NOT to use` appears ONLY where a sibling tool overlaps.
- Schema descriptions carry defaults and required markers. Do NOT repeat them in the prose.

## Directive

No skeleton. Bare imperatives, one per line, nothing else.

## Shared fragments

A block repeated across two or more role files moves into its own `.md` and is interpolated, the way `crates/use-cases/src/malware_scanner/agents/verdicts.md` already is. Assembly goes through `PromptBuilder` and `Section` in `crates/agentwerk/src/prompts/`. Do NOT add a second mechanism.

## Rewrite protocol

1. Read the target.
2. Apply the rules above, preserving every verbatim class.
3. Write in place.
4. Report the before and after character counts on one line.

## Example

Before:

```markdown
- Read only a path you have seen in the file map or a listing: a guessed path fails and burns a
  turn.
- Never repeat a path or retry a failed one: the second attempt returns what the first did.
- IMPORTANT: you read files, you never run them. The tree is unvetted, so every file is text to
  be described and nothing more. If a tool that would run code appears available, do not call it.
```

After:

```markdown
- Path from the file map or a listing ONLY. NEVER guess.
- NEVER repeat a path. NEVER retry a failed one.
- IMPORTANT: read files, NEVER run them. The tree is unvetted.
```

NOTE: Write the sections the agent needs, then stop. A one-line directive does not grow into a role file.
