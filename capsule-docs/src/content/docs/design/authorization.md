---
title: Authorization
description: Ensuring access is done by someone authorized
---

We want to pull out all authorization-related logic (validated by both server and client) into a centralized core to minimize implementation risks and isolating sensitive code to enforce authorization end-to-end. Both server and client validate against the same core, so a client cannot be tricked into accepting an operation the server would reject, and vice versa.

## Asset Lifecycle

**Key Problem:** Clients may want to destructively delete or replace assets, which servers must execute remotely. We want robust, centralized control over the lifecycle of every asset.

Capsule treats every lifecycle transition as an authorized, signed, auditable operation. The design reuses the cryptographic machinery already defined for asset writes rather than inventing a parallel mechanism.

### The Closed Action Set

Every lifecycle operation is expressed as an [asset manifest](/design/cryptography/#provenance-and-signed-manifest) whose `action` field is one of the following **closed enum** (a value outside this set is a structural error, never a "future value to ignore" — see [Threat Model — Schema Evolution and Field Grammar](/design/threat-model/#schema-evolution-and-field-grammar)):

| Action               | Meaning                                                                                                                                 |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `create`             | First write of an asset; `prior_provenance_hash` is `null`.                                                                             |
| `replace`            | Replace the original bytes (e.g. a re-encryption under a new AMK epoch); identity preserved.                                            |
| `delete`             | Soft-delete; the asset enters trash with a [retention window](/design/organization/#recycling).                                         |
| `metadata-update`    | Edit to the encrypted metadata blob or sidecar fields.                                                                                  |
| `derivative-add`     | Add a thumbnail, preview, LQIP, or embedding (see [Cryptography — Derivative Provenance](/design/cryptography/#derivative-provenance)). |
| `derivative-replace` | Replace an existing derivative — the only authorized path; a silent overwrite is rejected.                                              |
| `trash-restore`      | Recover a soft-deleted asset from trash within its retention window.                                                                    |

Adding a value to this enum bumps `protocol_version` and the old albums remain pinned to their original set — a faulty or new client cannot inject an unknown action into a v_k album.

### Authorizing a lifecycle operation

Authorization is established exactly as for a write:

- The operation must carry a valid signature under the album's per-epoch **write-tier key** — only writers at that epoch hold it.
- It must also carry the device's hybrid `device_sig` for provenance.
- A client acknowledges the operation only after **both** signatures verify through the single [`verify_asset`](/design/cryptography/#write-authorization) chokepoint.
- The manifest's `prior_provenance_hash` must match the asset's current chain head — a stale or forked chain position is rejected (see [Cryptography — Provenance of Library Modifications](/design/cryptography/#provenance-of-library-modifications)). This applies uniformly to every action except `create`.

A `delete` or `replace` is therefore authorized by the same proof as the original `create`: there is no weaker path to destroy data than to add it. Similarly, a `derivative-replace` is authorized as strongly as the original `derivative-add` — a buggy client cannot quietly poison a thumbnail.

### The server executes but never authorizes

Per the principle of [trusting the server for storage, never for authorization](/design/cryptography/#implementation), the server **carries out** a remote delete or replace but is **never** the authority that permits it. A server-asserted lifecycle change with no valid write-tier signature is rejected by every client. This bounds the damage a compromised or buggy server can do: it can refuse to store data, but it cannot forge its destruction.

That said, the server is not *passive*. Even without keys, it enforces the structural envelope of every manifest before persisting it — `action` is in the closed enum, `prior_provenance_hash` matches the stored chain head, `created_by_device` is in the user's published device directory, the device's hybrid signature is structurally well-formed (correct curve, correct key lengths), `crypto_suite_id` and `protocol_version` match the album's pin, and the timestamp is within the ±30-day window. The full checklist is owned by [Threat Model — Server-Side Validation Invariants](/design/threat-model/#server-side-validation-invariants). A rejection here means no row is written and no provenance record is appended; the rejection itself is logged.

### Deletes are soft first

Destructive operations are staged, not immediate:

- A `delete` first soft-deletes the asset — it is flagged and moved to trash, recoverable for a retention window before any hard purge.
- The retention window is **signed into the delete manifest at delete time**, not server-configured, so a hostile server cannot accelerate or delay a user-configured window (see [Asset Organization — Recycling](/design/organization/#recycling)).
- Only after the window expires is the underlying blob hard-purged. A `trash-restore` action issued before expiry returns the asset to the live set and appends another provenance record — recovery is itself audited.

This is the [trash soft-delete recovery path](/design/cryptography/#failure-modes-and-recovery) and gives a reversal window for both buggy and erroneous deletes.

### Every transition is auditable

Each lifecycle operation emits a [provenance record](/design/cryptography/#provenance-of-library-modifications) — timestamp, device, client version, and action — anchored by the signed manifest. The chain is **append-only** (see [Threat Model — Provenance Immutability Rules](/design/threat-model/#provenance-immutability-rules)): even an attacker holding every current key cannot rewrite a past record. This audit trail is what lets an operator distinguish a legitimate delete from a malicious or bug-induced one after the fact.

### Federated peers

A lifecycle operation arriving from a [federated](/design/federation/) peer is subject to the same `verify_asset` check plus the server's structural envelope check; peer-asserted ordering and timestamps are never trusted for authorization. Peer attempts at [stale revival](/design/import-synchronization/#stale-revival-detection) — submitting an old-but-validly-signed manifest to resurrect a deleted asset — are caught by the `prior_provenance_hash` chain check and quarantined.
