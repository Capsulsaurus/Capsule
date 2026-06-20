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

Rebuildable data is deliberately **not** stored in OS-managed cache locations: the OS evicts indiscriminately, and a thumbnail that is expensive to regenerate is not genuinely disposable. Capsule manages reclamation itself, on two paths — an automatic, bounded cache it keeps within budget on its own, and an explicit user-driven path for deeper reclamation. Either way, an original that is server-only after eviction is transparently re-fetched on demand (see [Import — Tiered, On-Demand Fetch](/design/import/download-sync/#tiered-on-demand-fetch)).

What is eligible for reclamation is exactly the rebuildable-or-refetchable set: the `cache/` tree (thumbnails, previews, parsed-metadata caches, transcodes) and fetched-but-unpinned originals. The canonical files under `media/` — originals the device itself holds as source of truth, their `.cbor` sidecars, and their `.provenance.cbor` chains — are **never** eviction targets; neither is the rebuildable `index/library.sqlite`, which is dropped and rebuilt only on a schema change.

### Automatic cache management

The reclaimable set is held within a **user-configurable cache budget**. When it grows past budget — typically while browsing a large library on a device that cannot hold everything — Capsule reclaims space itself rather than waiting for the user or letting the OS decide:

- **Recency promotion.** Viewing or opening an asset stamps a last-access time on its fetched representations in `library.sqlite`. Recently-viewed content is therefore the *last* to go, so scrolling back through an album already browsed on a high-latency or metered connection hits local cache instead of the network.
- **LRU eviction.** When over budget, representations are evicted **least-recently-accessed first**, by that last-access stamp — the same bounded-cache discipline the federation layer applies to its rejected-hash table (see [Federation — Soft-Fail Semantics](/design/federation/#soft-fail-semantics)).
- **Tier order within a sweep.** Where recency does not decide it, eviction proceeds in descending size and ascending value: **original → preview → thumbnail**. The metadata tier — the sidecar and its embedded LQIP (see [Thumbnails](/design/thumbnails/)) — is tiny and canonical and is effectively never reclaimed, so an asset always remains listable and previewable at LQIP fidelity even after every heavier representation is gone.
- **Pin exemption.** Representations the user has explicitly pinned for offline use, and originals the device itself uploaded and still owns as source of truth, are exempt from automatic eviction regardless of recency or budget pressure.

**Releasing a device-owned original is gated on server durability.** A pinned or device-owned original is exempt from *automatic* eviction, but it can still be **deliberately released** — by the [user-driven path](#user-driven-reclamation) below, or automatically by a storage-constrained [streaming import](/design/import/pipeline/#import-upload-streaming-mode). Because such an original may be the only copy of irreplaceable bytes, a client releases it only after [`POST /storage/verify`](/design/import/storage-verification/#verify-before-destroy) returns a `durable` verdict and [`verify_asset`](/design/cryptography/keys/#write-authorization) accepts it; once released it becomes an ordinary server-only asset, transparently re-fetched on demand. This verify-before-destroy gate governs every post-write local cleanup, not just eviction — see [Storage Verification](/design/import/storage-verification/).

An evicted representation is not lost: the next access transparently re-fetches it through the [tiered fetch](/design/import/download-sync/#tiered-on-demand-fetch) path, under the prevailing connection rules. This keeps the cache faithful to the recovery-first and ephemeral-derived-data [principles](/design/principles/#principles) — nothing here is a source of truth, so reclaiming it is always safe.

### User-driven reclamation

Beyond the automatic budget, Capsule surfaces the biggest storage consumers and lets the user selectively delete — for reclaiming below the configured budget, or for dropping pinned content the user no longer wants offline. This path can release pinned representations the automatic sweep would not, but it still never touches the canonical `media/` files, and releasing a device-owned original is gated on a `durable` [storage-verification](/design/import/storage-verification/#verify-before-destroy) verdict just as above.

## Validation

- **Library init/open round-trip (unit).** Create an empty library; open it; assert all directories present and `version`/`config` populated. Re-open; assert idempotency.
- **Date-bucketing correctness (unit).** Given a sidecar's `capture_timestamp`, assert the asset lands in exactly `media/{YYYY}/{YYYY-MM}/`. Negative test: capture timestamp inconsistent with directory bucket triggers a [maintenance](/design/filesystem/maintenance/) repair.
- **Process lock contention (smoke).** Open the library in process A; attempt to open in process B; assert clean refusal with a structured error.
- **Mobile sandbox placement (smoke per platform).** Per-platform test asserts the library is placed in the OS-blessed location for app private storage and survives an app cold-start.
- **Local index rebuild from sidecars (smoke).** Populate a library; drop `library.sqlite`; re-open; assert the index is rebuilt and queries return the same results as before.
- **LRU eviction order (unit).** Fill the reclaimable set past the cache budget; assert the least-recently-accessed representations are evicted first and the budget is restored; assert no canonical `media/` file is touched.
- **Tier-order eviction (unit).** With representations of equal recency over budget, assert eviction proceeds original → preview → thumbnail, and that the metadata tier (sidecar + LQIP) is never reclaimed.
- **Recency promotion (unit).** View an asset to stamp its last-access, then trigger an over-budget sweep; assert its representations survive while older ones are evicted.
- **Pin exemption (unit).** Pin a representation for offline use; push the cache over budget; assert the pinned representation survives the automatic sweep and is reclaimable only via the user-driven path.
- **Verify-before-release gate (smoke).** Attempt to release a device-owned original with [`/storage/verify`](/design/import/storage-verification/) mocked to return non-`durable`; assert the original is retained and the unconfirmed state surfaced; return `durable`; assert the release proceeds and the asset becomes a server-only, re-fetchable representation.

Cross-module case (full library lifecycle: import → upload → restore on a fresh client) is bounded E2E surface in [Module Map](/design/module-map/#e2e-test-surface).
