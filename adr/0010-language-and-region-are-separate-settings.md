# ADR-0010 — Language and region are separate settings

- **Status:** accepted
- **Date:** 2026-10-21
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Internationalization — Language and Region](../capsule-docs/src/content/docs/design/i18n.md#language-and-region)
- **Issues:** #690

## Context

The shared i18n layer resolved one locale tag (exact tag, then primary subtag, then English)
and implemented no date or number formatting, so "locale" meant only which strings to show.
The Apple client already treated region as a separate axis taken from the OS; Android and the
web client had no notion of it. A user who wants Hong Kong Traditional Chinese text with British
date formatting could not express that consistently across clients.

## Decision

Every app takes two settings: **language**, which selects the catalog and text direction, and
**region**, which drives dates, times, numbers, first weekday, units and calendar. Both default
from the operating system, prefer the platform's per-app language setting where one exists, and
stay on the device.

## Consequences

- Catalog messages receive raw values and let the platform format them with the region.
- Android and the web client gain a region setting (#690); the Apple client already conforms.
