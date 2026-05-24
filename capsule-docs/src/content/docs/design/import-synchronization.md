---
title: Import and Synchronization
description: How Capsule imports and synchronizes assets across devices and platforms
---

We define **import** as the process of taking assets from an external source (e.g. a camera, a directory on the filesystem) and bringing them into Capsule's management. This involves scanning the files, extracting metadata, and preparing them for upload.

We split [synchronization](#synchronization) into two parts:

- Upload: Locally stored assets are uploaded to the server and made available across devices.
- Download: Assets are downloaded from the server to local devices as needed.

Capsule additionally produces [encrypted backups](/design/backup-recovery/) — encrypted, portable exports of a library — which are covered separately.

## Import

Every import is deterministic and idempotent. But imports can be partially completed. Every import is identified by an *import ID*.

### Import Pipeline

Our import pipeline is as follows:

- Initiate import: Users initiate an import in one of the following methods:
  - Manual: User selects files or directories to import through the UI. It can either point to a flat structure or a standardized directory structure (e.g. DCIM)
  - Automated: Platforms (primarily mobile) can automatically detect new media in directories being watched and appropriately trigger imports.
- File scanning and metadata extraction: *See [Metadata](/design/metadata/)* for details on how we extract metadata and organize files.
- Import planning and confirmation:
  - Before we import any file, we parse and verify it is a format we support. We strictly reject unsupported formats to avoid any issues later on. The server independently enforces a closed-enum `content_type` allow-list at session creation (see [Threat Model — Server-Side Validation Invariants](/design/threat-model/#server-side-validation-invariants)), so a malicious or buggy client declaring an unsupported format is rejected before any bytes are uploaded. Bytes received over the wire are decoded only inside the [client's sandboxed decoder](/design/clients/#sandboxed-decoder), so a format-mismatch attack cannot reach the host process.
  - Based on the scanned files and extracted metadata, we can provide users with a summary of what will be imported (e.g. number of files, total size, any issues detected) and allow them to confirm or adjust the import.
  - If uploaded assets are detected locally, we will refuse to import them. Note even if asset exists remotely, since we defer encryption and hash of encrypted blob until upload, we will allow import but upload will involve a merge operation.
- Execute import on each new file to be imported in order specified by [Upload Prioritization](#upload-prioritization):

  - Import into detected space: We can automatically move the files that are to be imported into the appropriate space. We compute the necessary metadata for cryptography (detailed in [Cryptography](/design/cryptography/)) and prepare the files for upload. This step can be optimized by parallelizing the processing of files and prioritizing certain files based on heuristics (see [Upload Prioritization](#upload-prioritization)).
  - Generate thumbnails and previews: *See [Thumbnails](/design/thumbnails/)* for details on how we generate thumbnails and previews.
  - Upload files: We choose to upload the files based on criterias outlined in [Sync](#synchronization).

## Synchronization

Core to the synchronization mechanism is the E2E/encryption requirements (see [Cryptography](/design/cryptography/)). This means that uploading and downloading require careful management of all asset metadata to ensure asset is accessible and properly decrypted on all devices (and inaccessible to unauthorized parties).

### Upload

Every upload is idempotent but stateful. Uploads can be completed partially and are identified by an *upload ID*.

The upload path is a critical hot path. Its design is held to a higher standard of correctness and performance than the rest of the API: it must behave predictably under interrupted connections, concurrent transfers, and constrained hardware. The protocol below is deliberately *strict* — ambiguity in a resumable transfer protocol is what produces silent corruption and orphaned state.

#### Protocol & Mechanics

##### What Gets Uploaded

An asset is never uploaded as a single plaintext file. Because Capsule is end-to-end encrypted (see [Cryptography](/design/cryptography/)), the client **encrypts and signs** everything *before* transmission, and the server only ever stores opaque, content-addressed ciphertext blobs. A single imported asset produces a **bundle** of blobs:

- The **original blob** — the source asset, encrypted under the [bulk AEAD](/design/cryptography/#bulk-aead) with the [STREAM construction](/design/cryptography/#stream-construction).
- **Derivative blobs** — thumbnails, previews, and LQIP, generated client-side during import (see [Thumbnails](/design/thumbnails/)), each encrypted independently.
- The **metadata blob** — the CBOR metadata document (capture date, dimensions, EXIF-derived fields, provenance), encrypted under the [bulk AEAD](/design/cryptography/#bulk-aead) (see [Metadata](/design/metadata/)).

Each blob is its own upload with its own upload ID; the protocol does not couple them. The client is responsible for completing the full set, and the server exposes the asset to other devices only once its required members (at minimum the original and metadata blobs) are finalized. Using one uniform mechanism for every blob type keeps the protocol small, and decoupling lets small derivatives land quickly while a large original is still transferring.

The server performs no decoding, no metadata extraction, and no thumbnail generation — it cannot, since it never holds a decryption key. All such work happens client-side during [import](#import).

##### Design Invariants

The upload protocol guarantees the following, and every endpoint is designed to uphold them:

- **Content-addressed.** Every blob is identified by its [ciphertext content hash](/design/cryptography/#primitives-inventory). The plaintext hash is never transmitted to the server.
- **Idempotent.** Re-creating a session for a blob already stored is a no-op that resolves to the existing asset. Re-sending a chunk at an already-acknowledged offset is accepted and simply returns the current offset.
- **Resumable.** A session survives connection loss for the lifetime of its TTL. A client resumes by querying the authoritative offset and continuing from there — no bytes are re-sent unnecessarily.
- **Strictly bounded.** The total ciphertext size is declared at session creation and immutable thereafter. The cumulative received bytes may never exceed it, nor exceed the server's per-file limit.
- **Verified.** No upload is marked complete until the server has recomputed the ciphertext hash and confirmed it matches the declared value.
- **Recoverable.** Every session is either driven to a terminal state or garbage-collected. There are no permanently orphaned chunks or pending asset rows.

##### Upload Protocol

We use a custom resumable-upload protocol modeled on [TUS](https://tus.io/) but trimmed to our needs: no per-request capability negotiation, no metadata smuggled in headers, ciphertext-only payloads. All endpoints are authenticated with a bearer JWT. Compatibility is instead gated once, up front — see [Protocol Versioning](#protocol-versioning).

| Method   | Path               | Purpose                                                                                                                                                                                                                                                                              |
| -------- | ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `POST`   | `/upload`          | Create a session. Body declares ciphertext `size`, `hash` (the [content hash](/design/cryptography/#primitives-inventory) as a tagged object `{ algo, value }`), `content_type` (closed enum), `crypto_suite_id`, `protocol_version`, `manifest_envelope` (the unencrypted manifest fields the server validates per [Threat Model — Server-Side Validation Invariants](/design/threat-model/#server-side-validation-invariants)), optional `album_id`, optional `owner_id`, optional `intent_id` (required only during an [album upgrade](/design/versioning/#album-upgrade-ceremony)). Returns `201` with `Location: /upload/{id}` and `X-Capsule-Suggested-Chunk-Size`. Rejects with `400` / `403` / `426` per the validation invariants. |
| `HEAD`   | `/upload/{id}`     | Query progress. Returns `X-Capsule-Offset` (next expected byte), `X-Capsule-Content-Length`, and session status. This is the resumption primitive.                                                                                                                                   |
| `PATCH`  | `/upload/{id}`     | Append a chunk at `X-Capsule-Offset`, with an optional per-chunk `X-Capsule-Checksum`. Returns `204` and the new offset.                                                                                                                                                             |
| `DELETE` | `/upload/{id}`     | Cancel the session — removes chunks, the session record, and the pending asset row.                                                                                                                                                                                                  |
| `GET`    | `/upload/sessions` | List the caller's active sessions, so a client can resume across app restarts or devices.                                                                                                                                                                                            |

Creating a session writes a *pending* asset row to Postgres (`uploaded = false`) and a session record to the configured **session-state store** (see [Filesystem — Stores by Deployment Profile](/design/filesystem/#stores-by-deployment-profile): Postgres by default, Valkey in the high-concurrency profile). The pending row reserves the asset ID that derivative and metadata blobs reference.

**Chunk rules.** These are enforced strictly; a violation fails the request rather than being silently corrected:

- Every chunk except the final one MUST be a multiple of 4 KiB (4096 bytes). This keeps server-side writes block-aligned, which is what makes the reflink assembly path (below) work. A non-aligned, non-final chunk is rejected with `400`.
- Offsets are strictly sequential. A `PATCH` must arrive at exactly the current received-byte count; an out-of-order or gapped write is rejected with `409`, and the client recovers by issuing `HEAD` to learn the authoritative offset.
- **Idempotency tuple.** The server keys each accepted PATCH by `(upload_id, offset, chunk_hash)` where `chunk_hash` is the SHA-256 of the chunk bytes (carried in the `X-Capsule-Checksum` header). A duplicate PATCH with the same tuple returns the same response — a re-send after a lost ACK is a no-op. A PATCH at an already-acknowledged offset *with a different `chunk_hash`* is rejected with `409` + a corruption error: this is the structural defense against a faulty client that retries with garbage. The complete idempotency contract is owned by [Threat Model — Idempotency Invariants](/design/threat-model/#idempotency-invariants).
- Cumulative size may never exceed the declared `size` nor the server's `max_file_size`. The server checks the cumulative count **at every chunk arrival**, not only at finalization — a buggy client that streams past the declared size is cut off before more bytes are persisted. Either ceiling is rejected (`400` / `413`) and the session is moved to a failed state.
- The upload completes exactly when received bytes equal the declared size; finalization then runs automatically.

##### Protocol Versioning

The upload protocol is the most fragile contract between client and server: a client that misunderstands chunk alignment, offset semantics, or finalization can silently corrupt or orphan data. The upload session is therefore gated by Capsule's universal protocol handshake, defined in [Threat Model — Protocol and Capability Negotiation](/design/threat-model/#protocol-and-capability-negotiation), so a client never begins a transfer against a server it is not known to be compatible with. This section names the upload-specific specializations.

Versioning is **date-based** (`YYYY-MM-DD` — the day a protocol revision is frozen), not integer or semver. An integer version conveys nothing about ordering granularity and invites a bump for every change; semver implies a minor/patch backward-compatibility contract finer than we are willing to maintain on a hot path. A date is unambiguously ordered, human-readable, and maps directly onto a release.

- Every client sends `X-Capsule-Protocol: <date>` on every request (the upload-specific alias `X-Capsule-Upload-Protocol` remains accepted but is deprecated). The server advertises the inclusive range it accepts via `X-Capsule-Protocol-Min` and `-Max` on every response, errors included.
- A `POST /upload` whose version falls outside the accepted range is rejected with `426 Upgrade Required` *before* any session or pending asset row is created. The response names the supported range so the client can show an actionable message ("update Capsule to keep uploading"). Per [Threat Model](/design/threat-model/#protocol-and-capability-negotiation), the same rule applies to every other write surface.
- This is a one-shot **compatibility gate**, not negotiation: there is no back-and-forth to settle on a shared version, and the protocol carries no capability flags. A client either speaks a version the server accepts, or it does not upload.
- The server supports a *window* of past protocol versions, not only the newest, so a staggered client rollout keeps working. A version leaves the window only after the deprecation period defined in [Threat Model — Min-Supported-Client Deprecation Policy](/design/threat-model/#min-supported-client-deprecation-policy); dropping one is a breaking change announced ahead of time.
- The date is bumped only for an **incompatible** wire change — offset semantics, alignment rules, finalization, the state machine. Purely additive, safely-ignorable changes do not bump it, and server-tunable parameters such as suggested chunk sizes and adaptive-sizing tiers are not protocol surface at all.

##### Session Lifecycle

A session moves through a strict state machine:

```plaintext
Pending ─▶ Uploading ─▶ WaitingForProcessing ─▶ Completed
                                            └─▶ FailedProcessing
```

- **Pending** — session created, no bytes received.
- **Uploading** — at least one chunk received, transfer in progress.
- **WaitingForProcessing** — all declared bytes received; finalization (assembly + hash verification) is running.
- **Completed** — hash verified, asset marked uploaded, now visible to other devices. Terminal.
- **FailedProcessing** — terminal failure (hash mismatch, assembly error). Chunks and the pending asset row are removed. Terminal.

Session records live in the [session-state store](/design/filesystem/#stores-by-deployment-profile) with a 24-hour TTL and a per-owner index for listing. This split is intentional: the session store holds only volatile transfer state, so the hot path — offset increments and status transitions — never touches the durable Postgres asset row. (In the default Postgres-only profile, sessions live in an `upload_sessions` table with an `expires_at` column and a periodic sweep; in the high-concurrency profile, they live in Valkey under keys `upload:session:{id}` with atomic `HINCRBY`/`HSET` and native TTL.) Postgres's durable asset record is written exactly twice per upload regardless of profile: once at session creation (the pending row) and once at finalization (mark uploaded). A session that reaches its TTL before completing is garbage-collected — chunks deleted, pending asset row removed — and the client treats an expired session as gone and re-imports. (Client should imply retries if this happens but halt after too many retries.)

#### Reliability & Integrity

##### Server-Side Storage and Assembly

Each chunk is written to disk as `{upload_id}_{n}.part`; the assembled blob is `{upload_id}.bin`. Because this is a hot path, the storage layer is aggressively optimized:

- **Streaming writes.** Chunk bytes are streamed from the request body straight to disk; large transfers must never accumulate in hot memory. On Linux, the write path uses `io_uring`.
- **Reflink assembly.** Finalization concatenates chunks into the final blob using `FICLONERANGE` (copy-on-write reflink) on CoW filesystems such as Btrfs and XFS. The 4 KiB chunk alignment is precisely what allows each chunk to be reflinked at its destination offset; only the final (possibly unaligned) chunk needs a plain copy. Reflink turns assembly into a near-instant metadata operation instead of an O(file size) copy. On filesystems without reflink support, the code falls back to a sequential copy.
- **Offloaded blocking work.** Chunk assembly and hashing run on a blocking thread pool, never on the async reactor.
- **Backpressure.** `max_cache_size` bounds the total in-flight upload bytes held on disk; `max_file_size` bounds any single blob. The configuration asserts `max_file_size < max_cache_size` and warns if fewer than ~10 concurrent maximum-size uploads would fit. The distinct task pools — network I/O, file I/O, and hashing — are sized and load-tested independently against realistic hardware limits.

##### Finalization and Integrity

When received bytes reach the declared size, the server finalizes:

1. Session transitions to **WaitingForProcessing**.
2. Chunks are assembled into the final blob.
3. The server recomputes the [content hash](/design/cryptography/#primitives-inventory) over the assembled ciphertext on the blocking pool and compares it to the declared `hash`.
4. **On match** — the pending asset is marked uploaded inside a Postgres transaction and the session transitions to **Completed**.
5. **On mismatch** — the blob and the pending asset row are deleted, the session transitions to **FailedProcessing**, and a checksum-mismatch error is returned. A mismatch is always treated as corruption or tampering and is never silently retried server-side.

The server verifies only the *ciphertext* hash — it has no other option. The client independently verifies the *plaintext* on download via the [STREAM construction](/design/cryptography/#stream-construction)'s per-chunk authentication tags, which detect truncation, reordering, and chunk deletion. The two checks are complementary: the server guarantees "the bytes I stored are the bytes you declared," and the AEAD guarantees "the plaintext I decrypted is authentic."

##### Robustness

- An upload is not expected to run to completion in a single connection. The server tolerates arbitrarily long pauses within the session TTL, and clients resume via `HEAD`. [Auto syncing](#auto-syncing) explicitly assumes interrupted transfers are normal.
- A chunk re-sent at an already-acknowledged offset is idempotent. A chunk at a stale offset receives `409` together with the authoritative offset so the client can re-align.
- Concurrent finalization attempts on a single session are guarded — a second attempt observes a non-`Pending`/`Uploading` status and returns a conflict rather than double-processing.
- Every critical step — session creation, each chunk, assembly, hash verification, finalization — is logged with the upload ID so an interrupted or failed upload can be reconstructed and recovered after the fact.

#### Performance

##### Adaptive Chunk Sizing

The server suggests an initial chunk size by file-size tier — `< 10 MB` → 256 KiB, `< 100 MB` → 1 MiB, `≥ 100 MB` → 4 MiB. The client may then adapt *within a tier-bounded range* based on throughput measured over a sliding 30-second window: doubling the chunk size when sustained throughput is high (`> 5 MB/s`), halving it when low (`< 1 MB/s`), and always staying 4 KiB-aligned. The rationale is a direct trade-off — chunks that are too small waste round-trips, while chunks that are too large waste re-transmission on a flaky link and pin more memory per in-flight request.

Adaptation is purely a client concern; the server only enforces alignment and bounds. The client must never let adaptation regress effective throughput — if a tier's range is mis-tuned, the conservative choice is the tier minimum.

We deliberately do **not** expose per-blob upload *ordering* as a protocol concern. Concurrent sessions plus the OS and TCP stack settle ordering naturally; see [Upload Prioritization](#upload-prioritization) for the client-side heuristics that decide which assets to *start*.

##### Upload Prioritization

We have a specific ordering which we pick how to upload many files simultaneously.

- **File Size:** Smaller files might be processed first to give a quicker sense of progress, or larger files might be prioritized if they are deemed more critical.
  - While file size is a useful heuristic, for internal ordering, we should let the order files are uploaded be naturally determined by simultaneous uploads and the network conditions, which fall to the underlying file transfer protocol — the custom resumable-upload protocol described above, running as concurrent sessions over the OS and TCP stack (see [Adaptive Chunk Sizing](#adaptive-chunk-sizing)).
- **Last Modified Times:** Newer or recently modified files might be more relevant to the user. (Note this filesystem metadata may not be always reliable so some fallbacks may be needed. Last accessed time was also considered but relatime makes this heuristic relatively noisy.)
- **Directory Depth:** Files closer to the root of the specified paths might be processed first.

Note that file **type/extension** is deliberately *not* a prioritization criterion — prioritizing purely by file type may result in anomalies. Instead we have exceptions for certain sidecar files (e.g. `.xmp` associated with an image, or `.wav` associated with a video file).

#### Access Control

##### Deduplication and Merge

Because blobs are addressed by their [ciphertext content hash](/design/cryptography/#primitives-inventory), the protocol can avoid redundant transfers:

- At session creation, the server checks for an asset with the same content hash already owned by the user. An exact duplicate that exists both locally and remotely is rejected up front — nothing is re-uploaded. The dedup check and the pending-row insert run inside a single PostgreSQL transaction (a `SELECT ... FOR UPDATE` followed by `INSERT ... ON CONFLICT`), so two concurrent uploaders cannot both observe "no existing row" and each insert their own — the TOCTOU race is closed at the database layer.
- [Import](#import) treats already-uploaded *local* assets as non-importable. But because encryption and hashing are deferred until upload, an asset may already exist remotely under a *different* ciphertext (for example, re-encrypted under a newer album key). Import still admits such an asset, and the upload then resolves to a **merge**: the server links the existing stored blob to the new asset and album reference rather than storing a second copy. The original blob's upload short-circuits, and only the new metadata blob is transferred.
- **Merge is strictly additive on the server.** A merge **never** deletes an existing blob or rewrites an existing manifest — it only adds a new reference. The blob's reference count goes up, never down, on merge. Reference removal happens only through an explicit `delete` lifecycle action signed by a current writer (see [Authorization](/design/authorization/)), and the underlying blob is hard-purged only after every reference is provably gone.

These checks deduplicate at upload time. Byte-identical assets that still slip into a client library — for example through overlapping folder imports or a restore over an existing library — are collapsed separately by client-side [intra-library deduplication](/design/filesystem/#deduplication).

##### Quota and Permissions

- An upload is attributed to `upload_user_id` (the authenticated uploader) for storage-quota accounting, which is distinct from `owner_id` (the asset's owner). Uploading on behalf of a different owner requires a verified relationship and is permission-checked at session creation.
- Adding an asset to an album requires write-tier album access (`AMK_write`; see [Cryptography](/design/cryptography/)); the server validates album write permission before creating the session.
- Only the uploader may append chunks. The uploader or the owner may query (`HEAD`) or cancel (`DELETE`) a session.

### Download

Download is the inverse of upload, and rests on the same two foundations: blobs are **content-addressed by ciphertext hash**, and the server never holds a key, so it serves only opaque ciphertext. Where the upload path optimises for correctness under interruption, the download path optimises for **bandwidth and storage frugality** — a client fetches the smallest representation that satisfies the user's current intent, and nothing more.

#### Discovering What Changed

A client never polls assets individually. It holds a single opaque **sync cursor** and asks the server for everything that changed after it:

| Method | Path           | Purpose                                                                                                                                         |
| ------ | -------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET`  | `/sync`        | Returns a page of asset changes (created, metadata-updated, deleted) after `cursor`, with a `next_cursor`. The feed is monotonic and resumable. |
| `GET`  | `/blob/{hash}` | Fetch a ciphertext blob by its content address. Supports HTTP `Range` for resumable and partial reads.                                          |

The `/sync` feed carries only the small encrypted **metadata blobs** and each asset's **blob manifest** — the content hashes of its original and derivative blobs — never original or derivative bytes. Discovering a thousand new assets costs a few hundred kilobytes. The client decrypts each metadata blob, learns the asset's dimensions, capture date, and LQIP, and only *then* decides what else, if anything, to fetch. A deleted or modified asset arrives as a tombstone or an updated metadata reference; the client reconciles local state against it (see [Synchronization Scope](#synchronization-scope)).

**Sync feed validation.** Every entry in a `/sync` response carries a `protocol_version` (matching the album's pin) and a per-album monotonic `sync_seq`. The client refuses to apply an entry whose `protocol_version` is above its max known (per the [tightened Postel's Law](/design/principles/)) and refuses any page whose `sync_seq` regresses against what the client has already seen for that album — a regressing `sync_seq` indicates a malicious or buggy server attempting to rewind the client's view, and the client surfaces it rather than applying it.

#### Stale-Revival Detection

A malicious or buggy server, peer, or backup could submit an old-but-validly-signed manifest to resurrect an asset that the receiving device has tombstoned at a later state. The defense — owned by [Cryptography — Provenance of Library Modifications](/design/cryptography/#provenance-of-library-modifications) — is the per-asset `prior_provenance_hash` chain. Two layers enforce it:

- **Client.** Every device's local index stores a `latest_provenance_hash` per `asset_id`. When a sync entry, federation pull, peering artifact, or backup restore proposes a manifest whose `prior_provenance_hash` is **behind** that local value, the entry is **quarantined** (see [Threat Model — Quarantine Surfaces](/design/threat-model/#quarantine-surfaces)) and surfaced as "peer sent stale state."
- **Server (no-key).** The server stores the same `latest_provenance_hash` per asset in PostgreSQL and rejects any incoming non-`create` manifest whose `prior_provenance_hash` does not match. This is described in the [server-side validation invariants](/design/threat-model/#server-side-validation-invariants).

A deleted asset cannot be silently resurrected, on either side, without the resurrection appearing as a quarantine surface to the user.

#### Tiered, On-Demand Fetch

Each asset has a ladder of representations, cheapest first:

1. **LQIP** — embedded in the metadata blob (see [Thumbnails](/design/thumbnails/)); available the instant metadata syncs, at zero extra request.
2. **Thumbnail** — fetched when the asset scrolls into, or near, view in a grid.
3. **Preview** — a screen-resolution derivative, fetched when the asset is opened.
4. **Original** — fetched only on explicit demand: viewing at full fidelity, exporting, or sharing the original.

The default policy follows the per-library setting in [Synchronization Scope](#synchronization-scope) — *metadata only*, *metadata + thumbnails*, or *metadata + thumbnails + original*. Anything above the configured tier is fetched lazily, on demand. The original is never fetched speculatively unless the device was its uploader, in which case it already holds the plaintext locally and downloads nothing.

Because every blob is content-addressed, a fetch is skipped entirely when the blob is already in the local cache — the client looks up its cache by hash before issuing any request, so a representation shared between assets (an identical thumbnail, a merged original) is only ever fetched once.

#### Resumption and Verification

- Large originals are fetched with HTTP `Range` requests; an interrupted download resumes from the last persisted byte instead of restarting, mirroring the upload protocol's resumability.
- The client verifies integrity itself. Since the server can only attest to ciphertext, the client recomputes the [ciphertext content hash](/design/cryptography/#primitives-inventory) against the requested content address, then decrypts and relies on the [STREAM construction](/design/cryptography/#stream-construction)'s authentication tags to detect truncation, reordering, or chunk deletion. Any failure discards the blob and re-fetches it.

#### Prefetch and Frugality

- Prefetch is bounded and predictive — thumbnails for assets just beyond the viewport, the preview for the likely-next asset in a sequence — and is cancelled as soon as the user's focus moves.
- Prefetch and any above-tier fetch obey the same connection rules as [Auto Syncing](#auto-syncing): on a metered connection the client fetches only what the user explicitly opens, and defers the rest.
- Fetched-but-unpinned blobs are ordinary cache citizens, subject to [Space Recovery](/design/filesystem/#space-recovery); the client transparently re-fetches them on demand if they are evicted.

### Auto Syncing

On mobile clients, we support auto syncing which can be very useful for ensuring new assets are backed up (not to be confused with [encrypted backups](/design/backup-recovery/)) to the server and assets from other device loaded onto device.

#### Synchronization Criteria

We are conservative in when we check whether synchronization is needed. To bypass the possibility of outdated reconciliations, we reconcile the assets that required syncing (both uploading and downloading), and immediately execute backup as long as criterias remain throughout the data transfer process. If conditions change (e.g. internet connection became metered), it will be re-evaluated and potentially paused gracefully. Upload server does not expect the client to always complete transfers to completion (e.g., due to network conditions).

Finally, the actual synchronization criteria are strict and scale with the reconciliation amount (i.e. total upload + download transfer):

- **Small reconciliation** — a handful of new assets, or metadata-only deltas: synced proactively whenever the device has any non-metered connection.
- **Large reconciliation** — bulk uploads, or original-tier downloads: deferred until the device is connected to unmetered Wi-Fi.

#### Platform Limitations

We strictly implement auto sync ONLY if we can guarantee it will behave appropriately under all scenarios. We explicitly do not implement it on platforms that do not give all the APIs we need (e.g., detecting metered connection) to avoid surprises.

#### Notifications

When the auto sync criteria have not been met for a prolonged period — **two weeks** specifically — the library falls silently out of date, which defeats the purpose of a backup. The client surfaces this rather than letting it pass unnoticed:

- After two weeks without a completed sync, the user is notified that the library is behind and offered a one-tap **force sync now**, which proceeds regardless of the metered/Wi-Fi criteria with their explicit consent.
- The notification can be **snoozed** until a later date (e.g. another two weeks) or **disabled** outright. Snoozing only suppresses the warning; disabling opts out of the warning entirely and does not affect auto sync itself.

### Synchronization Scope

- Uploadable new content: We upload the source (i.e. original) asset as well as all associated metadata and derivatives.
- Modified/deleted content: We update the associated metadata.
- Fetch new content: Depending on setting, it either fetches *metadata only*, *metadata + thumbnails*, or *metadata + thumbnails + original* for all new assets. Unless original already exists locally (e.g., if device was the original uploader), the original is only fetched on demand (e.g. user explicitly tries to view original or share original with others). This is to save bandwidth and storage on client devices. Note that metadata includes LQIP which can be used as a preview before even thumbnails are fetched.
