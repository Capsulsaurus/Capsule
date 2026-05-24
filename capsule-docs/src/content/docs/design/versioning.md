---
title: Versioning
description: Handling versioning gracefully
---

Changes are inevitable. Capsule minimizes breaking changes but generously accepts compatible ones. The aim is backward-compatible reads forever and a deliberately fail-closed write path — a [version-mismatched client](/design/threat-model/) never silently corrupts state, it is rejected at the handshake.

Versioning happens on multiple layers:

- **Metadata CBOR schema** — `sidecar_schema` field 0 of every sidecar (see [Metadata — Schema Versioning Rules](/design/metadata/#schema-versioning-rules)).
- **Cryptographic primitive bundle** — `crypto_suite_id` on every manifest and metadata blob (see [Cryptography — Versioning Identifiers](/design/cryptography/#versioning-identifiers)).
- **Wire protocol** — `protocol_version` (date-based, `YYYY-MM-DD`) on every API request and album pin. See [Threat Model — Protocol and Capability Negotiation](/design/threat-model/#protocol-and-capability-negotiation) for the universal handshake.
- **Client cache** — internal and rebuildable; cache schema changes drop and rebuild rather than migrate.
- **Server data structures** — PostgreSQL schema migrations forward-only. The session-state store is a deployment choice, not a versioned API surface: by default `upload_sessions` lives in PostgreSQL, and high-concurrency deployments may relocate it to Valkey for hot-path performance only. The wire protocol is identical in both cases (see [Filesystem — Stores by Deployment Profile](/design/filesystem/#stores-by-deployment-profile)).

## Compatibility Verification

Initial startups of a client and server always strictly check for version compatibility and **crash early** rather than soft-degrade. The single handshake in [Threat Model — Protocol and Capability Negotiation](/design/threat-model/#protocol-and-capability-negotiation) is the only point at which compatibility is determined; once an operation is past the handshake, both sides know they agree on `protocol_version`, `crypto_suite_id`, and `sidecar_schema`.

Capsule does **not** support backwards migrations or version downgrades. Server-side schema migrations are forward-only; if a migration fails, the server refuses to start and the operator restores from backup. There is no "rollback then continue" — that path is what corrupts data.

## Album Protocol Version Pinning

Each album declares a **protocol version at creation, and that version is immutable** for the album's lifetime. Every event in the album must conform to it. Adopting a new protocol feature does not mutate an existing album — it requires either creating a new album, or an explicit [upgrade ceremony](#album-upgrade-ceremony) that tombstones the old album and creates a new one.

This bounds the blast radius of a buggy or malicious implementation: a faulty v4 implementation can only ever corrupt v4 albums, because v1–v3 validation rules never change. It matters most under [Federation](/design/federation/), where Capsule cannot assume a peer is running the same version — pinning is what lets old albums keep working when a peer ships bad v4 code.

## Album Upgrade Ceremony

A version-pinned album is upgraded by a **tombstone-plus-fork** ceremony: the old album is frozen, a new album at the target version is forked from its frozen state, and all members migrate. The ceremony is **atomic at the user level** — there is no halfway state visible to one client — and **resumable** if any participant crashes partway through. Every step is keyed by an `intent_id: UUIDv7` to defeat duplicate or contradictory upgrade proposals.

```text
[v_old normal] --UpgradeIntent--> [v_old quiescing] --drain--> [v_old frozen]
                                                                     |
                                                            AlbumTombstone commit
                                                                     |
                                                                     v
                                                              [v_new active]
                                                                     ^
                                                          queued v_old writes replayed
```

### Steps

1. **Freeze proposal.** An album admin issues an MLS application message `UpgradeIntent { from_version, to_version, intent_id, proposer_device, deadline }`, hybrid-signed by the admin's [DSK](/design/cryptography/#device-keys). The proposal carries a deadline (default 7 days). Any member's client receiving an `UpgradeIntent` for an album that is already in upgrade quiescence under a *different* `intent_id` rejects the new proposal — only one upgrade can be in flight per album.
2. **Quiesce writes.** Members enter upgrade quiescence on receipt of `UpgradeIntent`:
   - In-flight uploads against the album are allowed to reach a terminal state.
   - New writes are queued **locally** with a `pending_until_upgrade` flag and the `intent_id`; they are not sent to the server.
   - The server augments the album row with `upgrade_pending_to = to_version, intent_id`. New upload sessions for this album whose `manifest.intent_id` does **not** match are rejected with `409 Conflict` — preventing a stale v_old client from writing past the freeze.
3. **Drain.** The upgrade cannot proceed while any session for this album is in `Uploading` or `WaitingForProcessing`. The server exposes the in-flight count to the proposer's client. The deadline from step 1 bounds the wait; on deadline expiry the upgrade aborts cleanly (state returns to v_old normal; queued local writes are flushed back to v_old).
4. **Tombstone.** Once drained, the proposing admin issues an MLS commit `AlbumTombstone { intent_id, frozen_state_hash }`. `frozen_state_hash` is a SHA-256 over the canonical CBOR of the album's full state: the sorted member list, every accepted manifest's hash, and the head of the album's provenance log. Every receiving member's client recomputes the hash against its own state; on mismatch the upgrade aborts (each member independently — the album returns to normal operation). Hash mismatch means at least one member's view of the album diverges and must be resolved before any upgrade.
5. **Fork.** A new album group is created at `to_version`, MLS-named `parent_id_v{n}`, with the manifest field `upgraded_from: { old_album_id, intent_id, frozen_state_hash }`. Assets are **not** re-encrypted: the new album references the existing ciphertext blobs by content hash. Members are added to the new MLS group via standard `Add` proposals; fresh `AMK_v1` and a fresh write-tier key are minted.
6. **Apply queued writes.** Each member's locally queued `pending_until_upgrade` writes are re-encoded against `to_version` (the album pin and `crypto_suite_id` may have changed) and replayed into the new album.
7. **Resumption (partial-failure recovery).** A client that crashes between step 2 and step 6 reads its local `upgrade_pending_to` on restart, queries the server for the upgrade's current phase via the album row, and resumes from there. The `intent_id` is the idempotency key — the same `UpgradeIntent` never produces two forks, and a duplicate `AlbumTombstone` commit is a no-op at the MLS layer.
8. **Atomicity guarantee.** The cutover is the single MLS commit in step 4. Until that commit is applied by a member's client, the client is operating in v_old; after, in v_new. There is no in-between state visible to one client. Cross-member, the cutover is observed as each member processes the commit; until the slowest member processes it, that member is still in v_old (and its `pending_until_upgrade` writes remain queued locally, never lost).

### What This Defends Against

- **Version-mismatched-client damage.** A v_old client cannot write into a v_new album because every write carries `protocol_version`, which is rejected by the [protocol handshake](/design/threat-model/#protocol-and-capability-negotiation) and the [server-side validation invariants](/design/threat-model/#server-side-validation-invariants).
- **Partial-upgrade corruption.** Quiescence + drain ensures no v_old write is mid-flight at the moment of cutover. The `intent_id` keys every step so a retried, duplicated, or contradictory proposal cannot produce two divergent v_new albums.
- **Hostile member sabotage.** A member whose computed `frozen_state_hash` differs from the proposer's rejects the tombstone, aborting the upgrade. A malicious member cannot trick the rest into a forged "post-upgrade" state.

The full atomicity rule lives in [Threat Model — Atomicity Invariants](/design/threat-model/#atomicity-invariants); stranded `pending_until_upgrade` writes are a [quarantine surface](/design/threat-model/#quarantine-surfaces).

## Min-Supported-Client Window

The server accepts a *window* of past `protocol_version` values, not only the newest, so a staggered client rollout keeps working. A version leaves the window only after a deprecation period; the policy is owned by [Threat Model — Min-Supported-Client Deprecation Policy](/design/threat-model/#min-supported-client-deprecation-policy).

The interaction with album pinning:

- A client whose `protocol_version` falls below the server's `Min` is rejected at the handshake for *any* write — it cannot upload into any album, including ones pinned to the version it can still parse.
- A client whose `protocol_version` falls below an album's pin is rejected for writes to *that album* — the album's pin is a per-album minimum, often higher than the server's minimum (e.g., a v_2024-09-01 album rejects v_2024-06-01 clients even on a server that still accepts v_2024-06-01 for other albums).
- **Reads are unaffected.** A v_old client can always *read* an album it cannot write to. The deprecation policy never makes historical state unreadable.
