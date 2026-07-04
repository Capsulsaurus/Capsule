---
title: Upload Protocol
description: The wire protocol between Capsule clients and the server for resumable, content-addressed uploads
status: draft
---

The upload protocol is a custom resumable-upload protocol modeled on [TUS](https://tus.io/) but trimmed to Capsule's needs: no per-request capability negotiation, no metadata smuggled in headers, ciphertext-only payloads. Compatibility is gated once, up front, via the universal [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation). (We follow TUS v1's offset/PATCH model, not the TUS v2 draft — see the media-type note in [Chunk Rules](#chunk-rules-and-strictness).)

This protocol is the most fragile contract between client and server: a client that misunderstands chunk alignment, offset semantics, or finalization can silently corrupt or orphan data. The endpoint table, the chunk rules, the session state machine, and the finalization steps below **are the contract** — every implementation MUST conform exactly. The client implementation lives in `capsule-sdk::upload`; the server in `capsule-api-upload`. The two are tested independently against the protocol surface.

The protocol is **strict by construction**: any request that violates the contract is rejected with a machine-readable error, never silently corrected or partially honored. Leniency here would hide client bugs until they corrupt data; strictness surfaces them as loud, attributable rejections on the first bad request. The [strictness table](#chunk-rules-and-strictness) enumerates every tolerated-vs-rejected behavior, and the [error taxonomy](#error-taxonomy) names the code a client receives for each.

Every upload is **idempotent** but stateful. Uploads can complete partially and are identified by an *upload ID*.

## What Gets Uploaded

An asset is never uploaded as a single plaintext file. Because Capsule is end-to-end encrypted, the client **encrypts and signs** everything *before* transmission, and the server only ever stores opaque, content-addressed ciphertext blobs. A single imported asset produces a **bundle** of blobs:

- The **original blob** — the source asset, encrypted under the [bulk AEAD](/design/cryptography/primitives/#bulk-aead) with the [STREAM construction](/design/cryptography/encryption/#stream-construction).
- **Derivative blobs** — thumbnails and previews, generated client-side during import (see [Thumbnails](/design/thumbnails/)), each encrypted independently.
- The **metadata blob** — the CBOR metadata document (capture date, dimensions, EXIF-derived fields, the [LQIP](/design/thumbnails/#lqip), provenance), encrypted under the [bulk AEAD](/design/cryptography/primitives/#bulk-aead) (see [Metadata](/design/metadata/)).

Each blob is its own upload with its own upload ID; the protocol does not couple them and imposes **no wire ordering**. Every session declares its blob's **role** at creation — a closed enum `original | derivative | metadata | provenance | backup` in the `POST /upload` body, recorded on the pending row — so the server can reason about bundle completeness without reading plaintext. The client may transfer the bundle in any order — decoupling lets small derivatives land while a large original is still in flight — and the server **gates visibility** on the pending-asset row: the asset becomes visible to other devices once its **manifest and metadata blob are finalized**. Whether the original is also held yet travels as a per-asset completeness fact on the sync feed (`original_held`), which is what makes [staged uploads](/design/import/download-sync/) possible; an asset whose original has not landed is in the derived `awaiting-original` state, and fetching its original returns the transient `error.blob.pending_upload` rather than `410`. Every blob in the bundle — original, derivatives, metadata, provenance — counts toward the uploader's storage [quota](/design/quota/#accounting-model).

"Blob" is defined once, in [Filesystem — Server: Uniform, Opaque Blobs](/design/filesystem/server/#uniform-opaque-blobs); this protocol is its transport, not its definition. Every asset and derivative blob carries a signed [manifest envelope](/design/cryptography/provenance/#asset-manifest): at `POST /upload` the server validates the envelope's `created_by_device` against the uploader's [device directory](/design/cryptography/keys/#device-directory) (invariant 7), and the client verifies the full write-tier signature on download via [`verify_asset`](/design/cryptography/keys/#write-authorization). **Backup artifacts are the one exception** — they carry no per-asset provenance of their own (the exporting device is not the original author); their integrity rides the library-level backup MANIFEST instead (see [Backup and Recovery](/design/backup-recovery/)).

The server performs no decoding, no metadata extraction, and no thumbnail generation — it cannot, since it never holds a decryption key. All such work happens client-side during [import](/design/import/pipeline/).

## Design Invariants

The upload protocol guarantees the following, and every endpoint upholds them:

- **Content-addressed.** Every blob is identified by its [ciphertext content hash](/design/cryptography/primitives/). The plaintext hash is never transmitted to the server.
- **Idempotent.** Re-creating a session for a blob with an *active* session returns that session; re-creating one for a hash already *finalized* is rejected with a conflict that names the existing asset so the client resolves it as a [merge](#deduplication-and-merge). Re-sending a chunk with the same idempotency tuple returns the same response.
- **Resumable.** A session is guaranteed to survive at least the [survival floor](#session-lifetime-and-discard) and may survive up to the 24-hour cap. A client resumes by querying the authoritative offset and continuing from there — no bytes are re-sent unnecessarily.
- **Strict.** Unknown request fields, empty chunks, wrong media types, and malformed headers are rejected outright. A buggy client is stopped at its first bad request, not accommodated.
- **Strictly bounded.** The total ciphertext size is declared at session creation and immutable thereafter. The cumulative received bytes may never exceed it, nor exceed the server's per-file limit.
- **Verified.** No upload is marked complete until the server has recomputed the ciphertext hash and confirmed it matches the declared value.
- **Recoverable.** Every session is either driven to a terminal state or garbage-collected. There are no permanently orphaned upload files or pending asset rows.

## Endpoints

All endpoints are authenticated with a bearer JWT.

| Method   | Path               | Purpose                                                                                                                                                                                                                                                                                                                                                                                                       |
| -------- | ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `POST`   | `/upload`          | Create a session. The JSON body declares ciphertext `size`, `hash` (lowercase hex; digest length fixed by `crypto_suite_id`), `content_type` (closed enum), `crypto_suite_id`, `protocol_version`, `blob_role` (closed enum), `manifest_envelope` (the unencrypted manifest fields the server validates per [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants)), optional `album_id`, optional `owner_id`, optional `intent_id` (required only during an [album upgrade](/design/versioning/#album-upgrade-ceremony)). Unknown fields are rejected. Returns `201` with `Location: /upload/{id}` and `X-Capsule-Suggested-Chunk-Size`. Rejects per the [error taxonomy](#error-taxonomy). |
| `HEAD`   | `/upload/{id}`     | Query progress. Returns `X-Capsule-Offset` (next expected byte), `X-Capsule-Content-Length` (declared total), and `X-Capsule-Upload-Status` (the session state). No body — HTTP forbids one on `HEAD`, which is why status rides a header. This is the resumption primitive.                                                                                                                                     |
| `PATCH`  | `/upload/{id}`     | Append a chunk at `X-Capsule-Offset`, with `Content-Type: application/octet-stream` and the **required** per-chunk `X-Capsule-Checksum`. Returns `204` and the new offset.                                                                                                                                                                                                                                      |
| `DELETE` | `/upload/{id}`     | Cancel the session — removes the upload file, the session record, and the pending asset row. Rejected with `409` while finalization is running.                                                                                                                                                                                                                                                                 |
| `GET`    | `/upload/sessions` | List the **uploader's** active sessions (keyed by `upload_user_id` — the resuming party — not `owner_id`), so a client can resume across app restarts.                                                                                                                                                                                                                                                          |

Creating a session writes a *pending* asset row to Postgres (`uploaded = false`) and a session record to the configured **session-state store** (see [Filesystem — Server: Deployment Profiles](/design/filesystem/server/#deployment-profiles): Postgres by default, Valkey in the high-concurrency profile). The pending row reserves the asset ID that derivative and metadata blobs reference. The session record carries everything finalization needs — declared size and hash, `content_type`, `crypto_suite_id`, `protocol_version`, `blob_role`, the manifest envelope, `album_id`/`owner_id`/`intent_id`, and the uploader — so a session is finalizable from its own record with no further client input.

Per-endpoint header census (the `X-Capsule-*` namespace itself is owned by the [universal header census](/design/threat-model/validation/#universal-headers)):

| Header                          | Direction | Where                    | Required |
| ------------------------------- | --------- | ------------------------ | -------- |
| `X-Capsule-Protocol` (+ `-Min`/`-Max` on responses) | both | every request/response | yes |
| `X-Capsule-Suggested-Chunk-Size` | response  | `POST /upload`           | yes      |
| `X-Capsule-Offset`              | both      | `PATCH` request; `HEAD`/`PATCH`/`409` responses | yes |
| `X-Capsule-Content-Length`      | response  | `HEAD`                   | yes      |
| `X-Capsule-Upload-Status`       | response  | `HEAD`                   | yes      |
| `X-Capsule-Checksum`            | request   | `PATCH`                  | **yes**  |

## Chunk Rules and Strictness

Enforced strictly; a violation fails the request rather than being silently corrected:

- **Media type.** A `PATCH` body MUST be `Content-Type: application/octet-stream` (parameters ignored) — the payload is literally opaque ciphertext bytes. Anything else, or a missing `Content-Type`, is rejected with `415`. We deliberately do not use the TUS v2 draft's `application/partial-upload`: we implement TUS v1 semantics with our own headers, and advertising v2's media type would claim a compatibility we do not have.
- **Non-empty.** An empty `PATCH` body is rejected with `400`. (An empty chunk can only be a client bug; accepting it as a no-op would mask that bug.)
- **Alignment.** Every chunk except the final one MUST be a multiple of 4 KiB (4096 bytes). This keeps server-side writes page/block-aligned — friendly to direct and batched I/O on the [append path](#append-only-storage) — and doubles as a cheap tripwire for client offset-arithmetic bugs: a client whose chunking drifts mis-aligns long before it corrupts anything. The final chunk is exempt because the blob's total size is arbitrary. A non-aligned, non-final chunk is rejected with `400`. (The [encryption layer's](/design/cryptography/encryption/#stream-construction) 65,536-byte ciphertext stride is a separate concern; 65,536 is a clean multiple of 4,096, so crypto-chunked ciphertext is transport-alignable by construction.)
- **Bounds.** A chunk is at least 4 KiB (except the final chunk) and at most **16 MiB**; a larger body is rejected with `413`. These bounds are protocol surface (see [Protocol Versioning](#protocol-versioning)); the *suggested* sizes and adaptation tiers are not.
- **Sequential offsets.** A `PATCH` must arrive at exactly the current received-byte count; an out-of-order or gapped write is rejected with `409` carrying the authoritative offset, and the client recovers by re-aligning (or issuing `HEAD`).
- **Required checksum + idempotency tuple.** Every `PATCH` carries `X-Capsule-Checksum`: the SHA-256 of the chunk bytes as bare lowercase hex. Missing or malformed → `400`. The server verifies the body against the header **before any write**; a mismatch is `400` with nothing persisted (the transit-corruption defense). The server keys each accepted PATCH by `(upload_id, offset, chunk_hash)`: a duplicate PATCH with the same tuple returns the same response — a re-send after a lost ACK is a no-op — while a PATCH at an already-acknowledged offset *with a different `chunk_hash`* is rejected with `409` + a corruption error: the structural defense against a faulty client that retries with garbage. The checksum is required precisely because this tuple is undefined without it. The complete idempotency contract is owned by [Threat Model — Idempotency Invariants](/design/threat-model/validation/#idempotency-invariants).
- **Cumulative bounds.** Cumulative size may never exceed the declared `size` nor the server's `max_file_size`. The server checks the cumulative count **at every chunk arrival**, not only at finalization — a buggy client that streams past the declared size is cut off before more bytes are persisted. Either ceiling is rejected (`400` / `413`) and the session is moved to `FailedProcessing`.
- The upload completes exactly when received bytes equal the declared size; finalization then runs automatically.

Strictness applies to the **transport layer** — the JSON request bodies (unknown fields rejected) and the header set above. It does not extend into the CBOR interiors (manifests, sidecars), where the schema rules deliberately require clients to *preserve* unknown keys for forward compatibility; that asymmetry is [Postel's law as scoped by Core Principles](/design/principles/#postels-law-asymmetric): be strict on the wire you own, tolerant inside the documents that outlive you.

| Candidate leniency                          | Behavior     | Rejection                                      |
| ------------------------------------------- | ------------ | ---------------------------------------------- |
| Unknown JSON field on `POST /upload`        | **rejected** | `400 error.upload.malformed_request`           |
| Empty `PATCH` body                          | **rejected** | `400 error.upload.empty_chunk`                 |
| Wrong / missing `Content-Type` on `PATCH`   | **rejected** | `415 error.upload.unsupported_media_type`      |
| Missing / malformed `X-Capsule-Checksum`    | **rejected** | `400 error.upload.missing_checksum`            |
| Checksum ≠ body hash                        | **rejected** | `400 error.upload.checksum_mismatch` (no write)|
| Unaligned non-final chunk                   | **rejected** | `400 error.upload.chunk_not_aligned`           |
| Chunk > 16 MiB                              | **rejected** | `413 error.upload.chunk_too_large`             |
| Stale / gapped offset                       | **rejected** | `409 error.upload.offset_mismatch` + offset    |
| Replay, same idempotency tuple              | *accepted*   | same response as the original (no-op)          |
| Same offset, different checksum             | **rejected** | `409 error.upload.chunk_conflict`              |
| Bytes past declared `size`                  | **rejected** | `400 error.upload.size_exceeded` → failed      |
| `PATCH`/`DELETE` on a finalizing session    | **rejected** | `409 error.upload.session_not_active`          |

## Protocol Versioning

The upload session is gated by Capsule's universal [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation), so a client never begins a transfer against a server it is not known to be compatible with. This section names the upload-specific specializations.

Versioning is **date-based** (`YYYY-MM-DD` — the day a protocol revision is frozen), not integer or semver. An integer version conveys nothing about ordering granularity and invites a bump for every change; semver implies a minor/patch backward-compatibility contract finer than we are willing to maintain on a hot path. A date is unambiguously ordered, human-readable, and maps directly onto a release.

- Every client sends `X-Capsule-Protocol: <date>` on every request (the upload-specific alias `X-Capsule-Upload-Protocol` remains accepted but is deprecated). The server advertises the inclusive range it accepts via `X-Capsule-Protocol-Min` and `-Max` on every response, errors included.
- A `POST /upload` whose version falls outside the accepted range is rejected with `426 Upgrade Required` *before* any session or pending asset row is created. The response names the supported range so the client can show an actionable message ("update Capsule to keep uploading"). Per [Threat Model](/design/threat-model/validation/#protocol-and-capability-negotiation), the same rule applies to every other write surface.
- This is a one-shot **compatibility gate**, not negotiation: there is no back-and-forth to settle on a shared version, and the protocol carries no capability flags. A client either speaks a version the server accepts, or it does not upload.
- The server supports a *window* of past protocol versions, not only the newest, so a staggered client rollout keeps working. A version leaves the window only after the deprecation period defined in [Threat Model — Min-Supported-Client Deprecation Policy](/design/threat-model/schema-rules/#min-supported-client-deprecation-policy); dropping one is a breaking change announced ahead of time.
- The date is bumped only for an **incompatible** wire change — offset semantics, alignment rules, the hard chunk bounds (raising the 16 MiB maximum is additive; lowering it or raising the 4 KiB minimum is breaking), finalization, the state machine. Purely additive, safely-ignorable changes do not bump it, and server-tunable parameters such as suggested chunk sizes and adaptive-sizing tiers are not protocol surface at all.

## Session State Machine

A session moves through a strict state machine:

```plaintext
Pending ─▶ Uploading ─▶ WaitingForProcessing ─▶ Completed
                                            └─▶ FailedProcessing
```

| From                   | To                     | On                                                        |
| ---------------------- | ---------------------- | --------------------------------------------------------- |
| `Pending`              | `Uploading`            | first **accepted** chunk (rejected chunks do not transition) |
| `Uploading`            | `WaitingForProcessing` | received bytes == declared `size`                          |
| `Pending`/`Uploading`  | *(discarded)*          | TTL expiry or pressure eviction (see below)                |
| `WaitingForProcessing` | `Completed`            | recomputed hash matches; pending row marked uploaded       |
| `WaitingForProcessing` | `FailedProcessing`     | hash mismatch, size mismatch, or envelope re-validation failure |

- **Pending** — session created, no bytes received.
- **Uploading** — at least one chunk accepted, transfer in progress. The transition is observable: a `HEAD` reflects it in `X-Capsule-Upload-Status`.
- **WaitingForProcessing** — all declared bytes received; finalization (hash verification) is running. Not evictable, not cancellable.
- **Completed** — hash verified, asset marked uploaded. Terminal.
- **FailedProcessing** — terminal failure. The upload file and the pending asset row are removed at the transition; the session *status record* is retained as a receipt. Terminal.

Session records live in the [session-state store](/design/filesystem/server/#deployment-profiles) with an uploader-keyed index for listing. This split is intentional: the session store holds only volatile transfer state, so the hot path — offset increments and status transitions — never touches the durable Postgres asset row. (In the default Postgres-only profile, sessions live in an `upload_sessions` table with an `expires_at` column and a periodic sweep; in the high-concurrency profile, they live in Valkey under keys `upload:session:{id}` with atomic `HINCRBY`/`HSET` and native TTL.) Postgres's durable asset record is written exactly twice per upload regardless of profile: once at session creation (the pending row) and once at finalization (mark uploaded).

## Session Lifetime and Discard

A session's lifetime is a **floor and a cap**, not a flat promise:

- **Survival floor (guaranteed).** A session in `Pending` or `Uploading` survives at least **1 hour after its last accepted chunk** (after creation, if none yet). Within the floor it is never evicted, under any pressure — a slow but live upload keeps its session as long as it keeps making progress.
- **Cap.** No session outlives **24 hours from creation**, progressing or not.
- **Discard window (server's right).** Between the floor and the cap, the server MAY discard sessions to reclaim space, least-recently-progressed first (ties broken toward the most on-disk bytes — reclaiming the most space from the most-stalled uploads first).
- **After discard: uniform `404`.** A discarded, expired, or never-existent session is indistinguishable — `HEAD`/`PATCH`/`DELETE` all return `404 error.upload.session_not_found`. No tombstones, no `410`: this matches the platform-wide indistinguishable-404 rule (invariant 26), and the client recovery is identical either way — re-create the session and resume from zero for that blob, retrying with backoff and halting after a bounded number of attempts.
- **Terminal receipts are exempt from pressure eviction** — their upload bytes are already gone (`Completed` renamed the file into the blob store; `FailedProcessing` deleted it), so evicting them reclaims nothing while destroying the lost-ACK receipt. They are removed only at the 24-hour cap.
- **Finalization is never interrupted.** A session in `WaitingForProcessing` is not evicted out from under the finalizer; it is driven to `Completed` or `FailedProcessing`.
- **Startup scrub.** On boot the server reconciles disk against the session store: an upload file with no session is deleted; a session whose file is shorter than its recorded received-byte count is moved to `FailedProcessing` (the file is authoritative — an ACK the disk cannot back must not stand). Discard, expiry, and cancellation all delete the upload file and the pending asset row.

The receipt behavior is why a client whose finalization ACK was lost re-queries (`HEAD`) and observes the terminal outcome — learning the upload already succeeded or failed — instead of seeing a vanished session and blindly re-uploading.

## Append-Only Storage

Each session owns exactly **one** file, `incoming/{upload_id}.bin`, and every accepted chunk is appended to it in order. There is no per-chunk file and no assembly step — sequential offsets are already a hard protocol rule, so append-only storage is correct by construction, and finalization has one less failure scenario (no reflink/concatenate stage that can half-complete).

- **Append with cross-check.** Before writing, the server compares the file's current length against the session's expected offset; a divergence is a `500 error.upload.storage_inconsistent` (server-side inconsistency, never the client's fault) and the write is refused. The file length is the on-disk truth; the session counter is its cache.
- **Durability before ACK.** A chunk is flushed to disk before its `204` is sent — an acknowledged byte is a byte on disk.
- **Offloaded blocking work.** Hashing runs on a blocking thread pool, never on the async reactor. (The write path uses plain aligned buffered writes today; `io_uring` is an optimization slot, not contract.)
- **Into the blob store.** Only after [finalization](#finalization-and-integrity) verifies the hash is the file renamed to its content address under `blobs/` — the atomic-rename discipline is owned by [Filesystem — Server](/design/filesystem/server/).
- **Backpressure.** `max_cache_size` bounds the total in-flight upload bytes held on disk; `max_file_size` bounds any single blob. The configuration asserts `max_file_size < max_cache_size` and warns if fewer than ~10 concurrent maximum-size uploads would fit. When the bound is hit, the [discard window](#session-lifetime-and-discard) is the relief valve.

## Finalization and Integrity

When received bytes reach the declared size, the server finalizes:

1. Session transitions to **WaitingForProcessing** via an **atomic compare-and-set** on the status — two racing finalizers cannot both proceed; the loser observes the transition and returns a conflict.
2. The server recomputes the [content hash](/design/cryptography/primitives/) over the upload file on the blocking pool and compares it to the declared `hash`.
3. The manifest envelope is re-validated (invariant 15) — the same checks as at session creation, inside the finalization transaction, so nothing that changed since creation (a revoked device, a closed album) slips through.
4. **On match** — the file is renamed to its content address, the pending asset is marked uploaded inside a Postgres transaction, and the session transitions to **Completed**.
5. **On mismatch** — the upload file and the pending asset row are deleted, the session transitions to **FailedProcessing**, and `error.upload.content_hash_mismatch` is returned. A mismatch is always treated as corruption or tampering and is never silently retried server-side.

The server verifies only the *ciphertext* hash — it has no other option. The client independently verifies the *plaintext* on download via the [STREAM construction](/design/cryptography/encryption/#stream-construction)'s per-chunk authentication tags, which detect truncation, reordering, and chunk deletion. The two checks are complementary: the server guarantees "the bytes I stored are the bytes you declared," and the AEAD guarantees "the plaintext I decrypted is authentic."

`Completed` is a one-time transfer receipt, not a standing durability guarantee a client can re-query later. After finalization, a client confirms an asset remains durably stored, indexed, and retrievable — the precondition for releasing its local copy — through the separate [storage-verification endpoint](/design/import/storage-verification/), which re-checks state that server-side GC, migration, or corruption could change after `Completed`.

## Idempotency and Resumption

- **Duplicate session creation** is keyed by `(owner_id, hash, album_id)` and split by state: if an **active** session for the tuple exists, `POST /upload` returns it (`200`, `Location` + current `X-Capsule-Offset`) — no second session is created; if the hash is already **finalized**, the request is rejected with `409 error.upload.duplicate_blob` carrying the existing asset reference, which is the client's trigger to resolve the situation as a [merge](#deduplication-and-merge) instead of a transfer. The dedup check and the pending-row insert run inside a single PostgreSQL transaction (`SELECT ... FOR UPDATE` + `INSERT ... ON CONFLICT`), closing the TOCTOU race at the database layer.
- **Chunk replay** is governed by the idempotency tuple in [Chunk Rules](#chunk-rules-and-strictness): same tuple → same response; same offset with different bytes → `409` corruption.
- **Resumption** is `HEAD`-driven: the response carries the authoritative offset, the declared length, and the session status. An upload is not expected to run to completion in a single connection; the server tolerates arbitrarily long pauses within the [session lifetime](#session-lifetime-and-discard), and [auto syncing](/design/import/download-sync/#auto-syncing) explicitly assumes interrupted transfers are normal.
- **Lost finalization ACK**: re-query `HEAD` and read the terminal receipt (see [Session Lifetime](#session-lifetime-and-discard)).
- Every critical step — session creation, each chunk, finalization — is logged with the upload ID so an interrupted or failed upload can be reconstructed after the fact.
- [Streaming import-upload mode](/design/import/pipeline/#import-upload-streaming-mode) for storage-constrained devices uses these same sessions unchanged: it creates, uploads, and finalizes them one bounded window at a time and releases each local original after a durable [storage-verification](/design/import/storage-verification/) check. The wire protocol is identical — only the pipeline's drive pattern differs — and a server connection loss simply leaves the in-flight sessions resumable via `HEAD`.

## Adaptive Chunk Sizing

Chunk-size selection is split cleanly between the two sides:

- **The server enforces, by rejection only, the protocol bounds**: 4 KiB alignment, the `[4 KiB, 16 MiB]` chunk range, and the cumulative ceilings. It never adapts, negotiates, or corrects.
- **`capsule-sdk` owns adaptation.** The `X-Capsule-Suggested-Chunk-Size` returned at session creation is a *starting point only*, from non-normative file-size tiers (currently `< 10 MB` → 256 KiB, `< 100 MB` → 1 MiB, `≥ 100 MB` → 4 MiB — server-tunable, not protocol surface).

The client algorithm is normative for `capsule-sdk` (another client may adapt differently, within the protocol bounds):

- Measure throughput over a sliding **30-second window**.
- **Warm-up:** no adjustment until ≥ 5 chunks *or* ≥ 8 MiB have been sent at the current size — decisions on a cold window oscillate.
- **Double** the chunk size when sustained throughput exceeds 5 MB/s; **halve** it below 1 MB/s.
- Clamp to the tier's range; every tier range sits inside the protocol bounds `[256 KiB, 16 MiB]` for non-final chunks.
- Every candidate size is a 4 KiB multiple **by construction** (the candidate set is doublings/halvings of 4 KiB-aligned tier bounds), so alignment never depends on a runtime check.

The rationale is a direct trade-off — chunks that are too small waste round-trips, while chunks that are too large waste re-transmission on a flaky link and pin more memory per in-flight request. Under an [adverse connection](/design/import/download-sync/), the conservative choice is the tier minimum; the client must never let adaptation regress effective throughput.

We deliberately do **not** expose per-blob upload *ordering* as a protocol concern. Concurrent sessions plus the OS and TCP stack settle ordering naturally; see [Pipeline — Upload Prioritization](/design/import/pipeline/#upload-prioritization) for the client-side heuristics that decide which assets to *start*.

## Error Taxonomy

Every rejection carries a machine-readable `error.*` code (the [i18n error-code contract](/design/i18n/#server-error-codes)); clients switch on codes, never on bare HTTP statuses. The full upload domain:

| Condition                                             | HTTP  | Code                                       | Invariant |
| ----------------------------------------------------- | ----- | ------------------------------------------ | --------- |
| Protocol version outside window                       | `426` | `error.protocol.version_unsupported`       | 1         |
| Unknown crypto suite                                  | `400` | `error.upload.unknown_crypto_suite`        | 2         |
| Hash length ≠ suite digest length / not hex           | `400` | `error.upload.invalid_hash`                | 3         |
| `size` ∉ `(0, max_file_size]`                         | `400`/`413` | `error.upload.invalid_size` / `error.upload.file_too_large` | 4 |
| `content_type` outside closed enum                    | `400` | `error.upload.unsupported_content_type`    | 5         |
| Album missing / no write capability / version drift   | `403` | `error.upload.album_access_denied`         | 6         |
| `created_by_device` not in device directory           | `403` | `error.upload.device_not_authorized`       | 7         |
| Envelope timestamp outside sanity window              | `400` | `error.upload.timestamp_out_of_range`      | 8         |
| Top-level fields contradict `manifest_envelope`       | `400` | `error.upload.envelope_mismatch`           | 15 family |
| Malformed body / unknown JSON fields                  | `400` | `error.upload.malformed_request`           | —         |
| On-behalf upload without verified relationship        | `403` | `error.upload.owner_not_permitted`         | —         |
| Quota would be exceeded by declared `size`            | `403` | `error.quota.exceeded`                     | —         |
| Hash already finalized (dup create)                   | `409` | `error.upload.duplicate_blob`              | —         |
| Wrong/missing `Content-Type` on `PATCH`               | `415` | `error.upload.unsupported_media_type`      | 10        |
| Empty chunk                                           | `400` | `error.upload.empty_chunk`                 | 10        |
| Unaligned non-final chunk                             | `400` | `error.upload.chunk_not_aligned`           | 10        |
| Chunk > 16 MiB                                        | `413` | `error.upload.chunk_too_large`             | 10        |
| Missing/invalid `X-Capsule-Offset` header             | `400` | `error.upload.missing_offset`              | 9         |
| Missing/malformed `X-Capsule-Checksum`                | `400` | `error.upload.missing_checksum`            | 12        |
| Checksum ≠ received bytes                             | `400` | `error.upload.checksum_mismatch`           | 12        |
| Offset ≠ current received count                       | `409` | `error.upload.offset_mismatch`             | 9         |
| Same offset, different chunk hash                     | `409` | `error.upload.chunk_conflict`              | 12        |
| Cumulative bytes exceed declared `size`               | `400` | `error.upload.size_exceeded`               | 11        |
| Unknown / expired / discarded session                 | `404` | `error.upload.session_not_found`           | —         |
| Session already terminal or finalizing                | `409` | `error.upload.session_not_active`          | —         |
| Concurrent finalization lost the CAS                  | `409` | `error.upload.finalize_in_progress`        | —         |
| Recomputed hash ≠ declared (finalization)             | `400` | `error.upload.content_hash_mismatch`       | 14        |
| Envelope re-validation failed (finalization)          | `400` | `error.upload.envelope_rejected`           | 15        |
| Caller is neither uploader nor owner                  | `403` | `error.upload.forbidden`                   | —         |
| Upload-file / session-counter divergence               | `500` | `error.upload.storage_inconsistent`        | —         |

## Edge Cases

The stateful surface, enumerated — each of these is a test in the Validation section:

- **`PATCH` after finalization began or completed** → `409 error.upload.session_not_active`; the terminal outcome is read via `HEAD`.
- **`DELETE` during `WaitingForProcessing`** → `409` — finalization is not interruptible; cancel before completion or observe the receipt after.
- **Duplicate `POST /upload`** → active session returned / finalized hash `409 duplicate_blob` (see [Idempotency and Resumption](#idempotency-and-resumption)).
- **Offset beyond current EOF** (client skipped ahead) → `409 offset_mismatch` with the authoritative offset — indistinguishable from any other stale offset, deliberately.
- **Offset correct but `offset + len > size`** → `400 size_exceeded`, session → `FailedProcessing` (the declaration was violated; the session is unsalvageable by definition).
- **Checksum mismatch mid-stream** → `400`, nothing persisted, offset unchanged; the client re-sends the same chunk.
- **`PATCH` against an expired/discarded session** → uniform `404`; the client re-creates and re-uploads that blob.
- **Zero-length blob** — impossible by construction: invariant 4 requires `size > 0` at creation, so no chunk sequence can validly be empty.

## Deduplication and Merge

Because blobs are addressed by their [ciphertext content hash](/design/cryptography/primitives/), the protocol avoids redundant transfers:

- At session creation, the server checks for an asset with the same content hash already owned by the user; the split behavior (active session returned; finalized hash → `409 duplicate_blob` + existing asset reference) is defined in [Idempotency and Resumption](#idempotency-and-resumption). Nothing is ever re-uploaded.
- The [import pipeline](/design/import/pipeline/#plan--confirm) treats already-uploaded *local* assets as non-importable. But because encryption and hashing are deferred until upload, an asset may already exist remotely under a *different* ciphertext (for example, re-encrypted under a newer album key). Import still admits such an asset, and the upload then resolves to a **merge**: the server links the existing stored blob to the new asset and album reference rather than storing a second copy. The original blob's upload short-circuits, and only the new metadata blob is transferred.
- **Merge is strictly additive on the server.** A merge **never** deletes an existing blob or rewrites an existing manifest — it only adds a new reference. The blob's reference count goes up, never down, on merge. Reference removal happens only through an explicit `delete` lifecycle action signed by a current writer (see [Authorization](/design/authorization/)), and the underlying blob is hard-purged only after every reference is provably gone.

These checks deduplicate at upload time. Byte-identical assets that still slip into a client library — for example through overlapping folder imports or a restore over an existing library — are collapsed separately by client-side [intra-library deduplication](/design/filesystem/maintenance/#deduplication).

## Quota and Permissions

- An upload is attributed to `upload_user_id` (the authenticated uploader) for storage-quota accounting, which is distinct from `owner_id` (the asset's owner). Uploading on behalf of a different owner requires a verified relationship and is permission-checked at session creation. The quota accounting model is owned by [Quota](/design/quota/).
- Adding an asset to an album requires write-tier album access (`AMK_write`; see [Cryptography — Keys](/design/cryptography/keys/#album-master-keys-amks)); the server validates album write permission before creating the session.
- For an ordinary asset bundle the client resolves a concrete container `album_id` — the user's choice or the [default album](/design/organization/#the-default-album) — **before** encryption, since the bytes are encrypted under that album's AMK. So `album_id` is effectively always present for asset uploads; the `optional` marking on `POST /upload` covers only non-asset/owner-scoped kinds and the `intent_id`-bearing [album upgrade](/design/versioning/#album-upgrade-ceremony). This is why [invariant 6](/design/threat-model/validation/#server-side-validation-invariants) can require `album_id` to exist and be writable.
- Only the uploader may append chunks. The uploader or the owner may query (`HEAD`) or cancel (`DELETE`) a session; anyone else receives `403`. Session listing is uploader-scoped — the uploader is the party that resumes.

## Validation

The wire protocol is the boundary across two modules, so both sides have rich isolated test surfaces:

- **Server protocol conformance (smoke).** Exercise the full state machine against the real server against a testcontainer Postgres (and Valkey for the high-concurrency profile): create session → PATCH chunks → finalize → verify Completed. Mock the client at the HTTP layer using a generated request fixture set.
- **Server strictness rejection (unit).** Every row of the [strictness table](#chunk-rules-and-strictness) and every row of the [error taxonomy](#error-taxonomy) has a unit test asserting the exact HTTP status **and** `error.*` code.
- **Server idempotency (unit).** Replay each idempotent endpoint with identical input; assert byte-identical response. Duplicate create returns the active session; finalized-hash create returns `duplicate_blob`.
- **Server lifetime (unit).** A session with an accepted chunk in the last hour is never evicted under injected pressure; a stalled session past the floor is; terminal receipts survive pressure and die at the cap.
- **Server append-only crash safety (smoke).** Kill the server between the append and the counter increment; on restart the scrub reconciles (file length is truth) and `HEAD` reports the on-disk offset. Kill between rename and commit; assert the atomicity invariants recover.
- **Server finalization integrity (smoke).** Upload; recompute hash; assert match. Inject a corrupted chunk stream; assert FailedProcessing and full cleanup of the pending row + upload file.
- **Client protocol conformance (smoke).** The client `capsule-sdk::upload` runs against a mocked HTTP layer that replays every taxonomy code; assert the client's recovery matrix (re-align on `offset_mismatch`, re-create on `session_not_found`, merge on `duplicate_blob`, abort-with-upgrade on `426`, re-send on `checksum_mismatch`).
- **Client resume semantics (smoke).** Start an upload, interrupt at random offset, resume; assert no bytes re-sent that the server already has.

The cross-module case — real client → real server full upload — is bounded E2E surface listed in [Module Map](/design/module-map/#e2e-test-surface). Because both sides have rich smoke coverage, the E2E case can be a single happy-path round-trip rather than the full rejection matrix.
