---
title: Server Architecture
description: The Kynos REST/OpenAPI server and generated client architecture
status: draft
---

Capsule exposes one REST/OpenAPI contract ([API Surfaces](/design/api-surfaces/)).

## Data Flow

```text
Native client
  ├─ capsule-core: plaintext, keys, media policy, sidecars, provenance
  ├─ Rawshift: media processing
  ├─ Chromahash: encrypted LQIP input
  └─ Spargen SDK: ciphertext REST transport
                      ↓
Kynos server
  ├─ auth and structural validation
  ├─ Capsule upload/sync state machines
  ├─ Postgres key-free index
  ├─ Valkey volatile state (required)
  └─ Capsule-owned opaque blob store
```

Timeline, search, tag, face, and memory queries run against the client's local SQLite catalog. The
server stores opaque ciphertext plus the minimum key-free index needed for authorization, ordering,
quota, lifecycle, and retrieval.

## Clients

- [Android](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-android) — Jetpack Compose over `capsule-core-kotlin`
- [iOS/iPadOS/macOS](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-swift) — SwiftUI over `capsule-core-swift`
- [Web](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-web) — read-only: authenticated viewing, share links, guest upload
- [CLI](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-cli) — development and advanced users

Native apps and the CLI share client logic through `capsule-core` and `capsule-sdk`, exposed to Swift
and Kotlin via uniffi. Per-platform contract: [Clients](/design/clients/).

## Dependency Boundaries

Kynos owns HTTP and the Tokio runtime; Spargen generates the client; Rawshift owns codecs and metadata
extraction; Chromahash is called directly. Blob storage, resumable upload, provenance, crypto, CRDTs,
asset lifecycle, and application-state ports stay in Capsule. Pins live in
[Dependencies](/design/dependencies/).

No reusable infrastructure library is extracted until a stable product-neutral contract has at least
two real consumers.
