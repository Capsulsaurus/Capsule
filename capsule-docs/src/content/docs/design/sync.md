---
title: Sync
description: The metadata sync feed — cursors, anti-rewind, stale-revival detection, auto-sync criteria and background execution
status: draft
---

The sync feed tells a client **what changed**; the [transfer protocol](/design/transfer/) moves the bytes. A client never polls assets individually: it holds one opaque cursor and reads everything after it. Server: `capsule-server::sync`; client: `capsule-sdk::sync`, applied by `capsule-library::lifecycle::sync_apply`.

## Discovering What Changed

`GET /v1/sync` returns a page of asset changes (create, update, delete) after a cursor, with the next cursor. With `album_id`, it returns one shared album's page to its owner, a current roster member, or a federated peer holding a capability for that album. Anyone else gets `403 error.sync.album_access_denied`; a peer naming another album gets `403 error.federation.audience_mismatch`. A member's album page starts at the album's first entry whatever epoch they joined at; what they can decrypt is the MLS `Welcome`'s decision, not the feed's.

Each entry carries:

- the signed manifest — **the exact bytes uploaded** as the provenance blob, never re-encoded;
- the small encrypted metadata blob;
- the blob manifest (content hashes of the original and derivatives);
- each derivative tier's signed `DerivativeManifest`, verbatim;
- `original_held`, the derived [awaiting-original](/design/transfer/download/#awaiting-original) fact.

Never original or derivative bytes. The client decrypts metadata, then decides what to [fetch](/design/transfer/download/).

A delete carries only its signed manifest, the `delete` record whose `retention_until` closes the asset's [retention window](/design/organization/#retention-window): no metadata, no blob manifest and no derivative manifest, since none is needed to apply a deletion and a blob reference would invite a fetch of collected bytes. A purge drops the record with the rest of the asset's references, so the delete entry of a purged asset carries no manifest at all, and carries `purged: true` instead. The purge mints no new position: the flag rides the tombstone's existing entry, so a client that read the entry before the purge judges the window from the record it already holds.

`purged` is the only evidence of a purge a client acts on. A client records a peering tombstone for a delete whose window it has seen close, or whose entry is `purged`; a delete entry with neither stays in trash. An absent manifest is never read as a purge: the server also omits it when the store cannot produce the blob, and a server that predates the record or the flag omits them on every tombstone, and a tombstone recorded on that reading would be permanent for an asset only moved to trash. The flag is additive and optional; its absence reads as `false`.

### Cursor Authenticity and Anti-Rewind

- The cursor is **MAC'd by the server** under a server-only key, bound to the caller and, for an album page, the album (`capsule-server::sync::cursor`). A forged, mutated or foreign cursor is refused; the client re-syncs from zero.
- The cursor carries the owner's **sync epoch**. After an [index rebuild](/design/filesystem/server/#sync-epoch-and-client-re-announce) moves the epoch, an older cursor is refused with `409 error.sync.cursor_stale`; the client drops its cursor and its high-water marks, resyncs from zero, and keeps everything it holds. What it holds that the resynced feed no longer lists it then [re-announces](/design/filesystem/server/#sync-epoch-and-client-re-announce): manifests only, which restore the rows whose blobs the server still holds.
- Every entry carries the album's `protocol_version` and a per-album, strictly increasing `sync_seq`. The client MUST refuse an entry above its maximum known version, and MUST surface — never apply — a page whose `sync_seq` regresses against its high-water mark.
- The two layers are independent: the MAC stops forgery, but only the client's high-water mark stops a server replaying its own older, valid cursor ([sync-cursor rewind class](/design/threat-model/scenarios/#damage-scenario--invariant-map)).
- A feed manifest is verified with [`verify_asset`](/design/cryptography/keys/#write-authorization) over the bytes as received. Bytes that do not decode as a provenance record are quarantined as malformed.

## Stale-Revival Detection

An old but validly signed manifest could resurrect an asset tombstoned later. The per-asset `prior_provenance_hash` chain ([Provenance](/design/cryptography/provenance/#provenance-of-library-modifications)) prevents it at two layers:

- **Client.** Each device stores `latest_provenance_hash` per asset. A manifest from sync, federation, peering or backup restore whose chain is behind it is [quarantined](/design/threat-model/scenarios/#quarantine-surfaces) as stale state.
- **Server.** The server stores the same value and rejects any non-`create` manifest whose `prior_provenance_hash` does not match ([validation invariants](/design/threat-model/validation/#server-side-validation-invariants)).

## Synchronization Scope

- **New local content** is uploaded with its metadata and derivatives, in the order the [upload policy](/design/transfer/upload/#upload-tiering-staged-uploads) sets.
- **Modified or deleted content** updates metadata or tombstones.
- **New remote content** is fetched per the library setting: metadata only, + thumbnails, or + thumbnails + original. The original is otherwise on demand.

## Auto Syncing

On mobile, auto sync uploads new assets and pulls others' assets. When a check fires, the client reconciles all pending uploads and downloads while the criteria hold, and re-evaluates if conditions change mid-transfer.

### Synchronization Criteria

- **Small reconciliation** (a few assets, metadata-only deltas): any non-metered [connection class](/design/networking/#connection-classes).
- **Large reconciliation** (bulk uploads, original-tier downloads, streaming import): unmetered Wi-Fi only.

### Platform Limitations

Auto sync is offered only on platforms that expose what it needs (e.g. metered-connection detection); elsewhere it is not implemented.

### Background Execution

Background and OS-scheduled transfer are post-v1 ([Clients — Platform Limitations](/design/clients/#platform-limitations)); v1 transfers run while the app is in the foreground. Any background transfer is held to:

- **Requests are self-contained.** Every chunk `PATCH` and `Range` `GET` is complete in its URL, headers and a body that is a slice of a file. An OS transfer daemon can therefore run it while the Rust core is suspended, and the core reconciles from server truth when it wakes.
- **Queued requests outlive the access token.** A request the OS queues may run after the access token has expired, so no queued request may depend on one; the credential that replaces it is specified with background transfer in [Authentication](/design/authentication/).

- **iOS.** Small work rides `BGAppRefreshTask`; large work requests `BGProcessingTask` with network (and power for very large batches).
- **Android.** `WorkManager` with explicit constraints (`UNMETERED` for large, `CONNECTED` for small, battery-not-low). A user-initiated large force-sync may run as a foreground service.
- **Desktop.** Self-throttled with the [maintenance gating rules](/design/filesystem/maintenance/#content-validation-expensive-scheduled).
- **All platforms.** Every window is preemptible: an OS kill loses at most the chunks in flight, uploads resume from the server's committed set and downloads via `Range`, and there is no "must finish" state. Retries follow the [bulk-transfer policy class](/design/networking/#retry-policy-classes).

### Notifications

After **two weeks** without a completed sync while changes remain un-synced (including originals pending under a staged policy), the client offers a one-tap **force sync now** that ignores the connection criteria with consent, routing through re-authentication if the session has [lapsed](/design/authentication/#sliding-inactivity-expiry). This is the `sync_stale` [alert class](/design/notifications/#alert-classes); it is [pre-armed](/design/notifications/#the-pre-arm-rule) so it fires on a device the OS never schedules. Delivery and snoozing are owned by [Notifications](/design/notifications/).
