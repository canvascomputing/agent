# This

How every file under `agentdocs/` is written. This file is itself an example of the format.

## File shape

**One topic per file. Start with a title and a one-sentence description.**

- `# Title`: one word or short phrase, no trailing punctuation.
- One sentence under the title states what the file covers.
- Sections use plain `##` headings without numbers.
- Make each section self-contained so a reader can skip to it directly.

## Section shape

**Put the rule first and supporting bullets second.**

- Start each section with a bold one-line instruction.
- Add three to five bullets that unpack only what the rule does not say.
- Add an example only when it makes the rule materially clearer.
- Add a closing sentence only when it carries new information.

## Bullets

**Use imperative, one-line bullets.**

- Start with a capital letter and end with a period.
- Lead with the action or the thing forbidden.
- Keep each bullet to one idea; use two short sentences only when needed.
- Nest bullets only under a parent line ending in a colon.

## Enumerations

**Use bullets instead of tables.**

- Write name and description pairs as `` `Name`: description. ``
- Group related bullets under a short framing line when needed.
- Keep commands and small examples in code fences.
- Reserve tables for public reference documents such as `README.md`.

## Voice

**Write direct, neutral instructions without decoration.**

- State the rule and justify only what a reader would question.
- Prefer present tense and second person over passive voice.
- Remove marketing language, hedging, and unnecessary jargon.
- Reserve blockquotes for important callouts at the top of a file.

## Emphasis

**Use MUST for correctness and IMPORTANT for easy-to-miss consequences.**

- Use MUST when violating a rule breaks compilation, a public contract, or an architectural invariant.
- Prefix an easy-to-miss operational consequence with IMPORTANT.
- Let the bold lead carry ordinary emphasis.
- Avoid RFC-style SHOULD, MAY, and CAN outside a formal specification.

## Code grounding

**Name real project identifiers and keep each statement true to the code.**

- Verify named types, functions, fields, methods, and paths under `crates/`.
- Describe what code does with concrete verbs.
- Drop opinions that cannot point to code or a documented repository decision.
- Update a rule in the same change as the code that invalidates it.

## Cross-linking

**Give each fact one home and link to it elsewhere.**

- Put commands in `workflow.md`.
- Put file placement in `layout.md` and cross-module invariants in `architecture.md`.
- Put naming and comment rules in `style.md` and test-specific rules in `testing.md`.
- Replace duplicated facts with a link to their authoritative file.

## Length

**Optimize for retrieval, not completeness.**

- Drop facts a careful reader can recover immediately from code or `INVENTORY.md`.
- Keep constraints, exceptions, negations, and surprising behavior.
- Merge overlapping sections before adding another file.
- Cut a rule that has not earned its skim cost.
