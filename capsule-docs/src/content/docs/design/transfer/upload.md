---
title: Upload
description: Resumable, content-addressed upload sessions — chunk rules, session states, finalization, staged tiers and the error codes a client branches on
status: draft
---

The client-to-server half of the [transfer protocol](/design/transfer/), bound by its [invariants](/design/transfer/#invariants). It borrows the `PATCH` shape of TUS v1, generalised from an append offset to a byte range on a fixed grid, without claiming compatibility. Server: `capsule-server::upload`; client: `capsule-sdk::upload`. Request and response shapes, headers and constants are the [OpenAPI contract](/reference/api/) and `capsule-server::limits`; this page fixes their semantics.

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

- It is applied as one act **when its manifest lands**; the other members commit bytes but change nothing observable. The manifest therefore lands **last** — the protocol's only ordering rule: the client opens the manifest's session only once every other member reads `Completed` on `HEAD`.
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
| `POST /v1/upload` | Create a session for a declared ciphertext `size` and `hash`. Reserves the pending asset and its quota; returns `201` with `Location`, the session's expiry and the [server advice](/design/networking/#server-advice), or `200` with the session's current state, expiry and advice for an active duplicate. |
| `HEAD /v1/upload/{id}` | Authoritative committed prefix (`X-Capsule-Offset`), full committed set (`X-Capsule-Committed`), declared length, status, expiry and advice, and for `FailedProcessing` the failure's `error.*` code (`X-Capsule-Upload-Error`). The resumption primitive. |
| `PATCH /v1/upload/{id}` | Write one range at `X-Capsule-Offset`; `204` with the committed prefix, committed set and status. |
| `DELETE /v1/upload/{id}` | Cancel; removes the staged file, session and pending reservation. `409 error.upload.session_not_active` while finalizing or once terminal. Serialised with admission: a range still in flight when its session is deleted is never recorded. |
| `GET /v1/upload/sessions` | The **uploader's** live sessions, for resume. An unknown `?status=` is `400 error.upload.invalid_status_filter`, never ignored. |
| `GET /v1/upload/{id}/receipt` | The session's [custody receipt](/design/transfer/durability/#custody-receipts) once `Completed`. |

## Chunk Rules and Strictness

Strictness covers the transport — JSON bodies (unknown fields rejected) and headers. Inside signed CBOR, unknown keys are preserved ([Postel's law, asymmetric](/design/principles/#postels-law-asymmetric)).

A session's bytes sit on a **4 KiB grid**, a protocol constant. A chunk is a contiguous range of whole grid blocks; its length is the client's choice per request and may change between requests without changing any address. Sending ranges in order is one valid schedule among many: reordering is the client choosing which range to send next, never a change of layout.

- A `PATCH` body MUST be `application/octet-stream` and non-empty.
- `X-Capsule-Offset` names the range's first byte and MUST be a multiple of 4 KiB. The range's length MUST be a multiple of 4 KiB, except a range that ends exactly at the declared `size`; a chunk is at most 16 MiB. These bounds are protocol surface.
- A range that would end past the declared `size` is refused `400 error.upload.size_exceeded`; nothing is written.
- Every `PATCH` MUST carry `X-Capsule-Checksum`, the SHA-256 of the chunk. The server verifies it **before writing**.
- The server holds the session's **committed set**: the coalesced ranges it has made durable. It is the only truth; staged bytes outside it are garbage, whatever the staging file holds.

**Admission** is decided atomically per session against the committed set and the ranges in flight, **before any byte is written**:

| The range is… | Answer |
| --- | --- |
| disjoint from every committed and in-flight range | claimed in flight, written, fsynced, recorded as committed, then `204` |
| wholly inside the committed set | `204` with the session's current state; nothing is written |
| overlapping an in-flight range | `409 error.upload.range_conflict` with `X-Capsule-Committed` and `Retry-After`; nothing is written |
| partly committed and partly not | `409 error.upload.range_conflict` with `X-Capsule-Committed`; nothing is written |

No write can therefore touch a committed byte, and two requests can never write the same byte concurrently. A claim is released only once every write issued under it has returned, so a write abandoned by its request can never land after a retry of the same range commits. A range wholly inside the committed set is answered as a duplicate without comparing bytes. The server keeps no per-write record, and a client that re-sends different bytes over committed ones learns nothing until finalize. The [source guards](/design/cryptography/encryption/#encryption-workflow) stop that on the client, and the finalize re-hash refuses it on the server. The full idempotency contract is [Threat Model — Idempotency Invariants](/design/threat-model/validation/#idempotency-invariants).

- **Bounded fan-in.** A session's in-flight cap is fixed at creation and advertised as the advice's `ranges` member on create and `HEAD`; one range more is `409 error.upload.too_many_in_flight` with `Retry-After`, which tells the client to narrow its window rather than back off the lane. A claim lives until its write returns, so a request the client abandoned can still hold one for a while. The committed set holds at most 64 fragments after coalescing; a write that would exceed that is `409 error.upload.too_fragmented`. Admission counts the committed fragments plus every in-flight range plus the new one against the bound, so a write that fails can never leave more than 64. A client that keeps within the cap, counts an abandoned request as in flight until its response-wait bound expires, and fills its lowest gap first meets neither bound.
- The session finalizes automatically when the committed set is exactly `[0, size)`.

## Hostile Clients

The server is friendly to a correct client and assumes any client may be broken, outdated or
malicious. No request, sequence of requests or connection behaviour may corrupt stored data,
wedge another request's session, exhaust shared resources, or reveal whether a session it does
not own exists.

- **A session never wedges.** Admission to one session is serialised; admitted disjoint ranges
  are written concurrently. A failed or partial write (I/O error, full disk, dropped connection
  mid-body) releases its in-flight claim and commits nothing, so the client's retry of the same
  range succeeds.
- **No request waits on finalization.** The `PATCH` that completes the committed set, and every
  `HEAD`, answer with the session's status as it stands. They never hold the
  request open for a re-hash, so a slow disk can never turn a large upload into a proxy timeout.
- **Disk full is the client's to wait out, not a server fault.** `ENOSPC` is
  `507 error.upload.storage_full`; a blob store or index outage is `503 error.upload.unavailable`
  with `Retry-After`. `500 error.upload.storage_inconsistent` is reserved for a state the server
  cannot explain, and is logged at `ERROR`.
- **Bounded per client.** Session creation and chunk writes are rate-limited per account and
  per source address (`429` with `Retry-After`); live sessions per account are capped. The
  source address is the client the deployment's trusted proxies resolve, never a forwarding
  header taken on its word. The account's budget is charged first, so a request it refuses
  spends nothing of the address's budget, which the other accounts behind that address share.
- **Bounded per connection.** Header and idle timeouts close a connection that sends too slowly.
  A chunk body is cut when it **stalls**, delivering no bytes for the stall bound, or falls
  below a minimum sustained rate. It is never cut for its total duration, so a link that sustains the
  minimum rate finishes any chunk. A cut chunk is `408 error.upload.chunk_timeout` and
  commits nothing. A request body is never buffered beyond the chunk maximum.
- **Requests never panic.** Every reachable failure is a typed rejection. A panic in a handler
  is a bug, caught at the router and answered `500` without leaking state.
- **Foreign is unknown.** A session belonging to another account is the same `404` as one that
  never existed; only the uploader may write to a session. An upload id is never authority on
  its own: every request also carries the uploader's credential. A session is bound to its
  uploader's account, never to an access token, so refreshing a token never ends an upload.
- **Every rejection is logged** with the upload id, account, offset and code, so a misbehaving
  client is diagnosable from the server alone.

### Protocol Versioning

The upload surface is gated once by the [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation): a date-versioned `X-Capsule-Protocol`, with the accepted window advertised on every response. There is no capability negotiation. A version outside the window is `426` before anything is reserved. The date is bumped only for an incompatible change (range or admission semantics, the 4 KiB grid, lowering the 16 MiB maximum or the 64-fragment bound, finalization, the state machine). [Server advice](/design/networking/#server-advice) is tunable, not protocol.

A session is frozen at creation: its `size`, `hash`, in-flight cap and protocol version hold until it reaches a terminal state or its cap, and a server upgraded mid-session keeps serving it under them.

## Session State Machine

```plaintext
Pending ─▶ Uploading ─▶ WaitingForProcessing ─▶ Completed
                                            └─▶ FailedProcessing
```

| From | To | On |
| --- | --- | --- |
| `Pending` | `Uploading` | first **accepted** chunk |
| `Uploading` | `WaitingForProcessing` | committed set = `[0, size)` (finalize claim) |
| `Pending`/`Uploading` | *(discarded)* | lifetime cap or pressure discard |
| `WaitingForProcessing` | `Completed` | finalization succeeded |
| `WaitingForProcessing` | `FailedProcessing` | hash, size or envelope failure |

`WaitingForProcessing` is neither evictable nor cancellable, and always reaches a terminal state ([Finalization and Integrity](#finalization-and-integrity)). `FailedProcessing` removes the staged bytes and reservation and keeps the status record so a client whose ACK was lost can read the outcome. Session state lives in the session store (Valkey via `redis-rs`, or in-memory), never in PostgreSQL; see [Filesystem — Server](/design/filesystem/server/#required-services).

## Session Lifetime and Discard

- **Survival floor.** A `Pending`/`Uploading` session survives at least 1 hour after its last accepted chunk (or creation).
- **Cap.** No session outlives 24 hours from creation.
- **Advertised, relative.** Create and `HEAD` state the cap and the floor as seconds remaining (`X-Capsule-Session-Expires-In`, `X-Capsule-Session-Idle-Expires-In`), never as wall-clock instants, so a client with a skewed clock schedules correctly.
- **Discard window.** Between floor and cap the server MAY discard sessions under pressure, least-recently-progressed first.
- **Uniform `404`.** Discarded, expired and never-existent sessions are indistinguishable: `404 error.upload.session_not_found` ([rejection mapping](/design/api-surfaces/#rejection-mapping)). The client re-creates the session and restarts that blob, with bounded backoff.
- **Terminal records** are exempt from pressure discard and live to the cap.
- **Startup scrub.** On boot a staged file with no session is deleted, and a session whose staged file is missing is discarded. In-flight claims do not survive a restart: a range written but never recorded is outside the committed set and is simply sent again.

## Staging

Each session owns one staged file, sized to the declared `size` and written positionally; there is no per-chunk file and no assembly step. A range's bytes are written at their offset and fsynced, and only then recorded in the committed set and answered `204`. The record is the commit point. A write that fails before it leaves bytes the committed set does not cover, which the next write of that range overwrites. A record the session store loses is a range the client sends again; the finalize re-hash, not the record, is what admits bytes to a content address. A discarded or deleted session takes no further record. Hashing runs off the async reactor. Total in-flight bytes are bounded by the cache limit, with the discard window as the relief valve.

## Finalization and Integrity

Order is the contract (`capsule-server::upload::finalize`):

1. **Claim** — compare-and-set into `WaitingForProcessing`. A loser is not an error: its chunk was accepted and it answers `204`; the outcome is read from `HEAD` or the session list.
2. **Re-hash** the staged file and compare to the declared hash. Mismatch → `FailedProcessing`, `error.upload.content_hash_mismatch`.
3. **Envelope checks** re-run against the current clock and authority, so a device revoked or an album closed since creation does not slip through (`error.upload.envelope_rejected`). A provenance blob is then admitted only as the canonical encoding of a manifest that agrees with the session's envelope and whose `device_sig` verifies under the key the uploader's published directory holds for the unrevoked device it names. The server cannot tell the signing device from another device of the account, so this runs on every provenance blob, delegated or not, and a failure is `error.upload.envelope_rejected`.
4. **Commit** — `BlobStore::commit`: atomic rename onto the content address, then directory fsync.
5. **Ledger** — append and fsync the [owner ledger](/design/filesystem/server/#the-owner-ledger) entry.
6. **Record and receipt** — in **one** PostgreSQL transaction, write the blob's index rows (minting the asset's next sequence number) and sign and log the [custody receipt](/design/transfer/durability/#custody-receipts). Either both commit or neither does: there is never an indexed blob without a receipt or a receipt without custody. A failure fails the session; the committed blob is unreferenced and collectable, and a retry is idempotent: a retry of a write that already committed returns the receipt it already has. A `replace` member other than its manifest writes no index row, because the manifest applies the whole bundle, so its receipt is the transaction's only write.
7. `Completed`.

The request that completes the committed set takes the claim and answers `204` with `X-Capsule-Upload-Status: waiting_for_processing`; it does not wait for steps 2–7. The client reads the outcome from `HEAD`, polling with backoff, as it reads any other state. A `FailedProcessing` outcome carries the step's `error.*` code in `X-Capsule-Upload-Error` and in the session list, which is how the client reaches its recovery row. A [guest drop](/design/web-upload/) is held to the same rule: its deposit runs detached, and the guest reads the outcome through the drop's status probe under the link capability. A claim is driven to `Completed` or `FailedProcessing` independent of the request that triggered it: a client that disconnects mid-finalize does not stop finalization, and a client that disconnects while its completing chunk is being recorded does not stop the claim that follows. A session holding every declared byte that nobody has claimed — its process died between recording the last chunk and claiming — has its claim taken by the first request that meets it: a duplicate of any of its ranges, the client's duplicate `create`, or its `HEAD`. Each starts finalization detached and answers the state as it stands, `waiting_for_processing`. A `create` whose session finalization refused opens a fresh session for the bytes. A claim whose process died is recovered on boot: before the listener accepts a request, the server lists every `WaitingForProcessing` session and drives each through the same steps, and it finalizes every session left full and unclaimed through the claim, so a client that reaches one first is not raced. If the stage was already committed, the committed blob is re-hashed in place of the stage and step 4 is skipped; the later steps are idempotent. A claim with neither a stage nor a committed blob fails. There is no claim lease: one process serves a blob root, so every claim listed before it serves belongs to a process that is gone.

The server can verify only ciphertext. The client verifies plaintext on download through STREAM's per-chunk tags. `Completed` is a one-time acknowledgement; standing durability is [`/storage/verify`](/design/transfer/durability/).

## Idempotency and Resumption

- **Duplicate create** keyed by `(owner_id, hash, album_id)`: an active session is returned (`200` + its state); an already-finalized hash is `409 error.upload.duplicate_blob` naming the existing asset. A `create` for a different asset first adopts the blob ([Deduplication and Merge](#deduplication-and-merge)).
- **Resume** is `HEAD`-driven. A sequential client reads the committed prefix; one that sends out of order reads the committed set, a comma-separated list of inclusive byte ranges such as `0-1048575,2097152-4194303`, never longer than the fragment bound. The client re-plans from the committed set it reads and never from its own record of what it sent. Interrupted transfers are normal; arbitrary pauses within the lifetime are tolerated.
- **Lost chunk ACK.** After a transport error the client re-sends the same bytes or reads the session state before it sends anything else. A re-send of a range that did commit, whole or resized within it, is a duplicate; a re-send while the first attempt is still in flight is a `range_conflict` with `Retry-After`. Neither fails the blob.
- **Lost finalization ACK**: `HEAD` (or the session list) shows the terminal outcome.
- Every step is logged with the upload id.

Client recovery matrix (`capsule-sdk::upload`):

| Code | Client action |
| --- | --- |
| `error.upload.range_conflict` | Re-plan from the committed set the refusal carries (or `HEAD`), after its `Retry-After` when it names one |
| `error.upload.too_fragmented` | Re-plan to fill the lowest gap first |
| `error.upload.too_many_in_flight` | Wait its `Retry-After`, narrow the session's window to the advertised `ranges`, then continue |
| `error.upload.content_hash_mismatch` (on `HEAD`) | Check the source identity: changed → fail the blob with a typed source-changed error; unchanged → re-create the session once |
| `error.upload.checksum_mismatch` | Re-send the same chunk |
| `error.upload.chunk_timeout` (`408`) | Re-send the same offset with a shorter range ([Adaptive Chunk Sizing](#adaptive-chunk-sizing)) |
| `error.upload.session_not_found` | Re-create the session, restart the blob |
| `error.upload.duplicate_blob` | Resolve as a [merge](#deduplication-and-merge) |
| `error.upload.replace_incomplete` | Upload the rest of the bundle, retry the manifest |
| `error.upload.stale_revival` | Re-read the asset and rebase |
| `error.protocol.version_unsupported` (`426`) | Stop; prompt to update |
| `error.quota.exceeded` | Pause; surface the remediable state |
| `error.upload.rate_limited`, `unavailable`, `storage_full` | Back off per `Retry-After`, then resume from `HEAD` |
| other `4xx` | Fail the blob; never retry unchanged |

## Adaptive Chunk Sizing

The server enforces bounds by rejection only and never adapts. `capsule-sdk::upload` adapts: it starts from the advised size, clamped to its own bounds, measures goodput over a sliding window, doubles or halves after a warm-up, and keeps every candidate a 4 KiB multiple by construction. Because the server cuts only a stalled chunk, a chunk's size never has to fit a total-time deadline. The client sizes a chunk so it completes in a target request duration at the measured goodput; that bounds what one lost request costs and also keeps the round trip a small share of each request. Under an [adverse connection](/design/networking/#connection-classes) the target duration is shorter. A `408 error.upload.chunk_timeout` means the link stalled: the client re-sends the same offset with a shorter range and does not grow again until a run of successes shows the link recovered. Each re-send charges the transfer's bounded recovery budget.

Which ranges are in flight, and in what order, is the client's schedule, never protocol. The client keeps at most the in-flight window of [Queueing Across Servers](#queueing-across-servers) per session, never above the session's advertised cap, and fills the lowest gap first, so a sequential schedule is the special case of a window of one. Across blobs, the pipeline decides what to [start](/design/import/pipeline/#upload-prioritization).

## Error Taxonomy

Clients branch on `error.*` codes ([i18n error-code contract](/design/i18n/#server-error-codes)), never on bare statuses. The authoritative list per endpoint is in the [OpenAPI contract](/reference/api/).

| Status | Codes |
| --- | --- |
| `400` | `malformed_request`, `unknown_crypto_suite`, `invalid_hash`, `invalid_size`, `unsupported_content_type`, `unknown_derivative_format`, `envelope_mismatch`, `timestamp_out_of_range`, `invalid_action`, `empty_chunk`, `chunk_not_aligned`, `missing_offset`, `missing_checksum`, `checksum_mismatch`, `size_exceeded`, `content_hash_mismatch`, `envelope_rejected`, `invalid_status_filter` |
| `403` | `album_access_denied`, `device_not_authorized`, `owner_not_permitted`, `error.quota.exceeded` |
| `404` | `session_not_found` |
| `408` | `chunk_timeout` |
| `409` | `duplicate_blob`, `range_conflict`, `too_fragmented`, `too_many_in_flight`, `session_not_active`, `receipt_not_available`, `replace_incomplete`, `stale_revival` |
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

- An upload is charged to `upload_user_id` (the uploader), distinct from `owner_id`. Uploading for another owner requires a verified relationship. Quota is reserved for the declared `size` at session creation, after the duplicate check, and the reservation is released when the session is deleted, discarded, expired or ends in `FailedProcessing` ([Quota](/design/quota/)).
- Adding to an album requires write-tier access. Asset uploads always carry a concrete `album_id` (the user's choice or the [default album](/design/organization/#the-default-album)), because the bytes are encrypted under that album's key.
- Only the uploader may `PATCH`. The uploader or owner may `HEAD` or `DELETE`; to anyone else the session does not exist (`404`).
- The uploading device need not be the manifest's signing device: any device of the same account may upload a bundle another device sealed and signed ([delegation](/design/peering/#delegation)). The signature is verified against the signer, never the uploader.

## Upload Tiering (Staged Uploads)

A per-device **upload policy** orders sessions; the server has no mode branch.

- **`full`** (default) — every session of a bundle is eligible at once, within the [in-flight windows](#queueing-across-servers), on a connection that permits bulk transfers; on any other a push runs `staged` (the [cache plan](/design/filesystem/client/#automatic-cache-management) defers bulk work).
- **`staged`** — sessions open in tier order per asset:

| Tier | Roles | Opens when |
| --- | --- | --- |
| **T0 — index** | provenance + metadata | any usable connection |
| **T1 — preview** | derivatives (+ derivative manifests) | any non-metered connection |
| **T2 — original** | original | unmetered Wi-Fi, or explicit force-sync |

T0 is exactly the visibility gate, so a staged asset is visible to other devices as soon as T0 finalizes, in the [awaiting-original](/design/transfer/download/#awaiting-original) state. Connection rules are [Sync — Synchronization Criteria](/design/sync/#synchronization-criteria). Types: `capsule-core::import::upload`; scheduler: `capsule-sdk::staged`.

- A staged asset pins its local original until T2 is [custody-confirmed](/design/transfer/durability/#verify-before-destroy).
- Staged and [streaming import](/design/import/pipeline/#import-upload-streaming-mode) are mutually exclusive per import; the planner rejects the combination.
- Each tier's sessions are charged at their own creation. Deleting an asset whose upload has not finished, an awaiting-original one included, [redacts](/design/organization/#deletion-state-machine) it before its `delete` is sent. Every open session `GET /v1/upload/sessions` lists for the asset's `asset_id` is cancelled with `DELETE /v1/upload/{id}`, and no pending tier of a trashed asset is opened. A session already finalizing answers `409 error.upload.session_not_active`. That is not a failure: its blob lands, and the `delete` sent after it supersedes the create it completes.
- Resume re-derives the tier queue from server truth: held roles on the feed entry plus `GET /v1/upload/sessions`.

Streaming import uses these same sessions unchanged, one bounded window at a time; only the drive pattern differs.

## Queueing Across Servers

The upload queue holds every tier a device uploads, from T0 records to the data-heavy originals, video and derivatives. It is the client's queue, not the server's. When a device is signed in to more than one server (for example, two accounts on different self-hosted servers), the queue keeps one **lane** per server, keyed by the server's canonical origin, and keeps four properties. Scheduler: `capsule-sdk::upload::queue`.

- **Per-server lanes.** Each lane has its own sessions, backoff, negotiated [protocol window](/design/versioning/#update-policy) and `Retry-After` deadline, so a `429`, an outage or a stall at one server never delays work bound for another.
- **Two in-flight windows per lane.** A lane holds up to **W** sessions open at once, and each session holds up to **P** ranges in flight. Both are client-local, capped by bounds compiled into the client and lowered by the server's [advice](/design/networking/#server-advice), and W never exceeds the account's live-session cap. They are not the protocol window. Several sessions in flight, each on its own connection, are what keep a high-latency or lossy link busy: one connection's throughput is bounded by its round trip and loss rate, however large its chunks.
- **One shared, bounded memory budget.** Every range in flight reserves its length against one device-wide byte budget, so adding a server divides the budget and never multiplies it. A blob's size reserves nothing, because a push holds only the windows it is sending ([bounded memory](/design/cryptography/encryption/#stream-construction)). A lane whose transfers report no progress for the stall bound has its reservations revoked: its work returns to the front of its queue, and the lane is not served again for one more stall bound, rather than holding buffers it cannot send.
- **Fair scheduling.** Lanes are served round-robin, and a lane always serves its lowest tier first and never skips its own head, so the [tier order](#upload-tiering-staged-uploads) holds per server. One lane's T0 work is never starved by another lane's T2 originals.
- **Rebuildable state.** On resume, each lane re-derives what remains from the local index (the Live assets that should reach that server; this device's own upload of a trashed asset stays [redacted](/design/organization/#deletion-state-machine)) and from its own server's truth (held roles on its feed plus its `GET /v1/upload/sessions`), as a single-server queue does. No lane's queue is derived from another server's, and losing the queue loses no work.

With one server the queue is one lane.

A [delegation](/design/peering/#delegation) hand-off is not a lane: the delegate pulls the bundle over the peer channel and uploads it from its own queue. Every album has exactly one home server, which takes every write to it, and multi-writer albums across servers are not part of v1 ([Federation](/design/federation/)). An asset is therefore only ever queued for its album's home server.
