---
title: Filesystem
description: How Capsule structures files on disk — server vs client, and what they share
---

Capsule's end-to-end encryption splits the filesystem into two fundamentally different roles. The **server** stores only opaque, content-addressed ciphertext — it never holds a decryption key and cannot interpret a single byte it stores. **Clients** hold the keys, so a client filesystem is a working library of plaintext media, sidecar metadata, and rebuildable caches. The two layouts share a small set of principles but otherwise have little in common.

The on-disk layout is itself part of the contract — the filenames, directory structure, and atomic-write conventions are how recovery-first becomes operational, so they appear here verbatim rather than as suggestion.

## Sub-docs

| Sub-doc                                         | Concern                                                                           | Primary crate(s)                                                     |
| ----------------------------------------------- | --------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| [Server Filesystem](/design/filesystem/server/) | Blob store layout, Postgres index, deployment profiles, ownership, deletion       | `capsule-api` + storage glue                                         |
| [Client Filesystem](/design/filesystem/client/) | Desktop / mobile library layout, local SQLite index, space recovery               | `capsule-core::{library,db}` + per-platform glue                     |
| [Maintenance](/design/filesystem/maintenance/)  | Self-validation, scrubbing, repair, intra-library dedup, atomic-write granularity | `capsule-core::library` (client) + `capsule-api` (server-side scrub) |

This index covers the principles both sides share. The import pipeline, the upload protocol, and synchronization are covered in [Import and Synchronization](/design/import/); metadata extraction in [Metadata](/design/metadata/); derivative generation in [Thumbnails and Previews](/design/thumbnails/); grouping and trash semantics in [Asset Organization](/design/organization/); backup and recovery in [Backup and Recovery](/design/backup-recovery/).

## Shared Principles

These follow directly from [Core Principles](/design/principles/):

- **Recovery-first.** No database is required to interpret canonical data. On the client, sidecar files are the source of truth and the index is a rebuildable cache. On the server, PostgreSQL is the authoritative index, but it holds only key-free facts.
- **Atomic writes.** Every write that must not tear uses temp-file + atomic rename on the same filesystem. Direct overwrites risk corruption on power loss. The full per-granularity rules live in [Maintenance — Atomic Writes](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery).
- **Ephemeral derived data.** Only originals and their canonical metadata are irreplaceable. Thumbnails, transcodes, parsed-metadata caches, and the query index can all be regenerated and are treated as such.
- **4 KiB alignment.** Data is processed and written block-aligned to 4 KiB, which matches memory and disks and enables the [reflink assembly path](/design/import/upload-protocol/#server-side-storage-and-assembly).
- **Content-addressing.** Stored blobs are named by their ciphertext content hash — the same hash everywhere a content address is needed (see [Cryptography — Primitives](/design/cryptography/primitives/)).

## Server vs Client at a Glance

| Concern      | Server                                     | Client                                        |
| ------------ | ------------------------------------------ | --------------------------------------------- |
| Holds keys   | No                                         | Yes                                           |
| Stored form  | Opaque ciphertext blobs                    | Plaintext media + CBOR sidecars               |
| Naming       | Content-addressed by ciphertext hash       | UUIDv7 stems, date-bucketed                   |
| Index        | PostgreSQL (key-free facts only)           | SQLite (rebuildable, full plaintext metadata) |
| Derived data | Stored as client-generated encrypted blobs | Generated locally, cached, rebuildable        |
| Originals    | Always retained while referenced           | Present only if synced locally                |
