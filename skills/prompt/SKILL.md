---
name: prompt
description: Write or rewrite roles, tasks, handoffs, schemas, tool descriptions, directives, and shared prompt fragments. Use when agent-facing text lacks orientation or disagrees with its runtime contract.
---

# Prompt

Write only what an agent needs to act and return a valid result. Edit a supplied path or write inline.

## Contract Audit

Before editing:

1. Read the prompt and its call site.
2. Inspect rendered tasks, handoffs, placeholders, and values.
3. Compare named tools with granted tools.
4. Read input and output schemas.
5. Trace requested fields to sources and inspect the consumer.
6. Find contradictions across prompts, examples, payloads, schemas, and conditions.

DO NOT rewrite from the prompt file alone. Clear prose can still describe a false contract.

Resolve contradictions before compression. Update every affected prompt surface and test.

## Room Test

Read the opening alone. Assume the agent has no conversation history.

Open every autonomous role with exactly these bullets:

- **Identity:** Start with `You are` and name the specialist.
- **Place:** Name its codebase, library, service, or environment.
- **Tools:** Name where its input and evidence live.
- **Job:** State one owned action or decision.
- **Stop:** State completion, including empty or negative results.

Rules:

- Keep each bullet to 16 words after its label.
- Use one sentence and one instruction per bullet.
- NEVER begin with `The task`, `You run`, or `You audit`.
- Give every reference an antecedent.
- Say who supplied or decided each verdict, finding, or result.
- Remove internal terms the agent does not read, call, or return.
- Define each necessary technical term at first use.
- Name an audience ONLY when it changes wording, evidence, or format.

Reject unexplained `bounded pass`, `carried finding`, `cited dossier`, `corpus`, and `boundary`.

### Reporter

Wrong:

```markdown
Write the decided verdict as `summary`.
```

Right:

```markdown
- **Identity:** You are the final report writer for a completed code review.
- **Place:** The examined codebase is available read-only in your working directory.
- **Tools:** The assignment contains the Analyst's final verdict and required wording.
- **Job:** Write `summary` for general readers and `details` for engineers.
- **Stop:** Return both fields without reconsidering the supplied verdict.
```

### Seeker

Wrong:

```markdown
Run one bounded pass over the scanned tree.
```

Right:

```markdown
- **Identity:** You are the code-search specialist in a security review.
- **Place:** Work inside the unfamiliar codebase in your working directory.
- **Tools:** `knowledge` contains its file map and saved queries.
- **Job:** Search file contents for one assigned or untried suspicious pattern.
- **Stop:** Save each query, then return one match or `nothing`.
```

## Complete Contract

Define four facts for every agent:

| Fact | Meaning |
|---|---|
| Input | Data and evidence it sees |
| Action | Transformation, investigation, or decision it owns |
| Output | Exact tools, fields, types, and limits |
| Stop | Completion, empty result, or negative result |

Put each fact at its narrowest stable layer:

| Surface | Content |
|---|---|
| Role | Stable behavior shared by every claimed task |
| Task | One assignment and its instance facts |
| Handoff | Exact facts established upstream |
| Schema | Field meaning, evidence source, and conditionality |
| Tool description | Facts needed to choose and call the tool |
| Shared fragment | One unchanged rule used by multiple prompts |

- DO NOT repeat a fact across layers.
- Name a consumer ONLY when it changes the returned evidence, wording, or shape.
- Omit hosts, queues, labels, storage, assemblers, installers, parsers, retries, and validators.

## Context and Data

- Include a runtime value ONLY when it changes an action.
- Prefer a specific value such as `{{ date }}` or `{{ task_id }}` over `{{ context }}`.
- Wrap injected requests, findings, research, and upstream results in descriptive XML tags.
- Mark an injected block as data when it could contain instructions.
- Carry exact upstream fields instead of paraphrasing them.
- Make optional input and output visibly conditional.
- NEVER ask an agent to retype a value the caller can carry exactly.

## Compression

Drop:

- Filler, hedging, marketing, and capability claims.
- Restatements, schema-enforced rules, and internal vocabulary.
- Advice without a check and rationales with obvious consequences.

Keep:

- Actions, decisions, outputs, stops, and recovery.
- Exact names, placeholders, fields, enums, numbers, paths, URLs, and literals.
- Every restriction and its original semantic force.
- Reasons, prohibition consequences, and sequence words that change behavior.

Preserve force deliberately:

- `NEVER`: absolute prohibition.
- `DO NOT`: recoverable prohibition.
- `NOT`: required contrast.
- `ONLY`: strict scope.
- Preserve existing `IMPORTANT`, `CRITICAL`, and `MUST` markers when their force remains required.

Remove obsolete placeholders, duplicate markers, and redundant examples ONLY after auditing their call sites.

Style:

- Use plain words and direct imperatives.
- Use one instruction per bullet.
- Keep body bullets below 24 words where practical.
- Avoid semicolons and nested conditions.
- Use decision tables for rules with multiple branches.
- NEVER invent abbreviations such as `cfg`, `impl`, `req`, `res`, or `fn`.
- NEVER use an em dash.

## Prompt Surfaces

### Role

- Start with the Room Test.
- Follow required section order and list ONLY granted tools.
- Put instance facts in the task and cap free-text fields.
- End with the task boundary.

### Task and Handoff

- Give one assignment and delimit its data.
- Omit stable role policy.
- Carry upstream fields the receiver cannot reconstruct.

### Schema Description

- Define meaning, evidence, and conditionality.
- DO NOT describe routing, storage, parsers, installers, or schema machinery.

### Tool Description

Write only what selects and invokes the tool:

```markdown
<Imperative affordance and returned result.>

- <Constraint preventing a wrong call.>

## When NOT to Use

- <Overlapping case>: use `<other_tool>`.
```

Omit `When NOT to Use` without an overlapping tool.

### Directive and Shared Fragment

- State the failure and next valid action.
- Bind byte-identical shared text once.

## Examples

Add examples ONLY when required or prose cannot show the shape.

- Show the input before the output.
- Derive every output fact from that shown input.
- Make every tool call schema-valid.
- NEVER invent a package, path, filename, citation, verdict reason, or literal.
- Use decision tables to avoid seeding. Use balanced cases when two examples are required.

## Verification

Verify rendered prompts:

- Every placeholder resolves.
- Named and granted task-critical tools match.
- Each requested field is available or conditional.
- Every example satisfies the active schema.
- Every injected data block is delimited.
- Conditional inputs produce conditional instructions and outputs.
- Shared context appears once.
- The opening passes the Room Test.
- The consumer receives exactly what it needs.

## Report

Report:

- Per-prompt and aggregate character counts.
- Semantic contract changes.
- Validation results and skipped checks.

NOTE: Compression starts after the contract is complete. A short prompt with a missing premise still fails.
