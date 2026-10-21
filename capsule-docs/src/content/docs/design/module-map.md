---
title: Module Map
description: The crate dependency graph, one line of responsibility per crate, and the bounded E2E surface
status: draft
---

The code is the source of truth for module layout; this page fixes only the crate graph, the
boundaries between crates, and the bounded E2E case list.

## Crates

```text
capsule-core            (no internal deps)
capsule-i18n            (no internal deps)
capsule-sdk          -> core, i18n
capsule-wasm         -> core
capsule-core-ffi     -> core, sdk
capsule-server       -> core, i18n   (dev: sdk, wasm, server-migration; never linked)
capsule-cli          -> core, sdk, i18n, cli-entity, cli-migration
capsule-e2e          -> server, sdk, cli, core, wasm   (tests only)
xtask                -> core, i18n
```

| Crate | Responsibility |
| --- | --- |
| `capsule-core` | Cryptography and MLS authority, canonical CBOR, sidecars, validation, library and SQLite index, import, media, LQIP, sharing and drop crypto, backup, ML orchestration |
| `capsule-i18n` | Canonical ICU catalogs and runtime localization ([Internationalization](/design/i18n/)) |
| `capsule-sdk` | Spargen-generated REST client plus Capsule's orchestration: auth refresh, resumable upload, sync, recovery, version negotiation, LAN peering |
| `capsule-wasm` | Browser sealing surface (share-link open, guest-drop seal) over `capsule-core` with default features off |
| `capsule-core-ffi` | The app-facing UniFFI staticlib |
| `capsule-server` | The Kynos REST/OpenAPI application ([API Surfaces](/design/api-surfaces/)) |
| `capsule-cli` (+ `entity`, `migration`) | The CLI and its local SQLite persistence; network calls ride the SDK |
| `capsule-e2e` | The cross-crate cases below, against the real server composition |
| `xtask` | Codegen, i18n generation, architecture checks |
| `capsule-core-swift`, `capsule-core-kotlin` | Platform harnesses and `HardwareSigner` implementations (Secure Enclave, StrongBox) |
| `capsule-web`, `capsule-swift`, `capsule-android` | The clients ([Clients](/design/clients/)) |

GraphQL and gRPC are retired
([ADR-0001](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0001-the-public-server-surface-is-kynos-rest-only.md));
`legacy-review/` is not buildable.

## Server Modules

One Kynos application of cohesive internal modules, not separate services: `routes/` carries the
HTTP surface; `auth`, `enrollment`, `directory`, `upload`, `blob`, `serve`, `verify`,
`attestation`, `gc`, `scrub`, `sync`, `album`, `share`, `drop`, `escrow`, `federation`, `quota`,
`moderation`, `store` and `problem` carry the behaviour, each owned by its namesake design doc. Every
`store` adapter runs one conformance suite, which is what lets the in-memory double be trusted
([Server Filesystem — Required Services](/design/filesystem/server/#required-services)).
Authentication and upload state are separate typed ports; no generic CAS, transfer or TTL library.

## Client Boundaries

| Boundary | Decision |
| --- | --- |
| REST client | Spargen-generated from the checked-in OpenAPI 3.2 document |
| Namespaces an app links | Exactly two, `capsule_core_ffi` and `capsule_sdk`, both inside the `capsule-core-ffi` staticlib (two Rust staticlibs cannot share a binary). Hardware seams reach Swift/Kotlin as `capsule_sdk` foreign traits. ADR-0005 |
| Workspace verbs over FFI | Orchestration only: each verb is one call into `capsule-core`, which keeps every cryptographic step |
| Media | Rawshift through `capsule-core::media`; Chromahash imported directly |
| Alerts | Classes and predicates in `capsule-core::notify`; scheduling and presentation are native ([Notifications](/design/notifications/)) |

## E2E Test Surface

The surface is **bounded**: adding a case means justifying why the cross-module surface is
irreducible, and each case stays backed primarily by unit and adapter-contract tests. Code names
the case it covers (`rg "E2E case N"`).

1. **Auth → sync → client-side library query.** Sign in, apply the sync feed, a local
   `library.sqlite` query lists the expected albums.
2. **Import + upload + finalize.** Scan → plan → execute → upload → finalize; the blob is at its
   [content address](/design/filesystem/server/#blob-store-layout) and the index row is uploaded.
3. **Sync feed pickup.** Device A uploads; device B's feed advances and B fetches metadata and, per
   scope, the original.
4. **Federation cross-server pull.** A capability lets Bob's server pull Alice's metadata and blobs
   and Bob's client renders them.
5. **LAN peering A→B.** Discovery → TLS → delta artifact → byte-equal libraries.
6. **Backup → restore on a fresh device.** Passphrase and escrow bootstrap; every asset present and
   verifiable.
7. **Full lifecycle.** Create → update → trash → restore → delete → purge after retention; the
   provenance chain advances and the server refuses purge before `retention_until`.
8. **Album upgrade ceremony.** Quiesce → drain → tombstone → fork → replay, including one resume
   from a mid-ceremony crash.
9. **Cross-version protocol gate.** An out-of-range `protocol_version` upload gets `426` and an
   actionable error.
10. **Model regen after version bump.** Stale embeddings are excluded from queries until regen
    replaces them.
11. **Server crash mid-finalization.** A crash between blob rename and index commit leaves the
    session `FailedProcessing`, the asset `Pending`, and **nothing referencing the blob**; an
    orphaned blob is permitted and collected. The process-restart variant is still owed.
12. **Cross-device enrollment.** A authorizes B over a verified channel, cross-signs it, B joins
    each album's MLS group and the libraries match; includes one MITM-on-relay abort.
13. **Web drop → adopt.** A WASM client seals a drop; the owner's native client rewraps and adopts
    it, and a second device `verify_asset`-accepts it.
14. **Share link over TCP.** A link issued by the CLI opens from the URL alone through the
    `capsule-wasm` entry points, and revocation turns it into the public path's `404`.
