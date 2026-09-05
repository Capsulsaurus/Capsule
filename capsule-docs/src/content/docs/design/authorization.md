---
title: Authorization
description: The closed lifecycle-action set and how every destructive operation is signed and audited
status: draft
---

Authorization in Capsule is **the same proof as a write**: every lifecycle transition — create, replace, delete, metadata-update, derivative add/replace, trash-restore — is an [asset manifest](/design/cryptography/provenance/#asset-manifest) signed under the album's per-epoch write-tier key. There is no weaker path to destroy data than to add it.

This rule pulls authorization decisions out of any single trust boundary: the server can refuse to execute (it cannot forge destruction), and the client can refuse to apply (it cannot be tricked by a server-asserted change). The logic lives in two places that share the same verification machinery: the planned `capsule-server::auth` module enforces structural envelope checks server-side, and `capsule-core::crypto::provenance` runs the [`verify_asset`](/design/cryptography/keys/#write-authorization) chokepoint client-side. Both pull from the same closed action enum below.

## The Closed Action Set

Every lifecycle operation's `action` field is one of the following **closed enum**. A value outside this set is a structural error, never a "future value to ignore" — see [Threat Model — Schema Rules](/design/threat-model/schema-rules/#closed-enums):

| Action               | Meaning                                                                                                                                            |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `create`             | First write of an asset; `prior_provenance_hash` is `null`.                                                                                        |
| `replace`            | Replace the original bytes (e.g. re-encryption under a new AMK epoch); same `file_id`/`album_id`, new ciphertext + manifest.                                                       |
| `delete`             | Soft-delete; the asset enters trash with a [retention window](/design/organization/#retention-window).                                             |
| `metadata-update`    | Edit to the encrypted metadata blob or sidecar fields.                                                                                             |
| `derivative-add`     | Add a thumbnail, preview, or embedding (see [Cryptography — Derivative Provenance](/design/cryptography/provenance/#derivative-provenance)). |
| `derivative-replace` | Replace an existing derivative — the only authorized path; a silent overwrite is rejected.                                                         |
| `trash-restore`      | Recover a soft-deleted asset from trash within its retention window.                                                                               |

Adding a value to this enum requires a new (later-dated) `protocol_version`, and old albums remain pinned to their original set — a faulty or new client cannot inject an unknown action into an older-pinned album.

## Authorizing a Lifecycle Operation

Authorization is established exactly as for a write:

- The operation must carry a valid signature under the album's per-epoch **write-tier key** — only writers at that epoch hold it.
- It must also carry the device's hybrid `device_sig` for provenance.
- A client acknowledges the operation only after **both** signatures verify through the single [`verify_asset`](/design/cryptography/keys/#write-authorization) chokepoint.
- The manifest's `prior_provenance_hash` must match the asset's current chain head — a stale or forked chain position is rejected (see [Cryptography — Provenance](/design/cryptography/provenance/#provenance-of-library-modifications)). This applies uniformly to every action except `create`.

A `delete` or `replace` is therefore authorized by the same proof as the original `create`: there is no weaker path to destroy data than to add it. Similarly, a `derivative-replace` is authorized as strongly as the original `derivative-add` — a buggy client cannot quietly poison a thumbnail.

## The Lifecycle Write Surface

Every non-upload lifecycle write — `delete`, `metadata-update`, `derivative-add`/`derivative-replace` when the manifest references already-stored blobs, and `trash-restore` — travels through **one generic REST endpoint**: `POST /v1/albums/{album_id}/ops`. (`create` and `replace`, and any derivative action carrying new bytes, ride the [upload protocol](/design/import/upload-protocol/) instead — a write that moves blob bytes is an upload by definition.) The body is the signed manifest bundle: the opaque canonical-CBOR manifest plus the encrypted metadata blob when the action carries one.

The endpoint is deliberately singular — one closed enum, one gate, one transaction shape:

- The structural envelope check above, [invariants 16–18](/design/threat-model/validation/#server-side-validation-invariants) and the metadata-blob binding ([invariant 25](/design/threat-model/validation/#server-side-validation-invariants)), runs **before any row is written**, for every action uniformly. A new action is a manifest-schema change under a new `protocol_version`, never a new endpoint.
- Accepted ops are **idempotent by content hash**: replaying a manifest whose content hash the server has already accepted returns the byte-identical prior response and writes nothing.
- The accepted op appends the provenance record and mints the per-album `sync_seq` in **one transaction**, the same finalization rule the [sync feed](/design/import/download-sync/) relies on — an op is visible on the feed exactly when it is durable.
- Rejections carry an [`error.*` code](/design/i18n/#server-error-codes) and write nothing; the rejection itself is logged.

The transport row lives in [API Surfaces](/design/api-surfaces/#surface--transport-map). Implementation is planned in `capsule-server::routes::ops` (slice `S-C16`, reusing the upload server's envelope gate).

## Album Membership on the Server

Album sharing between accounts is an MLS group whose roster the server cannot read, by design. What the server holds instead is the album owner's **signed roster** — the whole member list, each with a role of `reader` or `writer`, under a strictly monotonic `roster_version` and the AMK epoch it reflects, signed by a non-revoked device in the owner's published device directory and published at `PUT /v1/albums/{album_id}/roster` (slice `S-C51`; [invariant 33](/design/threat-model/validation/#server-side-validation-invariants)). Only the owner account publishes; removal is a later roster that omits the member, and the server records the version and epoch at which they vanished rather than deleting the row.

That stored fact widens two decisions that were owner-only until it existed. A **writer** on the current roster may write to the album through the upload path and `POST /v1/albums/{album_id}/ops`; the write is filed under the *owner's* namespace — the owner's feed is the one every member's devices read — and billed to the uploader. Any account on the roster, in either role, may fetch the album's blobs; a **former** member receives the `403` [Download & Sync](/design/import/download-sync/) describes; an account the roster never named gets the same `404` an unknown address gets. The roster is a transport control over who the server serves, never a confidentiality control: the server executes what the owner's signed statement permits, and, as below, authorizes nothing itself.

## The Server Executes But Never Authorizes

Per the principle of [trusting the server for storage, never for authorization](/design/cryptography/), the server **carries out** a remote delete or replace but is **never** the authority that permits it. A server-asserted lifecycle change with no valid write-tier signature is rejected by every client. This bounds the damage a compromised or buggy server can do: it can refuse to store data, but it cannot forge its destruction.

That said, the server is not *passive*. Even without keys, it enforces the structural envelope of every manifest before persisting it — `action` is in the closed enum, `prior_provenance_hash` matches the stored chain head, `created_by_device` is in the user's published device directory, the device's hybrid signature is structurally well-formed (correct curve, correct key lengths), `crypto_suite_id` is in the [inventory](/design/cryptography/primitives/#primitives-inventory) and `protocol_version` matches the album's pin (invariants 2 and 6), and the `timestamp` passes the [sanity bound](/design/threat-model/schema-rules/#timestamp-grammar). The full checklist is owned by [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants). A rejection here means no row is written and no provenance record is appended; the rejection itself is logged.

## Deletes Are Soft First

Destructive operations are staged, not immediate:

- A `delete` first soft-deletes the asset — it is flagged and moved to trash, recoverable for a retention window before any hard purge.
- The retention window is **signed into the delete manifest at delete time**, not server-configured, so a hostile server cannot accelerate or delay a user-configured window (see [Asset Organization — Recycling](/design/organization/#recycling)).
- Only after the window expires is the underlying blob hard-purged. A `trash-restore` action issued before expiry returns the asset to the live set and appends another provenance record — recovery is itself audited.

This is the [trash soft-delete recovery path](/design/cryptography/failure-modes/#redundant-recovery-paths) and gives a reversal window for both buggy and erroneous deletes.

## Every Transition Is Auditable

Each lifecycle operation emits a [provenance record](/design/cryptography/provenance/#provenance-of-library-modifications) — timestamp, device, client version, and action — anchored by the signed manifest. The chain is **append-only** (see [Threat Model — Provenance Immutability Rules](/design/threat-model/scenarios/#provenance-immutability-rules)): even an attacker holding every current key cannot rewrite a past record. This audit trail is what lets an operator distinguish a legitimate delete from a malicious or bug-induced one after the fact.

## Federated Peers

A lifecycle operation arriving from a [federated](/design/federation/) peer is subject to the same `verify_asset` check plus the server's structural envelope check; peer-asserted ordering and timestamps are never trusted for authorization. Peer attempts at [stale revival](/design/import/download-sync/#stale-revival-detection) — submitting an old-but-validly-signed manifest to resurrect a deleted asset — are caught by the `prior_provenance_hash` chain check and quarantined.

## Validation

- **Per-action signing/verify (unit).** Each of the seven actions gets a unit test: build a manifest of that action, sign with the correct (device DSK, epoch write-tier) pair, run `verify_asset`, assert acceptance. Then build the same with the wrong write-tier key, wrong device, missing `prior_provenance_hash`, wrong `prior_provenance_hash`; assert rejection with the right structural code.
- **Closed-enum rejection (unit).** Submit a manifest with `action = "future-action-not-yet-defined"`; assert structural rejection at the envelope layer.
- **Stale-chain detection (unit).** Build a delete-then-restore chain; submit a second delete with a stale `prior_provenance_hash`; assert quarantine.
- **Server-side envelope (smoke).** All [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants) items 16–18 (non-upload action manifests) exercised against a real Postgres.

The cross-module case — full lifecycle (create → metadata-update → trash → restore → re-delete → hard-purge after retention) across server + client — is bounded E2E surface in [Module Map](/design/module-map/#e2e-test-surface).
