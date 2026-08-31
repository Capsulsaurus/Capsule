---
title: Versioning
description: How Capsule pins each album to a protocol version, upgrades safely, and bounds client deprecation
status: draft
---

Changes are inevitable. Capsule minimizes breaking changes but generously accepts compatible ones. The aim is backward-compatible reads forever and a deliberately fail-closed write path — a [version-mismatched client](/design/threat-model/) never silently corrupts state; it is rejected at the handshake.

The enforcement is cross-cutting: every wire request, every album commit, and every sidecar carries a version identifier. The header set below is the **contract** that lets two implementations agree (or fail-closed) without negotiating. Album pinning lands in the album metadata model (`capsule-api` + `capsule-core`; planned with the networked surface); the upgrade ceremony is an MLS application-layer flow in `capsule-core::crypto::mls` driven by client UI (planned with the MLS layer — see the [status note](/design/cryptography/mls/)). The min-supported-client window is enforced server-side in `capsule-api` (planned).

## Versioned Surfaces

Versioning happens on multiple layers, each owned by the doc that defines it:

- **Metadata CBOR schema** — `sidecar_schema` field 0 of every sidecar (see [Metadata — Schema Versioning Rules](/design/metadata/#schema-versioning-rules)).
- **Cryptographic primitive bundle** — `crypto_suite_id` on every manifest and metadata blob (see [Cryptography — Versioning Identifiers](/design/cryptography/primitives/#versioning-identifiers)).
- **Wire protocol** — `protocol_version` (date-based, `YYYY-MM-DD`) on every API request and album pin. See [Threat Model — Protocol Negotiation](/design/threat-model/validation/#protocol-and-capability-negotiation) for the universal handshake.
- **Client derived cache** — thumbnails, previews, transcodes: purely derived, so a format or layout change drops and regenerates rather than migrates.
- **Client catalog** — `index/library.sqlite`, versioned by the database's own `PRAGMA user_version` and migrated forward stepwise, never dropped (see [Client Catalog Migration](#client-catalog-migration)).
- **Server data structures** — PostgreSQL schema migrations forward-only. Volatile session state in Valkey is not a versioned API surface (see [Filesystem — Server: Required Services](/design/filesystem/server/#required-services)).

## Negotiation Headers

The negotiation-header set — `X-Capsule-Protocol`, `X-Capsule-Crypto-Suite`, `X-Capsule-Sidecar-Schema`, and the server's `X-Capsule-Protocol-Min`/`-Max` and `X-Capsule-Min-Client-Build` responses — is declared **once**, in the registry at [Threat Model — Universal Headers](/design/threat-model/validation/#universal-headers), together with the fail-closed rules; the cross-transport carriage is [API Surfaces](/design/api-surfaces/#negotiation-across-transports). This doc adds only the versioning semantics: `protocol_version` is **date-based** (`YYYY-MM-DD`, ordered lexicographically = chronologically), a request is written against exactly one version, and the server advertises the closed `[Min, Max]` window it accepts on every response.

## Compatibility Verification

Initial startups of a client and server always strictly check for version compatibility and **crash early** rather than soft-degrade. The single handshake in [Threat Model — Protocol and Capability Negotiation](/design/threat-model/validation/#protocol-and-capability-negotiation) is the only point at which compatibility is determined; once an operation is past the handshake, both sides know they agree on `protocol_version`, `crypto_suite_id`, and `sidecar_schema`.

Capsule does **not** support backwards migrations or version downgrades. Server-side schema migrations are forward-only; if a migration fails, the server refuses to start and the operator restores from backup. There is no "rollback then continue" — that path is what corrupts data.

## Client Catalog Migration

"Backward-compatible reads forever" is not a server-only promise. The client catalog — `index/library.sqlite` in the [client library layout](/design/filesystem/client/#desktop-library-layout) — **is** the user's library as they experience it, so it carries the same obligation as the server schema and is given the same mechanism: a **forward-only stepwise migrator keyed on `PRAGMA user_version`**, the client-side analogue of the server's forward-only migrations above. Each schema version has exactly one step to the next; opening a catalog stamped below the current version walks it up, in order, to the current one. There is no downgrade step, for the same reason the server has none — "rollback then continue" is what corrupts data, on a laptop as much as on a server.

The objection this displaces is that the catalog is *derived*, so a schema change could simply drop it and rebuild from the sidecars. That reasoning is what the drop-and-rebuild rule rested on, and it does not hold. Slice `S-D21` found that a rebuild read the **unsigned** sidecar shape rather than the signed one the write path emits — and because the two are disjoint on the wire, it did not merely lose the [hidden](/design/organization/#hidden-assets) and [stack-membership](/design/organization/#asset-stacking) registers, it reconstructed **nothing at all** from a signed library. That is fixed, and rebuild now projects those registers and replays trash state from the provenance chain. What survives the fix is the limit that matters here: rebuild can only return what was written to disk, so an importer-formed stack placement (`S-B15`) is unrecoverable from a lost index, and any future column not carried by a sidecar would be too. Even once that is repaired, rebuild is the *repair* path ([Maintenance](/design/filesystem/maintenance/#repair)), reached when the index is already known-inconsistent; spending it on every shipped column would re-derive the whole library on each release and re-lose whatever the sidecars do not yet carry. The migrator is the durability mechanism; rebuild stays the recovery path it was.

### Catalogs newer than the binary

Forward-only settles the direction of migration, not what happens when a catalog is stamped *above* the running build's `SCHEMA_VERSION` — the case a user reaches the first time they install an older app build, or open a library synced from a device that updated first. That catalog is **refused, not opened and not downgraded**: the open fails with an error naming both versions and telling the user to update Capsule, and the catalog is left byte-for-byte untouched — no stamp rewrite, no DDL, no drop.

Opening it read-write is the unsafe option, not the accommodating one. The older binary cannot know which invariants the newer schema added, so its writes are the divergence: it fills a column it does not know is authoritative, or omits a register the newer build treats as present, and the damage is only visible after the user goes back to the build that could read the library correctly. Refusal costs the user an app update, which is always available. There is no recovery from silent divergent writes, which is why this is the same call as having no downgrade step at all.

Implementation is slice `S-D23`.

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

1. **Freeze proposal.** An album admin issues an MLS application message `UpgradeIntent { from_version, to_version, intent_id, proposer_device, deadline }`, hybrid-signed by the admin's [DSK](/design/cryptography/keys/#device-keys). `deadline` is a **duration** (default 7 days); the effective expiry is `received_at + deadline` on the **server's trusted clock** ([Filesystem — Server](/design/filesystem/server/#postgresql-what-the-server-knows)), and the abort-on-expiry in step 3 is evaluated against that server-attested time — a skewed member clock can neither extend nor shorten the window. Any member's client receiving an `UpgradeIntent` for an album that is already in upgrade quiescence under a *different* `intent_id` rejects the new proposal — only one upgrade can be in flight per album.
2. **Quiesce writes.** Members enter upgrade quiescence on receipt of `UpgradeIntent`:
   - In-flight uploads against the album are allowed to reach a terminal state.
   - New writes are queued **locally** with a `pending_until_upgrade` flag and the `intent_id`; they are not sent to the server.
   - The server augments the album row with `upgrade_pending_to = to_version, intent_id`. New upload sessions for this album whose `manifest.intent_id` does **not** match are rejected with `409 Conflict` — preventing a stale v_old client from writing past the freeze.
3. **Drain.** The upgrade cannot proceed while any session for this album is in `Uploading` or `WaitingForProcessing`. The server exposes the in-flight count to the proposer's client. The deadline from step 1 bounds the wait; on deadline expiry the upgrade aborts cleanly (state returns to v_old normal; queued local writes are flushed back to v_old).
4. **Tombstone.** Once drained, the proposing admin issues an MLS commit `AlbumTombstone { intent_id, frozen_state_hash }`. `frozen_state_hash` is the [content hash fixed by `crypto_suite_id`](/design/cryptography/primitives/#cryptographic-hash) over the canonical CBOR of the album's full state: the sorted member list, every accepted manifest's hash, and the head of the album's provenance log. Every receiving member's client recomputes the hash against its own state; on mismatch the upgrade aborts (each member independently — the album returns to normal operation). Hash mismatch means at least one member's view of the album diverges and must be resolved before any upgrade.
5. **Fork.** A new album group is created at `to_version` (its MLS group naming is an MLS-layer detail owned by [Cryptography — MLS](/design/cryptography/mls/); the normative link between old and new album is the field below, never the group name), with the manifest field `upgraded_from: { old_album_id, intent_id, frozen_state_hash }`. Assets are **not** re-encrypted: the new album references the existing ciphertext blobs by content hash. Members are added to the new MLS group via standard `Add` proposals; fresh `AMK_v1` and a fresh write-tier key are minted.
6. **Apply queued writes.** Each member's locally queued `pending_until_upgrade` writes are re-encoded against `to_version` (the album pin and `crypto_suite_id` may have changed) and replayed into the new album.
7. **Resumption (partial-failure recovery).** A client that crashes between step 2 and step 6 reads its local `upgrade_pending_to` on restart, queries the server for the upgrade's current phase via the album row, and resumes from there. The `intent_id` is the idempotency key — the same `UpgradeIntent` never produces two forks, and a duplicate `AlbumTombstone` commit is a no-op at the MLS layer.
8. **Atomicity guarantee.** The cutover is the single MLS commit in step 4. Until that commit is applied by a member's client, the client is operating in v_old; after, in v_new. There is no in-between state visible to one client. Cross-member, the cutover is observed as each member processes the commit; until the slowest member processes it, that member is still in v_old (and its `pending_until_upgrade` writes remain queued locally, never lost).

### The Server's Halves

Four of the steps above are the server's, and each one is the server's **because the clients cannot do it themselves** (slice `S-C24`). They are exposed as three operations on the album:

| | |
| --- | --- |
| `POST /v1/albums/{id}/upgrade` | enters quiescence, carrying the `SignedUpgradeIntent` as `application/cbor` |
| `GET /v1/albums/{id}/upgrade` | the phase, the expiry, and the **drain count** |
| `DELETE /v1/albums/{id}/upgrade?intent_id=…` | aborts, named by the id that holds the album |

- **The proposer is verified, against the account's [published device directory](/design/cryptography/keys/#device-directory).** Without that check anyone holding an access token could freeze an album by posting a struct, which is the opposite of a ceremony keyed to an admin device. What the server does **not** verify — and has no surface that could carry — is the `frozen_state_hash`: that is each member's independent statement about its own view, and a server that adjudicated it would be the single point the hostile-member defence exists to avoid.
- **The window is `received_at + deadline` on the server's clock**, which is the whole reason the deadline is a duration. `received_at` is stamped when the proposal is accepted and never moves.
- **Expiry is not a job.** An expired quiescence is treated *everywhere* as absent — by the write gate, by the phase, and by a fresh proposal, which replaces it rather than conflicting with it. Step 3's *"on deadline expiry the upgrade aborts cleanly"* is implemented as an absence of state rather than as a worker, which is what stops a proposer who vanished from freezing an album forever.
- **Only one ceremony per album**, and the same `intent_id` twice is idempotent — a proposer that lost an acknowledgement re-POSTs the same bytes. A *different* id while one is live is `409 error.album.upgrade_in_flight`, carrying the live id.
- **A write that does not name the live `intent_id` is `409 error.upload.album_quiescing`**, carrying it, so a client that *is* participating can tell "wrong ticket" from "somebody else's upgrade". The ceremony's own writes go through, which is what makes quiescence a filter rather than a freeze: in-flight uploads reach a terminal state instead of being abandoned.
- **`in_flight` is a count, not a listing.** The proposer needs to know *whether* to wait and has no business seeing other members' upload identifiers to find out. It counts every non-terminal session against the album, `Pending` included: a session opened with no bytes sent is exactly as much in flight as one mid-transfer.

**Lineage rides the signed manifest, and only the signed manifest.** `upgraded_from` is a field of the [asset manifest's](/design/cryptography/provenance/#asset-manifest) signed core, wire-absent when there is none. It is deliberately **not** mirrored into the `manifest_envelope` projection: that projection exists so a key-free server can validate a write without the manifest bytes, and lineage gates nothing the server decides — a projected field the server never reads is a field that can disagree with the manifest without anything noticing. A joining device reads the manifest itself, which the feed serves byte-for-byte.

### What This Defends Against

- **Version-mismatched-client damage.** A v_old client cannot write into a v_new album because every write carries `protocol_version`, which is rejected by the [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation) and the [server-side validation invariants](/design/threat-model/validation/#server-side-validation-invariants).
- **Partial-upgrade corruption.** Quiescence + drain ensures no v_old write is mid-flight at the moment of cutover. The `intent_id` keys every step so a retried, duplicated, or contradictory proposal cannot produce two divergent v_new albums.
- **Hostile member sabotage.** A member whose computed `frozen_state_hash` differs from the proposer's rejects the tombstone, aborting the upgrade. A malicious member cannot trick the rest into a forged "post-upgrade" state.

The full atomicity rule lives in [Threat Model — Atomicity Invariants](/design/threat-model/validation/#atomicity-invariants); stranded `pending_until_upgrade` writes are a [quarantine surface](/design/threat-model/scenarios/#quarantine-surfaces).

## Min-Supported-Client Window

The server accepts a *window* of past `protocol_version` values, not only the newest, so a staggered client rollout keeps working. A version leaves the window only after a deprecation period; the policy is owned by [Threat Model — Min-Supported-Client Deprecation Policy](/design/threat-model/schema-rules/#min-supported-client-deprecation-policy).

The interaction with album pinning:

- A client whose `protocol_version` falls below the server's `Min` is rejected at the handshake for *any* write — it cannot upload into any album, including ones pinned to the version it can still parse.
- A client whose `protocol_version` falls below an album's pin is rejected for writes to *that album* — the album's pin is a per-album minimum, often higher than the server's minimum (e.g., a v_2024-09-01 album rejects v_2024-06-01 clients even on a server that still accepts v_2024-06-01 for other albums).
- **Reads are unaffected.** A v_old client can always *read* an album it cannot write to. The deprecation policy never makes historical state unreadable.

## Validation

- **Handshake fail-closed (unit, both sides).** Client-side: send a request with `X-Capsule-Protocol` outside the server-advertised range; assert refusal and structured error surfacing in the UI. Server-side: receive such a request; assert `426` response with the supported range in headers.
- **Album pin immutability (unit).** Attempt to write into an album with a `protocol_version` other than the pin; assert rejection at the server envelope.
- **Upgrade ceremony idempotency (smoke).** Run the 8-step ceremony against a multi-member testcontainer setup. Inject a crash after step 4 (the tombstone commit); resume; assert the same `intent_id` produces no second fork. Inject a divergent member state before step 4; assert the abort path triggers cleanly.
- **Stranded write queue (smoke).** During quiescence, a member writes; the write is queued locally; the upgrade completes; the queued write is re-encoded against v_new and replayed. Assert no write is lost.
- **Deprecation cutoff (unit).** Mock the cutoff date past; assert a request from a now-deprecated client returns `426` and the well-known announcement is served.
- **Client catalog migration (unit).** Open a fixture library created at each historical `user_version`; assert it migrates stepwise to the current version with every column present, and that both default and gated projections answer correctly afterwards.

The cross-module case — full upgrade ceremony exercised through a real client UI + server + MLS group — is one bounded E2E test in [Module Map](/design/module-map/#e2e-test-surface).
