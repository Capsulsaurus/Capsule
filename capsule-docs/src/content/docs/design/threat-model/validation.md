---
title: Validation Invariants
description: Server and client refuse-by-default checklists; protocol handshake; idempotency; atomicity
audit: none
---

Refuse-by-default checks every receiver runs before persisting a write, against every client class ([](/design/threat-model/#client-class-taxonomy)).

## Server-Side Validation Invariants

Key-free **structural** checks. This is the canonical list, and its numbers are stable and cited elsewhere: a new invariant takes the next free number and sits in the section of the surface it guards. Request and response shapes per endpoint are in the generated reference ([](/reference/api/upload/), [](/reference/api/albums/), [](/reference/api/drops/), [](/reference/api/federation/)). Every rejection carries an `error.*` code ([](/design/i18n/#server-error-codes); mapping in [](/reference/api/conventions/#errors)) and is logged.

### On `POST /v1/upload` (session creation)

- **1.** `X-Capsule-Protocol` ∈ server `[Min, Max]` → else `426`, no session.
- **2.** `crypto_suite_id` ∈ [](/design/cryptography/primitives/#primitives-inventory) → else `400`.
- **3.** `hash` length = the suite's digest size → else `400`.
- **4.** `size` ∈ (0, `max_file_size`] → else `400`/`413`.
- **5.** `content_type` ∈ the closed enum for this protocol version → else `400`.
- **6.** `album_id` exists, caller has write capability, album's pinned `protocol_version` = request's → else `403`.
- **7.** `created_by_device` is listed and not revoked in the uploader's device directory **version in force when the server receives the request** → else `403`. No `timestamp` is compared: a self-asserted time cannot prove when a device was enrolled, and a backdated one must buy nothing. An account with no published directory has no device and is refused. Re-run at finalization (15), so a device revoked in between is refused; a rebuild re-checks against the version in force at the manifest's ledger position ([](/design/filesystem/server/#rebuild)).
- **8.** `timestamp` within the gross-drift bound ([](/design/threat-model/schema-rules/#timestamp-grammar)), which is not an authorization control → else `400`.

### On each `PATCH /v1/upload/{id}` chunk

- **9.** The range is admitted against the committed set and the in-flight ranges before any byte is written (admission rules in [](/design/transfer/upload/#chunk-rules-and-strictness)): disjoint → written and committed; wholly committed → duplicate, nothing written; any other overlap → `409 range_conflict`; over the session's in-flight cap → `409 too_many_in_flight`; past the fragment bound → `409 too_fragmented`.
- **10.** Well-shaped chunk: `application/octet-stream` (`415`), non-empty, offset 4 KiB-aligned and length 4 KiB-aligned unless it ends at `size` (`400`), ≤ protocol max (`413`).
- **11.** Range end ≤ declared `size` → else `400 size_exceeded`, nothing written.
- **12.** `chunk_hash` comes from the **required** `X-Capsule-Checksum` and matches the body → else `400`, nothing persisted.

### At finalization

- **13.** Committed set = `[0, size)`, the claim's precondition.
- **14.** Recomputed ciphertext hash = declared `hash` → else `FailedProcessing`. The server computes it with the same `capsule_core` hasher the client uses, so this check does not catch a hasher bug both share ([](/design/threat-model/risks/)).
- **15.** 1–8 re-run inside the finalization transaction.
- **36.** Each blob's header agrees with the signed manifest that names it, checked without a key: known `magic` and `format_version`, reserved bytes zero, `suite_id` = `crypto_suite_id`, `key_epoch` = `0` when `key_mode` is `wrapped` and `amk_version` otherwise, the STREAM nonce prefix = `nonce_prefix`, and `commitment` = the manifest's mirrored commitment ([](/design/cryptography/encryption/#header-and-manifest-agreement)) → else `FailedProcessing` with `error.upload.envelope_rejected`, nothing committed. The address already matched, so a disagreement is the signer's own inconsistency.

### In the index critical section (every manifest write, upload or lifecycle)

Enforced inside the critical section that mints the sequence number, for lifecycle writes ([](/design/authorization/#the-lifecycle-write-surface)) **and** upload `replace`. Checked at the gate instead, concurrent replaces could both pass.

- **16.** `action` ∈ closed enum → else `400`.
- **17.** `prior_provenance_hash` = last accepted manifest hash for the `asset_id` → else `409` (stale revival).
- **18.** `amk_version` never regresses per album **and** is within what the admin-signed MLS commit chain attests ([](/design/cryptography/keys/#write-authorization)) → else `400`. A `create` refused here was never published; its client re-seals it at the album's current epoch and retries once ([](/design/transfer/upload/#stale-epoch-create)). A device that does not yet hold that epoch's key cannot, and keeps the asset local until the key arrives.

### On federation pull (server-to-server)

- **19.** Capability verifies, unexpired, `jti` unrevoked (≤ 15 min cache), and recorded here for the member's grant epoch → else `401`/`403`.
- **20.** 1–18, 25 and 36 re-applied to every pulled entry (36 needs no key, and the pull worker caches pulled blobs), each as strict as the compacted feed lets a receiver check it. There are no inbound federated writes yet. Where the feed cannot show what the write-time rule reads, the pull applies it as follows:
  - **7** is left to the receiving client, which holds the device directory a capability does not reach.
  - **17** refuses a manifest behind the held head and a second `create`. The feed serves only an asset's current manifest, so a pull cannot require `prior_provenance_hash` to equal the held head.
  - **18** holds **per asset**: an asset's `amk_version` never regresses along its own chain. The feed re-mints an asset's position without a new record and replays from zero after a restart, so an honest feed serves an older epoch's record after a newer one. The receiving client checks the epoch against the MLS commit chain.
  - **25**: a record that binds a metadata blob is served with exactly that blob. A record that binds none never replaces a metadata blob this server holds. Only a signed binding record proves a newer blob.
- **21.** Per-peer budgets (events, bytes, CPU per hour), keyed on peer origin, not token → else `429`. CPU is the pull worker's and not yet metered.

### On the sync feed, directory publish, and federated reports

- **22.** `sync_cursor` carries a server-only MAC; a forged cursor → `400`. Clients separately enforce `sync_seq` monotonicity ([](/design/sync/#discovering-what-changed)).
- **23.** `DeviceDirectory` publish: `directory_version` strictly increases (else `409`) and the master signature verifies under the account's **identity anchor**, the immutable identity key from its first publish (`X-Capsule-Identity-Key`) → else `403` ([](/design/cryptography/keys/#device-directory)).
- **24.** Federated report: signature verifies under the peer's pinned key **before** its budget is charged; unpinned peer → `403` ([](/design/moderation/#federated-reporting)).

### On `PUT /v1/albums/{album_id}/roster` (album roster publish)

- **35.** Canonical, bounded CBOR naming this album, no duplicates, owner excluded (`400`); caller owns the album (`404`); `attester_sig` under a non-revoked caller device (`403`); `roster_version` strictly greater than held or an identical replay, `amk_epoch` non-regressing (`409`); at most 16 above held, so membership cannot be frozen (`400`). Omitted members are marked revoked, never deleted. Owner: [](/design/authorization/).

### On any write whose bundle carries a metadata blob

- **25.** The metadata blob's content hash = the manifest's `metadata_blob_hash` → else `400`, nothing written ([](/design/metadata/#local-and-server-metadata-equivalence)).

### On `POST /d/{opaque_id}` (upload-link drop session) and adoption

A drop ([](/design/web-upload/#security-contract)) has no manifest, so these replace 1–8; chunks and finalization reuse 9–14.

- **26.** Link is live (exists, unexpired, unrevoked) → else indistinguishable `404` (never `410`); cumulative caps exhausted → `409`/`413`.
- **27.** `content_type` ∈ the link's pinned closed enum → else `400`.
- **28.** `size` ∈ (0, link `max_file_size`] → else `400`/`413`.
- **29.** Owner quota admits the declared size at session creation ([](/design/quota/#enforcement-points)) → else `403`.
- **30.** `DropDescriptor` well-formed, `kem_ct` length matches the suite; **no** album, `amk_version`, manifest, or provenance field → else `400`.
- **31.** Rate-limited per `{opaque_id}` and per IP → else `429`.
- **32.** Adoption: the `create` manifest passes 1–8, 16–18, 25 and 36 (the drop's blob header against the adopter's manifest, so `key_epoch` is `0`); references a blob in **the caller's own** inbox; `key_mode` closed. Inbox→album promotion and inbox-row delete are one transaction → else `400`/`403`/`409`, nothing written.

### On `POST /v1/auth/register` (password account creation)

- **37.** The request carries the User IK public key and an IK signature over a domain-separated message naming the address and this server's origin, and the signature verifies → else `400`, nothing created. The IK is pinned as the account's identity anchor in the same transaction that creates the account, so no later credential can pin another ([](/design/authentication/#the-identity-anchor)). Every later directory publish verifies under it (23).

### On custody receipts and signed attestations

- **33.** A `CustodyReceipt` ([](/design/transfer/durability/#custody-receipts)) is signed and persisted only inside the finalization transaction (both or neither); `receipt_seq` strictly monotonic and `prior_receipt_hash` chained within the owner's chain, never overwritten. Before `Completed` → `409`.
- **34.** A `StorageAttestation` ([](/design/transfer/durability/#signed-storage-attestation)) echoes the client `nonce` and signs the same state read that produced its verdict.

## Client-Side Validation Invariants

Run before applying any received data; skipping one makes a client _faulty_.

- Run `verify_asset` ([](/design/cryptography/keys/#write-authorization)) on every `AssetManifest`; quarantine on failure.
- Refuse a `sidecar_schema` above the client's max (write never; read only by opt-in read-only mode).
- Refuse a `protocol_version` outside the client's `[Min, Max]`.
- Refuse unknown closed-enum values; preserve unknown CBOR keys, never act on them.
- Keep `latest_provenance_hash` per asset; refuse and surface a manifest behind it.
- Persisted sidecar MUST equal the decrypted blob byte-for-byte, blob hash = `metadata_blob_hash`; else quarantine ([](/design/metadata/#local-and-server-metadata-equivalence)).
- Verify and keep every upload's `CustodyReceipt` ([](/design/transfer/durability/#custody-receipts)); failure → write unconfirmed.
- Discarding the only copy of irreplaceable bytes requires a verified receipt, a `durable` verdict ([](/design/transfer/durability/#verify-before-destroy)), and a `verify_asset` accept. User deletes are not gated.
- Keep a per-user `directory_version` high-water mark; refuse and surface a regression.
- Refuse an OR-set remove of an unobserved `add_id`.
- Refuse a `revoke_all_sessions` without master-key proof.
- Decode remote-origin bytes only in the sandboxed decoder ([](/design/clients/#sandboxed-decoder)). Until a platform has it, guest drops are decoded only by the memory-safe core codecs ([](/design/web-upload/#security-contract)); other remote-origin bytes are decoded in process, a documented deviation.
- Classify a blob that will not open into exactly one state of [](/design/cryptography/encryption/#reading-a-blob): never decrypt on a commitment mismatch, and never report a ranged-read tag failure as _sealed bad at source_ before a whole-blob address check.
- Cap every response body after content decoding, per operation, before parsing it; never size an allocation from a length the server or a peer states ([](/design/networking/#http-transport)).
- Accept an `AlbumKeyDistribution` only if it equals the receiver's own export for that epoch; never overwrite a held key ([](/design/cryptography/keys/#key-delivery-and-who-may-send-it)).

### Bulk and Automatic Changes

Rules for any operation that changes many assets at once or runs without a per-asset decision: a scan, a watched folder, an import rule, a duplicate review, a migration.

- **Absence is never a delete.** A file missing from a scanned folder, a source that disappears or a scan that matches nothing changes nothing in the library. It is reported, never acted on.
- **Destructive bulk operations are previewed and capped.** Any bulk trash, release or replace shows a dry-run list first, and a single run past its cap pauses for confirmation ([](/design/organization/#import-rules)). No automatic process deletes or purges.
- **Merges are reversible operations.** Merging people, duplicates or stacks writes ordinary signed operations that never delete their inputs, so a merge can be undone and a replayed or racing merge is a no-op ([](/design/people/#corrections), [](/design/organization/#near-duplicate-review)).
- **Upgrades migrate, never reset.** A model bump, schema bump or feature migration carries user corrections forward or keeps them verbatim; it never asks the user to discard names, labels or edits to proceed ([](/design/people/#corrections-outlive-models)).
- **State applies to a whole stack.** Hiding, trashing or restoring a stack member applies to every member in one atomic batch, so no member is stranded outside the state its stack is in.
- **The server never moves files a user can see.** Server storage layout is opaque and content-addressed; user-facing folder layouts exist only in exports ([](/design/export/)).

## Protocol and Capability Negotiation

One fail-closed, one-shot gate on every versioned surface, before any state is written.

### Universal Headers

The negotiation headers are specified in [](/reference/api/conventions/#protocol-negotiation). Clients and peers send `X-Capsule-Protocol` (the `YYYY-MM-DD` protocol version), `X-Capsule-Crypto-Suite` on writes (the suite id from [](/design/cryptography/primitives/#primitives-inventory)) and `X-Capsule-Sidecar-Schema` (the SDK sends it on every request). The server returns the accepted window as `X-Capsule-Protocol-Min/Max` on every response, and `X-Capsule-Min-Client-Build`, an advisory semver deprecation cutoff.

**Census of `X-Capsule-*`:** new headers MUST register here. By pointer: `X-Capsule-Offset`, `-Committed`, `-Content-Length`, `-Upload-Status`, `-Upload-Error`, `-Checksum` (required on `PATCH`), `-Session-Expires-In`, `-Session-Idle-Expires-In` ([](/design/transfer/upload/#endpoints)); `X-Capsule-Advice` ([](/design/networking/#server-advice)); `X-Capsule-Identity-Key` (required on directory publish; invariant 23). `cohort_hash` is deliberately a body field ([](/design/authentication/#device-cohorts)).

### Fail-Closed Rules

- Protocol outside `[Min, Max]` on a write → `426`. Reads of any past version succeed ([](/design/versioning/)).
- Unknown crypto suite or sidecar schema above max → `400`.
- Federation capability is an extra `401`/`403` layer; it never substitutes for the protocol header.

## Idempotency Invariants

Duplicates are no-ops; the same key with different content is corruption. An unlisted surface is **not** idempotent.

| Surface                                                                                                                            | Key                                                | Duplicate                                                                                                                                                                                                                                                                    |
| ---------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Upload chunk                                                                                                                       | `(upload_id, byte range)`                          | Wholly committed → the session's state, nothing written, bytes not compared and never written (a changed source is stopped by the client's chunk-tag guard in [](/design/cryptography/encryption/#encryption-workflow)); partial overlap → `409 error.upload.range_conflict` |
| Session creation                                                                                                                   | `(owner_id, hash, album_id)`                       | Active session returned; finalized → `409 error.upload.duplicate_blob` + existing asset                                                                                                                                                                                      |
| Lifecycle manifest                                                                                                                 | `(asset_id, prior_provenance_hash, manifest_hash)` | Chain advances once                                                                                                                                                                                                                                                          |
| Metadata update                                                                                                                    | op id + `(asset_id, prior_provenance_hash)`        | Structurally identical                                                                                                                                                                                                                                                       |
| Federation capability proof                                                                                                        | `(peer_id, jti)`                                   | Same response                                                                                                                                                                                                                                                                |
| Federation pull                                                                                                                    | `(peer_id, sync_cursor)`                           | Same page                                                                                                                                                                                                                                                                    |
| MLS commit                                                                                                                         | Group commit chain (OpenMLS)                       | Rejected                                                                                                                                                                                                                                                                     |
| Album upgrade ([](/design/versioning/#album-upgrade-ceremony)) / MLS re-key ([](/design/mls-resilience/#group-re-keying-ceremony)) | `intent_id`                                        | Never forks or re-keys twice                                                                                                                                                                                                                                                 |
| Device enrollment ([](/design/device-enrollment/#cross-device-add))                                                                | Single-use enrollment code                         | Re-redemption rejected                                                                                                                                                                                                                                                       |
| Share/upload-link create                                                                                                           | Client op id                                       | Returns the minted link                                                                                                                                                                                                                                                      |
| Share/upload-link revoke                                                                                                           | `link_id`                                          | No-op                                                                                                                                                                                                                                                                        |
| Drop adoption                                                                                                                      | `drop_id` (invariant 32)                           | Returns the promoted asset                                                                                                                                                                                                                                                   |
| Album roster publish                                                                                                               | `(album_id, roster_version)` (invariant 35)        | Identical bytes → `200 replayed`; different → `409 error.album.roster_stale`                                                                                                                                                                                                 |

## Atomicity Invariants

- **Blob finalization:** each blob of a bundle finalizes on its own, and its index rows and custody receipt commit in one transaction; failure → `FailedProcessing`, partials GC'd ([](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery)). A bundle becomes visible only once its provenance and metadata blobs have finalized (the visibility gate in [](/design/transfer/upload/#what-gets-uploaded)), and a `replace` applies as one act when its manifest lands.
- **Stack edits:** all `.tmp` staged, renamed together; any failure discards the bundle.
- **Batch metadata changes** (time corrections, duplicate merges, rule actions, stack-wide hide or trash): staged and committed together under the same rule.
- **AMK epoch bump + write-tier rotation:** one MLS commit.
- **Album upgrade:** one MLS commit (`AlbumTombstone`) ([](/design/versioning/#album-upgrade-ceremony)).
- **Lifecycle manifest + provenance:** the same act, because the entry _is_ the manifest plus its chain link.

## Known Gaps

- Hiding or trashing one stack member does not yet carry the rest of the stack with it; the bulk-change rules have no shared preview and cap mechanism.

- **7** compares the device's `added_at` with the manifest's `timestamp` (`capsule-server/src/upload/authority.rs`, `capsule-server/src/album/authority.rs`) instead of reading the directory version in force at receipt.
- **14** has no independent counterpart: the content address is checked only by the hasher both sides share ([](/design/threat-model/risks/)).
- **36** cannot run: blobs carry no header yet ([](/design/cryptography/encryption/#known-gaps)).
- **37** is not enforced: the registration request carries no identity key (`RegisterRequest` in `capsule-server/src/routes/auth.rs`), so a password account's anchor is still pinned by its first directory publish.
- On the client, no response body is capped after decoding ([](/design/networking/#known-gaps)), and key deliveries are applied without the sender or export check ([](/design/cryptography/keys/#known-gaps)).
