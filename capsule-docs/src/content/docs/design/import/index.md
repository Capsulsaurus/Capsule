---
title: Import and Synchronization
description: Overview of how Capsule imports assets and synchronizes them across devices
status: draft
---

We define **import** as the process of taking assets from an external source (a camera, a directory on the filesystem) and bringing them into Capsule's management. Once imported, assets travel between devices via the **upload protocol** (client → server) and the **sync feed** (server → client).

The three concerns live in separate sub-docs because they correspond to distinct modules that can be implemented and validated independently:

| Sub-doc                                            | Concern                                                                                            | Primary crate(s)                                               |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| [Pipeline](/design/import/pipeline/)               | Local scan, plan, execute — the import workflow on a single device                                 | `capsule-core::import`                                         |
| [Upload Protocol](/design/import/upload-protocol/) | Capsule-owned encrypted resumable transfer, session lifecycle, finalization, reliability | planned `capsule-sdk::upload` + `capsule-api::upload` |
| [Download & Sync](/design/import/download-sync/)   | Sync feed, tiered fetch, stale-revival defense, auto-sync                                | planned `capsule-sdk` + `capsule-api::sync`          |
| [Storage Verification](/design/import/storage-verification/) | Confirming an asset is durably stored, indexed, and retrievable before any destructive local action | planned `capsule-api::blob` + `capsule-sdk`         |

[Encrypted backups](/design/backup-recovery/) are a separate artifact format; [peering](/design/peering/) reuses the backup artifact for device-to-device sync rather than the upload/sync protocols.

## End-to-End Flow

```text
[Local source]
     │
     ▼ scan, extract metadata
[Pipeline] ── plan ──▶ user confirms
     │
     ▼ encrypt + sign + generate derivatives
[Upload Protocol] ── session → chunks → finalize ──▶ server blob store + Postgres
     │
     ▼ sync feed advances
[Download & Sync] ── /sync (metadata) → /blob/{hash} (lazy original) ──▶ peer devices
```

Every stage is content-addressed, idempotent, and resumable. Session state in the upload path and cursor state in the sync feed are the two pieces of mutable cross-module state; both are owned by their respective sub-docs.
