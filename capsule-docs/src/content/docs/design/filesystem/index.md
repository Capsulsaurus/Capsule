---
title: Filesystem
description: How Capsule structures files on disk — server vs client, and what they share
status: draft
---

The **server** stores only opaque, content-addressed ciphertext and never holds a key. **Clients** hold keys, so a client stores plaintext media, signed sidecars, and rebuildable caches. The on-disk layouts are part of the contract.

| Sub-doc | Concern |
| --- | --- |
| [Server Filesystem](/design/filesystem/server/) | Blob store layout, Postgres index, required services, deletion |
| [Client Filesystem](/design/filesystem/client/) | Library layout, local SQLite index, space recovery |
| [Maintenance](/design/filesystem/maintenance/) | Validation, scrubbing, repair, intra-library dedup, atomic writes |

Byte transfer is the [transfer protocol](/design/transfer/); metadata sync is [Sync](/design/sync/).

## Shared Principles

- **Recovery-first.** No database is needed to interpret canonical data. Client sidecars are the source of truth; the server's PostgreSQL index holds only [key-free facts](/design/filesystem/server/#postgresql-what-the-server-knows) and is rebuildable from stored manifests.
- **Atomic writes.** Temp file + atomic rename on the same filesystem; never overwrite in place ([rules](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery)).
- **Ephemeral derived data.** Only originals and their canonical metadata are irreplaceable.
- **4 KiB alignment.** Upload chunks are 4 KiB-aligned so the [append path](/design/transfer/upload/#append-only-storage) stays block-aligned.
- **Content-addressing.** Server blobs are named by their [ciphertext hash](/design/cryptography/primitives/).

| Concern | Server | Client |
| --- | --- | --- |
| Stored form | Opaque ciphertext blobs | Plaintext media + CBOR sidecars |
| Naming | Ciphertext hash | UUIDv7, date-bucketed |
| Index | PostgreSQL, authoritative, key-free | SQLite, rebuildable |
| Derived data | Client-generated encrypted blobs | Generated locally, evictable |
| Originals | Retained while referenced | Present only if synced locally |
