---
title: Validation Invariants
description: Server and client refuse-by-default checklists; protocol handshake; idempotency; atomicity
---

The cross-cutting refuse-by-default rules every Capsule receiver runs before persisting any incoming write. These are the operational core of the threat model — a server or client that skips one of them silently widens the blast radius for the entire client class taxonomy.

The server-side invariants are enforced in `capsule-api` (every write path passes through them); the client-side invariants are enforced via the single `verify_asset` chokepoint in `capsule-core::crypto` plus the per-receiver decoder paths. The protocol handshake is a one-shot pre-flight check on every request; idempotency and atomicity invariants are properties of specific write surfaces, each cross-linked to the doc that owns the surface.

## Server-Side Validation Invariants

The server holds no keys — it cannot verify any signature against a key it owns. But it **does** validate the *structure* of every write before persisting state. These checks are refuse-by-default and intentionally exhaustive; a buggy server that skips one of them silently widens the blast radius for the entire [client class taxonomy](/design/threat-model/#client-class-taxonomy).

This list is the canonical statement; [Filesystem](/design/filesystem/), [Import](/design/import/), [Federation](/design/federation/), [Authorization](/design/authorization/), and [Authentication](/design/authentication/) reference it without restating.

Invariants carry **stable numbers** (referenced across docs as "invariant 17", "items 1–18", etc.); they are grouped by write phase but the numbering is continuous.

### On `POST /upload` (session creation)

- **1.** `X-Capsule-Protocol` is within the server's `[Min, Max]` range. Otherwise `426 Upgrade Required`, no session created.
- **2.** `crypto_suite_id` is a row of the [Primitives Inventory](/design/cryptography/primitives/#primitives-inventory). Otherwise `400`.
- **3.** `hash` length matches the digest size for `crypto_suite_id` (32 bytes for SHA-256). Otherwise `400`.
- **4.** `size` ∈ (0, `max_file_size`]. Otherwise `400` / `413`.
- **5.** `content_type` ∈ closed enum for this protocol version. Otherwise `400`.
- **6.** `album_id` exists; authenticated user has server-visible write capability on it; album's pinned `protocol_version` equals the request's. Otherwise `403`.
- **7.** `created_by_device` is in the user's published device directory, and the directory entry's `added_at` precedes the request's `timestamp`. Otherwise `403`.
- **8.** `timestamp` passes a gross-drift **sanity** bound (default ±30 days of server clock, configurable). This is a non-security guard that surfaces a wildly-wrong honest client, **not** an authorization control — authorization and ordering ride the epoch and chain, and the server records its own trusted `received_at` as the authoritative time for time-based policy. The client `timestamp` is stored verbatim for audit. See [Keys — Write Authorization](/design/cryptography/keys/#write-authorization). Otherwise `400`.

### On each `PATCH /upload/{id}` chunk

- **9.** Offset is exactly the current received-byte count. Otherwise `409`, with `X-Capsule-Offset` returned.
- **10.** Non-final chunk size is a multiple of 4 KiB. Otherwise `400`.
- **11.** Cumulative received ≤ declared `size`. Otherwise `400` / `413`, session moves to `FailedProcessing`.
- **12.** The `(upload_id, offset, chunk_hash)` idempotency tuple is new OR matches an exact prior PATCH. Otherwise (same offset, different hash) `409` + corruption error.

### At finalization

- **13.** Total received == declared `size`. Otherwise `FailedProcessing`.
- **14.** Recomputed ciphertext hash == declared `hash`. Otherwise `FailedProcessing` + corruption error.
- **15.** Manifest envelope re-validated (rerun 1–8) inside the finalization transaction.

### On non-upload writes (lifecycle action manifest, metadata-update, derivative-add/replace, trash-restore)

- **16.** `action` is in the closed enum. Otherwise `400`.
- **17.** `prior_provenance_hash` equals the last accepted manifest's content hash for this `asset_id`. Otherwise `409` (stale-revival).
- **18.** `amk_version` is monotonic per album (never regresses) **and within the range the album's admin-signed MLS commit chain attests**. The server's stored counter is a structural backstop; the authoritative ceiling is MLS, so a server cannot fabricate a future epoch a client will honor — see [Write Authorization](/design/cryptography/keys/#write-authorization). Otherwise `400`.

### On federation pull (server-to-server)

- **19.** Capability token verifies under home server's signing key; `exp` in future; `jti` not in revocation list (cached ≤ 15 min). Otherwise `401` / `403`.
- **20.** All checks (1)–(18) re-applied — federation does not unlock looser rules.
- **21.** Per-peer rate budgets unbroken (events/hour, bytes/hour, CPU/hour). Otherwise `429`.

### On the `/sync` feed, directory publish, and federated reports

- **22.** The `sync_cursor` carries a server MAC under a server-only key; a forged or mutated cursor is rejected (`400`). This is the authenticity layer; the client independently enforces per-album `sync_seq` monotonicity (client-side invariants below). Owner: [Import — Download & Sync](/design/import/download-sync/#discovering-what-changed).
- **23.** A published `DeviceDirectory` has `directory_version` **strictly greater** than the version currently stored for that user, and the master signature covers it. A non-advancing or regressing publish is rejected (`409`). Owner: [Cryptography — Device Directory](/design/cryptography/keys/#device-directory).
- **24.** A federated **report** (an out-of-band moderation message, not a state write) carries a valid signature from the reporting server and is within that peer's report rate budget; otherwise it is dropped before reaching the admin queue. Owner: [Moderation — Federated Reporting](/design/moderation/#federated-reporting).

### On any write whose bundle carries a metadata blob

- **25.** The encrypted metadata blob in the bundle has a content hash equal to the manifest's `metadata_blob_hash`. The server holds no key, but it can compare the content address it stores against the value the signed manifest commits to, so a client cannot present the server a metadata blob different from the one its asset manifest is signed over. A mismatch is rejected (`400`) and no state is written. This applies on `POST /upload` (the `create` bundle), at finalization, and on a non-upload `metadata-update`. Owner: [Metadata — Local and Server Metadata Equivalence](/design/metadata/#local-and-server-metadata-equivalence).

### On `POST /drop` (upload-link drop session) and adoption

A [web-upload](/design/web-upload/) drop carries **no `AssetManifest`** — no signatures, no `album_id`, no provenance — so it runs its own structural checks instead of 1–8, and is written only to the provisioning user's inbox, never the library. Owner: [Web Upload — Security Contract](/design/web-upload/#security-contract).

- **26.** `{opaque-id}` resolves to a **live** upload link: it exists, is not expired, is not revoked, and its per-link caps (cumulative bytes, file count) are not already exhausted. A not-found, expired, or revoked link returns an **indistinguishable `404`** (never `410`); a cap exhausted on an otherwise-live link returns `409` / `413`.
- **27.** `content_type` ∈ the closed enum for the link's pinned `protocol_version` (the same set as invariant 5). Otherwise `400`.
- **28.** `size` ∈ (0, the link's `max_file_size`]. Otherwise `400` / `413`.
- **29.** The provisioning (link-owner) user's quota admits the drop at session creation: `quota_used(owner) + declared_size ≤ hard_limit`. Otherwise `403 Quota Exceeded`. This reuses the single [quota enforcement point](/design/quota/#enforcement-points) with `upload_user_id = owner_id`.
- **30.** The `DropDescriptor` is structurally well-formed and `kem_ct`'s length matches the KEM ciphertext size for the link's `crypto_suite_id`; the drop request carries **no** `album_id`, `amk_version`, manifest, or provenance field. A drop that names an album or supplies signatures is rejected (`400`) — a drop can only ever land in the inbox.
- **31.** Drop-session creation is rate-limited per `{opaque-id}` and per source IP (the same two limiters as the [share-link serve path](/design/share-links/#security-contract)). Otherwise `429`.
- **32.** On **adoption** (`POST /drops/{id}/adopt`, a `create` manifest referencing an inbox blob): the manifest re-runs invariants 1–8, 16–18, and 25; additionally the manifest's `ciphertext_hash` must reference a drop blob in **the caller's own inbox**, and `key_mode` must be in its closed enum (`derived | wrapped`). The server then atomically promotes the blob from inbox to album asset and deletes the inbox row, in one transaction. Otherwise `400` / `403` / `409`, with no state written.

Drop **chunks** reuse the `PATCH` chunk rules (9–12) and **finalization** reuses the integrity checks (13–14) unchanged; only drop-session creation (26–31) and adoption (32) differ from the album upload path.

Every rejection is logged with a structured reason code; the rejected hash is remembered (bounded, see [Federation — Soft-Fail Semantics](/design/federation/#soft-fail-semantics)) so divergence between Capsule's view and a permissive peer's view is detectable.

## Client-Side Validation Invariants

Mirror checklist that every client implements before applying any received data — local or remote. A client that skips one of these is in the *faulty* class.

- Run [`verify_asset`](/design/cryptography/keys/#write-authorization) on every received `AssetManifest`. Quarantine on failure; never silent-drop, never silent-accept.
- Reject an incoming `sidecar_schema` greater than the client's `max_known_sidecar_schema`. Refuse to write that sidecar; refuse to read in normal mode (read-only opt-in is allowed).
- Reject an incoming `protocol_version` outside `[Min, Max]` known to the client. The same handshake the server runs.
- Reject an unknown enum value for any field whose enum is closed at the current schema (notably `action`, `content_type`, `gps.source`, `DerivativeManifest.role`). Unknown CBOR map keys are preserved per [Postel's Law](/design/principles/#postels-law-asymmetric) and never executed.
- Maintain a local `latest_provenance_hash` per `asset_id`. Refuse to apply any manifest whose `prior_provenance_hash` is behind the local value. Surface it.
- Round-trip the metadata blob on decode: the plaintext sidecar a client persists MUST be byte-identical to the canonical CBOR obtained by decrypting the asset's metadata blob, and the blob's content hash MUST equal the manifest's `metadata_blob_hash`. A divergence is quarantined, never persisted. See [Metadata — Local and Server Metadata Equivalence](/design/metadata/#local-and-server-metadata-equivalence).
- Before any post-write local cleanup that would discard the only copy of irreplaceable bytes — releasing a device-owned original, deleting a move-import source, streaming-mode release — confirm a `durable` verdict from the [storage-verification endpoint](/design/import/storage-verification/#verify-before-destroy) *in addition to* a `verify_asset` accept. A non-`durable` verdict means the local copy is retained, not dropped. This does not gate intentional deletes (trash/hard-purge) or reclaiming rebuildable/re-fetchable data.
- Maintain a per-user `directory_version` high-water mark. Refuse a `DeviceDirectory` whose `directory_version` is below it (a server attempting to roll back a revocation or hide a device); pin and surface the regression.
- Reject an OR-set remove whose `add_id` was never observed locally as an add.
- Refuse to follow a `revoke_all_sessions` confirmation that did not include a master-key proof.
- Decode remote-origin asset bytes only in the [sandboxed decoder](/design/clients/#sandboxed-decoder).

## Protocol and Capability Negotiation

Every versioned API surface — client-to-server uploads, sync feed, federation pull, peering — runs the same compatibility gate. The gate is **fail-closed**: a mismatch is a hard reject before any state is written, never a silent degrade.

### Universal Headers

| Header                       | Sent by                   | Meaning                                                                                               |
| ---------------------------- | ------------------------- | ----------------------------------------------------------------------------------------------------- |
| `X-Capsule-Protocol`         | client / peer             | `YYYY-MM-DD` protocol version the request is written against                                          |
| `X-Capsule-Crypto-Suite`     | client / peer on writes   | `u16` suite id from the [Primitives Inventory](/design/cryptography/primitives/#primitives-inventory) |
| `X-Capsule-Sidecar-Schema`   | client on metadata-update | `u16` schema version declared at `sidecar_schema` field 0                                             |
| `X-Capsule-Protocol-Min`     | server on every response  | the lowest protocol version this server accepts                                                       |
| `X-Capsule-Protocol-Max`     | server on every response  | the highest protocol version this server accepts                                                      |
| `X-Capsule-Min-Client-Build` | server on responses       | semver deprecation cutoff; advisory unless the path is hard-deprecated                                |

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
| Session creation (`POST /upload`)   | `(owner_id, hash, album_id)` — server's existing dedup check                       | Returns the existing session; no second session   |
| Lifecycle manifest write            | `(asset_id, prior_provenance_hash, manifest_hash)`                                 | No-op append; chain advances exactly once         |
| Metadata-update operation           | Operation id (UUIDv7) + `(asset_id, prior_provenance_hash)`                        | Re-applying the same op is structurally identical |
| Federation capability proof         | `(peer_id, jti)`                                                                   | Refresh with same `jti` returns the same response |
| Federation pull                     | `(peer_id, sync_cursor)` — the sync cursor itself is the key                       | Re-pull returns the same page                     |
| MLS commit                          | Handled by OpenMLS; commits are ordered by the group's commit chain                | OpenMLS rejects duplicates                        |
| Album upgrade ceremony              | `intent_id` (UUIDv7); see [Versioning](/design/versioning/#album-upgrade-ceremony) | Same intent never produces two forks              |

A write surface that does not appear here is, by default, **not** idempotent and must be designed before it ships.

## Atomicity Invariants

Multi-write operations that must succeed-as-one or not at all. A partial success on any of these is itself a damage scenario.

- **Asset bundle finalization.** The manifest, ciphertext blob, metadata blob, and provenance blob commit together in a single Postgres transaction. Server failure between any pair leaves the entire bundle un-finalized; the session moves to `FailedProcessing` and the partial blobs are GC'd. ([Filesystem — Atomic Writes](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery))
- **Stack edits.** All affected sidecars stage as `.tmp` files first; renames happen together. Any rename failure discards every `.tmp` in the bundle. ([Filesystem — Atomic Writes](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery))
- **AMK epoch bump + write-tier key rotation.** A new AMK and a new write-tier key are minted as a single MLS commit. The two cannot exist out of sync.
- **Album upgrade ceremony.** The cutover is one MLS commit, the `AlbumTombstone`. Until applied, the client is in v_old; after, in v_new. ([Versioning — Album Upgrade Ceremony](/design/versioning/#album-upgrade-ceremony))
- **Lifecycle manifest + provenance record.** Writing a lifecycle manifest and appending its provenance entry are the same act, because the provenance entry **is** the manifest plus the chain link. There is no separate "now record provenance" step that can race.
