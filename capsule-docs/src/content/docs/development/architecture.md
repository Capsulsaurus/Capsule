---
title: Server Architecture
description: The planned Kynos REST/OpenAPI server and generated client architecture
---

Capsule exposes one REST/OpenAPI contract. GraphQL and gRPC were removed because they duplicated
schemas and conflicted with the encrypted, offline-first client model.

## Data Flow

```text
Native client
  ├─ capsule-core: plaintext, keys, media policy, sidecars, provenance
  ├─ Rawshift: media processing
  ├─ Chromahash v1: encrypted LQIP input
  └─ Spargen SDK: ciphertext REST transport
                      ↓
Kynos server
  ├─ auth and structural validation
  ├─ Capsule upload/sync state machines
  ├─ Postgres key-free index
  ├─ optional Valkey hot state
  └─ Capsule-owned opaque blob store
```

Rich timeline, search, tag, face, and memory queries run against the client's local SQLite catalog.
The server stores and synchronizes opaque ciphertext plus the minimum key-free index required for
authorization, ordering, quota, lifecycle, and retrieval.

## Clients

Native clients are preferred wherever the platform supports one:

- [Android](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-android) — Jetpack Compose app over the `capsule-core-kotlin` shared library
- [iOS/macOS](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-swift) — SwiftUI app over the `capsule-core-swift` shared library
- [Web](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-web) — React client (guest drops and the share-link viewer; the authenticated read path is a key-free projection of the sync feed)
- [CLI](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-cli) — for development and advanced users primarily

The native apps and the CLI share their client logic through `capsule-core` and `capsule-sdk`,
exposed to Swift and Kotlin as uniffi bindings. **There is no desktop application**: `capsule-desktop`
does not exist, and Windows and Linux users are served by the CLI and the web client. See
[Clients](/design/clients/) for the per-platform contract.

## Dependency Boundaries

- Kynos replaces the previous HTTP framework and owns Tokio/runtime policy.
- Spargen replaces the previous client generator.
- Rawshift replaces in-repository codecs and metadata extraction.
- Capsule calls Chromahash directly after v1.
- Blob storage, resumable upload, provenance, crypto, CRDTs, asset lifecycle, and application state
  ports stay in Capsule.

No new reusable infrastructure library is created until a stable product-neutral contract and at
least two real consumers demonstrate that extraction removes net complexity.
