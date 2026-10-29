---
title: Transfer Protocol
description: The bidirectional, content-addressed protocol that moves encrypted blob bytes between Capsule clients and their home server
status: draft
---

The **transfer protocol** moves blob bytes between a client and its home server, in both directions. It is Capsule-owned: session state, encrypted-blob provenance, finalization and access are one end-to-end security contract, so no generic transfer library or TUS compatibility is offered. The server never holds a key; every byte it moves is opaque ciphertext, except the signed provenance manifest, which is server-visible by design.

| Page | Covers |
| --- | --- |
| [Upload](/design/transfer/upload/) | Resumable upload sessions, chunk rules, finalization, staged tiers T0/T1/T2, error codes |
| [Download](/design/transfer/download/) | Tiered and ranged fetch, blob access scoping, the fallback ladder, prefetch |
| [Durability](/design/transfer/durability/) | Storage verification, deep re-hash, custody receipts, releasing local originals |

*What* changed — the metadata feed a client reads before deciding what to fetch — is [Sync](/design/sync/), not part of this protocol. Implementations: `capsule-server::upload`, `capsule-server::blob`, `capsule-server::verify`, `capsule-server::attestation`, `capsule-sdk::upload`, `capsule-sdk::fetch`, `capsule-sdk::verify`, `capsule-sdk::staged`. The wire shapes are the [OpenAPI contract](/reference/api/); these pages fix the semantics the schema cannot express.

## Invariants

Every implementation, on either side, MUST uphold:

1. **Blobs are immutable and content-addressed.** A blob is named by the hash of its stored bytes ([ciphertext content hash](/design/cryptography/primitives/)); bytes at an address never change. Identical bytes are one object.
2. **The wire is strict.** A request that violates the contract is rejected with a machine-readable `error.*` code, never silently corrected or partially honoured.
3. **A chunk is acknowledged only after it is durably committed.** The server fsyncs the staged range and records it in the committed set before answering `204`; an acknowledged byte is a byte on disk, and no write ever touches a committed byte.
4. **Finalize re-hashes before commit.** The server recomputes the content hash over the staged bytes and re-runs the envelope checks before the blob is committed to its address. A mismatch is corruption, never retried server-side.
5. **An index row never points at a missing blob.** The blob is committed (atomic rename plus directory fsync) before any index row references it. The failure direction is an unreferenced blob, which GC collects, never a dangling reference.
6. **Custody and receipt are one fact.** A receipt is written in the same transaction that indexes the committed blob; neither exists without the other.
7. **A client releases a local original only on server custody**: a `durable` storage verdict, a verified signed custody receipt whose server-recomputed hash matches the bytes the client sent (the read-back hash), and [`verify_asset`](/design/cryptography/keys/#write-authorization) acceptance. See [Verify Before Destroy](/design/transfer/durability/#verify-before-destroy).
8. **Resume is driven by server truth, not client state.** The committed set comes from `HEAD`, in-flight sessions from `GET /v1/upload/sessions`, held tiers from the sync feed. Client-side queues are rebuildable caches.
9. **Every transfer step is idempotent.** Re-creating a session, re-sending a chunk, re-finalizing, re-fetching a range and re-verifying all converge on the same state.
10. **Every session terminates.** A session is driven to `Completed` or `FailedProcessing`, or discarded; no staged file or reserved row outlives it.
11. **The server never produces manifest bytes.** Manifests travel as `provenance` blobs and are served verbatim.
12. **Semantics never depend on the transport.** Every operation means the same over HTTP/1.1, HTTP/2 and HTTP/3, and correctness survives the loss of any connection at any moment ([HTTP Transport](/design/networking/#http-transport)).
13. **No request waits on unbounded server work.** A request answers with the state as it stands; finalization and other long work run detached and are observed through `HEAD`.

## Flow

```text
client: encrypt + sign bundle
  │  POST /v1/upload ─▶ PATCH chunks ─▶ finalize (claim → re-hash → envelope → commit → index → receipt)
  ▼
server blob store + asset index ──▶ sync feed advances
  │
  ▼  other devices: GET /v1/sync (metadata) ─▶ GET /v1/blob/{hash} (ranged, per tier)
uploader: POST /v1/storage/verify + receipt ─▶ release local original
```
