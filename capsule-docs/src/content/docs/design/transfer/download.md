---
title: Download
description: Tiered, ranged, on-demand blob fetch — access scoping, the fallback ladder, client-side verification and prefetch
audit: none
---

The server-to-client half of the [](/design/transfer/), bound by its invariants ([](/design/transfer/#invariants)). Upload optimises for correctness under interruption; download optimises for **bandwidth and storage frugality**, fetching the smallest representation that serves the user's current intent. A client learns _what_ to fetch from the sync feed ([](/design/sync/)). Server: `capsule-server::serve` behind `GET /v1/blob/{hash}` ([](/reference/api/sync/)); client: `capsule-sdk::fetch`.

## Tiered, On-Demand Fetch

Each asset has a ladder of representations, cheapest first:

1. **LQIP** — inside the metadata blob; present the moment metadata syncs.
2. **Thumbnail** — fetched when the asset scrolls into or near view.
3. **Preview** — fetched when the asset is opened.
4. **Original** — fetched only on explicit demand (full-fidelity view, export, share).

The per-library synchronization scope ([](/design/sync/#synchronization-scope)) sets which tiers are fetched eagerly; anything above it is fetched lazily, and the original never speculatively. The client looks up its cache by content hash before any request, so a representation shared between assets is fetched once.

### Render Ladder

What a client _shows_ follows the same ladder, extended with what is already on the device. The order is computed once, in Rust, and reaches every native app through the FFI; apps render the representation it names and keep no ladder of their own. The keyless web client shows what the sync feed and its share paths serve. From least to most faithful:

1. **LQIP** from the metadata blob, or its dominant colour when the LQIP cannot be decoded ([](/design/thumbnails/#lqip)).
2. **Thumbnail**: the encoded tier, or, until it exists, an embedded preview ([](/design/thumbnails/#embedded-previews)) whose long edge reaches the Thumbnail tier's.
3. **Preview**: the encoded tier, or, on the device holding the original until it holds an encoded Preview, an embedded preview that covers the surface showing it when the view opens. A view that later grows keeps the embedded preview rather than dropping a rung.
4. **Original**, decoded.

A surface asks for the rung its intent needs (Thumbnail for the grid, Preview for the single-asset view) and at once shows the best rung held at or below it. The view then **upgrades in place** as a better rung arrives, generated on the device or fetched, and never shows a blank while it waits. It never downgrades a rung it has shown, except through the [](#fallback-ladder).

## Blob Access

`GET /v1/blob/{hash}` is **membership-scoped** (`capsule-server::serve::authority`):

- An account may fetch blobs of assets filed under it and of every album whose current signed roster names it ([](/design/authorization/)).
- A **former** member gets `403`; every other caller gets `404`, byte-identical to an unknown address. `403` is reserved for a caller the server knows once had access, because it confirms the address is referenced.
- **Backup** blobs are owner-only; everyone else gets `404`, decided on the role before the roster is consulted.
- The roster is a transport control over who is handed bytes, never a confidentiality control; removal is paired with an AMK epoch bump.
- Share-link and drop paths serve from their own capabilities and answer `404` once withdrawn; they do not use membership.

### Trash

A `delete` moves an asset to trash, restorable by a `trash-restore` until the `retention_until` its signed record fixed ([](/design/organization/#retention-window)). Until then the asset's blobs serve as the live asset's did: its provenance records (including those the delete moved past), its live metadata blob, its original and its derivatives. A device that first sees the asset at its `delete` fetches them to adopt the asset from its create and restore it.

- The window that serves the blobs is the window that admits the restore, judged on the server's clock, so no instant serves a blob of an asset whose restore it would refuse. A delete that signed no `retention_until` never closes its window, as it never makes the asset purgeable.
- Past the window the asset's blobs are `410`; after the purge drops their references they are `404`, as an address nothing references.
- The authority is decided first in every state. A stranger gets `404` and a former member `403` whether the asset is live, in trash or past its window, and a serving hold is `410` inside the window too.
- **Trash is not a confidentiality boundary.** Until the window closes, every current member of the album can fetch a trashed asset's original and history, including a member who joined after the delete, because any member's device may need them to restore it. Deleting hides an asset; it does not withdraw it from the people the album is shared with. To withdraw it from someone, remove them from the album, which bumps the album's epoch ([](/design/authorization/)). The user-facing description of delete says so.

## Awaiting-Original

An asset whose T0 tier has finalized but whose original has not ([](/design/transfer/upload/#upload-tiering-staged-uploads)) carries `original_held = false` on its feed entry. The state is **derived** from held blob roles, never stored.

- Other devices list it immediately (LQIP, then T1 as it lands) with an "original still on device" badge.
- Fetching its original returns the transient `409 error.blob.pending_upload`, distinct from `410`, **only** when the fetching account itself has an active upload session declaring those bytes. The open session is the promise, so it expires with the session. A sharee or federated peer gets `404` and waits for `original_held` to flip.
- GC and the index rebuild treat the missing original as expected state, not a dangling reference.

## Fallback Ladder

- **Transient** failure (network drop, `5xx`, content-hash mismatch): retry with backoff, resuming via `Range`.
- **`403`**: an authorization change, not data loss. Re-sync membership for the album, then degrade.
- **Permanent** failure (`410`, purged origin, unreachable federated home server, [](/design/federation/#robustness-against-connectivity-loss)): degrade to the best representation already held — preview → thumbnail → LQIP — and show a non-destructive "full resolution unavailable" state.
- **`409 error.blob.pending_upload`**: show the awaiting-original badge, never a failure; re-fetch when the feed flips.

A missing representation MUST NOT remove the asset's metadata or index entry, and MUST NOT thrash the fetch. The asset re-fetches automatically once the representation is reachable.

## Resumption and Verification

- **Every fetch streams.** A blob is fetched as a sequence of bounded `Range` windows (at most 16 MiB each, smaller on an adverse link), with offsets counted from the first byte of the blob's header ([](/design/cryptography/encryption/#content-address)), and each response body is streamed into its destination (a partial file, or the decoder for a representation that is only displayed) through a fixed I/O buffer as it arrives. No response is ever buffered whole, so a fetch's memory is the I/O buffer whatever the window or blob size, and a dropped connection costs at most the unwritten part of one window. There is no fetch path that requests an open-ended range into memory.
- An interrupted download resumes from the last persisted byte. The resume position is the partial file's length after an fsync, never a separate counter that could run ahead of it, so a download also resumes across a process restart. Before extending a partial, the client re-checks the held prefix: a client holding the key verifies it chunk by chunk against the STREAM tags and truncates at the first chunk that fails, treating that chunk as corrupt at rest, as any ranged tag failure is until the whole blob is checked ([](/design/cryptography/encryption/#reading-a-blob)). A write torn by a crash is therefore fetched again rather than carried to the end. A keyless holder relies on the final address check ([](/design/transfer/#failure-model)).
- A download is written to a temporary file and appears under its final name only by an atomic rename after its content address verifies, so a partial or unverified file is never read as the blob. A blob's address is its content and its `ETag`, so two ranges of one address can never splice two different objects.
- The client recomputes the ciphertext content hash against the requested address. A mismatch discards the bytes and is retried as transient.
- The client then checks the header and decrypts, relying on STREAM's authentication tags to detect truncation, reordering or deletion. A blob that will not open is classified into exactly one state (corrupt, unsupported, key not held, wrong key, or sealed bad at source), each with its own action ([](/design/cryptography/encryption/#reading-a-blob)); a fetch never reports one undifferentiated failure. A derivative sealed bad at source is discarded and **not** re-fetched, since the manifest commits to that address; the client regenerates it from the original ([](/design/thumbnails/)).
- **Decryption is one pass.** Where the client wants plaintext (opening an original, export, restore), it hashes the ciphertext and decrypts it in the same streaming read, writing plaintext to a temporary file that is renamed into place only after the final STREAM tag and the content address both verify. The ciphertext is not staged on disk and read a second time. A restore interrupted mid-blob resumes by re-sealing the held plaintext prefix, which reproduces the ciphertext prefix exactly and so restores the hash state, or else starts the blob again.
- A write that fails for lack of space keeps the partial and pauses the fetch's class rather than retrying it ([](/design/transfer/#failure-model)).
- Discarding the only local copy of a _local-origin_ asset is gated by [](/design/transfer/durability/#verify-before-destroy). Re-fetchable server-origin blobs are always safe to drop.

## Prefetch and Frugality

- Prefetch is bounded and predictive (thumbnails just beyond the viewport, the likely-next preview) and is cancelled when focus moves ([](#transfer-scheduler)).
- Prefetch and above-tier fetches obey the synchronization criteria ([](/design/sync/#synchronization-criteria)): on a metered connection the client fetches only what the user explicitly opens.
- Fetched-but-unpinned blobs are cache, evicted per [](/design/filesystem/client/#automatic-cache-management) and re-fetched on demand.

## Transfer Scheduler

What a user is looking at decides what moves first. One `TransferScheduler` per client process, in Rust core and shared by every app through the FFI, admits **every** blob transfer in both directions: fetches, upload sessions from every lane ([](/design/transfer/upload/#queueing-across-servers)) and delegated hand-offs. It decides order, deduplication and cancellation, and stamps each request with its class. _Whether_ a class may send at all on the current path (allow, ask or deny; metering, power, heat) is decided per request by the admission layer at Spargen's transport seam ([](/design/networking/#admission-and-priority)), so a backend swap keeps both and no scheduling or metering ever lives in the HTTP backend ([](/design/networking/#one-http-stack)).

**Priority classes**, highest first:

| Class                | What                                                            | Typical source                                                                                                |
| -------------------- | --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| `visible`            | A representation a surface is showing now                       | Grid cells and the open asset, from viewport hints                                                            |
| `near_view`          | A representation just outside the viewport, in scroll direction | Viewport hints' lookahead                                                                                     |
| `requested_original` | An original the user explicitly asked for                       | Full-fidelity view, export, share                                                                             |
| `prefetch`           | A predicted need                                                | The likely-next preview                                                                                       |
| `sync`               | Keeping devices converged                                       | Feed metadata, eager-scope fetches ([](/design/sync/#synchronization-scope)), index- and preview-tier uploads |
| `backup`             | Moving custody to the server                                    | Original uploads, backup artifacts, delegated hand-offs                                                       |

- **Strict order for admission.** When a slot frees, the highest class with waiting work takes it. To keep custody moving, the lowest class the plan permits keeps one slot whenever it has work, so `backup` is slowed, never starved.
- **One request per address.** A representation wanted by two classes is one transfer at the higher class; a lower-class request for it joins the transfer already admitted.
- **Held content is skipped.** The scheduler checks the local cache by content hash when work is enqueued and again when it is dispatched, so nothing already on disk is fetched (the eager-scope planner does the same, `capsule-sdk::fetch`).
- **Viewport hints.** Native code pushes a hint whenever a scrolling surface settles or a surface gains or loses focus: the surface, the assets visible in display order, the lookahead in scroll direction, and a hint generation. A newer hint replaces the older for its surface. Assets that leave the visible set drop to `near_view` or `prefetch`, and assets that leave the lookahead are dequeued.
- **Cancel on focus change.** When a surface loses focus, its `visible` and `near_view` work that has not started is dropped, and work in flight is cancelled at its next window boundary. Because fetches resume by `Range` from the persisted partial, a cancel costs at most one window, and a later hint for the same asset resumes it.
- **Path changes win.** A new path generation cancels what the new plan forbids, whatever its class ([](/design/networking/#path-snapshot)). A class the plan holds back stays queued in order and is not re-ranked.
- **Disk floor.** The client keeps a free-space floor on the library's volume. Below it only `visible` runs, a cache sweep runs first, and every other class waits for space ([](/design/transfer/#failure-model)).
- **Observable.** Queue depth per class, admissions, cancellations and the time from enqueue to first byte are emitted as typed progress events, so a live-metrics view can show why a thumbnail was late.

The scheduler is policy and holds no durable state: dropping it loses no work, because every queue it orders is rebuilt from the local index and server truth.

## Validation

- **Scheduler order (unit).** With every class waiting, admissions follow the class order; the lowest permitted class still gets its one slot; one address wanted by two classes is one transfer.
- **Viewport and focus (unit).** A newer hint re-ranks and dequeues; a focus loss drops unstarted work and cancels in-flight work at its window boundary; a later hint resumes from the partial.
- **Bounded memory (unit).** A multi-gigabyte fetch through a mock source never holds more than the I/O buffer.
- **Torn partial (unit).** A partial with a corrupted tail chunk is truncated at that chunk on resume with the key, and discarded at the address check without it.
- **Disk full (unit).** A sink that fails with no space keeps the partial, pauses the class and evicts only reclaimable cache.
- **Single-pass open (unit).** Plaintext lands only after the last tag and the address verify; a mismatch leaves nothing under the final name.
