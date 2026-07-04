---
title: Download and Synchronization
description: How Capsule clients discover changes, fetch blobs on demand, and auto-sync
status: draft
---

Download is the inverse of [upload](/design/import/upload-protocol/), and rests on the same two foundations: blobs are **content-addressed by ciphertext hash**, and the server never holds a key, so it serves only opaque ciphertext. Where the upload path optimises for correctness under interruption, the download path optimises for **bandwidth and storage frugality** — a client fetches the smallest representation that satisfies the user's current intent, and nothing more.

The download client lives in `capsule-sdk` (per-platform glue handles cache placement and [connection-class detection](/design/networking/#connection-classes); planned). Server-side, the two surfaces ride different transports per the [API surface map](/design/api-surfaces/#surface--transport-map): the **sync feed** is the gRPC `capsule.sync.v1.SyncService` in `capsule-api-sync` (a stub today), and **blob fetch** is a REST ranged `GET` served by `capsule-api-media`. The sync feed format is the **contract** other modules consume; its versioning and per-album monotonic ordering are what defeats the stale-rewind attack class.

## Discovering What Changed

A client never polls assets individually. It holds a single opaque **sync cursor** and asks the server for everything that changed after it:

| Surface                                       | Transport                                | Purpose                                                                                                                                         |
| --------------------------------------------- | ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `Sync(cursor, page_size)`                     | gRPC (`capsule.sync.v1.SyncService`)     | Returns a page of asset changes (created, metadata-updated, deleted) after `cursor`, with a `next_cursor`. The feed is monotonic and resumable. |
| `GET /blob/{hash}`                            | REST (HTTP `Range`)                      | Fetch a ciphertext blob by its content address; ranged for resumable and partial reads.                                                         |

Each sync entry carries the asset's signed manifest as **opaque canonical CBOR** (never re-modeled as proto fields — re-encoding would detach it from its signatures), the small encrypted **metadata blob**, and the asset's **blob manifest** — the content hashes of its original and derivative blobs — never original or derivative bytes. Discovering a thousand new assets costs a few hundred kilobytes. The client decrypts each metadata blob, learns the asset's dimensions, capture date, and LQIP, and only *then* decides what else, if anything, to fetch. A deleted or modified asset arrives as a tombstone or an updated metadata reference; the client reconciles local state against it (see [Synchronization Scope](#synchronization-scope)).

**Cursor authenticity.** The opaque sync cursor is **MAC'd by the server** (HMAC-SHA256 — a server-internal construction: the cursor is opaque to clients and never verified by them, so it sits outside the client-facing [primitives inventory](/design/cryptography/primitives/#primitives-inventory)) under a server-only key and verified on every `Sync` call (and [federation pull](/design/federation/#federation-reuses-existing-primitives)), so a client cannot forge or mutate a cursor and a cursor lifted from another context is rejected at the boundary. The MAC is the *authenticity* layer; the per-album monotonic `sync_seq` check below is the independent *anti-rewind* layer. They are separate on purpose: a malicious server can always hand back one of its own *older*, validly-MAC'd cursors, and only the client-held high-water mark defeats that. Together they close the [sync-cursor rewind class](/design/threat-model/scenarios/#damage-scenario--invariant-map).

**Sync feed validation.** Every entry in a `Sync` response carries a `protocol_version` (matching the album's pin) and a per-album monotonic `sync_seq` (a `u64`, strictly increasing per album). The client refuses to apply an entry whose `protocol_version` is above its max known (per the [tightened Postel's Law](/design/principles/#postels-law-asymmetric)) and refuses any page whose `sync_seq` regresses against what the client has already seen for that album — a regressing `sync_seq` indicates a malicious or buggy server attempting to rewind the client's view, and the client surfaces it rather than applying it.

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

**When an above-tier fetch cannot succeed.** A lazily-fetched representation may be temporarily or permanently unavailable. The client distinguishes the two: a **transient** failure (network drop, `5xx`) retries with backoff and resumes via `Range`; a **permanent** failure (`410 Gone`, a purged origin, or an unreachable [federated home server](/design/federation/#robustness-against-connectivity-loss)) **degrades gracefully** to the best representation already in hand. A **`403`** is neither: it signals an *authorization change*, not a durability loss — the client re-syncs its membership/capability state for the album before retrying, and only then degrades (the asset may have been unshared), so a revocation event is surfaced as such rather than masked as a missing file — preview → thumbnail → LQIP, down to the always-present LQIP — and surfaces a non-destructive "full resolution unavailable" state on the asset. It never thrashes the fetch, and it never removes the asset's metadata or local index entry over a missing derivative. The asset stays listed and re-fetches automatically once the representation becomes reachable again.

## Resumption and Verification

- Large originals are fetched with HTTP `Range` requests; an interrupted download resumes from the last persisted byte instead of restarting, mirroring the [upload protocol's](/design/import/upload-protocol/) resumability.
- The client verifies integrity itself. Since the server can only attest to ciphertext, the client recomputes the [ciphertext content hash](/design/cryptography/primitives/) against the requested content address, then decrypts and relies on the [STREAM construction](/design/cryptography/encryption/#stream-construction)'s authentication tags to detect truncation, reordering, or chunk deletion. Any failure discards the blob and re-fetches it.
- Before any sync-driven reconciliation drops the only local copy of a *local-origin* asset (for instance an upload the device just completed), the client first confirms durable server storage via [`/storage/verify`](/design/import/storage-verification/#verify-before-destroy) — the same verify-before-destroy gate that governs [cache eviction](/design/filesystem/client/#space-recovery). Re-fetchable server-origin blobs are unaffected: they came from the server, so discarding them is always safe.

## Prefetch and Frugality

- Prefetch is bounded and predictive — thumbnails for assets just beyond the viewport, the preview for the likely-next asset in a sequence — and is cancelled as soon as the user's focus moves.
- Prefetch and any above-tier fetch obey the same connection rules as [Auto Syncing](#auto-syncing): on a metered connection the client fetches only what the user explicitly opens, and defers the rest.
- Fetched-but-unpinned blobs are ordinary cache citizens, subject to [Space Recovery](/design/filesystem/client/#space-recovery); the client transparently re-fetches them on demand if they are evicted. Recently-viewed content is retained preferentially — so scrolling back through an already-browsed album is served from cache rather than re-fetched — while the bounded, last-access-ordered eviction policy that decides what stays is owned by [Filesystem — Client](/design/filesystem/client/#automatic-cache-management).

## Auto Syncing

On mobile clients, auto syncing keeps new assets backed up (not to be confused with [encrypted backups](/design/backup-recovery/)) to the server and pulls assets from other devices onto the device.

### Synchronization Criteria

Sync is checked conservatively. When a check fires, the client reconciles everything that needs syncing — uploads and downloads — and proceeds as long as the criteria below hold throughout the transfer. If conditions change mid-transfer (e.g. the connection becomes metered), it re-evaluates and pauses gracefully; the server never assumes a transfer runs to completion in one session (see [Upload Protocol — Idempotency and Resumption](/design/import/upload-protocol/#idempotency-and-resumption)).

The actual synchronization criteria are strict and scale with the reconciliation amount (i.e. total upload + download transfer):

- **Small reconciliation** — a handful of new assets, or metadata-only deltas: synced proactively whenever the device has any non-metered [connection class](/design/networking/#connection-classes).
- **Large reconciliation** — bulk uploads, or original-tier downloads: deferred until the device is connected to unmetered Wi-Fi. A storage-constrained [streaming import](/design/import/pipeline/#import-upload-streaming-mode) is a large reconciliation and obeys these same rules, pausing if the connection drops or becomes metered.

### Platform Limitations

Auto sync is implemented **only** if it can be guaranteed to behave appropriately under all scenarios. It is explicitly not implemented on platforms that lack the APIs we need (e.g., detecting metered connections), to avoid surprises.

### Background Execution

Mobile OSes do not let an app sync whenever it likes; the scheduler is written against the platform contracts, not around them:

- **iOS.** Change detection and small reconciliations ride `BGAppRefreshTask` (short, OS-budgeted, no guaranteed cadence); large reconciliations request `BGProcessingTask` with `requiresNetworkConnectivity` (+ `requiresExternalPower` for very large batches). The OS may grant nothing for days — that is exactly the case the [two-week staleness notification](#notifications) exists for, and why it is a product surface rather than a bug.
- **Android.** All background sync rides `WorkManager` with explicit constraints (`UNMETERED` for large reconciliations, `CONNECTED` for small; battery-not-low), surviving Doze and app restarts. A *user-initiated* force-sync of a large reconciliation may run as a user-visible foreground service with progress; background work never does.
- **Desktop.** No OS budget applies; the scheduler self-throttles (idle + on-power gating for bulk work), reusing the [maintenance gating rules](/design/filesystem/maintenance/#content-validation-expensive-scheduled).
- **Uniform rules, all platforms.** Every background window is treated as *preemptible*: work is chunked so an OS kill mid-window loses at most one chunk (uploads resume via [HEAD offsets](/design/import/upload-protocol/#idempotency-and-resumption), downloads via `Range`); the scheduler never holds a wake lock across a transfer; and a window that ends mid-reconciliation simply leaves the remainder for the next window — there is no "must finish" state, by construction. Retry/backoff inside a window follows the [bulk-transfer policy class](/design/networking/#retry-policy-classes).

### Notifications

When the auto sync criteria have not been met for a prolonged period — **two weeks** specifically — the library falls silently out of date, which defeats the purpose of a backup. The client surfaces this rather than letting it pass unnoticed:

- After two weeks without a completed sync *while changes remain un-synced* — including originals still pending under a [staged upload policy](#upload-tiering-staged-uploads) — the user is notified that the library is behind and offered a one-tap **force sync now**, which proceeds regardless of the metered/Wi-Fi criteria with their explicit consent. (A device idle long enough to trigger this may also be approaching the session's [sliding inactivity expiry](/design/authentication/#sliding-inactivity-expiry); the force-sync flow routes through re-authentication when the session has lapsed rather than failing the sync.)
- The notification can be **snoozed** until a later date (e.g. another two weeks) or **disabled** outright. Snoozing only suppresses the warning; disabling opts out of the warning entirely and does not affect auto sync itself.

## Synchronization Scope

- **Uploadable new content:** the source (original) asset is uploaded along with all associated metadata and derivatives — in the session order chosen by the [upload policy](#upload-tiering-staged-uploads) (`full` today's behavior, `staged` the low-data ladder).
- **Modified/deleted content:** associated metadata is updated.
- **Fetch new content:** depending on setting, metadata only / metadata + thumbnails / metadata + thumbnails + original is fetched for all new assets. Unless the original already exists locally (e.g., if the device was the original uploader), the original is only fetched on demand (e.g. the user explicitly views the original or shares the original with others). This is to save bandwidth and storage on client devices. Metadata includes LQIP which can be used as a preview before even thumbnails are fetched.

## Upload Tiering (Staged Uploads)

Downloads have always been tiered; uploads were all-or-nothing. **Staged uploads** add the upload-direction ladder for low-data situations — traveling with a metered plan, weeks away from Wi-Fi — where what matters most is that the *index* of what exists escapes the device: if the phone drowns, the user knows exactly what was lost and holds a preview of it. (The name avoids "backup" deliberately — that term is reserved for the [encrypted export artifact](/design/backup-recovery/).)

The per-device **upload policy** is a closed enum:

- **`full`** (default) — every session of an asset's bundle opens eagerly, in any order (today's behavior).
- **`staged`** — sessions open in tier order per asset, each tier gated by the connection rules below.

The **upload tier ladder** mirrors the download ladder and maps directly onto existing [blob roles](/design/import/upload-protocol/#what-gets-uploaded) — no new blob kind, no new wire surface:

| Tier | Blobs (by role) | Opens when |
| --- | --- | --- |
| **T0 — index** | signed manifest + metadata blob (with embedded LQIP) | any usable connection, even `constrained`/`adverse` — a few KB per asset |
| **T1 — preview** | thumbnail + preview derivative blobs | any non-metered connection (small-reconciliation rule) |
| **T2 — original** | original blob | unmetered Wi-Fi (large-reconciliation rule) or explicit force-sync |

**The policy is client-side session ordering only.** The server has zero mode branches: the same `POST /upload` sessions, the same bundle mechanics, the same finalization — under `staged` the client simply hasn't opened the T2 session yet. This is what keeps the two policies on one code path: the scheduler takes the tier ladder as an ordering input; nothing else in the pipeline knows the policy exists.

**The `awaiting-original` state.** Visibility already flips on manifest + metadata finalization ([upload protocol](/design/import/upload-protocol/#what-gets-uploaded)); whether the original has landed travels as the derived per-asset fact `original_held` on each sync feed entry. An asset with `original_held = false` is in the derived state **awaiting-original**:

- Other devices see it in the timeline immediately (LQIP, then T1 tiers as they land) with an "original still on *device*" badge.
- Fetching its original returns the transient `error.blob.pending_upload` — explicitly distinct from `410 Gone`; the client shows the badge, never a failure, and re-fetches when the feed flips `original_held`.
- Server GC and the index-rebuild rule treat a missing original on an `awaiting-original` asset as expected state, not a dangling reference ([Filesystem — Server](/design/filesystem/server/)).
- The state is always **derived** from the blob-role rows / feed field — never stored as a second source of truth.

**What staged uploads never change:**

- **Verify-before-destroy is untouched.** No release path — cache eviction of a device-owned original, Move-import source deletion, streaming release — may fire until the **original** is uploaded and [`/storage/verify`](/design/import/storage-verification/#verify-before-destroy) returns durable. A staged asset pins its local original until T2 completes, by the same gate that always governed release.
- **Staged and [streaming import](/design/import/pipeline/#import-upload-streaming-mode) are mutually exclusive per import.** Streaming exists to release local bytes quickly; staged defers exactly the upload that release depends on. The planner rejects the combination outright.
- **Quota** charges each tier's session at its own creation — the existing enforcement point, just later in time for T2. Deleting an `awaiting-original` asset cancels its pending tiers and tombstones normally.

Resume needs no new client state: the tier queue is re-derived from server truth (held roles on the feed entry + `GET /upload/sessions` for in-flight tiers); `library.sqlite`'s work queue stays a rebuildable cache.

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
- **Staged ladder order (unit).** Under `staged`, sessions open strictly T0 → T1 → T2 per asset, T2 only under the large-reconciliation criteria; under `full`, eagerly.
- **awaiting-original semantics (unit).** Visibility flips at metadata finalization with `original_held = false`; flips true inside the T2 finalization transaction; a skeleton fetch surfaces the transient pending state and never removes metadata or the index entry; `pending` is distinguishable from `410`.
- **Staged release gate (unit).** Under `staged`, every release path refuses while T2 is not durable.
- **Staged resume from server truth (smoke).** Kill the client mid-ladder; on restart the tier queue re-derives from the feed + session list and re-uploads only missing tiers.
- **Planner staged × streaming exclusion (unit).** A plan configured with both is rejected at confirmation.
- **Background window preemption (smoke).** Kill a sync window mid-transfer; assert at most one chunk of progress is lost and the next window resumes from server truth.

The cross-module case — server emits a sync entry → client applies and fetches blob — is bounded E2E surface listed in [Module Map](/design/module-map/#e2e-test-surface).
