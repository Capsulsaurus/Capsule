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

## Dependency Boundaries

- Kynos replaces the previous HTTP framework and owns Tokio/runtime policy.
- Spargen replaces the previous client generator.
- Rawshift replaces in-repository codecs and metadata extraction.
- Capsule calls Chromahash directly after v1.
- Blob storage, resumable upload, provenance, crypto, CRDTs, asset lifecycle, and application state
  ports stay in Capsule.

No new reusable infrastructure library is created until a stable product-neutral contract and at
least two real consumers demonstrate that extraction removes net complexity.
