---
title: Quota
description: Storage quota accounting, thresholds, and enforcement points
---

Storage quota in Capsule is accounted to `upload_user_id` (the authenticated uploader), which is distinct from `owner_id` (the asset's owner). This separation lets a user upload on behalf of a different owner (with verified permission) while keeping storage cost attributed correctly. The accounting model is enforced at the [server filesystem](/design/filesystem/server/#ownership-partitioning-and-quota) and at [upload session creation](/design/import/upload-protocol/#quota-and-permissions); this doc owns the threshold model and what happens when limits are hit.

Implementation will live in `capsule-api-service::quota`. Accounting reads from the Postgres asset index (size sums per `upload_user_id`); enforcement runs at session creation, before any chunks are accepted.

## Accounting Model

```text
quota_used(user) = SUM(ciphertext_size) for all blobs where upload_user_id = user
                 + SUM(metadata_blob_size)
                 + SUM(derivative_blob_size for derivatives the user generated)
```

Notable:

- **Content-addressed dedup is global.** A blob shared between two uploaders counts against *only the first uploader* — the second is a merge (see [Upload Protocol — Deduplication and Merge](/design/import/upload-protocol/#deduplication-and-merge)). This is what stops a malicious user from racking up another user's quota by re-uploading their public assets.
- **Derivatives count.** Thumbnails and previews are real storage, attributed to whichever device generated them.
- **Provenance blobs count.** Each per-asset `.provenance.cbor` (server-side encrypted blob) is small but accumulates.
- **Federated-received blobs count against the receiver.** When a user's home server caches a blob pulled from a [federated](/design/federation/) peer on that user's behalf, the cached bytes count against the **receiving** user's quota, deduped by content hash so a blob the server already holds is never counted twice. A per-`(receiving_user, source_peer)` caching budget (deployment-configurable; default 25% of the receiver's hard quota per source peer) bounds how much one user can pull from any single peer, so a user receiving from many peers cannot push the home server's storage past their own quota. This is the storage-side counterpart of [Federation's per-peer compartmentalization](/design/federation/#per-peer-compartmentalization) and is the resolution of the federated-receive DoS.
- **Trash-retained assets count fully.** An asset in trash still occupies storage until its [retention window](/design/organization/#retention-window) expires and it is hard-purged, so it counts against quota at full size. This is deliberate: it keeps accounting honest and gives users a concrete reason to empty trash rather than treating it as free overflow.
- **Derivatives are reclaimed on hard-purge.** When an asset is hard-purged, its derivative and metadata blob references drop alongside the original's; any blob whose reference count reaches zero is [garbage-collected](/design/filesystem/server/#deletion-and-garbage-collection) and the freed bytes are credited back to whichever user they were attributed to. A purged asset never leaves orphaned derivatives silently inflating a quota.

## Thresholds and States

A user account exists in one of these quota states:

| State             | Threshold                                                         | Behavior                                                                                                                                                             |
| ----------------- | ----------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **OK**            | quota_used < soft_limit                                           | All uploads succeed normally.                                                                                                                                        |
| **Soft warning**  | soft_limit ≤ quota_used < hard_limit                              | Uploads succeed, but the UI surfaces a warning.                                                                                                                      |
| **Hard exceeded** | quota_used ≥ hard_limit                                           | New uploads rejected at session creation with a structured error. Existing assets remain accessible.                                                                 |
| **Grace expired** | quota_used ≥ hard_limit for > `grace_window` (default 14 days)    | Read-only mode: reads, deletes, and restore-from-trash still work; only new uploads and metadata-growth writes are refused. Freeing space (emptying trash) lifts it. |
| **Suspended**     | (admin or billing action — see [Moderation](/design/moderation/)) | Server-defined; possibly upload refusal, possibly full lockout.                                                                                                      |

Defaults for `soft_limit`, `hard_limit`, and `grace_window` are deployment-configurable. Self-hosted servers might run with no quota (`hard_limit = ∞`); hosted services set per-tier limits.

## Enforcement Points

Where the quota check actually runs:

- **At [`POST /upload`](/design/import/upload-protocol/#endpoints) session creation.** The server computes `quota_used(upload_user_id) + declared_size` and rejects with `403 Quota Exceeded` (or similar structural code) if it crosses the hard limit. This is the *only* hard enforcement point — once a session is open, the declared size is the cap, and the session is allowed to complete.
- **At session cancellation.** When a session is cancelled or expires, the reserved-but-uncommitted bytes are released; the next quota check sees the new (lower) usage.
- **At [finalization](/design/import/upload-protocol/#finalization-and-integrity).** Cumulative size is bounded by the declared size at chunk acceptance; no separate quota check at finalization is needed because the declared size was already approved at session creation.
- **At metadata-update writes.** A metadata-update creates a new encrypted metadata blob; the size delta is checked against quota. Tiny but non-zero.

## Scope Decisions

- **Sponsored-account attribution.** A sponsoree's uploads count against the **sponsor's** quota — the sponsoree's `upload_user_id` derives from the sponsor ([Keys — Delegated/Sponsored](/design/cryptography/keys/#delegatedsponsored-accounts)), so storage rolls up to the sponsoring (billing) account. There is no separate sponsoree quota.
- **Web-upload drops.** A pending [web-upload drop](/design/web-upload/) counts against the **provisioning user's** quota — the link owner is the `upload_user_id`, charged at [drop-session creation](/design/web-upload/#drop-and-adoption-lifecycle) like any other session, so a leaked link cannot push storage past the owner's hard limit. [Adoption in place](/design/web-upload/#why-adopt-in-place) only reclassifies the already-stored blob from inbox to album asset, so it incurs no new quota; a discarded or expired drop frees its bytes on the next GC. There is no separate inbox quota.
- **Per-album quotas.** Out of scope for v1 — quota is per `upload_user_id` only. A deployment that later wants per-album caps adds them as a second, independent check at the same enforcement point; the accounting model above does not change.
- **Streaming import.** A storage-constrained [streaming import](/design/import/pipeline/#import-upload-streaming-mode) creates one upload session per asset, so it is bounded by the same per-session check at creation — there is no new enforcement point. Quota exhaustion mid-stream refuses the next session; the pipeline pauses, and no local original is released for an asset that did not upload.
- **Grace-window UX.** The structural rule is "upload session creation refused" in read-only mode; the client surfaces this as a discoverable, remediable state (what is full, what to delete) rather than an opaque mid-import error. Concrete copy is a client-UX detail.
- **Billing integration.** Out of scope and deliberately decoupled: this doc owns *accounting and enforcement* (what `quota_used` is, where the check runs); a billing/tier system, where present, only *sets* `soft_limit` / `hard_limit` / `grace_window`. Self-hosted deployments run with no billing and `hard_limit = ∞`.

## Contract Skeleton

```rust
// in capsule-api-service::quota
struct QuotaStatus {
    used: u64,
    soft_limit: u64,
    hard_limit: u64,
    state: QuotaState,  // OK | SoftWarning | HardExceeded | GraceExpired | Suspended
}

fn check_quota(user: UserId, additional_bytes: u64) -> Result<(), QuotaError>;
fn current_status(user: UserId) -> QuotaStatus;
```

Concrete error types, the `GET /quota` response shape, and admin controls are an implementation detail; the accounting model and enforcement points above are the contract.

## Validation

- **Hard-limit enforcement (unit).** A session creation that would cross the hard limit is rejected with the right code; no pending row is written.
- **Dedup attribution (unit).** Two users upload the same content; assert only the first user's quota is debited.
- **Trash-retention accounting (unit).** Soft-delete an asset; assert it still counts at full size until hard-purge; hard-purge it; assert the bytes are released.
- **Federated-receive accounting (unit).** Cache a federated blob for a receiving user; assert it debits the receiver, deduped (a blob the server already holds is not double-counted); exhaust a `(receiving_user, source_peer)` caching budget; assert further pulls from that peer are refused.
- **Derivative reclaim on purge (unit).** Hard-purge an asset; assert its derivative + metadata blob references drop and any zero-reference blob is GC'd, with bytes credited back — no orphaned derivative left counting.
- **Grace expiry (smoke).** Mock the grace window past; assert read-only mode behavior.
- **Quota status reporting (unit).** `GET /quota` returns accurate `used` + `state` for a fixture user.
