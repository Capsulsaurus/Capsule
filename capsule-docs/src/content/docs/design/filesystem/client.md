---
title: Client Filesystem
description: How clients lay out a library on disk — desktop, mobile, local index, and space recovery
---

Clients hold keys, so a client stores plaintext. Desktop clients keep a self-contained library directory; mobile clients use platform-sandboxed storage. The cross-platform logic lives in `capsule-core::library` (paths, init, open) and `capsule-core::db` (SQLite cache); per-platform glue lives in `capsule-sdk` and native client code.

What a client keeps locally depends on its sync setting — *metadata only*, *metadata + thumbnails*, or *metadata + thumbnails + original* (see [Import — Synchronization Scope](/design/import/download-sync/#synchronization-scope)). A library therefore routinely contains assets whose original is server-only, and the layout must represent an asset whether or not its original bytes are present locally.

The directory layout below is itself a contract — the recovery-first rebuild assumes exactly these filenames and sharding rules.

## Desktop Library Layout

```text
{library_root}/
├── media/
│   └── {YYYY}/{YYYY-MM}/
│       ├── {uuid}.{ext}            # original media (plaintext; absent if not synced locally)
│       ├── {uuid}.cbor             # canonical metadata sidecar (plaintext, signed)
│       └── {uuid}.provenance.cbor  # append-only signed provenance chain
├── cache/
│   ├── thumbnails/{size}/{uuid[0:2]}/{uuid[2:4]}/{uuid}.{fmt}
│   ├── meta/{uuid[0:2]}/{uuid[2:4]}/{uuid}.meta.cbor    # verbose parsed metadata
│   └── transcodes/{uuid[0:2]}/{uuid[2:4]}/{uuid}.{ext}
├── index/
│   └── library.sqlite              # rebuildable query + vector index
└── .library/
    ├── version                     # library schema version
    ├── config                      # user preferences and library state
    ├── lock                        # process lock file (ephemeral)
    ├── trash/
    │   └── {uuid}.{ext}            # soft-deleted media
    └── quarantine/
        ├── {uuid}.{ext}            # irreplaceable bytes that failed validation
        └── {uuid}.reason.json      # parse error / signature failure / schema mismatch
```

- **`media/`**: originals, their sidecars, and their provenance chains. Filenames are `{UUIDv7}.{extension}` (always lowercase), `{UUIDv7}.cbor`, and `{UUIDv7}.provenance.cbor` respectively. The CBOR sidecar is the client's canonical, self-describing metadata record (see [Metadata — Sidecar Schema v1](/design/metadata/#sidecar-schema-v1)) — the plaintext counterpart of the encrypted metadata blob the server stores. The `.provenance.cbor` file is an append-only signed log per asset (see [Cryptography — Provenance](/design/cryptography/provenance/#provenance-of-library-modifications)); the client never deletes it, so a hard-deleted asset leaves a tombstone-with-history. Per the recovery-first principle, the entire library is reconstructible from these three files alone. Files are date-bucketed by capture timestamp because the client, unlike the server, can read capture dates.
- **`cache/`**: purely derived and rebuildable — thumbnails and previews (formats declared in [Thumbnails — Thumbnail and Preview Formats](/design/thumbnails/#thumbnail-and-preview-formats)), verbose parsed-metadata caches, and transcodes. Sharded by UUID prefix to bound directory sizes. Deletable at any time; never a source of truth.
- **`index/library.sqlite`**: a rebuildable query cache over the sidecars, and the local vector index backing AI features (`sqlite-vec` — see [AI/ML Integrations](/design/ai/)). It is also the substrate for [view albums](/design/organization/#system--smart-albums-views) — system aggregations like *All* and user-defined smart albums are materialized by querying this index entirely client-side, with no server involvement. On a schema change it may be dropped and rebuilt rather than migrated, since it is always reconstructible.
- **`.library/`**: library-scoped state — schema version, user configuration, a process lock file that prevents two app instances from opening the same library, the trash (soft-delete retention area), and `quarantine/` (where irreplaceable bytes that failed structural or signature validation are preserved verbatim alongside a `.reason.json` recording the rejection). The quarantine area is the union surface listed in [Threat Model — Quarantine Surfaces](/design/threat-model/scenarios/#quarantine-surfaces). The `version` file pins the on-disk layout schema; a layout bump rebuilds derived structures (cache, index) and never touches the canonical original/sidecar/provenance files, so it cannot lose data.

The full sidecar and SQLite schemas are owned by [Metadata](/design/metadata/) and not duplicated here.

## Mobile Clients

Android and iOS use platform-sandboxed storage rather than a user-visible library directory. The logical model is the same — originals (when synced), canonical metadata, rebuildable caches, and a local SQLite index — but placement follows each platform's sandbox rules. Capsule deliberately does **not** store rebuildable derivatives in OS-managed cache locations: the OS may evict them indiscriminately, and a thumbnail that is expensive to regenerate is not genuinely disposable (see [Space Recovery](#space-recovery)).

## Local Index Staleness

SQLite may lag the filesystem after external edits or interrupted operations. The client verifies file existence before acting on an index row and triggers a full rebuild from sidecars when it detects structural inconsistency. Because the index is always rebuildable, this recovery is safe. Detection and rebuild details are owned by [Maintenance](/design/filesystem/maintenance/).

## Space Recovery

Rebuildable data is deliberately **not** stored in OS-managed cache locations: the OS evicts indiscriminately, and a thumbnail that is expensive to regenerate is not genuinely disposable. Capsule manages reclamation itself — it surfaces the biggest storage consumers and lets the user selectively delete, and an original that is server-only after eviction is transparently re-fetched on demand.

## Validation

- **Library init/open round-trip (unit).** Create an empty library; open it; assert all directories present and `version`/`config` populated. Re-open; assert idempotency.
- **Date-bucketing correctness (unit).** Given a sidecar's `capture_timestamp`, assert the asset lands in exactly `media/{YYYY}/{YYYY-MM}/`. Negative test: capture timestamp inconsistent with directory bucket triggers a [maintenance](/design/filesystem/maintenance/) repair.
- **Process lock contention (smoke).** Open the library in process A; attempt to open in process B; assert clean refusal with a structured error.
- **Mobile sandbox placement (smoke per platform).** Per-platform test asserts the library is placed in the OS-blessed location for app private storage and survives an app cold-start.
- **Local index rebuild from sidecars (smoke).** Populate a library; drop `library.sqlite`; re-open; assert the index is rebuilt and queries return the same results as before.

Cross-module case (full library lifecycle: import → upload → restore on a fresh client) is bounded E2E surface in [Module Map](/design/module-map/#e2e-test-surface).
