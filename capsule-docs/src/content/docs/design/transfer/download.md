---
title: Download
description: Tiered, ranged, on-demand blob fetch — access scoping, the fallback ladder, client-side verification and prefetch
status: draft
---

The server-to-client half of the [transfer protocol](/design/transfer/), bound by its [invariants](/design/transfer/#invariants). Where upload optimises for correctness under interruption, download optimises for **bandwidth and storage frugality**: a client fetches the smallest representation that serves the user's current intent. A client learns *what* to fetch from the [sync feed](/design/sync/). Server: `capsule-server::serve` behind `GET /v1/blob/{hash}`; client: `capsule-sdk::fetch`.

## Tiered, On-Demand Fetch

Each asset has a ladder of representations, cheapest first:

1. **LQIP** — inside the metadata blob; present the moment metadata syncs.
2. **Thumbnail** — fetched when the asset scrolls into or near view.
3. **Preview** — fetched when the asset is opened.
4. **Original** — fetched only on explicit demand (full-fidelity view, export, share).

The per-library [synchronization scope](/design/sync/#synchronization-scope) sets which tiers are fetched eagerly; anything above it is fetched lazily. The original is never fetched speculatively. The client looks up its cache by content hash before any request, so a representation shared between assets is fetched once.

### Render Ladder

What a client *shows* follows the same ladder, extended with what is already on the device. The order is computed once, in Rust, and reaches every app through the FFI; apps render the representation it names and keep no ladder of their own. From least to most faithful:

1. **LQIP** from the metadata blob, or its dominant colour when the LQIP cannot be decoded ([LQIP](/design/thumbnails/#lqip)).
2. **Thumbnail**: the encoded tier, or, until it exists, an [embedded preview](/design/thumbnails/#embedded-previews) whose long edge reaches the Thumbnail tier's.
3. **Preview**: the encoded tier, or, on the device that holds the original and until an encoded Preview exists, an embedded preview that covers the surface showing it when the view opens. A view that later grows keeps the embedded preview rather than dropping a rung.
4. **Original**, decoded.

A surface asks for the rung its intent needs (the grid asks for Thumbnail, the single-asset view for Preview) and is shown the best rung held at or below it at once. The view then **upgrades in place** as a better rung arrives, whether it is generated on the device or fetched from the server, and never shows a blank while it waits. It never downgrades a rung it has already shown, except through the [fallback ladder](#fallback-ladder).

## Blob Access

`GET /v1/blob/{hash}` is **membership-scoped** (`capsule-server::serve::authority`):

- An account may fetch blobs of assets filed under it and of every album whose current [signed roster](/design/authorization/) names it.
- A **former** member gets `403`; every other caller gets `404`, byte-identical to an unknown address. `403` is reserved for a caller the server knows once had access, because it confirms the address is referenced.
- **Backup** blobs are owner-only; everyone else gets `404`, decided on the role before the roster is consulted.
- The roster is a transport control over who is handed bytes, never a confidentiality control; removal is paired with an AMK epoch bump.
- Share-link and drop paths serve from their own capabilities and answer `404` once withdrawn; they do not use membership.

## Awaiting-Original

An asset whose T0 tier has finalized but whose original has not (see [staged uploads](/design/transfer/upload/#upload-tiering-staged-uploads)) carries `original_held = false` on its feed entry. The state is **derived** from held blob roles, never stored.

- Other devices list it immediately (LQIP, then T1 as it lands) with an "original still on device" badge.
- Fetching its original returns the transient `409 error.blob.pending_upload`, distinct from `410`, **only** when the fetching account itself has an active upload session declaring those bytes. The open session is the promise, so the promise expires with the session. A sharee or federated peer gets `404` and waits for `original_held` to flip.
- GC and the index rebuild treat the missing original as expected state, not a dangling reference.

## Fallback Ladder

- **Transient** failure (network drop, `5xx`, content-hash mismatch): retry with backoff, resuming via `Range`.
- **`403`**: an authorization change, not data loss. Re-sync membership for the album, then degrade.
- **Permanent** failure (`410`, purged origin, unreachable [federated home server](/design/federation/#robustness-against-connectivity-loss)): degrade to the best representation already held — preview → thumbnail → LQIP — and show a non-destructive "full resolution unavailable" state.
- **`409 error.blob.pending_upload`**: show the awaiting-original badge, never a failure; re-fetch when the feed flips.

A missing representation MUST NOT remove the asset's metadata or index entry, and MUST NOT thrash the fetch. The asset re-fetches automatically once the representation is reachable.

## Resumption and Verification

- Large blobs are fetched with HTTP `Range`; an interrupted download resumes from the last persisted byte.
- The client recomputes the ciphertext content hash against the requested address. A mismatch discards the bytes and is retried as transient.
- The client then decrypts, relying on STREAM's authentication tags to detect truncation, reordering or deletion. A derivative whose bytes match their address but fail authentication is discarded and **not** re-fetched (the manifest commits to that address); the client regenerates it from the original ([Thumbnails](/design/thumbnails/)).
- Discarding the only local copy of a *local-origin* asset is gated by [Verify Before Destroy](/design/transfer/durability/#verify-before-destroy). Re-fetchable server-origin blobs are always safe to drop.

## Prefetch and Frugality

- Prefetch is bounded and predictive (thumbnails just beyond the viewport, the likely-next preview) and is cancelled when focus moves.
- Prefetch and above-tier fetches obey the [synchronization criteria](/design/sync/#synchronization-criteria): on a metered connection the client fetches only what the user explicitly opens.
- Fetched-but-unpinned blobs are cache, evicted per [Filesystem — Client](/design/filesystem/client/#automatic-cache-management) and re-fetched on demand.
