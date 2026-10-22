---
title: Durability
description: Storage verification, deep re-hash, custody receipts, proof of loss, and the gate a client passes before releasing a local original
status: draft
---

Finalization's `Completed` is a one-time transfer acknowledgement, and [`verify_asset`](/design/cryptography/keys/#write-authorization) proves cryptographic validity, never that the server still holds the bytes. This page defines the re-checkable durability verdict, the signed custody receipt, and the rule every client follows before destroying local data — the [transfer protocol's](/design/transfer/) invariants 6 and 7. Server: `capsule-server::verify`, `capsule-server::attestation`; client gate: `capsule-core::library::ReleaseGate`, driven by `capsule-sdk::verify`.

## What "Safely Stored" Means

`durable` is the conjunction, over **every hash the client declared** for the asset, of three key-free facts:

- **indexed** — a live asset row of the caller's references the address;
- **stored** — the bytes are present at the content address in the blob store;
- **retrievable** — indexed ∧ stored ∧ nothing withholds it: not marked by [GC](/design/filesystem/server/#deletion-and-garbage-collection), not quarantined, not under a moderation hold.

`durable` attests **this home server's** storage only — not replicas, peers or [backups](/design/backup-recovery/).

## Endpoint

`POST /v1/storage/verify` takes a list of assets, each with the exact blob hashes the client relies on, and returns one verdict per asset with a per-blob breakdown and the server's `checked_at`. It is a read, idempotent and cheap. The shape is in the [OpenAPI contract](/reference/api/).

- **Fail closed.** An unknown asset is not durable; a collaborator that cannot answer is an error, never an optimistic verdict. An empty declaration is `400`, not a vacuous `durable: true`.
- **Owner-scoped.** Another account's asset is indistinguishable from a non-existent one.
- **No existence oracle.** `indexed` is checked first: a hash the asset does not reference reports `stored = false, indexed = false` even if the store holds those bytes. Every declared hash appears in the response, in order.
- **`deep`** re-reads and re-hashes the declared blobs, reporting `intact`, `corrupt` or `rate_limited` per blob. It is rate-limited per account; a throttled request still returns the structural verdict. `deep` is **absent** when no scan ran, which a client MUST distinguish from `intact`. Results are not coalesced: a cached integrity result would be a durability claim with an undisclosed age.

## Custody Receipts

A verdict says the bytes are serveable *now*. A **custody receipt** proves the server *accepted* them, so "the server lost my photo" and "the client never uploaded it" stop being symmetric claims. The type is `capsule-core::crypto::receipts::CustodyReceipt`, shared by client and server.

- **Contents.** Server identity and signing-key id, a per-server `receipt_seq` and `prior_receipt_hash`, the upload and asset ids, the blob role, the **server-recomputed** `ciphertext_hash` and size, the uploader, and `received_at` from the server clock. `envelope_hash` (SHA-256 over the signed manifest bytes) is present only on the `provenance` blob's receipt; the manifest already commits to the other blobs.
- **Signed** under the server attestation key, a long-lived hybrid Ed25519 + ML-DSA-65 key distinct from the [operational key](/design/federation/#server-identity-and-key-rotation), with an append-only key history so old receipts verify forever.
- **Issued with custody**, in the same transaction as the index record ([Upload — Finalization](/design/transfer/upload/#finalization-and-integrity)). There is no receipt without custody and no indexed blob without a receipt.
- **Hash-chained and append-only.** `receipt_seq` and `prior_receipt_hash` form the server's receipt log; no API path overwrites or deletes an entry. Position allocation and signing happen in one operation, so the chain cannot fork.
- **Fetched** via `GET /v1/upload/{id}/receipt` within the session window and `GET /v1/assets/{asset_id}/receipts` durably. The durable fetch is **owner-only**; a non-owner gets the unknown-asset `404`.
- **Verified and kept by the client.** The client checks the signature under the pinned key, that `ciphertext_hash`, `size` and `envelope_hash` match what it sent, and a gross clock-drift bound on `received_at`. It stores receipts beside the provenance chain and in the backup artifact; the client copy is evidence, not cache.

## Signed Storage Attestation

The verdict MAY be requested in a signed form carrying a client nonce and signed under the attestation key, so an honest server's self-reported loss and periodic audits are retainable evidence. The type is `StorageAttestation` in `capsule-core::crypto::storage_attestation`, shared by client and server.

- **Requested** by sending `attestation_nonce` — 32 bytes the client draws fresh per request, as lowercase hex — on `POST /v1/storage/verify`. The response then also carries `attestation`, the canonical-CBOR attestation in base64.
- **Contents.** Server identity and signing-key id, the account the verdicts were computed for, the client's nonce, the server's `checked_at`, and the same per-asset, per-blob verdicts the unsigned response carries, `deep` included when a scan ran. The `storage-attestation/v1` version and field set keep it from being mistaken for a custody receipt, which the same key signs.
- **Priced like `deep`.** Signing spends the account's deep-verify budget, charged once per request that asks for either. A throttled signed request is refused whole with `429` and `Retry-After`, never downgraded to an unsigned answer. A client that asked for evidence and got none would not know it.
- **Verified by the client.** The signature verifies under the pinned attestation key, the nonce is the one the client sent, and the verdicts answer exactly the assets and declared hashes asked about, in order. A replayed or tampered attestation fails one of these checks. The client acts on the verdicts read from the signed core, and keeps the bytes as evidence.

## Proof of Loss

A client proves loss with: (1) a receipt for hash `H`; (2) later non-holding — a signed attestation reporting `H` not stored, or a failed content-addressed fetch, which is self-verifying; and (3) the absence of an authorized purge. The server's rebuttal is a device-signed `delete` with an elapsed [retention window](/design/organization/#retention-window). Conversely, a custody claim with no valid receipt is unbacked. A receipt from another server fails on its server binding. Accepted v1 limitation: a server can show different log views to different auditors; gossiped signed log heads are the planned extension.

## Verify Before Destroy

Before any post-write local cleanup that drops irreplaceable bytes, a client MUST hold all three:

1. a verified [custody receipt](#custody-receipts) for the write;
2. `durable = true` from `/storage/verify` for every declared blob;
3. `verify_asset` acceptance.

A transport failure on any check retains the bytes. A non-durable verdict never triggers a destructive action: the client retains the copy, retries with backoff, and shows "not yet confirmed on server". A server that withholds receipts therefore never becomes the sole holder of an only copy.

The gate applies to:

- releasing or evicting a **device-owned original** ([Space Recovery](/design/filesystem/client/#space-recovery));
- **Move-mode** source deletion after import. Not yet enforced; tracked by #671: `import --move` outside streaming mode does not wait on the gate;
- **streaming import** release ([Pipeline](/design/import/pipeline/#import-upload-streaming-mode));
- discarding local working state tied to a write (staged temporaries, the pre-edit copy across a `replace`).
- releasing an original handed to a [delegate](/design/peering/#delegation): the receipt is fetched from the server by the origin and checked against the hash the origin computed, never relayed by the delegate.

It does not apply to intentional deletes (governed by the retention window), to rebuildable derivative cache, or to re-fetchable server-origin blobs. The SQLite catalog is **not** rebuildable cache: it [migrates in place](/design/versioning/#client-catalog-migration) and evicting it is a repair action.

The verdict is point-in-time: the client re-verifies if a bounded interval passes before release, and on the server a blob that just answered `durable` cannot be deleted faster than the GC grace window. Together these make the gate sound without a lease.
