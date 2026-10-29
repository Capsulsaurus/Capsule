---
title: Quota
description: Storage quota accounting, thresholds, and enforcement points
status: draft
---

Quota is charged to `upload_user_id` (the authenticated uploader), not `owner_id`, so an on-behalf upload costs the account that spent the storage. This doc owns the accounting model, the thresholds, and the enforcement points; the code is [`capsule-server/src/quota/mod.rs`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/quota/mod.rs).

## Accounting Model

`quota_used(user)` is the sum of the ciphertext sizes of every blob attributed to that user: originals, derivatives, metadata blobs, and provenance blobs.

- **Attribution is global by content address.** A blob counts against only the first uploader; a later identical upload is a [merge](/design/transfer/upload/#deduplication-and-merge) and charges nothing.
- **Trash counts at full size** until the [retention window](/design/organization/#retention-window) expires and the asset is hard-purged.
- **Credit belongs to the GC sweep, not the purge.** A purge drops one asset's references; the bytes are credited back, to whoever the ledger says was charged, only when the [collector](/design/filesystem/server/#deletion-and-garbage-collection) releases a zero-reference blob. Refunding at purge time would refund bytes still stored for another holder.
- **Federated-received blobs count against the receiver**, deduped by content hash, bounded by a per-`(receiving_user, source_peer)` caching budget (deployment-configurable, default 25% of the receiver's hard limit) — the storage side of [per-peer compartmentalization](/design/federation/#per-peer-compartmentalization). Not yet enforced; tracked with federation's receiving half, #964.

## Thresholds and States

| State             | Condition                                         | Refuses                                                                                                                       |
| ----------------- | ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| **OK**            | `used < soft_limit`                               | nothing                                                                                                                       |
| **Soft warning**  | `soft_limit ≤ used < hard_limit`                  | nothing; the client raises the `quota_soft` [alert](/design/notifications/#alert-classes)                                     |
| **Hard exceeded** | `used ≥ hard_limit`                               | new upload sessions and new [share-link metadata records](/design/share-links/#quota); all other writes work                  |
| **Grace expired** | hard-exceeded for longer than `grace_window`      | additionally every metadata-growth write (caption/tag edits, new share or upload links)                                       |
| **Suspended**     | set by an operator, whatever `used`               | every growth write: new upload sessions, drop sessions charged to the account, every metadata-growth write                    |

- The writes a `delete`, `trash-restore`, or trash-empty produces MUST always be admitted: a user must be able to delete their way back under quota.
- Entering the grace window raises `quota_grace_expiring`. Reads are never refused by quota.
- Limits and grace window are deployment-configurable; the default is no quota (`hard_limit = u64::MAX`), which runs the same code path.

### Suspension

- **Suspended** is an administrative or billing act, not a threshold: it outranks every other state, so a suspended account reports `suspended` whatever it uses, on an unlimited deployment too. A growth write it refuses answers `403 error.quota.suspended`.
- Only an operator sets or lifts it: `capsule-server quota suspend <USER_ID>` and `capsule-server quota unsuspend <USER_ID>`. Freeing space does not lift it, and no charge or release touches it.
- It is durable (the instant it began is kept in the quota ledger), and suspending a suspended account keeps the first instant.
- It is a quota state, not the [moderation suspension](/design/moderation/#account-suspension): that one removes the ability to write at all and answers `error.moderation.account_suspended`. This one leaves reads, deletes and restores admitted.

## Enforcement Points

- **Upload session creation is the only hard enforcement point.** [`POST /v1/upload`](/design/transfer/upload/#endpoints) refuses `403 error.quota.exceeded` if `used + declared_size` crosses the hard limit, and `403 error.quota.suspended` for a suspended account, before any pending row is written. Once open, the declared size is the cap and the session may complete; finalization does no quota check.
- **Cancellation and expiry** release the reservation.
- **Metadata-update writes** are checked as metadata growth.
- **[Staged uploads](/design/transfer/upload/#upload-tiering-staged-uploads) and [streaming import](/design/import/pipeline/#import-upload-streaming-mode)** need no special case: each session is charged at its own creation. Exhaustion mid-stream refuses the next session; no local original is released for an asset that did not upload.

## Scope Decisions

- **Sponsored accounts** roll up to the sponsor's quota ([Keys — Delegated/Sponsored](/design/cryptography/keys/#delegatedsponsored-accounts)).
- **Web-upload drops** are charged to the link owner at [drop-session creation](/design/web-upload/#drop-and-adoption-lifecycle), so a leaked link cannot exceed the owner's hard limit. [Adoption in place](/design/web-upload/#why-adopt-in-place) charges only its new metadata and provenance blobs; a discarded drop frees its bytes at the next GC.
- **Departed album members keep their charge.** A member's upload to a shared album stays attributed to the member after removal, until a current writer deletes the asset and the collector releases it.
- **Per-album quotas and billing** are out of scope; billing, where present, only sets the limits.

## Validation

- Session creation crossing the hard limit is refused with `error.quota.exceeded` and writes no pending row.
- Two users upload the same content; only the first is debited.
- Trash counts until hard-purge; purge plus GC sweep credits the bytes back with no orphaned derivative still counting.
- A departed member stays charged until the collector releases the address (`quota::conformance`).
- Grace expiry refuses metadata growth but admits delete and restore.
- Suspension refuses a growth write at every enforcement point and admits it again after unsuspend; reads and deletes are never refused, and the state survives a restart.
- `GET /v1/quota` reports accurate `used` and `state`.
