---
title: Authorization
description: The closed lifecycle-action set and how every destructive operation is signed and audited
---

Authorization in Capsule is **the same proof as a write**: every lifecycle transition — create, replace, delete, metadata-update, derivative add/replace, trash-restore — is an [asset manifest](/design/cryptography/provenance/#asset-manifest) signed under the album's per-epoch write-tier key. There is no weaker path to destroy data than to add it.

This rule pulls authorization decisions out of any single trust boundary: the server can refuse to execute (it cannot forge destruction), and the client can refuse to apply (it cannot be tricked by a server-asserted change). The logic lives in two places that share the same verification machinery: `capsule-api-auth::roles` enforces structural envelope checks server-side, and `capsule-core::crypto::provenance` runs the [`verify_asset`](/design/cryptography/keys/#write-authorization) chokepoint client-side. Both pull from the same closed action enum below.

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

Adding a value to this enum bumps `protocol_version` and old albums remain pinned to their original set — a faulty or new client cannot inject an unknown action into a v_k album.

## Authorizing a Lifecycle Operation

Authorization is established exactly as for a write:

- The operation must carry a valid signature under the album's per-epoch **write-tier key** — only writers at that epoch hold it.
- It must also carry the device's hybrid `device_sig` for provenance.
- A client acknowledges the operation only after **both** signatures verify through the single [`verify_asset`](/design/cryptography/keys/#write-authorization) chokepoint.
- The manifest's `prior_provenance_hash` must match the asset's current chain head — a stale or forked chain position is rejected (see [Cryptography — Provenance](/design/cryptography/provenance/#provenance-of-library-modifications)). This applies uniformly to every action except `create`.

A `delete` or `replace` is therefore authorized by the same proof as the original `create`: there is no weaker path to destroy data than to add it. Similarly, a `derivative-replace` is authorized as strongly as the original `derivative-add` — a buggy client cannot quietly poison a thumbnail.

## The Server Executes But Never Authorizes

Per the principle of [trusting the server for storage, never for authorization](/design/cryptography/), the server **carries out** a remote delete or replace but is **never** the authority that permits it. A server-asserted lifecycle change with no valid write-tier signature is rejected by every client. This bounds the damage a compromised or buggy server can do: it can refuse to store data, but it cannot forge its destruction.

That said, the server is not *passive*. Even without keys, it enforces the structural envelope of every manifest before persisting it — `action` is in the closed enum, `prior_provenance_hash` matches the stored chain head, `created_by_device` is in the user's published device directory, the device's hybrid signature is structurally well-formed (correct curve, correct key lengths), `crypto_suite_id` and `protocol_version` match the album's pin, and the `timestamp` passes the [sanity bound](/design/threat-model/schema-rules/#timestamp-grammar). The full checklist is owned by [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants). A rejection here means no row is written and no provenance record is appended; the rejection itself is logged.

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
