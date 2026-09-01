# Architecture Decision Records

The design docs under `capsule-docs/src/content/docs/design/` state what is true. This
directory states **what changed, and why it changed**.

That split exists because the two answer different questions on different schedules. A
reader implementing the upload protocol needs the contract; a reader wondering why the
contract forbids something needs the argument that settled it. Mixing them makes the
contract longer without making it clearer, and it makes the argument harder to find,
because it is scattered across whatever doc happened to be edited that day.

Like the repo-root `SLICES.md`, this is a plain repository file set and **not part of the
documentation site**. The site is the specification a user and an implementer read. The
project's memory sits beside it, not inside it.

## The rule that decides where a passage goes

> A passage is **rationale** if deleting it would leave a reader unable to derive the
> contract's shape from the contract alone. It is **history** if deleting it would only
> leave a reader unable to say what the project used to think.

Rationale stays inline in the design doc. History moves here.

Three tells that a passage is history:

1. **It has a tense.** Past tense about the project's own decisions — "previously",
   "the earlier gate is amended", "has since", "used to", a bare `(decision 2026-07-12)`.
2. **It names a road not taken, by name.** "Considered and rejected: ThumbHash on its
   merits…", "The alternative — decrypt the drop and re-encrypt it under the AMK…".
3. **It justifies a boundary rather than stating one.** An argument for why a file lives
   where it lives is not part of the contract that file declares.

Two tells that a passage is rationale, and stays:

1. **It states a property the contract depends on.** "This is a non-security guard that
   surfaces a wildly-wrong honest client, **not** an authorization control" — delete it
   and an implementer makes `timestamp` load-bearing.
2. **It states an assumption.** "We explicitly assume the host language and decoder are
   memory-safe." An unstated assumption is a latent defect.

## Format

One file per decision, `NNNN-kebab-title.md`, numbered in the order they are recorded
rather than the order they were taken. Numbers are never reused.

```markdown
# ADR-0007 — Short statement of the decision, in the present tense

- **Status:** accepted | superseded | proposed
- **Date:** 2026-09-01
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [<the doc that now states this normatively>](../capsule-docs/src/content/docs/design/<doc>.md)
- **Slices:** S-C16, S-C20

## Context
What was true before, with evidence.

## Decision
One paragraph, present tense.

## Consequences
What it costs, what it forecloses, what else has to change.
```

The one field that is not standard practice is **`Contract:`**, and it is the field that
makes the split checkable. An ADR without a live contract is unfalsifiable prose: nobody
can tell whether it still describes the system. With one, the ADR says where to go and
read the rule as it stands today, and a reviewer can confirm the rule is actually there.

Write it as a link, not a bare path. `mise run check-docs-truth` resolves every
repo-relative Markdown link, so a `Contract:` pointing at a doc that has been renamed or
deleted fails the build — which is exactly the condition that turns an ADR back into
unfalsifiable prose.

A decision that has not landed anywhere yet is `Status: proposed` and carries no
`Contract:` line until it does.

## Granularity

One ADR per decision, not one per passage. A single decision — retiring the Salvo tree —
is narrated in several docs, and all of that narration collapses into one record here.
Splitting per passage would reproduce the scattering this directory exists to end.
