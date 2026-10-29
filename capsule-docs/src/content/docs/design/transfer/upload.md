---
title: Upload
description: Resumable, content-addressed upload sessions — chunk rules, session states, finalization, staged tiers and the error codes a client branches on
status: draft
---

The client-to-server half of the [transfer protocol](/design/transfer/), bound by its [invariants](/design/transfer/#invariants). It borrows the offset/`PATCH` shape of TUS v1 without claiming compatibility. Server: `capsule-server::upload`; client: `capsule-sdk::upload`. Request and response shapes, headers and constants are the [OpenAPI contract](/reference/api/) and `capsule-server::limits`; this page fixes their semantics.

## What Gets Uploaded

The client encrypts and signs everything before transmission; the server performs no decoding, extraction or thumbnailing. One asset is a **bundle** of blobs, each its own session with its own upload id:

- **original** — the source, under the [bulk AEAD](/design/cryptography/primitives/#bulk-aead) with the [STREAM construction](/design/cryptography/encryption/#stream-construction);
- **derivative** — thumbnails and previews ([Thumbnails](/design/thumbnails/)), each encrypted independently;
- **metadata** — the encrypted CBOR [metadata](/design/metadata/) document, including the [LQIP](/design/thumbnails/#lqip);
- **provenance** — the asset's [signed manifest](/design/cryptography/provenance/#asset-manifest), the one non-ciphertext member: server-visible signed CBOR, stored and served byte-for-byte;
- **derivative_manifest** — the signed [`DerivativeManifest`](/design/cryptography/provenance/#derivative-provenance) for one derivative tier;
- **backup** — a [backup artifact](/design/backup-recovery/), covered by the library-level backup manifest instead of per-asset provenance.

Every session declares its `blob_role` (a closed enum) at creation, so the server can reason about bundle completeness without a key. "Blob" is defined in [Filesystem — Server](/design/filesystem/server/#uniform-opaque-blobs).

- **No wire ordering for a `create`.** Members land in any order into an invisible pending asset. The asset becomes **visible** to other devices once its **provenance and metadata blobs** are finalized (`capsule-server::upload::visibility`). Whether the original has landed travels separately on the sync feed as `original_held`; see [awaiting-original](/design/transfer/download/#awaiting-original).
- **Two manifest paths, never confused.** The `manifest_envelope` on `POST /v1/upload` is a key-free **projection** the server validates ([server-side validation invariants](/design/threat-model/validation/#server-side-validation-invariants)). The signed bytes are the provenance blob. The server MUST NOT serve the projection, or anything re-encoded from it, where a manifest is expected.
- **Derivative format.** A `derivative` session's envelope MUST declare `derivative_role` and `derivative_format`; they are refused on every other role (`error.upload.envelope_mismatch`). A `thumbnail`/`preview` format outside the closed still set is `error.upload.unknown_derivative_format`.
- **Quota.** Every member counts toward the uploader's [quota](/design/quota/#accounting-model).

### Replace

Only `create` and `replace` move blob bytes; every other action is a [lifecycle write](/design/authorization/#the-lifecycle-write-surface). A replace mutates an already-visible asset, so:

- It is applied as one act **when its manifest lands**; the other members commit bytes but change nothing observable. The manifest therefore lands **last** — the protocol's only ordering rule.
- The manifest names the bundle it commits to via `original_blob_hash` (required on a replace). An early manifest is refused `409 error.upload.replace_incomplete` and retried.
- Stale-chain and epoch checks (invariants 17 and 18) run inside the index's critical section that re-points roles and mints the sequence number. A stale chain is `409 error.upload.stale_revival`.
- An abandoned replace's committed-but-unreferenced blobs are collected by GC and the quota credited back.

### Stale-Epoch Create

A `create` is sealed under the album epoch current on the device that wrote it. When the album's epoch has moved past it by the time its manifest lands (it was imported offline before a rotation, or a sibling sealed under the newer epoch landed first), invariant 18 refuses it at finalization with `error.upload.envelope_rejected`, and the refusal is final for that envelope. The check runs only on a row with no chain head and writes nothing, so a refused create was never published.

The client recovers by **re-sealing** the create at the album's current epoch and pushing it again, once:

- Only a lone, unpublished, key-derived `create` is re-sealed. The refusal must be of its provenance blob, because a refusal of a later member can follow a provenance blob that landed. Its chain must hold no later record, and the library must hold no custody receipt for it.
- An epoch rotation is a confidentiality boundary, so everything sealed under the old epoch is sealed again under the new one: the original under a fresh `nonce_prefix`, the unchanged signed sidecar under a fresh metadata-blob nonce, and its derivatives regenerated as fresh chains. No prefix or nonce the asset has spent is drawn again, and no blob sealed under the old epoch stays reachable from the library.
- The new genesis keeps the `file_id`, self-verifies before anything is written, and replaces the refused one. A second refusal fails the push.

**Known gap: a device without the new key.** A device learns of a newer epoch only from its own album keystore. When another device rotated the album and the new album key has not reached this one, there is no epoch to re-seal at. The push fails as the upload refusal it is, with the server's code and a detail saying there is no newer epoch to re-seal at, and the asset stays local, its original untouched. The detail names no missing key: the server refuses a revoked device, withdrawn write access and a role conflict the same way, and a device holding no newer epoch cannot tell those apart from a rotation it has not been handed.

## Endpoints

All endpoints require a bearer token and the [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation).

| Endpoint | Semantics |
| --- | --- |
| `POST /v1/upload` | Create a session for a declared ciphertext `size` and `hash`. Reserves the pending asset; returns `201` with `Location` and a suggested chunk size, or `200` with the current offset for an active duplicate. |
| `HEAD /v1/upload/{id}` | Authoritative offset, declared length and status. The resumption primitive. |
| `PATCH /v1/upload/{id}` | Append one chunk at `X-Capsule-Offset`; `204` with the new offset. |
| `DELETE /v1/upload/{id}` | Cancel; removes the staged file, session and pending reservation. `409` while finalizing. |
| `GET /v1/upload/sessions` | The **uploader's** live sessions, for resume. An unknown `?status=` is `400 error.upload.invalid_status_filter`, never ignored. |
| `GET /v1/upload/{id}/receipt` | The session's [custody receipt](/design/transfer/durability/#custody-receipts) once `Completed`. |

## Chunk Rules and Strictness

Strictness covers the transport — JSON bodies (unknown fields rejected) and headers. Inside signed CBOR, unknown keys are preserved ([Postel's law, asymmetric](/design/principles/#postels-law-asymmetric)).

- A `PATCH` body MUST be `application/octet-stream` and non-empty.
- Every chunk except the final one MUST be a multiple of 4 KiB; a chunk is at most 16 MiB. These bounds are protocol surface.
- A chunk MUST arrive at exactly the current received-byte count.
- Every `PATCH` MUST carry `X-Capsule-Checksum`, the SHA-256 of the chunk. The server verifies it **before writing**.
- The server keys accepted chunks by `(upload_id, offset, chunk_hash)`: the same tuple replays the original response; the same offset with a different hash is `409 error.upload.chunk_conflict`. The full idempotency contract is [Threat Model — Idempotency Invariants](/design/threat-model/validation/#idempotency-invariants).
- Cumulative bytes are checked on **every** chunk against the declared `size` and the server's maximum file size; overrun fails the session.
- The session finalizes automatically when received bytes equal the declared size.

## Hostile Clients

The server is friendly to a correct client and assumes any client may be broken, outdated or
malicious. No request, sequence of requests or connection behaviour may corrupt stored data,
wedge another request's session, exhaust shared resources, or reveal whether a session it does
not own exists.

- **A session never wedges.** Appends to one session are serialised. A failed or partial append
  (I/O error, full disk, dropped connection mid-body) is truncated back to the session offset
  before the error is returned, so the client's retry at the same offset succeeds.
- **Disk full is the client's to wait out, not a server fault.** `ENOSPC` is
  `507 error.upload.storage_full`; a blob store or index outage is `503 error.upload.unavailable`
  with `Retry-After`. `500 error.upload.storage_inconsistent` is reserved for a state the server
  cannot explain, and is logged at `ERROR`.
- **Bounded per client.** Session creation and chunk appends are rate-limited per account and
  per source address (`429` with `Retry-After`); live sessions per account are capped. The
  source address is the client the deployment's trusted proxies resolve, never a forwarding
  header taken on its word. The account's budget is charged first, so a request it refuses
  spends nothing of the address's budget, which the other accounts behind that address share.
- **Bounded per connection.** Header, body-read and idle timeouts close a connection that sends
  too slowly; a request body is never buffered beyond the chunk maximum.
- **Requests never panic.** Every reachable failure is a typed rejection. A panic in a handler
  is a bug, caught at the router and answered `500` without leaking state.
- **Foreign is unknown.** A session belonging to another account is the same `404` as one that
  never existed; only the uploader may append to a session.
- **Every rejection is logged** with the upload id, account, offset and code, so a misbehaving
  client is diagnosable from the server alone.

### Protocol Versioning

The upload surface is gated once by the [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation): a date-versioned `X-Capsule-Protocol`, with the accepted window advertised on every response. There is no capability negotiation. A version outside the window is `426` before anything is reserved. The date is bumped only for an incompatible change (offset semantics, alignment, lowering the 16 MiB maximum or raising the 4 KiB minimum, finalization, the state machine). Suggested chunk sizes are tunable, not protocol.

## Session State Machine

```plaintext
Pending ─▶ Uploading ─▶ WaitingForProcessing ─▶ Completed
                                            └─▶ FailedProcessing
```

| From | To | On |
| --- | --- | --- |
| `Pending` | `Uploading` | first **accepted** chunk |
| `Uploading` | `WaitingForProcessing` | received bytes = declared `size` (finalize claim) |
| `Pending`/`Uploading` | *(discarded)* | lifetime cap or pressure discard |
| `WaitingForProcessing` | `Completed` | finalization succeeded |
| `WaitingForProcessing` | `FailedProcessing` | hash, size or envelope failure |

`WaitingForProcessing` is neither evictable nor cancellable, and always reaches a terminal state ([Finalization and Integrity](#finalization-and-integrity)). `FailedProcessing` removes the staged bytes and reservation and keeps the status record so a client whose ACK was lost can read the outcome. Session state lives in the session store (Valkey via `redis-rs`, or in-memory), never in PostgreSQL; see [Filesystem — Server](/design/filesystem/server/#required-services).

## Session Lifetime and Discard

- **Survival floor.** A `Pending`/`Uploading` session survives at least 1 hour after its last accepted chunk (or creation).
- **Cap.** No session outlives 24 hours from creation.
- **Discard window.** Between floor and cap the server MAY discard sessions under pressure, least-recently-progressed first.
- **Uniform `404`.** Discarded, expired and never-existent sessions are indistinguishable: `404 error.upload.session_not_found` ([rejection mapping](/design/api-surfaces/#rejection-mapping)). The client re-creates the session and restarts that blob, with bounded backoff.
- **Terminal records** are exempt from pressure discard and live to the cap.
- **Startup scrub.** On boot a staged file with no session is deleted; a session whose file is shorter than its recorded offset fails. The file is the truth; the counter is its cache.

## Append-Only Storage

Each session owns one staged file, appended in order; there is no per-chunk file and no assembly step. Before each append the server checks the file length against the session offset: bytes beyond the offset were never acknowledged and are truncated; a file shorter than the offset fails the session. Each append is fsynced before `204`. Hashing runs off the async reactor. Total in-flight bytes are bounded by the cache limit, with the discard window as the relief valve.

## Finalization and Integrity

Order is the contract (`capsule-server::upload::finalize`):

1. **Claim** — compare-and-set into `WaitingForProcessing`. A loser is not an error: its chunk was accepted and it answers `204`; the outcome is read from `HEAD` or the session list.
2. **Re-hash** the staged file and compare to the declared hash. Mismatch → `FailedProcessing`, `error.upload.content_hash_mismatch`.
3. **Envelope checks** re-run against the current clock and authority, so a device revoked or an album closed since creation does not slip through (`error.upload.envelope_rejected`). A provenance blob is then admitted only as the canonical encoding of a manifest that agrees with the session's envelope and whose `device_sig` verifies under the key the uploader's published directory holds for the unrevoked device it names. The server cannot tell the signing device from another device of the account, so this runs on every provenance blob, delegated or not, and a failure is `error.upload.envelope_rejected`.
4. **Commit** — `BlobStore::commit`: atomic rename onto the content address, then directory fsync.
5. **Ledger** — append and fsync the [owner ledger](/design/filesystem/server/#the-owner-ledger) entry.
6. **Record and receipt** — in **one** PostgreSQL transaction, write the blob's index rows (minting the asset's next sequence number) and sign and log the [custody receipt](/design/transfer/durability/#custody-receipts). Either both commit or neither does: there is never an indexed blob without a receipt or a receipt without custody. A failure fails the session; the committed blob is unreferenced and collectable, and a retry is idempotent: a retry of a write that already committed returns the receipt it already has. A `replace` member other than its manifest writes no index row, because the manifest applies the whole bundle, so its receipt is the transaction's only write.
7. `Completed`.

A claim is driven to `Completed` or `FailedProcessing` independent of the request that triggered it: a client that disconnects mid-finalize does not stop finalization, and a client that disconnects while its completing chunk is being recorded does not stop the claim that follows. A session holding every declared byte that nobody has claimed — its process died between recording the last chunk and claiming — is finalized by the first request that meets it: a replay of any of its chunks, which is answered as the replay it is; the client's duplicate `create`, answered with the session at its full offset only once it has finalized; or its `HEAD`, which reports the state finalization left. A `create` whose session finalization refused opens a fresh session for the bytes. A claim whose process died is recovered on boot: before the listener accepts a request, the server lists every `WaitingForProcessing` session and drives each through the same steps, and it finalizes every session left full and unclaimed through the claim, so a client that reaches one first is not raced. If the stage was already committed, the committed blob is re-hashed in place of the stage and step 4 is skipped; the later steps are idempotent. A claim with neither a stage nor a committed blob fails. There is no claim lease: one process serves a blob root, so every claim listed before it serves belongs to a process that is gone.

The server can verify only ciphertext. The client verifies plaintext on download through STREAM's per-chunk tags. `Completed` is a one-time acknowledgement; standing durability is [`/storage/verify`](/design/transfer/durability/).

## Idempotency and Resumption

- **Duplicate create** keyed by `(owner_id, hash, album_id)`: an active session is returned (`200` + offset); an already-finalized hash is `409 error.upload.duplicate_blob` naming the existing asset. A `create` for a different asset first adopts the blob ([Deduplication and Merge](#deduplication-and-merge)).
- **Resume** is `HEAD`-driven. Interrupted transfers are normal; arbitrary pauses within the lifetime are tolerated.
- **Lost finalization ACK**: `HEAD` (or the session list) shows the terminal outcome.
- Every step is logged with the upload id.

Client recovery matrix (`capsule-sdk::upload`):

| Code | Client action |
| --- | --- |
| `error.upload.offset_mismatch` | Re-align to the returned offset (or `HEAD`) |
| `error.upload.checksum_mismatch` | Re-send the same chunk |
| `error.upload.session_not_found` | Re-create the session, restart the blob |
| `error.upload.duplicate_blob` | Resolve as a [merge](#deduplication-and-merge) |
| `error.upload.replace_incomplete` | Upload the rest of the bundle, retry the manifest |
| `error.upload.stale_revival` | Re-read the asset and rebase |
| `error.protocol.version_unsupported` (`426`) | Stop; prompt to update |
| `error.quota.exceeded` | Pause; surface the remediable state |
| `error.upload.rate_limited`, `unavailable`, `storage_full` | Back off per `Retry-After`, then resume from `HEAD` |
| other `4xx` | Fail the blob; never retry unchanged |

## Adaptive Chunk Sizing

The server enforces bounds by rejection only and never adapts. `capsule-sdk::upload` adapts: it starts from the suggested size, measures throughput over a sliding window, doubles or halves after a warm-up, and keeps every candidate a 4 KiB multiple by construction. Under an [adverse connection](/design/networking/#connection-classes) it holds the tier minimum. Per-blob ordering is not protocol; the pipeline decides what to [start](/design/import/pipeline/#upload-prioritization).

## Error Taxonomy

Clients branch on `error.*` codes ([i18n error-code contract](/design/i18n/#server-error-codes)), never on bare statuses. The authoritative list per endpoint is in the [OpenAPI contract](/reference/api/).

| Status | Codes |
| --- | --- |
| `400` | `malformed_request`, `unknown_crypto_suite`, `invalid_hash`, `invalid_size`, `unsupported_content_type`, `unknown_derivative_format`, `envelope_mismatch`, `timestamp_out_of_range`, `invalid_action`, `empty_chunk`, `chunk_not_aligned`, `missing_offset`, `missing_checksum`, `checksum_mismatch`, `size_exceeded`, `content_hash_mismatch`, `envelope_rejected`, `invalid_status_filter` |
| `403` | `album_access_denied`, `device_not_authorized`, `owner_not_permitted`, `error.quota.exceeded` |
| `404` | `session_not_found` |
| `409` | `duplicate_blob`, `offset_mismatch`, `chunk_conflict`, `session_not_active`, `receipt_not_available`, `replace_incomplete`, `stale_revival` |
| `413` | `file_too_large`, `chunk_too_large` |
| `415` | `unsupported_media_type` |
| `426` | `error.protocol.version_unsupported` |
| `429` | `rate_limited` |
| `500` | `storage_inconsistent` |
| `503` | `unavailable` |
| `507` | `storage_full` |

Unprefixed codes are `error.upload.*`.

## Deduplication and Merge

Blobs are content-addressed, so identical bytes are stored once. A finalized hash on create is `409 duplicate_blob` naming the existing asset; for a retry of the same blob that is complete. An asset that already exists remotely under a different ciphertext is admitted by the [pipeline](/design/import/pipeline/#plan--confirm) and MUST resolve as a **merge**: the existing blob is linked to the new asset reference, never re-uploaded. A merge is strictly additive — it never deletes a blob or rewrites a manifest; references are removed only by a signed `delete` ([Authorization](/design/authorization/)).

The server performs the link. A `create` whose hash another asset of the same owner and album already holds is **adopted** before it is refused: the existing blob is recorded against the requesting asset, with the same owner-ledger entries and custody receipt a finalization writes, and only then is the `409` returned. Every asset that depends on a blob is therefore a row naming it, which is what [garbage collection](/design/filesystem/maintenance/) counts, so purging one holder never collects bytes another still references. Adoption never stores bytes and is never charged against quota. It does not apply to a retry of the same asset, which already names the blob; to a `replace` member, whose manifest records the whole bundle; or to a signed manifest, which names its own asset. Bytes the store no longer holds are not adopted: the create opens an ordinary session so they can be uploaded again. An adoption that would re-point a singular role the asset already holds is `400 error.upload.envelope_mismatch`, the same answer finalization gives.

Duplicates already in a library are collapsed by [intra-library deduplication](/design/filesystem/maintenance/#deduplication).

## Quota and Permissions

- An upload is charged to `upload_user_id` (the uploader), distinct from `owner_id`. Uploading for another owner requires a verified relationship. Quota is enforced at session creation, after the duplicate check ([Quota](/design/quota/)).
- Adding to an album requires write-tier access. Asset uploads always carry a concrete `album_id` (the user's choice or the [default album](/design/organization/#the-default-album)), because the bytes are encrypted under that album's key.
- Only the uploader may `PATCH`. The uploader or owner may `HEAD` or `DELETE`; to anyone else the session does not exist (`404`).
- The uploading device need not be the manifest's signing device: any device of the same account may upload a bundle another device sealed and signed ([delegation](/design/peering/#delegation)). The signature is verified against the signer, never the uploader.

## Upload Tiering (Staged Uploads)

A per-device **upload policy** orders sessions; the server has no mode branch.

- **`full`** (default) — every session of a bundle opens eagerly, on a connection that permits bulk transfers; on any other a push runs `staged` (the [cache plan](/design/filesystem/client/#automatic-cache-management) defers bulk work).
- **`staged`** — sessions open in tier order per asset:

| Tier | Roles | Opens when |
| --- | --- | --- |
| **T0 — index** | provenance + metadata | any usable connection |
| **T1 — preview** | derivatives (+ derivative manifests) | any non-metered connection |
| **T2 — original** | original | unmetered Wi-Fi, or explicit force-sync |

T0 is exactly the visibility gate, so a staged asset is visible to other devices as soon as T0 finalizes, in the [awaiting-original](/design/transfer/download/#awaiting-original) state. Connection rules are [Sync — Synchronization Criteria](/design/sync/#synchronization-criteria). Types: `capsule-core::import::upload`; scheduler: `capsule-sdk::staged`.

- A staged asset pins its local original until T2 is [custody-confirmed](/design/transfer/durability/#verify-before-destroy).
- Staged and [streaming import](/design/import/pipeline/#import-upload-streaming-mode) are mutually exclusive per import; the planner rejects the combination.
- Each tier's sessions are charged at their own creation. Deleting an awaiting-original asset cancels its pending tiers ([redaction](/design/organization/#deletion-state-machine)).
- Resume re-derives the tier queue from server truth: held roles on the feed entry plus `GET /v1/upload/sessions`.

Streaming import uses these same sessions unchanged, one bounded window at a time; only the drive pattern differs.

## Queueing Across Servers

The upload queue holds every tier a device uploads, from T0 records to the data-heavy originals, video and derivatives. It is the client's queue, not the server's. When a device is signed in to more than one server (for example, two accounts on different self-hosted servers), the queue holds work for each of them and keeps four properties:

- **Per-server lanes.** The queue is keyed by server. Each lane has its own sessions, backoff, negotiated [protocol window](/design/versioning/#update-policy) and `Retry-After` state, so a `429`, an outage or a stall at one server never delays work bound for another.
- **One shared, bounded memory budget.** All lanes draw on one device-wide byte budget, and each open session holds at most one chunk window. Adding a server divides the budget and never multiplies it. A lane that makes no progress gives up its share, rather than holding buffers it cannot send.
- **Fair scheduling.** Lanes are served round-robin, and within a lane the [tier order](#upload-tiering-staged-uploads) holds. One lane's T0 work is never starved by another lane's T2 originals.
- **Rebuildable state.** On resume, each lane re-derives what remains from the local index (the Live assets that should reach that server; this device's own upload of a trashed asset stays [redacted](/design/organization/#deletion-state-machine)) and from its own server's truth (what it already has), as a single-server queue does. Losing the queue loses no work.

A [delegation](/design/peering/#delegation) hand-off is not a lane: the delegate pulls the bundle over the peer channel and uploads it from its own queue. Every album has exactly one home server, which takes every write to it, and multi-writer albums across servers are not part of v1 ([Federation](/design/federation/)). An asset is therefore only ever queued for its album's home server.
