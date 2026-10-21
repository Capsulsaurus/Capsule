---
title: Validation Invariants
description: Server and client refuse-by-default checklists; protocol handshake; idempotency; atomicity
status: draft
---

Refuse-by-default checks every receiver runs before persisting a write, against every [client class](/design/threat-model/#client-class-taxonomy).

## Server-Side Validation Invariants

Key-free **structural** checks. Canonical list; numbers are stable and cited elsewhere. Every rejection carries an [`error.*` code](/design/i18n/#server-error-codes) ([mapping](/design/api-surfaces/#rejection-mapping)) and is logged.

### On `POST /v1/upload` (session creation)

- **1.** `X-Capsule-Protocol` ∈ server `[Min, Max]` → else `426`, no session.
- **2.** `crypto_suite_id` ∈ [Primitives Inventory](/design/cryptography/primitives/#primitives-inventory) → else `400`.
- **3.** `hash` length = the suite's digest size → else `400`.
- **4.** `size` ∈ (0, `max_file_size`] → else `400`/`413`.
- **5.** `content_type` ∈ the closed enum for this protocol version → else `400`.
- **6.** `album_id` exists, caller has write capability, album's pinned `protocol_version` = request's → else `403`.
- **7.** `created_by_device` is in the user's device directory with `added_at` before `timestamp` → else `403`.
- **8.** `timestamp` within the gross-drift bound ([Timestamp Grammar](/design/threat-model/schema-rules/#timestamp-grammar)); not an authorization control → else `400`.

### On each `PATCH /v1/upload/{id}` chunk

- **9.** Offset = current received count → else `409` + `X-Capsule-Offset`.
- **10.** Well-shaped chunk: `application/octet-stream` (`415`), non-empty, non-final chunks 4 KiB-aligned (`400`), ≤ protocol max (`413`).
- **11.** Cumulative received ≤ declared `size` → else `400`/`413`, session `FailedProcessing`.
- **12.** `(upload_id, offset, chunk_hash)` is new or an exact repeat; `chunk_hash` comes from the **required** `X-Capsule-Checksum` (missing or body mismatch → `400`, nothing persisted). Same offset, different hash → `409`.

### At finalization

- **13.** Total received = declared `size` → else `FailedProcessing`.
- **14.** Recomputed ciphertext hash = declared `hash` → else `FailedProcessing`.
- **15.** 1–8 re-run inside the finalization transaction.

### In the index critical section (every manifest write, upload or lifecycle)

Enforced inside the critical section that mints the sequence number, for [lifecycle writes](/design/authorization/#the-lifecycle-write-surface) **and** upload `replace` — at the gate, concurrent replaces could both pass.

- **16.** `action` ∈ closed enum → else `400`.
- **17.** `prior_provenance_hash` = last accepted manifest hash for the `asset_id` → else `409` (stale revival).
- **18.** `amk_version` never regresses per album **and** is within what the admin-signed MLS commit chain attests ([Write Authorization](/design/cryptography/keys/#write-authorization)) → else `400`.

### On federation pull (server-to-server)

- **19.** Capability verifies, unexpired, `jti` unrevoked (≤ 15 min cache), and recorded here for the member's grant epoch → else `401`/`403`.
- **20.** 1–18 re-applied; federation never loosens rules. Open: #476 (no inbound federated writes yet).
- **21.** Per-peer budgets (events, bytes, CPU per hour), keyed on peer origin not token → else `429`. Open: #476 (bytes and CPU unmetered).

### On the sync feed, directory publish, and federated reports

- **22.** `sync_cursor` carries a server-only MAC; forged → `400`. Clients separately enforce `sync_seq` monotonicity ([Sync](/design/sync/#discovering-what-changed)).
- **23.** `DeviceDirectory` publish: `directory_version` strictly increases (else `409`) and the master signature verifies under the account's **identity anchor** — the identity key from its first publish (`X-Capsule-Identity-Key`), immutable — else `403` ([Device Directory](/design/cryptography/keys/#device-directory)).
- **24.** Federated report: signature verifies under the peer's pinned key **before** its budget is charged; unpinned peer → `403` ([Federated Reporting](/design/moderation/#federated-reporting)).

### On `PUT /v1/albums/{album_id}/roster` (album roster publish)

- **33.** Canonical, bounded CBOR naming this album, no duplicates, owner excluded (`400`); caller owns the album (`404`); `attester_sig` under a non-revoked caller device (`403`); `roster_version` strictly greater than held or an identical replay, `amk_epoch` non-regressing (`409`); at most 16 above held, so membership cannot be frozen (`400`). Omitted members are marked revoked, never deleted. Owner: [Authorization](/design/authorization/).

### On any write whose bundle carries a metadata blob

- **25.** The metadata blob's content hash = the manifest's `metadata_blob_hash` → else `400`, nothing written. ([Metadata Equivalence](/design/metadata/#local-and-server-metadata-equivalence)).

### On `POST /d/{opaque_id}` (upload-link drop session) and adoption

A [drop](/design/web-upload/#security-contract) has no manifest, so these replace 1–8; chunks and finalization reuse 9–14.

- **26.** Link is live (exists, unexpired, unrevoked) → else indistinguishable `404` (never `410`); cumulative caps exhausted → `409`/`413`.
- **27.** `content_type` ∈ the link's pinned closed enum → else `400`.
- **28.** `size` ∈ (0, link `max_file_size`] → else `400`/`413`.
- **29.** Owner quota admits the declared size at session creation ([enforcement point](/design/quota/#enforcement-points)) → else `403`.
- **30.** `DropDescriptor` well-formed, `kem_ct` length matches the suite; **no** album, `amk_version`, manifest, or provenance field → else `400`.
- **31.** Rate-limited per `{opaque_id}` and per IP → else `429`.
- **32.** Adoption: the `create` manifest passes 1–8, 16–18, 25; references a blob in **the caller's own** inbox; `key_mode` closed. Inbox→album promotion and inbox-row delete are one transaction → else `400`/`403`/`409`, nothing written.

### On custody receipts and signed attestations

- **33.** A [`CustodyReceipt`](/design/transfer/durability/#custody-receipts) is signed and persisted only inside the finalization transaction (both or neither); `receipt_seq` strictly monotonic, `prior_receipt_hash` chained, never overwritten. Before `Completed` → `409`.
- **34.** A [`StorageAttestation`](/design/transfer/durability/#signed-storage-attestation) echoes the client `nonce` and signs the same state read that produced its verdict.

## Client-Side Validation Invariants

Run before applying any received data; skipping one makes a client *faulty*.

- [`verify_asset`](/design/cryptography/keys/#write-authorization) every `AssetManifest`; quarantine on failure.
- Refuse a `sidecar_schema` above the client's max (write never; read only by opt-in read-only mode).
- Refuse a `protocol_version` outside the client's `[Min, Max]`.
- Refuse unknown closed-enum values; preserve unknown CBOR keys, never act on them.
- Keep `latest_provenance_hash` per asset; refuse and surface a manifest behind it.
- Persisted sidecar MUST equal the decrypted blob byte-for-byte, blob hash = `metadata_blob_hash`; else quarantine ([Metadata Equivalence](/design/metadata/#local-and-server-metadata-equivalence)).
- Verify and keep every upload's [`CustodyReceipt`](/design/transfer/durability/#custody-receipts); failure → write unconfirmed.
- Discarding the only copy of irreplaceable bytes requires a verified receipt, a `durable` [verdict](/design/transfer/durability/#verify-before-destroy), and a `verify_asset` accept. User deletes are not gated.
- Keep a per-user `directory_version` high-water mark; refuse and surface a regression.
- Refuse an OR-set remove of an unobserved `add_id`.
- Refuse a `revoke_all_sessions` without master-key proof.
- Decode remote-origin bytes only in the [sandboxed decoder](/design/clients/#sandboxed-decoder).

## Protocol and Capability Negotiation

One fail-closed, one-shot gate on every versioned surface, before any state is written.

### Universal Headers

| Header                       | Sent by                    | Meaning                                   |
| ---------------------------- | -------------------------- | ----------------------------------------- |
| `X-Capsule-Protocol`         | client/peer                | `YYYY-MM-DD` protocol version             |
| `X-Capsule-Crypto-Suite`     | client/peer on writes      | suite id ([inventory](/design/cryptography/primitives/#primitives-inventory)) |
| `X-Capsule-Sidecar-Schema`   | client (SDK: every request) | sidecar schema version                   |
| `X-Capsule-Protocol-Min/Max` | server, every response     | accepted protocol window                  |
| `X-Capsule-Min-Client-Build` | server                     | semver deprecation cutoff (advisory)      |

**Census of `X-Capsule-*`**: new headers MUST register here. By pointer: `X-Capsule-Offset`, `-Content-Length`, `-Upload-Status`, `-Checksum` (required on `PATCH`), `-Suggested-Chunk-Size` ([Transfer — Upload](/design/transfer/upload/#endpoints)); `X-Capsule-Identity-Key` (required on directory publish; invariant 23). `cohort_hash` is deliberately a body field ([Device Cohorts](/design/authentication/#device-cohorts)).

### Fail-Closed Rules

- Protocol outside `[Min, Max]` on a write → `426`. Reads of any past version succeed ([Versioning](/design/versioning/)).
- Unknown crypto suite or sidecar schema above max → `400`.
- Federation capability is an extra `401`/`403` layer; it never substitutes for the protocol header.

## Idempotency Invariants

Duplicates are no-ops; same key, different content is corruption. An unlisted surface is **not** idempotent.

| Surface                          | Key                                              | Duplicate                                                  |
| -------------------------------- | ------------------------------------------------ | ---------------------------------------------------------- |
| Upload chunk                     | `(upload_id, offset, chunk_hash)`                | Returns current offset                                     |
| Session creation                 | `(owner_id, hash, album_id)`                     | Active session returned; finalized → `409 error.upload.duplicate_blob` + existing asset |
| Lifecycle manifest               | `(asset_id, prior_provenance_hash, manifest_hash)` | Chain advances once                                      |
| Metadata update                  | op id + `(asset_id, prior_provenance_hash)`      | Structurally identical                                     |
| Federation capability proof      | `(peer_id, jti)`                                 | Same response                                              |
| Federation pull                  | `(peer_id, sync_cursor)`                         | Same page                                                  |
| MLS commit                       | Group commit chain (OpenMLS)                     | Rejected                                                   |
| [Album upgrade](/design/versioning/#album-upgrade-ceremony) / [MLS re-key](/design/mls-resilience/#group-re-keying-ceremony) | `intent_id` | Never forks or re-keys twice                  |
| [Device enrollment](/design/device-enrollment/#cross-device-add) | Single-use enrollment code | Re-redemption rejected                                 |
| Share/upload-link create         | Client op id                                     | Returns the minted link                                    |
| Share/upload-link revoke         | `link_id`                                        | No-op                                                      |
| Drop adoption                    | `drop_id` (invariant 32)                         | Returns the promoted asset                                 |
| Album roster publish             | `(album_id, roster_version)` (invariant 33)      | Identical bytes → `200 replayed`; different → `409 error.album.roster_stale` |

## Atomicity Invariants

- **Bundle finalization:** manifest, ciphertext, metadata and provenance commit in one transaction; failure → `FailedProcessing`, partials GC'd ([Atomic Writes](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery)).
- **Stack edits:** all `.tmp` staged, renamed together; any failure discards the bundle.
- **AMK epoch bump + write-tier rotation:** one MLS commit.
- **Album upgrade:** one MLS commit (`AlbumTombstone`) ([Versioning](/design/versioning/#album-upgrade-ceremony)).
- **Lifecycle manifest + provenance:** the same act — the entry *is* the manifest plus its chain link.
