---
title: Threat Model
description: How Capsule contains damage from faulty, malicious, or version-mismatched clients
---

This doc catalogues the ways a client can damage user data, the invariant in each owner doc that defeats each scenario, and the universal rules that bind them — protocol negotiation, server-side validation duties, idempotency, atomicity, and provenance immutability.

It is **not** a primitives doc. Every primitive Capsule uses is declared in its [owner doc](/design/principles/#single-source-of-truth); this doc references those declarations rather than re-stating them. Where a specific invariant lives, the relevant owner doc enforces it; where a *defense* spans multiple docs, the canonical statement lives here.

## Purpose and Scope

E2EE shifts most of the trust to the client. The server holds no keys; clients write the canonical state. That makes the question "what damage can a client cause?" load-bearing for the design — a single buggy implementation, a hostile keyholder inside an album, a stranded old build, or a too-new prototype all have to fail safely.

A faulty, malicious, or version-mismatched client must not be able to cause **irreparable** damage (loss of original bytes, loss of audit trail, undetected silent overwrite of user intent) and should not be able to cause more than **transient** damage (a quarantined asset surfaces to the user; a rejected write returns a clear error; a divergence is detected and reconciled). The recovery paths in [Cryptography — Failure Modes and Recovery](/design/cryptography/#failure-modes-and-recovery) cover key loss; this doc covers the *write-path* harm a wrong-but-signed client can attempt.

## Client Class Taxonomy

Every client request can be classified by one of these models. The defenses listed below apply to **all** of them — none of them are trusted to enforce their own correctness:

| Class         | Description                                                                                                                                                                  | What authenticates them                                                       | What stops them                                                                                                                                                                     |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Honest**    | Conforming implementation, correct keys, correct version.                                                                                                                    | Session token + access token + device DSK + epoch write-tier signature.       | Nothing to stop. This is the baseline.                                                                                                                                              |
| **Faulty**    | Conforming intent, buggy implementation. Writes structurally invalid or semantically wrong manifests under real keys.                                                        | Same as honest — the keys are correct.                                        | Server-side [structural validation](#server-side-validation-invariants) + client-side [`verify_asset`](/design/cryptography/#write-authorization) chokepoint + quarantine surfaces. |
| **Malicious** | Adversary in possession of a current device's DSK and the album's epoch write-tier key. Writes deliberately malformed or destructive operations.                             | Same as honest — the keys are real, because the adversary owns them.          | Provenance chain immutability + soft-delete window + per-album/per-event compartmentalization + audit trail for after-the-fact attribution.                                         |
| **Old**       | A signed-in client that predates a feature, schema, or suite the server now considers minimum. Cannot produce structurally valid writes for albums pinned above its version. | Authenticated, but `X-Capsule-Protocol` is below the server's accepted range. | [Protocol handshake](#protocol-and-capability-negotiation) rejects writes with `426 Upgrade Required` before any state is written.                                                  |
| **New**       | A prototype or staging build that writes a `protocol_version`/`crypto_suite_id`/`sidecar_schema` ahead of what the receiver knows.                                           | Authenticated, but the version is higher than the receiver's max known.       | Receiver's refuse-by-default rule on unknown enum values, unknown schemas, and forward-jumping protocol versions; closed schema evolution boundary (see below).                     |

The deliberate choice in the matrix above: a *malicious* client with real keys is the hardest to stop, because confidentiality and authentication don't help when the adversary already holds the keys. Capsule's response is to ensure such an adversary can do nothing **silently** — every write produces a signed provenance record, soft-delete is the default, and history is append-only. The audit trail is the recovery surface.

## Damage Containment Layers

Restating the boundary hierarchy from [Core Principles](/design/principles/) as concentric containment shells, with the owner doc that enforces each:

| Shell                     | Boundary                                                         | Owner doc                                                                                                       |
| ------------------------- | ---------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| **Per-version**           | Album protocol pinning isolates a buggy v_k from v_{k-1} albums. | [Versioning](/design/versioning/#album-protocol-version-pinning)                                                |
| **Per-album**             | MLS group + per-epoch AMK + per-epoch write-tier key.            | [Cryptography — Group Membership](/design/cryptography/#group-membership)                                       |
| **Per-event** (manifest)  | Each lifecycle action is its own signed, chained record.         | [Cryptography — Provenance of Library Modifications](/design/cryptography/#provenance-of-library-modifications) |
| **Per-user**              | Owner Group Key, sponsored-account isolation.                    | [Cryptography — Owner Group Keys](/design/cryptography/#owner-group-keys-ogks)                                  |
| **Per-peer** (federation) | Capability tokens, error budgets, quarantine for new peers.      | [Federation](/design/federation/)                                                                               |
| **Per-device** (peering)  | Device directory enforced via the TLS handshake.                 | [Peering — Establishing the Channel](/design/peering/#establishing-the-channel)                                 |

A bug or compromise on one side of any shell cannot cross it.

## Damage Scenario → Invariant Map

The lookup table for "what damage X is prevented by which invariant Y in which doc Z." Each row names a concrete vector found during the audit and the single owner-doc anchor that defeats it.

| #   | Damage scenario                                                                            | Defense                                                                                                                                                           | Owner doc                                                                                                                                                         |
| --- | ------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Old client writes a sidecar after stripping unknown fields                                 | Sidecar signature covers `_unknown`; old client refuses to write when `sidecar_schema` > its max known                                                            | [Metadata — Schema Versioning Rules](/design/metadata/#schema-versioning-rules)                                                                                   |
| 2   | Faulty client uploads bytes that don't match the declared content type                     | Server's `content_type` allow-list per protocol version (no-key check) + receiving client decoder sandbox                                                         | [Threat Model §5](#server-side-validation-invariants), [Clients — Sandboxed Decoder](/design/clients/#sandboxed-decoder)                                          |
| 3   | Buggy client uploads chunk with wrong offset and re-tries                                  | Idempotency tuple `(upload_id, offset, chunk_hash)`; duplicate at offset with different hash → reject                                                             | [Import & Sync — Upload Protocol](/design/import-synchronization/#upload-protocol)                                                                                |
| 4   | Hostile peer sends an old-but-validly-signed manifest to revive a deleted asset            | `prior_provenance_hash` chain advance check on both client and server                                                                                             | [Cryptography — Provenance](/design/cryptography/#provenance-of-library-modifications), [§ Server-Side Validation Invariants](#server-side-validation-invariants) |
| 5   | Malicious client re-signs an existing manifest under a weaker `crypto_suite_id`            | Signatures cover `crypto_suite_id` and `protocol_version`                                                                                                         | [Cryptography — Write Authorization](/design/cryptography/#write-authorization)                                                                                   |
| 6   | Two devices concurrently caption the same photo                                            | Caption LWW + `superseded_captions` array surfaces the loser                                                                                                      | [Metadata — Surfacing Concurrent Edits](/design/metadata/#surfacing-concurrent-edits)                                                                             |
| 7   | Client issues an OR-set remove for an element it never observed an add for                 | Add-id binding: removes target a specific `add_id`; unknown `add_id` is rejected                                                                                  | [Metadata — Add-id Binding](/design/metadata/#add-id-binding)                                                                                                     |
| 8   | Buggy client overwrites a good thumbnail with a corrupt one                                | Every derivative carries a signed `DerivativeManifest` on its own chain; overwrite is a `derivative-replace` lifecycle action                                     | [Cryptography — Derivative Provenance](/design/cryptography/#derivative-provenance)                                                                               |
| 9   | A client declares `timestamp = 2099-01-01` to distort the audit                            | Server rejects timestamp outside ±30 days of server clock at accept                                                                                               | [Cryptography — Write Authorization](/design/cryptography/#write-authorization)                                                                                   |
| 10  | Server-side TOCTOU on blob dedup creates a duplicate                                       | Dedup-check and pending-row insert are atomic on a single Postgres transaction                                                                                    | [Filesystem — Content-Addressing and Deduplication](/design/filesystem/#content-addressing-and-deduplication)                                                     |
| 11  | A faulty client uploads bytes that exceed its declared size                                | Server bounds cumulative received at every chunk, not only at finalization                                                                                        | [Import & Sync — Chunk rules](/design/import-synchronization/#upload-protocol)                                                                                    |
| 12  | A new client writes a manifest with a `crypto_suite_id` the server does not recognize      | Refuse-by-default at handshake: 400 before any session is created                                                                                                 | [§ Protocol and Capability Negotiation](#protocol-and-capability-negotiation)                                                                                     |
| 13  | A federated peer floods the rejected-hash table to exhaust memory                          | Per-peer quota; bounded LRU memory                                                                                                                                | [Federation — Soft-Fail Semantics](/design/federation/#soft-fail-semantics)                                                                                       |
| 14  | A model swap silently invalidates the AI tag namespace                                     | Every `tags_ai` entry carries `model_id`+`model_version`; cross-model comparison is forbidden                                                                     | [Metadata — Tag Provenance and Namespacing](/design/metadata/#tag-provenance-and-namespacing)                                                                     |
| 15  | A leaked session token revokes all of a user's other sessions to lock them out             | `revoke_all_sessions` requires master-key proof, not session auth                                                                                                 | [Authentication — Explicit revocation](/design/authentication/#explicit-revocation)                                                                               |
| 16  | An attacker holding every current key tries to rewrite the asset's history                 | Provenance chain references each predecessor's hash; rewriting any past record requires forging an earlier (possibly retired) device's hybrid signature           | [Cryptography — Provenance of Library Modifications](/design/cryptography/#provenance-of-library-modifications)                                                   |
| 17  | A client picks a random `amk_version` to skip MLS                                          | Server's no-key check: `amk_version` must be monotonic per album and known to the server                                                                          | [§ Server-Side Validation Invariants](#server-side-validation-invariants)                                                                                         |
| 18  | A v_old client tries to write into an album that has been upgraded to v_new                | Album pinning + upgrade ceremony quiescence: server returns `409` for writes carrying a stale `intent_id`                                                         | [Versioning — Album Upgrade Ceremony](/design/versioning/#album-upgrade-ceremony)                                                                                 |
| 19  | A malformed CBOR sidecar lands on disk after a crash mid-write                             | Malformed sidecar → quarantined to `.library/quarantine/`; never silent-skipped                                                                                   | [Filesystem — Repair](/design/filesystem/#repair)                                                                                                                 |
| 20  | A federation pull returns a manifest claiming a device that's not in the user's directory  | Server's no-key check: `created_by_device` must be in the user's published device directory                                                                       | [§ Server-Side Validation Invariants](#server-side-validation-invariants)                                                                                         |
| 21  | A buggy client uploads a metadata blob with a hand-crafted wire format                     | Metadata blob wire format is byte-exact; mismatched envelope rejected at decode                                                                                   | [Cryptography — Metadata Blob Wire Format](/design/cryptography/#metadata-blob-wire-format)                                                                       |
| 22  | A retry of a delete manifest decrements blob refcount twice                                | Manifest idempotency keyed by `prior_provenance_hash`: a duplicate manifest is a no-op                                                                            | [§ Idempotency Invariants](#idempotency-invariants)                                                                                                               |
| 23  | A backup restore from 6 months ago silently overwrites current state                       | Restore-as-chain-fork: every restored manifest with a stale `prior_provenance_hash` is quarantined and surfaced for explicit merge                                | [Backup & Recovery — Backup Verification](/design/backup-recovery/#backup-verification) ([open question](#open-questions))                                        |
| 24  | A new device claims its key is older than the account itself                               | Device entry in the device directory is signed by the IK and carries `added_at`; a server rejects an upload from a device whose `added_at` postdates the manifest | [Cryptography — Device Keys](/design/cryptography/#device-keys), [§ Server-Side Validation Invariants](#server-side-validation-invariants)                        |
| 25  | A peer floods notifications to make Capsule pull garbage                                   | Notifications are advisory; pull is on Capsule's schedule and goes through full validation                                                                        | [Federation — Pull-Only Federation](/design/federation/#pull-only-federation)                                                                                     |
| 26  | A federated server's TLS endpoint silently changes its public key                          | Servers cache each other's keys; rotation requires a notary endpoint co-sign                                                                                      | [Federation — Server Identity and Key Rotation](/design/federation/#server-identity-and-key-rotation)                                                             |
| 27  | A buggy client writes a stack edit that updates one member's sidecar and not the others    | Stack edits are bundle-atomic: all `.tmp` files staged first, all renamed together; any rename failure discards the bundle                                        | [Filesystem — Atomic Writes and Crash Recovery](/design/filesystem/#atomic-writes-and-crash-recovery)                                                             |
| 28  | A federated peer serves a stale capability token after revocation                          | Capability TTL ≤ 24h + published revocation list polled ≤ 15 min                                                                                                  | [Federation — Federation Capabilities](/design/federation/#federation-capabilities)                                                                               |
| 29  | A faulty client uploads embeddings derived from a model the receiver does not run          | Vector index refuses inserts whose `model_id` is unknown                                                                                                          | [ML Models — Embedding Provenance](/design/ml-models/#embedding-provenance)                                                                                       |
| 30  | A client tries to write directly to a server-derived field (e.g. computed ciphertext hash) | Server recomputes ciphertext hash at finalization and rejects mismatch                                                                                            | [Import & Sync — Finalization and Integrity](/design/import-synchronization/#finalization-and-integrity)                                                          |

When a scenario surfaces during implementation that does not match any of the above, the rule is: add a row here, then declare the defense in exactly one owner doc. Never restate a defense in multiple docs.

## Server-Side Validation Invariants

The server holds no keys — it cannot verify any signature against a key it owns. But it **does** validate the *structure* of every write before persisting state. These checks are refuse-by-default and intentionally exhaustive; a buggy server that skips one of them silently widens the blast radius for the entire client class taxonomy above.

This list is the canonical statement; [Filesystem](/design/filesystem/), [Import & Synchronization](/design/import-synchronization/), [Federation](/design/federation/), [Authorization](/design/authorization/), and [Authentication](/design/authentication/) reference it without restating.

### On `POST /upload` (session creation)

1. `X-Capsule-Protocol` is within the server's `[Min, Max]` range. Otherwise `426 Upgrade Required`, no session created.
2. `crypto_suite_id` is a row of the [Primitives Inventory](/design/cryptography/#primitives-inventory). Otherwise `400`.
3. `hash.algo` matches the algorithm declared by `crypto_suite_id`. Otherwise `400`.
4. `size` ∈ (0, `max_file_size`]. Otherwise `400` / `413`.
5. `content_type` ∈ closed enum for this protocol version. Otherwise `400`.
6. `album_id` exists; authenticated user has server-visible write capability on it; album's pinned `protocol_version` equals the request's. Otherwise `403`.
7. `created_by_device` is in the user's published device directory, and the directory entry's `added_at` precedes the request's `timestamp`. Otherwise `403`.
8. `timestamp` is within ±30 days of server clock. Otherwise `400`.

### On each `PATCH /upload/{id}` chunk

9. Offset is exactly the current received-byte count. Otherwise `409`, with `X-Capsule-Offset` returned.
10. Non-final chunk size is a multiple of 4 KiB. Otherwise `400`.
11. Cumulative received ≤ declared `size`. Otherwise `400` / `413`, session moves to `FailedProcessing`.
12. The `(upload_id, offset, chunk_hash)` idempotency tuple is new OR matches an exact prior PATCH. Otherwise (same offset, different hash) `409` + corruption error.

### At finalization

13. Total received == declared `size`. Otherwise `FailedProcessing`.
14. Recomputed ciphertext hash == declared `hash.value`. Otherwise `FailedProcessing` + corruption error.
15. Manifest envelope re-validated (rerun 1–8) inside the finalization transaction.

### On non-upload writes (lifecycle action manifest, metadata-update, derivative-add/replace, trash-restore)

16. `action` is in the closed enum. Otherwise `400`.
17. `prior_provenance_hash` equals the last accepted manifest's content hash for this `asset_id`. Otherwise `409` (stale-revival).
18. `amk_version` is monotonic per album (never regresses). Otherwise `400`.

### On federation pull (server-to-server)

19. Capability token verifies under home server's signing key; `exp` in future; `jti` not in revocation list (cached ≤ 15 min). Otherwise `401` / `403`.
20. All checks (1)–(18) re-applied — federation does not unlock looser rules.
21. Per-peer rate budgets unbroken (events/hour, bytes/hour, CPU/hour). Otherwise `429`.

Every rejection is logged with a structured reason code; the rejected hash is remembered (bounded, see [Federation — Soft-Fail Semantics](/design/federation/#soft-fail-semantics)) so divergence between Capsule's view and a permissive peer's view is detectable.

## Client-Side Validation Invariants

Mirror checklist that every client implements before applying any received data — local or remote. A client that skips one of these is in the *faulty* class.

- Run [`verify_asset`](/design/cryptography/#write-authorization) on every received `AssetManifest`. Quarantine on failure; never silent-drop, never silent-accept.
- Reject an incoming `sidecar_schema` greater than the client's `max_known_sidecar_schema`. Refuse to write that sidecar; refuse to read in normal mode (read-only opt-in is allowed).
- Reject an incoming `protocol_version` outside `[Min, Max]` known to the client. The same handshake the server runs.
- Reject an unknown enum value for any field whose enum is closed at the current schema (notably `action`, `content_type`, `gps.source`, `DerivativeManifest.role`). Unknown CBOR map keys are preserved per [Postel's Law](/design/principles/) and never executed.
- Maintain a local `latest_provenance_hash` per `asset_id`. Refuse to apply any manifest whose `prior_provenance_hash` is behind the local value. Surface it.
- Reject an OR-set remove whose `add_id` was never observed locally as an add.
- Refuse to follow a `revoke_all_sessions` confirmation that did not include a master-key proof.
- Decode remote-origin asset bytes only in the [sandboxed decoder](/design/clients/#sandboxed-decoder).

## Protocol and Capability Negotiation

Every versioned API surface — client-to-server uploads, sync feed, federation pull, peering — runs the same compatibility gate. The gate is **fail-closed**: a mismatch is a hard reject before any state is written, never a silent degrade.

### Universal Headers

| Header                       | Sent by                   | Meaning                                                                                    |
| ---------------------------- | ------------------------- | ------------------------------------------------------------------------------------------ |
| `X-Capsule-Protocol`         | client / peer             | `YYYY-MM-DD` protocol version the request is written against                               |
| `X-Capsule-Crypto-Suite`     | client / peer on writes   | `u16` suite id from the [Primitives Inventory](/design/cryptography/#primitives-inventory) |
| `X-Capsule-Sidecar-Schema`   | client on metadata-update | `u16` schema version declared at `sidecar_schema` field 0                                  |
| `X-Capsule-Protocol-Min`     | server on every response  | the lowest protocol version this server accepts                                            |
| `X-Capsule-Protocol-Max`     | server on every response  | the highest protocol version this server accepts                                           |
| `X-Capsule-Min-Client-Build` | server on responses       | semver deprecation cutoff; advisory unless the path is hard-deprecated                     |

### Fail-Closed Rules

- `X-Capsule-Protocol` outside `[Min, Max]` on a **write**: `426 Upgrade Required`. No session created, no row written.
- `X-Capsule-Crypto-Suite` not in the inventory: `400 Bad Request`.
- `X-Capsule-Sidecar-Schema` above the server's max known: `400 Bad Request`. (The server does not parse sidecars itself, but it refuses to acknowledge writes whose schema number it does not index.)
- **Reads of any past version succeed.** Read invariants are deliberately stable per [Versioning](/design/versioning/), so a current server still serves v_{k-N} blobs from years ago.
- Federation capability is an additional `401` / `403` layer on top of the protocol gate. A valid token never substitutes for a valid protocol header.

The handshake is **one-shot per request**, not a negotiation. Either both sides agree by inspection, or the request fails. There is no back-and-forth that could leak partial state.

## Idempotency Invariants

Every write surface has a single idempotency key. Duplicates are no-ops; conflicts (same key, different content) are corruption errors.

| Surface                             | Idempotency key                                                                    | Duplicate behavior                                |
| ----------------------------------- | ---------------------------------------------------------------------------------- | ------------------------------------------------- |
| Upload chunk (`PATCH /upload/{id}`) | `(upload_id, offset, chunk_hash)`                                                  | Returns current offset; no double-write           |
| Session creation (`POST /upload`)   | `(owner_id, hash.value, album_id)` — server's existing dedup check                 | Returns the existing session; no second session   |
| Lifecycle manifest write            | `(asset_id, prior_provenance_hash, manifest_hash)`                                 | No-op append; chain advances exactly once         |
| Metadata-update operation           | Operation id (UUIDv7) + `(asset_id, prior_provenance_hash)`                        | Re-applying the same op is structurally identical |
| Federation capability proof         | `(peer_id, jti)`                                                                   | Refresh with same `jti` returns the same response |
| Federation pull                     | `(peer_id, sync_cursor)` — the sync cursor itself is the key                       | Re-pull returns the same page                     |
| MLS commit                          | Handled by OpenMLS; commits are ordered by the group's commit chain                | OpenMLS rejects duplicates                        |
| Album upgrade ceremony              | `intent_id` (UUIDv7); see [Versioning](/design/versioning/#album-upgrade-ceremony) | Same intent never produces two forks              |

A write surface that does not appear here is, by default, **not** idempotent and must be designed before it ships.

## Atomicity Invariants

Multi-write operations that must succeed-as-one or not at all. A partial success on any of these is itself a damage scenario.

- **Asset bundle finalization.** The manifest, ciphertext blob, metadata blob, and provenance blob commit together in a single Postgres transaction. Server failure between any pair leaves the entire bundle un-finalized; the session moves to `FailedProcessing` and the partial blobs are GC'd. ([Filesystem — Atomic Writes](/design/filesystem/#atomic-writes-and-crash-recovery))
- **Stack edits.** All affected sidecars stage as `.tmp` files first; renames happen together. Any rename failure discards every `.tmp` in the bundle. ([Filesystem — Atomic Writes](/design/filesystem/#atomic-writes-and-crash-recovery))
- **AMK epoch bump + write-tier key rotation.** A new AMK and a new write-tier key are minted as a single MLS commit. The two cannot exist out of sync.
- **Album upgrade ceremony.** The cutover is one MLS commit, the `AlbumTombstone`. Until applied, the client is in v_old; after, in v_new. ([Versioning — Album Upgrade Ceremony](/design/versioning/#album-upgrade-ceremony))
- **Lifecycle manifest + provenance record.** Writing a lifecycle manifest and appending its provenance entry are the same act, because the provenance entry **is** the manifest plus the chain link. There is no separate "now record provenance" step that can race.

## Quarantine Surfaces

Every "don't apply, surface it" code path. The union exists so the UI surface and operator audit have a single inventory of "things that need a human to look at."

| Surface                                                | Where it lives on disk (client)                              | Source of truth doc                                                                     |
| ------------------------------------------------------ | ------------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| `verify_asset` reject (any signature or chain failure) | Quarantine area surfaced via the audit log                   | [Cryptography — Write Authorization](/design/cryptography/#write-authorization)         |
| Federation soft-fail                                   | Rejected-hash table, bounded LRU                             | [Federation — Soft-Fail Semantics](/design/federation/#soft-fail-semantics)             |
| Orphaned original (no sidecar)                         | `.library/quarantine/` after a failed recovery               | [Filesystem — Repair](/design/filesystem/#repair)                                       |
| Malformed CBOR sidecar                                 | `.library/quarantine/` (the unparseable bytes are preserved) | [Filesystem — Repair](/design/filesystem/#repair)                                       |
| Stale-revival (peer or restore sends old manifest)     | Audit log + UI surface "peer sent stale state"               | [Cryptography — Provenance](/design/cryptography/#provenance-of-library-modifications)  |
| Album upgrade stranded write                           | Local `pending_until_upgrade` queue                          | [Versioning — Album Upgrade Ceremony](/design/versioning/#album-upgrade-ceremony)       |
| Backup restore chain conflict                          | Audit log + UI surface "restore conflicts"                   | [Backup & Recovery — Backup Verification](/design/backup-recovery/#backup-verification) |

A quarantined item is **never silently dropped and never silently applied**. The user (or operator) can inspect, repair, or discard explicitly.

## Provenance Immutability Rules

The append-only hash-chained record per asset is defined in [Cryptography — Provenance of Library Modifications](/design/cryptography/#provenance-of-library-modifications). This section is the policy layer.

- **No path exists to overwrite or delete an existing provenance entry.** Not via the API, not via the local filesystem (the client treats `.provenance.cbor` as append-only), not via federation. The constraint is structural, not enforced by a permission check.
- **Even a hard-delete preserves provenance.** When an asset is purged, its `media/{YYYY}/{YYYY-MM}/{uuid}.provenance.cbor` remains as a tombstone-with-history. The bytes that go away are the ciphertext blob and the encrypted metadata; the audit trail does not.
- **Export and backup carry the chain.** A backup artifact includes every asset's full provenance chain. On restore, the chain re-enters the local index — see the [open question on restore conflicts](#open-questions).
- **What a key-holding attacker still cannot do.** A complete current-key compromise lets the attacker append forward. It does not let them rewrite the past — every prior record is bound by a signature from a (possibly retired) device whose public half is still in the device directory.

## Schema Evolution and Field Grammar

The owner of "what a Capsule schema looks like" is each individual schema's owner doc; the owner of "what evolution is allowed" is this doc.

### Deny-by-Default for Unknown Request Fields

[Postel's Law](/design/principles/) — as tightened in principles — applies asymmetrically:

- **In requests (client → server, or peer → server):** unknown fields at known positions in a known schema are accepted and preserved verbatim. Unknown fields at the **top level** that the receiver does not declare are **rejected**. Schema-bearing requests that announce a `sidecar_schema` or `crypto_suite_id` the receiver does not implement are rejected. The asymmetry is deliberate: liberal acceptance in requests is what lets new clients write extensions, but only *inside* a known schema envelope.
- **In responses (server → client):** unknown fields are preserved verbatim. A new server sending an old client a response with a new field does not break the old client.

### Closed Enums

The following enums are closed per `protocol_version` — a value outside the enum is a structural error, never a "future value to ignore":

- `AssetManifest.action`
- `Sidecar.content_type`
- `Sidecar.gps.source`
- `DerivativeManifest.role`

Adding a value to a closed enum bumps `protocol_version`. Old albums never see the new value because they are pinned.

### Timestamp Grammar

All `timestamp` and `ts` fields are RFC 3339 strings. Server-accepted values are bounded to **±30 days** of server wall-clock at the moment of accept (configurable per deployment). The bound applies to writes; reads serve whatever timestamp was historically accepted.

A client whose system clock drifts more than 30 days from the server is rejected at handshake. This is the *honest* class's protection from a faulty NTP — the bound surfaces the drift instead of silently distorting audit timestamps.

### Bounded String and Collection Sizes

Every field has a maximum length declared in the schema (e.g. `caption_lww.value ≤ 4096 bytes`; `superseded_captions ≤ 16 entries`). The receiver rejects an oversized value. No field is unbounded.

## Forbidden Client Behaviors

A correct Capsule client implementation must never:

- Back-date or post-date a `timestamp` outside the ±30-day window.
- Re-sign or re-issue a manifest under a `crypto_suite_id` lower than the original.
- Sign for an album epoch the client does not currently hold the write-tier key for.
- Issue an OR-set remove for an `add_id` it has not locally observed an add for.
- Strip `_unknown` fields from a sidecar it intends to write back. Round-trips must preserve everything the schema allows.
- Strip `superseded_captions` entries.
- Overwrite an existing `.provenance.cbor` file (the file is append-only).
- Submit a `revoke_all_sessions` without proof of master-key possession.
- Decode bytes received from a non-home peer outside the [sandboxed decoder](/design/clients/#sandboxed-decoder).
- Promote an AI tag to a user tag silently — promotion is an explicit, signed lifecycle operation.
- Treat a `429`, `409`, or `426` as a retry-with-the-same-payload. Each one requires a fix on the client (back off, re-align offset, upgrade) before retry.

A client implementation that does any of the above is **buggy by definition**. The check belongs in the client implementation's own correctness tests; the network layers above protect against the consequences.

## Min-Supported-Client Deprecation Policy

Dropping a `protocol_version` from the server's accepted window is a breaking change. The policy:

1. **Announcement.** A deprecation cutoff date is published at `/.well-known/capsule/deprecation` ahead of the cutoff by at least the announcement window (default 90 days, deployment-configurable). The announcement names the cutoff date and the minimum `protocol_version` that will remain accepted.
2. **Server response.** Below the cutoff, every response carries `X-Capsule-Min-Client-Build` and a `Warning:` header pointing to the deprecation URL.
3. **Hard cutoff.** On the cutoff date, the dropped version moves outside `[Min, Max]`. Writes from clients pinned to that version receive `426`. Reads still succeed.
4. **Stranded user.** A user whose only client is below the cutoff still has every recovery path from [Cryptography — Failure Modes and Recovery](/design/cryptography/#failure-modes-and-recovery): master key, cross-device, OGK, backup artifact. The deprecation does not strand data; it strands a specific old binary.

The deprecation surface is **never** retroactive against historical state. Old albums pinned to a dropped version remain readable forever — they just cannot be written to from a current client.

## Open Questions

These survive the current design and should be resolved before the docs are considered final.

1. **Restore-vs-stale-revival.** A restore from a 6-month-old backup hands the system manifests whose `prior_provenance_hash` is older than the local `latest_provenance_hash`. The naive defense quarantines every entry, which is a foot-gun. Two candidate resolutions: (a) restore enters a `restore_from_backup` chain branch the user explicitly merges, or (b) restore resets `latest_provenance_hash` from the backup contents under additional authentication. Resolution lives in [Backup & Recovery](/design/backup-recovery/).
2. **Sync cursor authenticity.** A malicious server could hand a client an older `sync_cursor` to rewind its view. The cursor is currently opaque; making it MAC'd by the server and validated as monotonic by the client is the leading fix.
3. **Cross-server album replication (v2).** v1 pins each album to a single home server; v2 will need a story for cross-server MLS state and federated commit ordering.
4. **Sponsored-account write damage.** A compromised registered account holds its sponsorees' KEKs and can manipulate their histories without their device keys. Enumerate the damage and bound it.
5. **AMK epoch monotonicity bootstrap.** A brand-new client cannot know the previous max `amk_version` without trusting the server. The fix bootstraps monotonicity from the MLS commit chain rather than the server's stored counter.
6. **Cross-language deterministic CBOR.** FFI consumers re-serializing may drift; no byte-identical cross-language test surface is documented.
7. **Federated quota DoS via honest user.** Per-peer quotas protect Capsule from a peer, but a single user receiving from many peers can exhaust the home server's storage. Needs a peer-attribution dimension.
8. **"New client" UI surface.** A client speaking a `protocol_version` ahead of an album's pin is rejected on writes but may *read* state a future client wrote. The unknown-extension UI surface needs definition in [Clients](/design/clients/).

## Cross-References

Each owner doc gains an invariant section or two that links back to this doc. The mapping:

| Owner doc                                                   | Threat-model section(s) it ties into                                                                                                                                      |
| ----------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Principles](/design/principles/)                           | [§ Damage Containment Layers](#damage-containment-layers)                                                                                                                 |
| [Versioning](/design/versioning/)                           | [§ Protocol and Capability Negotiation](#protocol-and-capability-negotiation), [§ Atomicity Invariants](#atomicity-invariants)                                            |
| [Filesystem](/design/filesystem/)                           | [§ Server-Side Validation Invariants](#server-side-validation-invariants), [§ Atomicity Invariants](#atomicity-invariants), [§ Quarantine Surfaces](#quarantine-surfaces) |
| [Cryptography](/design/cryptography/)                       | [§ Provenance Immutability Rules](#provenance-immutability-rules), [§ Damage Scenario Map](#damage-scenario--invariant-map) (signature/chain rows)                        |
| [Metadata](/design/metadata/)                               | [§ Schema Evolution and Field Grammar](#schema-evolution-and-field-grammar), [§ Damage Scenario Map](#damage-scenario--invariant-map) (CRDT rows)                         |
| [Import & Synchronization](/design/import-synchronization/) | [§ Server-Side Validation Invariants](#server-side-validation-invariants), [§ Idempotency Invariants](#idempotency-invariants)                                            |
| [Federation](/design/federation/)                           | [§ Server-Side Validation Invariants](#server-side-validation-invariants), [§ Quarantine Surfaces](#quarantine-surfaces)                                                  |
| [Peering](/design/peering/)                                 | [§ Client-Side Validation Invariants](#client-side-validation-invariants), [§ Damage Scenario Map](#damage-scenario--invariant-map) (peer rows)                           |
| [Authentication](/design/authentication/)                   | [§ Forbidden Client Behaviors](#forbidden-client-behaviors), [§ Damage Scenario Map](#damage-scenario--invariant-map) (revoke-all row)                                    |
| [Authorization](/design/authorization/)                     | [§ Server-Side Validation Invariants](#server-side-validation-invariants)                                                                                                 |
| [Backup & Recovery](/design/backup-recovery/)               | [§ Quarantine Surfaces](#quarantine-surfaces), [§ Open Questions](#open-questions)                                                                                        |
| [Thumbnails](/design/thumbnails/)                           | [§ Damage Scenario Map](#damage-scenario--invariant-map) (derivative row)                                                                                                 |
| [ML Models](/design/ml-models/)                             | [§ Damage Scenario Map](#damage-scenario--invariant-map) (embedding model row)                                                                                            |
| [AI](/design/ai/)                                           | [§ Forbidden Client Behaviors](#forbidden-client-behaviors) (AI tag namespace)                                                                                            |
| [Organization](/design/organization/)                       | [§ Atomicity Invariants](#atomicity-invariants), [§ Forbidden Client Behaviors](#forbidden-client-behaviors)                                                              |
| [Clients](/design/clients/)                                 | [§ Client-Side Validation Invariants](#client-side-validation-invariants), [§ Min-Supported-Client Deprecation Policy](#min-supported-client-deprecation-policy)          |
