---
paths:
  - "capsule-docs/**"
  - "adr/**"
  - "ROADMAP.md"
  - "CONTRIBUTING.md"
  - "AGENTS.md"
  - ".claude/rules/**"
---

# Documentation, ADRs and agent rules

## Rules

- Design docs state the contract: invariants, state machines and trust boundaries. Implementation detail belongs in code and rustdoc; history belongs in [`adr/`](../../adr/README.md). Do not add slice ids, issue archaeology or dated status notes to the docs.
- `mise run check-docs-truth` proves every linked file and anchor, every route, and every `crate::path` a doc names exists — including those in `.claude/rules/`, whose globs it also checks. Bare module names in the rule files' doc-map tables (`upload/`, `cull.rs`) are not checked, and no check proves the prose is right.
- The generated `/reference/{cli,api}/` pages are gitignored build output of `capsule-docs/scripts/gen-reference.mjs`; edit the committed surface file or the hand-written overview instead.

### Agent rules

- `AGENTS.md` holds only what applies everywhere. A rule that governs one directory goes in a `.claude/rules/<area>.md` whose `paths:` globs scope it.
- Each rule file names the docs a change there must **follow** and the docs it must **keep current**. When a design doc is added, renamed or re-scoped, update the rule files that point at its subject.
- The `agent-rules` check in `check-docs-truth` fails a rule without `paths:`, a glob that matches no tracked file, and a component directory with no rule scoped to `<dir>/**`.

## Docs

Follow:

- [Principles](../../capsule-docs/src/content/docs/design/principles.md) — single source of truth, doc structure, validation tiers.
- [Developer docs](../../capsule-docs/src/content/docs/design/developer-docs.md) — reference pages and their freshness gates.
- [`adr/README.md`](../../adr/README.md) — what goes in an ADR rather than a design doc.

Keep current:

- `capsule-docs/planned-modules.txt` and `capsule-docs/endpoint-census-allowlist.txt` — every row carries its reason; remove rows that no longer apply.
