---
title: Server Architecture
description: The Kynos REST/OpenAPI server and generated client architecture
status: draft
---

Capsule exposes one REST/OpenAPI contract ([](/design/api-surfaces/)).

## Data Flow

```mermaid title="Plaintext stays on the client; the server holds ciphertext and a key-free index"
flowchart TB
  subgraph client [Native client]
    core[capsule-core<br/>plaintext, keys, media policy,<br/>sidecars, provenance]:::client
    rawshift[Rawshift<br/>media processing]:::external
    chromahash[Chromahash<br/>LQIP input]:::external
    sdk[Spargen SDK<br/>ciphertext REST]:::client
    core --> sdk
    rawshift --> core
    chromahash --> core
  end
  subgraph server [Kynos server]
    api[Auth and structural validation]:::server
    machines[Upload and sync<br/>state machines]:::server
    pg[(Postgres<br/>key-free index,<br/>sessions and counters)]:::store
    blobs[(Opaque blob store)]:::store
    api --> machines
    machines --> pg
    machines --> blobs
  end
  sdk -- ciphertext only --> api
```

Timeline, search, tag, face and memory queries run against the client's local SQLite catalog. The
server stores opaque ciphertext plus the minimum key-free index needed for authorization, ordering,
quota, lifecycle and retrieval.

## Clients

- [Android](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-android): Jetpack Compose over `capsule-core-kotlin`.
- [iOS/iPadOS/macOS](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-swift): SwiftUI over `capsule-core-swift`.
- [Web](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-web): read-only, for authenticated viewing, share links and guest upload.
- [CLI](https://github.com/Capsulsaurus/Capsule/tree/master/capsule-cli): development and advanced users.

Native apps and the CLI share client logic through `capsule-core` and `capsule-sdk`, exposed to Swift
and Kotlin via uniffi. The per-platform contract is [](/design/clients/).

## Dependency Boundaries

Kynos owns HTTP and the Tokio runtime, Spargen generates the client, Rawshift owns codecs and
metadata extraction, and Chromahash is called directly. Blob storage, resumable upload, provenance,
crypto, CRDTs, asset lifecycle and application-state ports stay in Capsule. Each pin carries its
rationale as a comment beside it in the declaring manifest.

No reusable infrastructure library is extracted until a stable product-neutral contract has at least
two real consumers.
