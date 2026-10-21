# ADR-0011 — The CLI and TUI are English-only

- **Status:** accepted
- **Date:** 2026-10-21
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Internationalization — The CLI Is English-Only](../capsule-docs/src/content/docs/design/i18n.md#the-cli-is-english-only)
- **Issues:** #680

## Context

The CLI routed its output and its `--help` text through the shared catalogs, negotiated from
`LANG`, with a test pinning every catalog entry to the derive text it replaced. The CLI is about
to grow into a full TUI, which would add hundreds of strings, each costing thirteen catalog
entries for an audience of operators and power users who read English. About a hundred CLI
strings were already exempted by the i18n guard's allowlist.

## Decision

The CLI and TUI are not localized. Their strings are inline English; the shared `error.*` codes
render through the English catalog. Localization belongs to the apps.

## Consequences

- The `cli.*` catalog keys, the help-text localization pass and the CLI allowlist entries are
  removed (#680).
- The "no hardcoded user-facing strings" rule applies to the apps, the web client and the
  server, not to `capsule-cli`.
