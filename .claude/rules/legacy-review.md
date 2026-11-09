---
paths:
  - "legacy-review/**"
---

# legacy-review

## Rules

- `legacy-review/` is non-buildable reference material. Restore code only after defining its contract and automated tests against the decisions in `AGENTS.md` and every rule in `.claude/rules/` that governs what it touches — read those files explicitly, since the ones scoped elsewhere do not load from here. Its links point at what was there; do not "fix" them.

## Docs

Follow:

- [Principles — Single Source of Truth](../../capsule-docs/src/content/docs/design/principles.md#single-source-of-truth) and the destination crate's `//!` docs — where restored code belongs.
