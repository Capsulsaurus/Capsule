# ADR-0015 — Translations track the source they were made from, and coverage is gated

- **Status:** accepted
- **Date:** 2026-11-09
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Internationalization — Translation Lifecycle, Glossary](../capsule-docs/src/content/docs/design/i18n.md#translation-lifecycle)
- **Issues:** #366, #889, #1217

## Context

The i18n contract made a string reach every platform correctly but said nothing about whether it
stayed current once there. Shortly after the twelve catalogs were completed (#889), more than a
hundred source keys already had no translation, and nothing could tell a translation made from an
older English message from a current one. The English fallback hid every gap at runtime, so
coverage was restored by occasional backfills rather than kept. Every translation was
machine-produced, and translations are expected to keep coming from LLM agents as well as from
human contributors, so a translator's only input is the message and its context.

## Decision

- Coverage is gated: every translation catalog defines every source key, and every source entry
  carries `context`.
- Each translation records `source`, a hash of the English it was made from; a mismatch fails the
  build. A change to the English updates every catalog in the same change. `xtask i18n --todo`
  and `--stamp` are the agent-facing interface for doing so.
- A glossary pins verbatim names (gated) and preferred renderings (reference only).

Rejected: per-key length limits, which nobody maintains and which push translations into
abbreviation; a hosted translation platform as the system of record (already settled in the
provenance contract); checking `terms` renderings mechanically, which inflection defeats.

## Consequences

- An English edit costs twelve translations in the same pull request. An agent produces them from
  `--todo`; a human contributor may mark them `machine` and leave them for review.
- The existing translations were stamped as current; they had been made from the English that is
  still in place. Missing keys were backfilled as `machine` translations.
- A translation that dropped a verbatim name (*Takeout*, *Capsule*) was corrected when the gate
  landed.
- Layout direction and text expansion are a separate decision: the logical-layout gate is tracked
  in #1218, and pseudo-locales with per-app render tests in #1219.
