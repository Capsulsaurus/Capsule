---
title: Quota
description: Storage quota accounting, thresholds, and enforcement points
status: draft
---

Quota is charged to `upload_user_id` (the authenticated uploader), not `owner_id`, so an on-behalf upload costs the account that spent the storage. This page owns the accounting model, the thresholds and the enforcement points; the code is [`capsule-server/src/quota/mod.rs`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/quota/mod.rs) and the wire contract is [](/reference/api/quota/).

## Accounting Model

`quota_used(user)` is the sum of the ciphertext sizes of every blob attributed to that user: originals, derivatives, metadata blobs and provenance blobs.

- **Attribution is global by content address.** A blob counts against only the first uploader; a later identical upload is a merge ([](/design/transfer/upload/#deduplication-and-merge)) and charges nothing. The address is over ciphertext ([](/design/cryptography/encryption/#content-address)) and every file key is fresh, so two users' copies of the same photo never share an address: a merge is the same sealed bytes arriving again (a retry, a re-announce, a federated pull), never evidence that two people hold the same plaintext.
- **Derivatives count.** Thumbnails, previews and model-tagged derivatives such as synced embeddings ([](/design/ai/#synced-embeddings)) are charged like any blob.
- **Trash counts at full size** until the retention window ([](/design/organization/#retention-window)) expires and the asset is hard-purged.
- **Credit belongs to the GC sweep, not the purge.** A purge drops one asset's references. The bytes are credited back, to whoever the ledger says was charged, only when the collector ([](/design/filesystem/server/#deletion-and-garbage-collection)) releases a zero-reference blob. Refunding at purge time would refund bytes still stored for another holder.
- **Federated-received blobs count against the receiver**, deduped by content hash, as described in [](#federated-blobs).

### Federated blobs

Pulled blobs are bounded by a per-`(receiving_user, source_peer)` caching budget, `FEDERATION_PEER_BUDGET_PERCENT` (0 to 100, default 25% of the receiver's hard limit): the storage side of per-peer compartmentalization ([](/design/federation/#per-peer-compartmentalization)). The number of albums an account may subscribe to is bounded separately, at 1,000 ([](/design/federation/#token-lifecycle-and-chain-of-trust)).

**Budget usage.** Usage on a peer is the distinct metadata blobs held by every album the account subscribes to there, live or lapsed, each blob once however many of those albums hold it. A pulled album is shared by all its local subscribers, so a blob it holds is charged in full to each of them, never split.

**The budget refuses growth, never holdings.**

- A pulled page that holds a blob new to some subscriber and leaves that subscriber's usage past its budget is not written at all. The pull worker ([](/design/federation/#egress-pull-worker)) defers the album to its next interval without counting a failure or spending the peer's error budget.
- A page that adds nothing new to anyone is always written, so lowering the budget stalls growth and never removes or blocks what is held.
- A subscriber's usage on a peer can pass the budget by at most what its other albums there commit concurrently, one page each.

**In `quota_used`**, a pulled blob counts once per receiver across every album it subscribes to on every peer, live or lapsed, in full to each subscriber.

- It is not counted again if the receiver's own ledger attribution (an upload or a reservation) already holds the address. It is still counted if another account holds that attribution, because leaving it out would tell the receiver that someone else on this server stored those bytes.
- The pulled charge is defined by what is held, never written by a caller. The Postgres ledger stores it as a per-account total beside a per-account-and-address reference count, and the schema maintains both in the same transaction as every write that changes a subscription, a held blob or an attribution. It therefore lands in the same transaction as the pulled page that holds the blob, and ends when the holding does: on unsubscribe, or when the job's rows are forgotten. It can never be credited twice or below zero, and the collector's release of an attribution never credits pulled bytes.
- The caching budget caps these same bytes rather than adding to them. Pulled growth is never refused at the hard limit, so it is one way an account becomes hard-exceeded; unsubscribing is always the way back.
- A usage read is one row whatever the account holds.

**Drift.** The stored total is a copy of a derivable fact, so the ledger keeps the derivation beside it. An audit compares every account's stored holdings and total with what its rows derive in one snapshot, and rebuilds an account that differs under that account's lock. The audit is part of the quota port, so every ledger answers it; one that derives the total on every read holds no copy and reports no drift. `capsule-server scrub` reports each drifted account as a finding ([](/design/filesystem/maintenance/#server-side-integrity-scrub)) and repairs nothing; `capsule-server rebuild` names them and, with `--apply`, rebuilds them.

## Thresholds and States

| State | Condition | Refuses |
| --- | --- | --- |
| **OK** | `used < soft_limit` | nothing |
| **Soft warning** | `soft_limit ≤ used < hard_limit` | nothing; the client raises the `quota_soft` alert ([](/design/notifications/#alert-classes)) |
| **Hard exceeded** | `used ≥ hard_limit` | new upload sessions and new share-link metadata records ([](/design/share-links/#quota)); all other writes work |
| **Grace expired** | hard-exceeded for longer than `grace_window` | additionally every metadata-growth write (caption/tag edits, new share or upload links) |
| **Suspended** | set by an operator, whatever `used` | every growth write: new upload sessions, drop sessions charged to the account, every metadata-growth write |

- The writes a `delete`, `trash-restore` or trash-empty produces MUST always be admitted: a user must be able to delete their way back under quota.
- Entering the grace window raises `quota_grace_expiring`. Reads are never refused by quota.
- Limits and grace window are deployment-configurable: a default soft and hard limit for every account, and a per-account override an operator sets with the server CLI. "No quota" is `hard_limit = u64::MAX`, which runs the same code path.
- **Open registration needs a finite default, or an explicit choice not to have one.** Any address may register while password registration is enabled ([](/design/authentication/)), so an unlimited default lets one source fill the disk with accounts it minted for free, each charged nothing. `serve` refuses to start with password registration enabled and no default limit configured. An operator who wants no quota says so explicitly (a default of `unlimited`), and the server logs that choice at `WARN` on every start. A server that accepts no open registration (password authentication disabled) may leave the default unset, which means unlimited.

### Suspension

- **Suspended** is an administrative or billing act, not a threshold. It outranks every other state, so a suspended account reports `suspended` whatever it uses, on an unlimited deployment too. A growth write it refuses answers `error.quota.suspended`.
- Only an operator sets or lifts it, with `capsule-server quota suspend <USER_ID>` and `capsule-server quota unsuspend <USER_ID>`. Freeing space does not lift it, and no charge or release touches it.
- It is durable (the quota ledger keeps the instant it began), and suspending a suspended account keeps the first instant.
- It is a quota state, not the moderation suspension ([](/design/moderation/#account-suspension)), which removes the ability to write at all and answers `error.moderation.account_suspended`. A quota suspension leaves reads, deletes and restores admitted.

## Enforcement Points

- **Upload session creation is the hard enforcement point**, with the dedup-release check below as its one follow-up. Session creation ([](/design/transfer/upload/#endpoints)) refuses with `error.quota.exceeded` if `used + declared_size` crosses the hard limit, and with `error.quota.suspended` for a suspended account, before any pending row is written. Once open and charged, the declared size is the cap and the session may complete; finalization does no quota check.
- **Cancellation, expiry, pressure discard and `FailedProcessing`** release the reservation. A live session of another account, admitted against the released attribution because it was deduped, is then charged for the bytes **past the hard-limit check**, and keeps running. It is never discarded for it: its uploader may already have sent most of the blob over a slow link, and a session that vanished as the uniform `404` would give it no cause to act on and no way back, since re-creating would be refused. Charging past the limit can carry that account over its hard limit, which then refuses its next session as usual. The overshoot is bounded by reservations another account was already admitted for, so it never stores bytes no account's quota covered. A suspended account's session is the one exception: it is cancelled, and its next request answers `error.quota.suspended` rather than `404`.
- **The blob root's free-space reserve** is the server-wide backstop under every account's quota. Below the reserve, session creation and chunk writes answer `507 error.upload.storage_full`, whatever the account's quota says. Finalization, ledger appends, deletes, purges and garbage collection still run, so bytes already on the server always finish, and the way back (deleting) always works ([](/design/transfer/#failure-model)). Finalization itself needs no new data blocks (a commit is a rename), so the reserve covers ledger entries, directory growth and anything else sharing the volume while writes drain. It defaults to the larger of 2% of the volume and 4 GiB and is deployment-configurable.
- **Metadata-update writes** are checked as metadata growth.
- **Staged uploads and streaming import** ([](/design/transfer/upload/#upload-tiering-staged-uploads), [](/design/import/pipeline/#import-upload-streaming-mode)) need no special case: each session is charged at its own creation. Exhaustion mid-stream refuses the next session, and no local original is released for an asset that did not upload.

## Scope Decisions

- **Sponsored accounts** roll up to the sponsor's quota ([](/design/cryptography/keys/#delegatedsponsored-accounts)).
- **Web-upload drops** are charged to the link owner at drop-session creation ([](/design/web-upload/#drop-and-adoption-lifecycle)), so a leaked link cannot exceed the owner's hard limit. Adoption in place ([](/design/web-upload/#why-adopt-in-place)) charges only its new metadata and provenance blobs. A drop in the inbox is a reference the collector honours ([](/design/filesystem/server/#deletion-and-garbage-collection)); a discarded or expired drop frees its bytes at the next GC.
- **Departed album members keep their charge.** A member's upload to a shared album stays attributed to the member after removal, until a current writer deletes the asset and the collector releases it.
- **Per-album quotas and billing** are out of scope; billing, where present, only sets the limits.

## Known Gaps

- Limits are not yet configurable: every deployment runs the unlimited default, so neither the open-registration refusal nor a per-account override exists.
- The blob root has no free-space reserve; `ENOSPC` is answered `507` wherever it strikes, finalization included.
- A deduped session whose charge is refused after a reservation transfer is still discarded as the uniform `404`.

## Validation

- Session creation crossing the hard limit is refused with `error.quota.exceeded` and writes no pending row.
- Two users upload the same content; only the first is debited.
- When the first cancels, the second's live session is charged past the hard limit and finishes; a suspended second account's session is cancelled and answers `error.quota.suspended` (`upload::reservation`).
- Below the free-space reserve, session creation and chunks answer `507 error.upload.storage_full` while a claimed finalization, a delete and a GC sweep still complete.
- `serve` refuses to start with password registration enabled and no default limit; an explicit `unlimited` starts and logs `WARN`.
- Trash counts until hard-purge; purge plus GC sweep credits the bytes back with no orphaned derivative still counting.
- A departed member stays charged until the collector releases the address (`quota::conformance`).
- Grace expiry refuses metadata growth but admits delete and restore.
- Suspension refuses a growth write at every enforcement point and admits it again after unsuspend; reads and deletes are never refused, and the state survives a restart.
- `GET /v1/quota` reports accurate `used` and `state`.
- A pulled page past a subscriber's caching budget on its peer writes nothing, a blob counts once across a subscriber's albums on a peer, and a page adding nothing new is admitted past the budget (`federation::pull::pulled::conformance`).
- A pulled blob counts in full against each subscriber's `quota_used`, once across its albums and once beside its own attribution, and an unsubscribe releases it for that subscriber only (`quota::conformance`).
- The stored pulled total equals the derived one after every write path, racing writers, the backfill and a rebuild of injected drift (`quota::postgres`).
- A ledger written only through its port reports no pulled drift and its reconcile moves nothing (`quota::conformance`).
- Drift injected behind the Postgres triggers is a scrub finding, named and left alone by a dry-run rebuild, and rebuilt by `rebuild --apply` (`quota::postgres`).
- Each drifted account is its own scrub finding, and the scrub never reconciles (`scrub`).
