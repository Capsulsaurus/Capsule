---
title: Upload Protocol
description: The wire protocol between Capsule clients and the server for resumable, content-addressed uploads
---

The upload protocol is a custom resumable-upload protocol modeled on [TUS](https://tus.io/) but trimmed to Capsule's needs: no per-request capability negotiation, no metadata smuggled in headers, ciphertext-only payloads. Compatibility is gated once, up front, via the universal [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation).

This protocol is the most fragile contract between client and server: a client that misunderstands chunk alignment, offset semantics, or finalization can silently corrupt or orphan data. The endpoint table, the chunk rules, the session state machine, and the finalization steps below **are the contract** — every implementation MUST conform exactly. The client implementation lives in `capsule-sdk::upload`; the server in `capsule-api-upload`. The two are tested independently against the protocol surface.

Every upload is **idempotent** but stateful. Uploads can complete partially and are identified by an *upload ID*.

## What Gets Uploaded

An asset is never uploaded as a single plaintext file. Because Capsule is end-to-end encrypted, the client **encrypts and signs** everything *before* transmission, and the server only ever stores opaque, content-addressed ciphertext blobs. A single imported asset produces a **bundle** of blobs:

- The **original blob** — the source asset, encrypted under the [bulk AEAD](/design/cryptography/primitives/#bulk-aead) with the [STREAM construction](/design/cryptography/encryption/#stream-construction).
- **Derivative blobs** — thumbnails and previews, generated client-side during import (see [Thumbnails](/design/thumbnails/)), each encrypted independently.
- The **metadata blob** — the CBOR metadata document (capture date, dimensions, EXIF-derived fields, the [LQIP](/design/thumbnails/#lqip), provenance), encrypted under the [bulk AEAD](/design/cryptography/primitives/#bulk-aead) (see [Metadata](/design/metadata/)).

Each blob is its own upload with its own upload ID; the protocol does not couple them and imposes **no wire ordering**. The client may transfer the bundle in any order — decoupling lets small derivatives land while a large original is still in flight — but the server **gates visibility** on the pending-asset row: the asset becomes visible to other devices only once its **required members, the original and the metadata blob, are both finalized**. This is enforced without reading plaintext — each blob's role is recorded on its pending row at session creation, and the visibility flip simply checks that the original and metadata roles are present and `uploaded`. Every blob in the bundle — original, derivatives, metadata, provenance — counts toward the uploader's storage [quota](/design/quota/#accounting-model).

"Blob" is defined once, in [Filesystem — Server: Uniform, Opaque Blobs](/design/filesystem/server/#uniform-opaque-blobs); this protocol is its transport, not its definition. Every asset and derivative blob carries a signed [manifest envelope](/design/cryptography/provenance/#asset-manifest): at `POST /upload` the server validates the envelope's `created_by_device` against the uploader's [device directory](/design/cryptography/keys/#device-directory) (invariant 7), and the client verifies the full write-tier signature on download via [`verify_asset`](/design/cryptography/keys/#write-authorization). **Backup artifacts are the one exception** — they carry no per-asset provenance of their own (the exporting device is not the original author); their integrity rides the library-level backup MANIFEST instead (see [Backup and Recovery](/design/backup-recovery/)).

The server performs no decoding, no metadata extraction, and no thumbnail generation — it cannot, since it never holds a decryption key. All such work happens client-side during [import](/design/import/pipeline/).

## Design Invariants

The upload protocol guarantees the following, and every endpoint upholds them:

- **Content-addressed.** Every blob is identified by its [ciphertext content hash](/design/cryptography/primitives/). The plaintext hash is never transmitted to the server.
- **Idempotent.** Re-creating a session for a blob already stored is a no-op that resolves to the existing asset. Re-sending a chunk at an already-acknowledged offset is accepted and simply returns the current offset.
- **Resumable.** A session survives connection loss for the lifetime of its TTL. A client resumes by querying the authoritative offset and continuing from there — no bytes are re-sent unnecessarily.
- **Strictly bounded.** The total ciphertext size is declared at session creation and immutable thereafter. The cumulative received bytes may never exceed it, nor exceed the server's per-file limit.
- **Verified.** No upload is marked complete until the server has recomputed the ciphertext hash and confirmed it matches the declared value.
- **Recoverable.** Every session is either driven to a terminal state or garbage-collected. There are no permanently orphaned chunks or pending asset rows.

## Endpoints

All endpoints are authenticated with a bearer JWT.

| Method   | Path               | Purpose                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| -------- | ------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `POST`   | `/upload`          | Create a session. Body declares ciphertext `size`, `hash` (the [content hash](/design/cryptography/primitives/) digest bytes; algorithm fixed by `crypto_suite_id`), `content_type` (closed enum), `crypto_suite_id`, `protocol_version`, `manifest_envelope` (the unencrypted manifest fields the server validates per [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants)), optional `album_id`, optional `owner_id`, optional `intent_id` (required only during an [album upgrade](/design/versioning/#album-upgrade-ceremony)). Returns `201` with `Location: /upload/{id}` and `X-Capsule-Suggested-Chunk-Size`. Rejects with `400` / `403` / `426` per the validation invariants. |
| `HEAD`   | `/upload/{id}`     | Query progress. Returns `X-Capsule-Offset` (next expected byte), `X-Capsule-Content-Length`, and session status. This is the resumption primitive.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `PATCH`  | `/upload/{id}`     | Append a chunk at `X-Capsule-Offset`, with an optional per-chunk `X-Capsule-Checksum`. Returns `204` and the new offset.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `DELETE` | `/upload/{id}`     | Cancel the session — removes chunks, the session record, and the pending asset row.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `GET`    | `/upload/sessions` | List the caller's active sessions, so a client can resume across app restarts or devices.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |

Creating a session writes a *pending* asset row to Postgres (`uploaded = false`) and a session record to the configured **session-state store** (see [Filesystem — Server: Deployment Profiles](/design/filesystem/server/#deployment-profiles): Postgres by default, Valkey in the high-concurrency profile). The pending row reserves the asset ID that derivative and metadata blobs reference.

## Chunk Rules

Enforced strictly; a violation fails the request rather than being silently corrected:

- Every chunk except the final one MUST be a multiple of 4 KiB (4096 bytes). This keeps server-side writes block-aligned, which is what makes the [reflink assembly path](#server-side-storage-and-assembly) work. A non-aligned, non-final chunk is rejected with `400`.
- Offsets are strictly sequential. A `PATCH` must arrive at exactly the current received-byte count; an out-of-order or gapped write is rejected with `409`, and the client recovers by issuing `HEAD` to learn the authoritative offset.
- **Idempotency tuple.** The server keys each accepted PATCH by `(upload_id, offset, chunk_hash)` where `chunk_hash` is the SHA-256 of the chunk bytes (carried in the `X-Capsule-Checksum` header). A duplicate PATCH with the same tuple returns the same response — a re-send after a lost ACK is a no-op. A PATCH at an already-acknowledged offset *with a different `chunk_hash`* is rejected with `409` + a corruption error: this is the structural defense against a faulty client that retries with garbage. The complete idempotency contract is owned by [Threat Model — Idempotency Invariants](/design/threat-model/validation/#idempotency-invariants).
- Cumulative size may never exceed the declared `size` nor the server's `max_file_size`. The server checks the cumulative count **at every chunk arrival**, not only at finalization — a buggy client that streams past the declared size is cut off before more bytes are persisted. Either ceiling is rejected (`400` / `413`) and the session is moved to a failed state.
- The upload completes exactly when received bytes equal the declared size; finalization then runs automatically.

## Protocol Versioning

The upload session is gated by Capsule's universal [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation), so a client never begins a transfer against a server it is not known to be compatible with. This section names the upload-specific specializations.

Versioning is **date-based** (`YYYY-MM-DD` — the day a protocol revision is frozen), not integer or semver. An integer version conveys nothing about ordering granularity and invites a bump for every change; semver implies a minor/patch backward-compatibility contract finer than we are willing to maintain on a hot path. A date is unambiguously ordered, human-readable, and maps directly onto a release.

- Every client sends `X-Capsule-Protocol: <date>` on every request (the upload-specific alias `X-Capsule-Upload-Protocol` remains accepted but is deprecated). The server advertises the inclusive range it accepts via `X-Capsule-Protocol-Min` and `-Max` on every response, errors included.
- A `POST /upload` whose version falls outside the accepted range is rejected with `426 Upgrade Required` *before* any session or pending asset row is created. The response names the supported range so the client can show an actionable message ("update Capsule to keep uploading"). Per [Threat Model](/design/threat-model/validation/#protocol-and-capability-negotiation), the same rule applies to every other write surface.
- This is a one-shot **compatibility gate**, not negotiation: there is no back-and-forth to settle on a shared version, and the protocol carries no capability flags. A client either speaks a version the server accepts, or it does not upload.
- The server supports a *window* of past protocol versions, not only the newest, so a staggered client rollout keeps working. A version leaves the window only after the deprecation period defined in [Threat Model — Min-Supported-Client Deprecation Policy](/design/threat-model/schema-rules/#min-supported-client-deprecation-policy); dropping one is a breaking change announced ahead of time.
- The date is bumped only for an **incompatible** wire change — offset semantics, alignment rules, finalization, the state machine. Purely additive, safely-ignorable changes do not bump it, and server-tunable parameters such as suggested chunk sizes and adaptive-sizing tiers are not protocol surface at all.

## Session Lifecycle

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

Session records live in the [session-state store](/design/filesystem/server/#deployment-profiles) with a 24-hour TTL and a per-owner index for listing. This split is intentional: the session store holds only volatile transfer state, so the hot path — offset increments and status transitions — never touches the durable Postgres asset row. (In the default Postgres-only profile, sessions live in an `upload_sessions` table with an `expires_at` column and a periodic sweep; in the high-concurrency profile, they live in Valkey under keys `upload:session:{id}` with atomic `HINCRBY`/`HSET` and native TTL.) Postgres's durable asset record is written exactly twice per upload regardless of profile: once at session creation (the pending row) and once at finalization (mark uploaded). A session that reaches its TTL before completing is garbage-collected — chunks deleted, pending asset row removed — and the client treats an expired session as gone and re-imports, retrying with backoff and halting after a bounded number of attempts.

**TTL applies only to in-progress transfer.** A session is eligible for TTL eviction only while in `Pending` or `Uploading`. Once it reaches `WaitingForProcessing`, finalization is running and the session is **not** evicted out from under it — finalization either drives it to `Completed` or fails it cleanly to `FailedProcessing`. Both terminal states are **retained for the remainder of the TTL** rather than deleted on transition, so a client whose finalization ACK was lost re-queries (`HEAD`) and observes the terminal outcome — learning the upload already succeeded or failed — instead of seeing a vanished session and blindly re-uploading. (The `FailedProcessing` cleanup of chunks and the pending row happens at the transition; only the session *status record* is what lingers for the TTL.)

## Server-Side Storage and Assembly

Each chunk is written to disk as `{upload_id}_{n}.part`; the assembled blob is `{upload_id}.bin`. Because this is a hot path, the storage layer is aggressively optimized:

- **Streaming writes.** Chunk bytes are streamed from the request body straight to disk; large transfers must never accumulate in hot memory. On Linux, the write path uses `io_uring`.
- **Reflink assembly.** Finalization concatenates chunks into the final blob with a copy-on-write reflink wherever the filesystem supports one — `FICLONERANGE` on Linux (Btrfs, XFS), `clonefile` on macOS (APFS), the equivalent on ReFS. The 4 KiB chunk alignment is precisely what allows each chunk to be reflinked at its destination offset; only the final (possibly unaligned) chunk needs a plain copy. Reflink turns assembly into a near-instant metadata operation instead of an O(file size) copy. On filesystems without reflink support, the code falls back to a sequential copy.
- **Offloaded blocking work.** Chunk assembly and hashing run on a blocking thread pool, never on the async reactor.
- **Backpressure.** `max_cache_size` bounds the total in-flight upload bytes held on disk; `max_file_size` bounds any single blob. The configuration asserts `max_file_size < max_cache_size` and warns if fewer than ~10 concurrent maximum-size uploads would fit. The distinct task pools — network I/O, file I/O, and hashing — are sized and load-tested independently against realistic hardware limits.

## Finalization and Integrity

When received bytes reach the declared size, the server finalizes:

1. Session transitions to **WaitingForProcessing**.
2. Chunks are assembled into the final blob.
3. The server recomputes the [content hash](/design/cryptography/primitives/) over the assembled ciphertext on the blocking pool and compares it to the declared `hash`.
4. **On match** — the pending asset is marked uploaded inside a Postgres transaction and the session transitions to **Completed**.
5. **On mismatch** — the blob and the pending asset row are deleted, the session transitions to **FailedProcessing**, and a checksum-mismatch error is returned. A mismatch is always treated as corruption or tampering and is never silently retried server-side.

The server verifies only the *ciphertext* hash — it has no other option. The client independently verifies the *plaintext* on download via the [STREAM construction](/design/cryptography/encryption/#stream-construction)'s per-chunk authentication tags, which detect truncation, reordering, and chunk deletion. The two checks are complementary: the server guarantees "the bytes I stored are the bytes you declared," and the AEAD guarantees "the plaintext I decrypted is authentic."

`Completed` is a one-time transfer receipt, not a standing durability guarantee a client can re-query later. After finalization, a client confirms an asset remains durably stored, indexed, and retrievable — the precondition for releasing its local copy — through the separate [storage-verification endpoint](/design/import/storage-verification/), which re-checks state that server-side GC, migration, or corruption could change after `Completed`.

## Robustness

- An upload is not expected to run to completion in a single connection. The server tolerates arbitrarily long pauses within the session TTL, and clients resume via `HEAD`. [Auto syncing](/design/import/download-sync/#auto-syncing) explicitly assumes interrupted transfers are normal.
- A chunk re-sent at an already-acknowledged offset is idempotent. A chunk at a stale offset receives `409` together with the authoritative offset so the client can re-align.
- Concurrent finalization attempts on a single session are guarded — a second attempt observes a non-`Pending`/`Uploading` status and returns a conflict rather than double-processing.
- Every critical step — session creation, each chunk, assembly, hash verification, finalization — is logged with the upload ID so an interrupted or failed upload can be reconstructed and recovered after the fact.
- [Streaming import-upload mode](/design/import/pipeline/#import-upload-streaming-mode) for storage-constrained devices uses these same sessions unchanged: it creates, uploads, and finalizes them one bounded window at a time and releases each local original after a durable [storage-verification](/design/import/storage-verification/) check. The wire protocol is identical — only the pipeline's drive pattern differs — and a server connection loss simply leaves the in-flight sessions resumable via `HEAD`.

## Adaptive Chunk Sizing

The server suggests an initial chunk size by file-size tier — `< 10 MB` → 256 KiB, `< 100 MB` → 1 MiB, `≥ 100 MB` → 4 MiB. The client may then adapt *within a tier-bounded range* based on throughput measured over a sliding 30-second window: doubling the chunk size when sustained throughput is high (`> 5 MB/s`), halving it when low (`< 1 MB/s`), and always staying 4 KiB-aligned. The rationale is a direct trade-off — chunks that are too small waste round-trips, while chunks that are too large waste re-transmission on a flaky link and pin more memory per in-flight request.

Adaptation is purely a client concern; the server only enforces alignment and bounds. The client must never let adaptation regress effective throughput — if a tier's range is mis-tuned, the conservative choice is the tier minimum.

We deliberately do **not** expose per-blob upload *ordering* as a protocol concern. Concurrent sessions plus the OS and TCP stack settle ordering naturally; see [Pipeline — Upload Prioritization](/design/import/pipeline/#upload-prioritization) for the client-side heuristics that decide which assets to *start*.

## Deduplication and Merge

Because blobs are addressed by their [ciphertext content hash](/design/cryptography/primitives/), the protocol avoids redundant transfers:

- At session creation, the server checks for an asset with the same content hash already owned by the user. An exact duplicate that exists both locally and remotely is rejected up front — nothing is re-uploaded. The dedup check and the pending-row insert run inside a single PostgreSQL transaction (a `SELECT ... FOR UPDATE` followed by `INSERT ... ON CONFLICT`), so two concurrent uploaders cannot both observe "no existing row" and each insert their own — the TOCTOU race is closed at the database layer.
- The [import pipeline](/design/import/pipeline/#plan--confirm) treats already-uploaded *local* assets as non-importable. But because encryption and hashing are deferred until upload, an asset may already exist remotely under a *different* ciphertext (for example, re-encrypted under a newer album key). Import still admits such an asset, and the upload then resolves to a **merge**: the server links the existing stored blob to the new asset and album reference rather than storing a second copy. The original blob's upload short-circuits, and only the new metadata blob is transferred.
- **Merge is strictly additive on the server.** A merge **never** deletes an existing blob or rewrites an existing manifest — it only adds a new reference. The blob's reference count goes up, never down, on merge. Reference removal happens only through an explicit `delete` lifecycle action signed by a current writer (see [Authorization](/design/authorization/)), and the underlying blob is hard-purged only after every reference is provably gone.

These checks deduplicate at upload time. Byte-identical assets that still slip into a client library — for example through overlapping folder imports or a restore over an existing library — are collapsed separately by client-side [intra-library deduplication](/design/filesystem/maintenance/#deduplication).

## Quota and Permissions

- An upload is attributed to `upload_user_id` (the authenticated uploader) for storage-quota accounting, which is distinct from `owner_id` (the asset's owner). Uploading on behalf of a different owner requires a verified relationship and is permission-checked at session creation. The quota accounting model is owned by [Quota](/design/quota/).
- Adding an asset to an album requires write-tier album access (`AMK_write`; see [Cryptography — Keys](/design/cryptography/keys/#album-master-keys-amks)); the server validates album write permission before creating the session.
- For an ordinary asset bundle the client resolves a concrete container `album_id` — the user's choice or the [default album](/design/organization/#the-default-album) — **before** encryption, since the bytes are encrypted under that album's AMK. So `album_id` is effectively always present for asset uploads; the `optional` marking on `POST /upload` covers only non-asset/owner-scoped kinds and the `intent_id`-bearing [album upgrade](/design/versioning/#album-upgrade-ceremony). This is why [invariant 6](/design/threat-model/validation/#server-side-validation-invariants) can require `album_id` to exist and be writable.
- Only the uploader may append chunks. The uploader or the owner may query (`HEAD`) or cancel (`DELETE`) a session.

## Validation

The wire protocol is the boundary across two modules, so both sides have rich isolated test surfaces:

- **Server protocol conformance (smoke).** Exercise the full state machine against the real server against a testcontainer Postgres (and Valkey for the high-concurrency profile): create session → PATCH chunks → finalize → verify Completed. Mock the client at the HTTP layer using a generated request fixture set.
- **Server chunk-rule rejection (unit).** Each rule (non-aligned non-final chunk, gapped offset, duplicate offset with different hash, cumulative-over-size, oversize file) has a unit test asserting the exact rejection code.
- **Server idempotency (unit).** Replay each idempotent endpoint with identical input; assert byte-identical response.
- **Server finalization integrity (smoke).** Concatenate chunks; recompute hash; assert match. Inject a corrupted chunk; assert FailedProcessing and full cleanup of the pending row + chunks.
- **Client protocol conformance (smoke).** The client `capsule-sdk::upload` runs against a mocked HTTP layer that replays the rejection codes the server's unit tests exercise; assert the client handles each correctly (re-align on 409, abort-and-reimport on 426, etc.).
- **Client resume semantics (smoke).** Start an upload, interrupt at random offset, resume; assert no bytes re-sent that the server already has.

The cross-module case — real client → real server full upload — is bounded E2E surface listed in [Module Map](/design/module-map/#e2e-test-surface). Because both sides have rich smoke coverage, the E2E case can be a single happy-path round-trip rather than the full rejection matrix.
