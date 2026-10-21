---
title: Client Filesystem
description: How clients lay out a library on disk — desktop, mobile, local index, and space recovery
status: draft
---

Clients hold keys, so a client stores plaintext. A library routinely holds assets whose original is server-only, depending on the [sync scope](/design/sync/#synchronization-scope), so the layout represents an asset with or without its original. The layout is a contract: rebuild assumes exactly these names. The offline-first requirements it serves are owned by [Local Gallery](/design/local-gallery/).

## Desktop Library Layout

```text
{library_root}/
├── media/{YYYY}/{YYYY-MM}/
│   ├── {uuid}.{ext}            # original (plaintext; absent if not synced locally)
│   ├── {uuid}.cbor             # canonical signed metadata sidecar
│   └── {uuid}.provenance.cbor  # append-only signed provenance chain
├── cache/                      # thumbnails/, meta/, transcodes/ — sharded {uuid[0:2]}/{uuid[2:4]}
├── index/library.sqlite        # rebuildable query + vector index
└── .library/
    ├── version, config, lock
    ├── trash/{uuid}.{ext}
    └── quarantine/{uuid}.{ext} + {uuid}.reason.json
```

- **`media/`** is canonical: original, [sidecar](/design/metadata/#sidecar-schema-v1), and [provenance chain](/design/cryptography/provenance/#provenance-of-library-modifications). The library MUST be reconstructible from these three files alone. Stems are UUIDv7; extensions lowercase. The provenance file is never deleted, so a hard-deleted asset leaves a tombstone-with-history.
- The date bucket is fixed at import from the capture timestamp then known. A later capture-date correction does not move the bundle; the sidecar is authoritative and [maintenance](/design/filesystem/maintenance/#self-validation) repairs drift opportunistically.
- **`cache/`** is derived and deletable at any time; never a source of truth. Formats: [Thumbnails](/design/thumbnails/#thumbnail-and-preview-formats).
- **`index/library.sqlite`** is a query cache over the sidecars, the `sqlite-vec` index for [AI](/design/ai/), and the substrate for [view albums](/design/organization/#system--smart-albums-views). It is **migrated forward in place**, not dropped: a rebuild cannot recover projections never written to disk ([Versioning](/design/versioning/#client-catalog-migration)).
- **`.library/`**: the `lock` file MUST prevent two processes opening one library. `quarantine/` preserves irreplaceable bytes that failed validation, verbatim, with the rejection reason ([Quarantine Surfaces](/design/threat-model/scenarios/#quarantine-surfaces)). A `version` bump rebuilds derived structures only and never touches canonical files.

## Mobile Clients

Android and iOS use the app's private sandbox with the same logical model. Rebuildable derivatives MUST NOT be placed in OS-managed cache locations: the OS evicts indiscriminately, and an expensive-to-regenerate thumbnail is not disposable.

## Local Index Staleness

SQLite may lag the filesystem. The client checks file existence before acting on a row and rebuilds from sidecars on structural inconsistency ([Maintenance](/design/filesystem/maintenance/)).

## Space Recovery

Capsule reclaims space itself. Eligible: `cache/` and fetched-but-unpinned originals. **Never** eligible: canonical `media/` files the device holds as source of truth, sidecars, provenance chains, and `library.sqlite`. An evicted representation re-fetches transparently via [tiered fetch](/design/transfer/download/#tiered-on-demand-fetch).

### Automatic cache management

The reclaimable set is held within a user-configurable budget ([`capsule-core::library::cache_sweep`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-core/src/library/cache.rs)):

- Viewing stamps a last-access time; eviction is least-recently-accessed first.
- At equal recency, evict original → preview → thumbnail. The metadata tier (sidecar + LQIP) is never reclaimed, so an asset always stays listable.
- Pinned representations and device-owned originals are exempt from automatic eviction.

Driving the budget from [connection class](/design/networking/#connection-classes) is not yet enforced — tracked in the MVP roadmap.

**Releasing a device-owned original** (via the user-driven path or [streaming import](/design/import/pipeline/#import-upload-streaming-mode)) MUST pass the [verify-before-destroy](/design/transfer/durability/#verify-before-destroy) gate. Once released it is an ordinary server-only, re-fetchable asset.

### User-driven reclamation

The client surfaces the largest consumers and lets the user delete selectively, including pinned content. It never touches canonical `media/` files, and releasing a device-owned original is gated as above.

## Validation

- **Init/open round-trip (unit).** Directories and `version`/`config` present; re-open is idempotent.
- **Date bucketing (unit).** Asset lands in `media/{YYYY}/{YYYY-MM}/`; unexplained drift triggers repair.
- **Lock contention (smoke).** A second process is refused with a structured error.
- **Sandbox placement (smoke, per platform).** Library in app-private storage; survives cold start.
- **Index rebuild (smoke).** Drop `library.sqlite`; re-open; queries match.
- **Eviction (unit).** LRU order, tier order at equal recency, recency promotion, pin exemption; no canonical file touched; metadata tier never reclaimed.
- **Release gate (smoke).** Non-`durable` verdict retains the original and surfaces it; `durable` releases.
