---
title: Download and Synchronization
description: How Capsule clients discover changes, fetch blobs on demand, and auto-sync
---

Download is the inverse of [upload](/design/import/upload-protocol/), and rests on the same two foundations: blobs are **content-addressed by ciphertext hash**, and the server never holds a key, so it serves only opaque ciphertext. Where the upload path optimises for correctness under interruption, the download path optimises for **bandwidth and storage frugality** — a client fetches the smallest representation that satisfies the user's current intent, and nothing more.

The download client lives in `capsule-sdk` (per-platform glue handles cache placement and connection-class detection); the server side — the sync feed and blob fetch — lives in `capsule-api-sync`. The `/sync` feed format is the **contract** other modules consume; its versioning and per-album monotonic ordering are what defeats the stale-rewind attack class.

## Discovering What Changed

A client never polls assets individually. It holds a single opaque **sync cursor** and asks the server for everything that changed after it:

| Method | Path           | Purpose                                                                                                                                         |
| ------ | -------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET`  | `/sync`        | Returns a page of asset changes (created, metadata-updated, deleted) after `cursor`, with a `next_cursor`. The feed is monotonic and resumable. |
| `GET`  | `/blob/{hash}` | Fetch a ciphertext blob by its content address. Supports HTTP `Range` for resumable and partial reads.                                          |

The `/sync` feed carries only the small encrypted **metadata blobs** and each asset's **blob manifest** — the content hashes of its original and derivative blobs — never original or derivative bytes. Discovering a thousand new assets costs a few hundred kilobytes. The client decrypts each metadata blob, learns the asset's dimensions, capture date, and LQIP, and only *then* decides what else, if anything, to fetch. A deleted or modified asset arrives as a tombstone or an updated metadata reference; the client reconciles local state against it (see [Synchronization Scope](#synchronization-scope)).

**Cursor authenticity.** The opaque sync cursor is **MAC'd by the server** (HMAC-SHA256) under a server-only key and verified on every `/sync` (and [federation pull](/design/federation/#federation-reuses-existing-primitives)) request, so a client cannot forge or mutate a cursor and a cursor lifted from another context is rejected at the boundary. The MAC is the *authenticity* layer; the per-album monotonic `sync_seq` check below is the independent *anti-rewind* layer. They are separate on purpose: a malicious server can always hand back one of its own *older*, validly-MAC'd cursors, and only the client-held high-water mark defeats that. Together they close the [sync-cursor rewind class](/design/threat-model/scenarios/#damage-scenario--invariant-map).

**Sync feed validation.** Every entry in a `/sync` response carries a `protocol_version` (matching the album's pin) and a per-album monotonic `sync_seq`. The client refuses to apply an entry whose `protocol_version` is above its max known (per the [tightened Postel's Law](/design/principles/#postels-law-asymmetric)) and refuses any page whose `sync_seq` regresses against what the client has already seen for that album — a regressing `sync_seq` indicates a malicious or buggy server attempting to rewind the client's view, and the client surfaces it rather than applying it.

## Stale-Revival Detection

A malicious or buggy server, peer, or backup could submit an old-but-validly-signed manifest to resurrect an asset that the receiving device has tombstoned at a later state. The defense — owned by [Cryptography — Provenance](/design/cryptography/provenance/#provenance-of-library-modifications) — is the per-asset `prior_provenance_hash` chain. Two layers enforce it:

- **Client.** Every device's local index stores a `latest_provenance_hash` per `asset_id`. When a sync entry, federation pull, peering artifact, or backup restore proposes a manifest whose `prior_provenance_hash` is **behind** that local value, the entry is **quarantined** (see [Threat Model — Quarantine Surfaces](/design/threat-model/scenarios/#quarantine-surfaces)) and surfaced as "peer sent stale state."
- **Server (no-key).** The server stores the same `latest_provenance_hash` per asset in PostgreSQL and rejects any incoming non-`create` manifest whose `prior_provenance_hash` does not match. This is described in the [server-side validation invariants](/design/threat-model/validation/#server-side-validation-invariants).

A deleted asset cannot be silently resurrected, on either side, without the resurrection appearing as a quarantine surface to the user.

## Tiered, On-Demand Fetch

Each asset has a ladder of representations, cheapest first:

1. **LQIP** — embedded in the metadata blob (see [Thumbnails](/design/thumbnails/)); available the instant metadata syncs, at zero extra request.
2. **Thumbnail** — fetched when the asset scrolls into, or near, view in a grid.
3. **Preview** — a screen-resolution derivative, fetched when the asset is opened.
4. **Original** — fetched only on explicit demand: viewing at full fidelity, exporting, or sharing the original.

The default policy follows the per-library setting in [Synchronization Scope](#synchronization-scope) — *metadata only*, *metadata + thumbnails*, or *metadata + thumbnails + original*. Anything above the configured tier is fetched lazily, on demand. The original is never fetched speculatively unless the device was its uploader, in which case it already holds the plaintext locally and downloads nothing.

Because every blob is content-addressed, a fetch is skipped entirely when the blob is already in the local cache — the client looks up its cache by hash before issuing any request, so a representation shared between assets (an identical thumbnail, a merged original) is only ever fetched once.

**When an above-tier fetch cannot succeed.** A lazily-fetched representation may be temporarily or permanently unavailable. The client distinguishes the two: a **transient** failure (network drop, `5xx`) retries with backoff and resumes via `Range`; a **permanent** failure (`410 Gone`, `403`, a purged origin, or an unreachable [federated home server](/design/federation/#robustness-against-connectivity-loss)) **degrades gracefully** to the best representation already in hand — preview → thumbnail → LQIP, down to the always-present LQIP — and surfaces a non-destructive "full resolution unavailable" state on the asset. It never thrashes the fetch, and it never removes the asset's metadata or local index entry over a missing derivative. The asset stays listed and re-fetches automatically once the representation becomes reachable again.

## Resumption and Verification

- Large originals are fetched with HTTP `Range` requests; an interrupted download resumes from the last persisted byte instead of restarting, mirroring the [upload protocol's](/design/import/upload-protocol/) resumability.
- The client verifies integrity itself. Since the server can only attest to ciphertext, the client recomputes the [ciphertext content hash](/design/cryptography/primitives/) against the requested content address, then decrypts and relies on the [STREAM construction](/design/cryptography/encryption/#stream-construction)'s authentication tags to detect truncation, reordering, or chunk deletion. Any failure discards the blob and re-fetches it.

## Prefetch and Frugality

- Prefetch is bounded and predictive — thumbnails for assets just beyond the viewport, the preview for the likely-next asset in a sequence — and is cancelled as soon as the user's focus moves.
- Prefetch and any above-tier fetch obey the same connection rules as [Auto Syncing](#auto-syncing): on a metered connection the client fetches only what the user explicitly opens, and defers the rest.
- Fetched-but-unpinned blobs are ordinary cache citizens, subject to [Space Recovery](/design/filesystem/client/#space-recovery); the client transparently re-fetches them on demand if they are evicted.

## Auto Syncing

On mobile clients, auto syncing keeps new assets backed up (not to be confused with [encrypted backups](/design/backup-recovery/)) to the server and pulls assets from other devices onto the device.

### Synchronization Criteria

Sync is checked conservatively. When a check fires, the client reconciles everything that needs syncing — uploads and downloads — and proceeds as long as the criteria below hold throughout the transfer. If conditions change mid-transfer (e.g. the connection becomes metered), it re-evaluates and pauses gracefully; the server never assumes a transfer runs to completion in one session (see [Upload Protocol — Robustness](/design/import/upload-protocol/#robustness)).

The actual synchronization criteria are strict and scale with the reconciliation amount (i.e. total upload + download transfer):

- **Small reconciliation** — a handful of new assets, or metadata-only deltas: synced proactively whenever the device has any non-metered connection.
- **Large reconciliation** — bulk uploads, or original-tier downloads: deferred until the device is connected to unmetered Wi-Fi.

### Platform Limitations

Auto sync is implemented **only** if it can be guaranteed to behave appropriately under all scenarios. It is explicitly not implemented on platforms that lack the APIs we need (e.g., detecting metered connections), to avoid surprises.

### Notifications

When the auto sync criteria have not been met for a prolonged period — **two weeks** specifically — the library falls silently out of date, which defeats the purpose of a backup. The client surfaces this rather than letting it pass unnoticed:

- After two weeks without a completed sync *while changes remain un-synced*, the user is notified that the library is behind and offered a one-tap **force sync now**, which proceeds regardless of the metered/Wi-Fi criteria with their explicit consent.
- The notification can be **snoozed** until a later date (e.g. another two weeks) or **disabled** outright. Snoozing only suppresses the warning; disabling opts out of the warning entirely and does not affect auto sync itself.

## Synchronization Scope

- **Uploadable new content:** the source (original) asset is uploaded along with all associated metadata and derivatives.
- **Modified/deleted content:** associated metadata is updated.
- **Fetch new content:** depending on setting, metadata only / metadata + thumbnails / metadata + thumbnails + original is fetched for all new assets. Unless the original already exists locally (e.g., if the device was the original uploader), the original is only fetched on demand (e.g. the user explicitly views the original or shares the original with others). This is to save bandwidth and storage on client devices. Metadata includes LQIP which can be used as a preview before even thumbnails are fetched.

## Validation

- **Sync feed monotonicity (unit).** Server-side unit tests assert that every `sync_seq` advance over a given album is strictly increasing; concurrent writes are linearised by the same Postgres transaction that mints the new `sync_seq`.
- **Sync feed forward-version rejection (unit).** Client-side unit test that a feed entry whose `protocol_version` is above the client's max known is rejected without partial application.
- **Sync feed rewind rejection (unit).** Client-side unit test that a page whose `sync_seq` regresses against the locally-seen high-water mark is surfaced, not applied.
- **Sync cursor authenticity (unit).** Server-side: present a cursor with a tampered or forged MAC; assert boundary rejection. Client-side: present a validly-MAC'd but *older* cursor; assert the monotonic `sync_seq` high-water-mark check still refuses the rewind.
- **Above-tier permanent unavailability (unit).** With scope set so the original is on-demand, make `/blob/{hash}` return `410`; assert the client degrades to the next-lower locally-held representation, surfaces "full resolution unavailable", and leaves the asset's metadata + index entry intact; restore availability; assert automatic re-fetch.
- **Tiered fetch correctness (unit).** Per-tier policy is unit-testable: configure scope = *metadata + thumbnails*, present a sync entry with original + thumbnails + LQIP, assert only metadata + thumbnails are fetched.
- **Resume after interrupt (smoke).** Start a large original fetch; interrupt mid-Range; resume; assert byte-identical result with no re-fetched bytes.
- **Auto-sync state machine (smoke).** Simulate connectivity changes (Wi-Fi → metered → offline → Wi-Fi); assert the scheduler pauses, resumes, and respects the small/large threshold.
- **Cross-asset dedup hit (unit).** Two assets with the same thumbnail hash; the second viewing must not refetch.

The cross-module case — server emits a sync entry → client applies and fetches blob — is bounded E2E surface listed in [Module Map](/design/module-map/#e2e-test-surface).
