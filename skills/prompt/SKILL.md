---
name: prompt
description: Write or rewrite agentwerk role files, tool definitions, and directives at maximum task-relevant compression. Keeps observable actions, tool names, placeholders, output fields, and task-facing consequences; removes filler, implementation details, hidden state, and behavior-seeding examples.
---

# Prompt

You write agentwerk's agent-facing text: role files, `*.tool.md` definitions, directives. Each one contains only what the model needs to act and return a valid result. Every sentence must change an action, tool choice, decision, output, or recovery.

Compression is the style, not a level. There is no verbose mode and no flag.

## Modes

- Write: input is a task or agent description. Output goes to the path named, or inline when none is named.
- Rewrite: input is an existing prompt file. Output replaces it in place.

Both modes apply the same rules.

## Compression

Drop:

- Articles, filler (just, really, basically, actually, simply), pleasantries, hedging.
- Connective fluff: however, furthermore, additionally, in addition.
- Implementation rationales. State the required behavior and its task-facing consequence instead.
- Restatements. Each fact appears once, in one section.
- Capability framing. A strengths block changes no decision the agent makes.
- Internal data structures, hidden state, storage, queues, schema machinery, routing, orchestration mechanics, and parser internals. Describe only observable inputs and outcomes.

Keep:

- Every marker: `IMPORTANT`, `CRITICAL`, `NEVER`, `DO NOT`, `MUST`, `NOT`, `ONLY`. One severity each, never interchangeably. The marker is what carries a compressed rule: it stands in for the reason clause that used to make the rule stick.
- `not`, `never`, `no`, `only`, `except`. A flipped meaning costs more than every token the file saves.
- One task-facing reason or consequence where a rule is non-obvious or a prohibition needs weight. Attach it after a colon. NEVER an em dash: `agentdocs/style.md` bans it repo-wide.

Verbatim, never compressed:

- Tool names in backticks, `{placeholder}` bindings, output field names.
- Required literal strings, numbers, units.
- Code fences, file paths, URLs.

NEVER invent an abbreviation (`cfg`, `impl`, `req`, `res`, `fn`): it obscures meaning without reliably reducing length. No arrows as a because-linker. No emoji, no decorative banners.

IMPORTANT: stop compressing where an ordered procedure goes ambiguous without its conjunctions. Order beats brevity.

## Role file

Fill ONLY the sections the agent needs.

````markdown
# <Role>

{context}

## Role

<What you do and output. Name a consumer ONLY when its needs change the work. One to two sentences.>

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

NOTE: <what this task is not>
````

- Tool list on one line. A per-tool bullet earns its place ONLY when the agent picks the wrong tool without it.
- One example output, added ONLY when the field list leaves the shape unclear and a neutral example is possible.
- Examples teach shape ONLY. Use inert values such as `"..."`; NEVER seed a verdict, recommendation, label, status, path, filename, factual answer, or strategy the model could copy.
- List enums and decision rules outside examples. Omit an example when no valid neutral shape exists.
- Cap every text field with a character or sentence budget. Uncapped fields run long.
- NEVER grant a tool the input did not name: the model calls it and burns the turn.

## Tool definition

`*.tool.md` contains only prose the model needs to choose and call the tool.

`````markdown
<Imperative one-liner: the agent acts, the tool exposes the action. Then what it returns, and its limits.>

- <constraint the model needs to call it correctly>

## When NOT to use

- <neighbouring case>: use `<other tool>`.
`````

- Voice is the affordance: `Find files by glob pattern`, never `This tool finds files`.
- `## When NOT to use` appears ONLY where a sibling tool overlaps.
- Do NOT repeat argument behavior unless it changes tool choice or prevents an invalid call.

## Directive

No skeleton. State the failure and next action without subsystem or state-management details.

## Shared fragments

Move a block repeated across role files into one shared fragment so every agent receives the same rule.

## Rewrite protocol

1. Read the target.
2. Apply the rules above, preserving every verbatim class.
3. Write in place.
4. Report the before and after character counts on one line.

## Example

Before:

```markdown
After all work has been completed, call the finish tool exactly once and put your final answer
inside the result argument. Do not add any other text because it will not be returned.
```

After:

```markdown
Call `finish({"result":"..."})` once after the work is complete. Text outside it is not returned.
```

NOTE: Write the sections the agent needs, then stop. A one-line directive does not grow into a role file.
