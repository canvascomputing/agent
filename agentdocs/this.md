# This

How every file under `agentdocs/` is written. This file is itself an example of the format.

## File Shape

**One topic per file. Start with a title and a one-sentence description.**

```markdown
# Style

Naming and comment rules, plus README structure. Skim the section matching what is being written.
```

- `# Title`: one word or short phrase, no trailing punctuation.
- One sentence under the title states what the file covers.
- Sections use plain headings: `## Title Cased Heading`. No numbers: adding a section must not force renumbering.
- Each section is self-contained, so a reader can skip straight to it.

## Section Shape

**Bold rule first. One example second. Bullets last.**

```markdown
## Builders

**Builder methods are bare nouns. No `with_` prefix.**

`.name()`, `.model()`, `.tool()`, `.label()`, `.concurrent()`

- The `with_` prefix is reserved for a bare name that would be ambiguous.
```

- The first line after the heading is a bold one-liner stating the rule as an instruction.
- An example follows whenever the rule is about code or about the shape of text. Use the smallest form that shows the rule: a fence, a line of identifiers, or a good and bad pair.
- A section whose own text already demonstrates the rule needs no separate example.
- Bullets carry what the rule and the example do not. A closing sentence is added only when it carries information the bullets cannot.

## Bullets

**Three to five bullets per section. One line each. Imperative voice.**

- Start with a capital letter; end with a period.
- Lead with the verb or with the thing being forbidden.
- Two short sentences per bullet are acceptable; longer bullets are not.
- Nested bullets are used only under a parent line ending in a colon.

## Enumerations

**Use bullets, not tables.**

```markdown
- `Persist`: `save(&self, dir)` and `load(dir, &Self::Key)`.
- `Append`: `append(dir, &Self::Record)`.
```

- Tables produce wide rows that are hard to compare.
- For `name: description` pairs, write `` `Name`: description. ``
- Group related bullets under a one-line header ending in a colon.
- Tables belong in the README, where a `<details>` fold gives them a place to sit. These files have no folds.

## Punctuation

**Colons, not em dashes.**

- Use `:` where an em dash would otherwise appear.
- Use commas or parentheses for short parenthetical asides.
- `>` blockquotes are reserved for callouts at the top of a file.

## Voice

**Direct and neutral. No marketing language. No unnecessary jargon.**

```markdown
GOOD: `Stats` records every event and exposes read accessors.
BAD:  `Stats` seamlessly wires a powerful metrics plane into the kernel.
```

- State the rule; justify only when the rule is not obvious on its own.
- Prefer present tense and second person over passive voice.
- Avoid adjectives that do not carry information ("powerful", "clean", "seamless").
- Avoid borrowed metaphors ("kernel", "plane", "seam", "pipeline") unless they are the precise technical term.

## Emphasis

**Use MUST for non-negotiable rules. Use IMPORTANT for easy-to-miss gotchas.**

- MUST: correctness-critical rules where a violation breaks compilation, the shape exchanged with LLM providers, or an architectural invariant.
- IMPORTANT: prefixes a bullet that a reader skimming would miss and regret later.
- Most rules need neither: the bold one-liner is already the rule.
- SHOULD, MAY, and CAN are not used: RFC-2119 without the full spec is noise.

## Code Grounding

**Rules name identifiers that exist in the crate. No invented vocabulary.**

- A type, function, field, or method named in a rule MUST be greppable in `crates/agentwerk/src/`.
- Verbs describe what the code does, not how it feels: avoid "wires", "magic", "ergonomic", "seamless".
- A rule that cannot point at code is opinion, not architecture: drop it or move it to the consuming application.
- When a name changes in code, the docs change in the same commit.

## Cross-Linking

**Each fact lives in one file. Other files link to it.**

- Commands belong in `workflow.md`; other files link there rather than restating them.
- File and module placement belongs in `layout.md`; `architecture.md` describes invariants and assumes placement is known.
- Naming and comment rules belong in `style.md`; `testing.md` covers test-specific naming and links out for the rest.
- A duplicated fact is a future inconsistency: when two files would say the same thing, one of them links instead.

## Length

**If agentdocs are getting too long, consolidate them. Information loss is acceptable.**

- Drop sections that restate what a careful reader of the code would already see.
- Merge two short, overlapping sections before splitting one long section.
- A rule that has not earned its line is cut, not rewritten shorter.
- Skim cost matters more than completeness: a forgotten file teaches nothing.
