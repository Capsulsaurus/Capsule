---
paths:
  - "capsule-swift/**"
  - "capsule-android/**"
  - "capsule-web/**"
  - "capsule-core-swift/**"
  - "capsule-core-kotlin/**"
---

# Native and web clients

## Rules

- Platform tiers: **iOS and Android phone stabilize first**, then iPadOS and Android tablet, then macOS; Windows and Linux are deferred. This is an ordering, not a hierarchy — every native client is meant to end up fully featured and platform-integrated. The canonical table is [Clients — Platform Tiers](../../capsule-docs/src/content/docs/design/clients.md).
- What clients share: **iPadOS and macOS share views**; **iOS and Android share layouts and feature sets but not design language**; Apple logic is shared across iOS/iPadOS/macOS behind one route enum and one router, with shells chosen by size class rather than `#if os(...)`. A difference between two clients must be justified by a difference in the platform — "written first" is not a justification.
- Design language: shared skeleton, per-platform material. HIG and SF Symbols on Apple, Material on Android; neither emulates the other and neither invents a third house style. Bespoke components only for concepts the host platform has no analogue for.
- The **web client is permanently read-only** — authenticated viewing plus public share and guest-upload links — server-owned, and version-locked to its server. It is the one client allowed to assume its peer's exact version; native clients must stay lenient, which is why protocol evolution is additive.
- A feature lands in the CLI/TUI reference client first (root `AGENTS.md`, Workflow).
- User-facing strings come from the catalogs; see [i18n.md](i18n.md).

## Docs

Follow:

- [Clients](../../capsule-docs/src/content/docs/design/clients.md) — tiers, client validation duties, reading state from a newer client, the sandboxed decoder, benchmark tooling.
- [Architecture](../../capsule-docs/src/content/docs/development/architecture.md) — data flow through `capsule-core-swift` / `capsule-core-kotlin` (the UniFFI harnesses over `capsule_sdk`).
- [Versioning](../../capsule-docs/src/content/docs/design/versioning.md) — why native clients stay lenient.
- `capsule-web/src/components/lazy-image.tsx` → [Thumbnails](../../capsule-docs/src/content/docs/design/thumbnails.md) (Chromahash LQIP); share and guest-upload routes → [Share links](../../capsule-docs/src/content/docs/design/share-links.md) and [Web upload](../../capsule-docs/src/content/docs/design/web-upload.md).

Keep current:

- [Dependencies](../../capsule-docs/src/content/docs/design/dependencies.md) — the Web, Swift and Kotlin sections, for any manifest change (see [dependencies.md](dependencies.md)).
- The generated catalogs under `capsule-web/src/i18n/messages/`, `capsule-swift/Generated/` and `capsule-android/src/main/res/values*/` are never edited by hand — `mise run i18n`.
