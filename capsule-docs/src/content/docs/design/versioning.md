---
title: Versioning
description: Update and versioning policy for servers and apps, and the invariants that stop any client and any server from corrupting each other's data
status: draft
---

Any server may meet any client: an app months older than its server, a server older than a
freshly updated app, a federated peer on code Capsule never shipped. Both sides hold data the
other cannot recreate — the client its keys and unsynced edits, the server the only copy of
uploaded ciphertext. This page fixes the rules that keep every pairing safe: **reads stay
compatible forever, writes fail closed, and nothing is ever dropped to resolve a mismatch.**

## System Invariants

Every client and server implementation, of any version, MUST uphold these. Together they make
the worst outcome of any version mismatch a refused write, never lost or corrupted data.

1. **Bytes are immutable and content-addressed.** A blob is named by the hash of its bytes and
   never changes, so its identity does not depend on any protocol version
   ([Transfer](/design/transfer/#invariants)).
2. **Every signed record leads with its version, and unknown fields survive.** A record's
   schema version is decoded before anything else. Fields a reader does not know are kept
   verbatim, re-emitted canonically, and covered by the signature, so stripping one fails
   verification ([Schema Rules](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar)).
3. **Never write what you do not understand.** A component that meets a record version, enum
   value, `crypto_suite_id` or `sidecar_schema` above what it implements may read and store it
   verbatim, but MUST NOT modify, re-sign or re-encode it. Reads of any version it does
   understand stay supported forever.
4. **The server never rewrites client-signed data.** It stores and serves signed manifests and
   sealed blobs byte for byte. Data leaves the server only through a signed lifecycle action
   followed by garbage collection after a grace window.
5. **A client never discards data to resolve a mismatch.** An entry it cannot apply is
   quarantined; a write it cannot send is queued; a catalog newer than the binary is refused
   and left untouched. Each resumes after the user updates.
6. **Concurrent edits converge deterministically.** Metadata merges by CRDT
   ([Metadata](/design/metadata/)); album membership and moves are ordered by MLS commit order.
   No merge depends on wall-clock agreement between devices or on which side is newer.
7. **Local originals are released only on server custody**: a durable verdict, a signed custody
   receipt and a matching read-back hash ([Durability](/design/transfer/durability/#verify-before-destroy)).
   A copy on another device, including a [delegate](/design/peering/#delegation), is never custody.
8. **One protocol version per binary.** A client sends the same `protocol_version` on every
   request it makes.
9. **Evolution is additive.** New fields are optional and absent from the wire until set; new
   behaviour is a new `protocol_version`. A change no window can span goes through the
   [album upgrade ceremony](#album-upgrade-ceremony), never an in-place rewrite.

## Version Identifiers

| Identifier | Carried on | Owner |
| --- | --- | --- |
| `protocol_version` (`YYYY-MM-DD`) | every request, every album pin | [Validation — Universal Headers](/design/threat-model/validation/#universal-headers) |
| `crypto_suite_id` | every manifest and metadata blob | [Primitives](/design/cryptography/primitives/#versioning-identifiers) |
| `sidecar_schema` | every sidecar, key 0 | [Metadata](/design/metadata/#schema-versioning-rules) |
| provenance record version | every provenance record, leading field | [Provenance](/design/cryptography/provenance/) |
| client build (semver) | `X-Capsule-Min-Client-Build` floor in every response | [Validation — Universal Headers](/design/threat-model/validation/#universal-headers) |
| catalog `PRAGMA user_version` | the client's SQLite catalog | [below](#client-catalog-migration) |
| server schema migrations | PostgreSQL `seaql_migrations` | [Filesystem — Server](/design/filesystem/server/) |

## Update Policy

**Servers update first.** A server release accepts a closed window `[Min, Max]` of
`protocol_version` values and advertises it on every response. The window MUST include every
version spoken by an app release still inside its support period, so a staggered app rollout
never strands a user. A version leaves the window only through the
[deprecation policy](/design/threat-model/schema-rules/#min-supported-client-deprecation-policy):
announced ahead, after which writes from it are refused with `426` and reads keep working.

**Apps stay lenient.** A native app must work against any server whose window contains its
version, and treats every unknown response field as preserved data, not an error. It refuses
to write — and tells the user to update — when the server answers `426` or advertises a minimum
client build above its own. The [web client](/design/clients/) is the one exception: it is
served by its own server and assumes that server's exact version.

**Server upgrades are forward-only.** Migrations run before `serve` (which refuses an
unmigrated schema), never downgrade, and are preceded by a backup. A failed migration means
restoring the backup, never rolling back and continuing.

**Compatibility is checked once, early.** Client and server check the window at the first
request and fail loudly rather than soft-degrade; past that handshake, both sides agree on
`protocol_version`, `crypto_suite_id` and `sidecar_schema` for the operation.

## Client Catalog Migration

The client catalog is the library as the user experiences it, so it has the server schema's
obligation: a **forward-only stepwise migrator keyed on `PRAGMA user_version`**, one step per
version, no downgrade. Rebuilding from sidecars is the repair path for a known-inconsistent
index ([Maintenance](/design/filesystem/maintenance/#repair)), not a migration strategy: it can
only return what the sidecars carry. The derived cache (thumbnails, previews) is dropped and
regenerated instead of migrated.

A catalog stamped **above** the binary's version is refused with an error naming both versions,
and left byte-for-byte untouched (invariant 5). An older binary writing to it would fill
columns it does not know are authoritative; refusing costs the user an update.

## Album Protocol Version Pinning

Each album declares a `protocol_version` at creation, immutable for its lifetime, and every
event in it must conform. A faulty implementation of a newer version can therefore only corrupt
albums of that version, which matters most under [federation](/design/federation/).

- A client below the server's `Min` cannot write to any album.
- A client below an album's pin cannot write to that album, even if the server still accepts
  its version elsewhere.
- Reads are never affected by either rule.

## Album Upgrade Ceremony

An album moves to a newer version by **tombstone plus fork**: the old album freezes, a new album
at the target version is forked from its frozen state, and members move over. It is atomic per
client and resumable, and every step is keyed by an `intent_id` (UUIDv7) so a retried or
contradictory proposal can never produce two forks.

```text
[v_old normal] --UpgradeIntent--> [v_old quiescing] --drain--> [v_old frozen]
                                                                     |
                                                            AlbumTombstone commit
                                                                     v
                                     queued v_old writes replayed -> [v_new active]
```

1. **Propose.** An admin sends `UpgradeIntent { from_version, to_version, intent_id,
   proposer_device, deadline }`, signed by its device key. `deadline` is a duration; expiry is
   measured on the server's clock. Only one intent may be live per album.
2. **Quiesce.** Members queue new writes locally, flagged with the `intent_id`. The server
   refuses writes that do not name the live `intent_id` (`409 error.upload.album_quiescing`).
3. **Drain.** The ceremony waits until no upload session against the album is in flight. On
   expiry it aborts by absence of state and queued writes return to the old album.
4. **Tombstone.** The admin commits `AlbumTombstone { intent_id, frozen_state_hash }`, a hash
   over the album's members, accepted manifests and provenance head. Every member recomputes it;
   a mismatch aborts the upgrade for that member.
5. **Fork.** A new album at `to_version` references the existing ciphertext by content hash (no
   re-encryption); its manifests carry `upgraded_from { old_album_id, intent_id,
   frozen_state_hash }` in the signed core; fresh album keys are minted.
6. **Replay.** Each member re-encodes its queued writes for `to_version` and replays them.
7. **Resume.** A client that crashed mid-ceremony reads its local pending state and the album's
   phase from the server and continues from there.

The server's part is limited to what clients cannot do themselves: verifying the proposer
against the account's [device directory](/design/cryptography/keys/#device-directory), the
expiry clock, the write filter and the drain count
(`POST`/`GET`/`DELETE /v1/albums/{album_id}/upgrade`). It never judges `frozen_state_hash`,
which is each member's independent statement about its own view.

## Validation

Each invariant above has an automated test at the layer that owns it
([Testing](/development/testing/)); the version-specific ones are:

- a request outside the window is refused with `426` on every route class, and the client
  surfaces one typed upgrade error;
- golden records and wire messages from every released protocol version still decode, verify
  and round-trip byte-identically, unknown fields included;
- an older reader handed a newer record stores it verbatim and refuses to modify it;
- a catalog at every historical `user_version` migrates to the current one; a newer catalog is
  refused untouched;
- the upgrade ceremony survives a crash after the tombstone without a second fork, and a
  divergent member aborts it cleanly with no queued write lost.
