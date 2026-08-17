# CLAUDE.md

agentwerk is a minimal Rust crate that gives any application agentic capabilities, plus the Python bindings that wrap it. It carries the features building agentic workflows needs and nothing beyond them. You work in this repository: read the convention file matching your task, change the code, and leave every document true to the code you left behind. The next agent reads those documents instead of your diff.

Guidelines:
- Read the convention file matching your task before writing code, because each holds rules the code alone does not state.
- Reject a feature that does not earn its place: every addition costs the crate its minimalism.
- IMPORTANT: update `README.md` when the public API changes, `agentdocs/` when a convention or the layout changes, and `INVENTORY.md` in the same commit that adds, renames, removes, or re-types any item.
- NEVER leave a document stating what the code no longer does, because the next agent trusts the document over the code and repeats the mistake.

Conventions:
- [agentdocs/project.md](agentdocs/project.md): vision and design philosophy
- [agentdocs/workflow.md](agentdocs/workflow.md): build, test, release commands
- [agentdocs/layout.md](agentdocs/layout.md): where code lives
- [agentdocs/architecture.md](agentdocs/architecture.md): rules that shape how code is organized
- [agentdocs/style.md](agentdocs/style.md): naming, comment, and prose style, plus README structure
- [agentdocs/testing.md](agentdocs/testing.md): how tests are organized and written
- [agentdocs/this.md](agentdocs/this.md): how the agentdocs files themselves are written

Inventory:
- `INVENTORY.md` lists every declaration of both crates, one table per source file, Rust rows next to Python rows.
- Read the section named after the file you are about to change, because it states what already exists there and what Python sees of it.
- CRITICAL: an item you add, rename, remove, or re-type changes its row in the same commit. A row that has stopped matching the code is worse than no row.
