---
title: Server Filesystem
description: The server's blob store layout, Postgres index, and required services
status: draft
---

The server holds ciphertext blobs and a key-free index mapping assets to blobs. It never decodes, extracts metadata, or generates thumbnails — it holds no key. The layout below is the contract: an index rebuild from blob bytes depends on it.

## Required Services

Three required systems, one code path:

- **Blob store** — the encrypted bytes. Adapters: filesystem and in-memory ([`capsule-server::blob`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/blob/mod.rs)).
- **PostgreSQL 18** — the authoritative durable index: ownership, album and blob references, lifecycle state, pending-asset rows, the device-cohort map, the quota ledger. Tested against `postgres:18` on glibc, because asset-id order relies on `COLLATE "C"` behaving as deployed.
- **Valkey** — volatile state only: upload-session records and [auth sessions](/design/authentication/), with native TTL. Session lifetime rules are owned by [Transfer — Upload](/design/transfer/upload/#session-lifetime-and-discard).

Rules:

- The server MUST refuse to start without `DATABASE_URL` and `VALKEY_URL` unless started with `--memory`, which binds every port to its in-memory adapter and logs a `WARN` on every start. `--memory` is a development profile, never a deployment mode.
- Each state port has exactly two adapters: volatile stores get Valkey (redis-rs) and in-memory; durable ports get PostgreSQL and in-memory. A Postgres-resident session table, or any generic TTL abstraction, MUST NOT be introduced. The map is stated in [`capsule-server::store`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/store/mod.rs).
- The hot upload path (offsets, status) never touches the durable asset row.

### Schema migrations

- `serve` MUST NOT migrate. On boot it refuses a schema behind the binary (naming `capsule-server-migration up`); a schema ahead of it is tolerated for rolling deploys, so a destructive migration is split in two.
- Every applied migration records a SHA-256 checksum of its source in `capsule_migration_checksums`; boot refuses a mismatch with `PostgresError::SchemaDrift`. **An applied migration is never edited** — a schema change is always a new ordinal. See [`capsule-server::postgres`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/postgres/mod.rs).

## Blob Store Layout

```text
{blob_root}/
├── incoming/{upload_id}.bin                 # append-only upload, pre-verification
├── blobs/{hash[0:2]}/{hash[2:4]}/{hash}.bin # finalized, content-addressed, immutable
├── quarantine/                              # rejected objects + .reason.json, for humans
└── .server/{version,config}                 # operator metadata (plaintext, not user data)
```

- The whole tree MUST be on one filesystem so finalization renames are atomic.
- `incoming/`: one append-only file per session; no per-chunk staging, no assembly ([Transfer — Upload](/design/transfer/upload/#append-only-storage)).
- `blobs/` is sharded on the hash's own hex prefix so the three enumerations — [integrity scrub](/design/filesystem/maintenance/#server-side-integrity-scrub), [GC](#deletion-and-garbage-collection), [index rebuild](#recovering-the-index-from-blobs-alone) — stay affordable at scale. Lookup is a `stat` at a known address either way.
- `incoming/` and `quarantine/` are not sharded: neither is content-addressed, and quarantine is read by an operator.

Commit discipline ([`capsule-server::blob::fs`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/blob/fs.rs)):

- Non-upload temp files live inside the target shard (`.{hash}.{uuid}.tmp`). Uploads need none: the `incoming/` file is renamed straight to its address.
- Order: staged bytes fsynced → each newly created shard directory fsynced in its parent → rename → destination directory fsynced → only then the index row. A lost directory entry under a committed row would be a dangling reference; the reverse is an orphan GC reclaims.
- The shard derivation is tied to the 64-hex-char digest; changing the digest is a layout change (`.server/version` bump and data move), guarded at compile time.

## Uniform, Opaque Blobs

An asset is a **bundle** of blobs ([Transfer — Upload](/design/transfer/upload/#what-gets-uploaded)): encrypted original, derivatives, metadata blob, and the signed manifest stored as the provenance blob. The store does not distinguish them; roles live in PostgreSQL, with the manifest envelopes as the key-free durable fallback.

## Recovering the Index from Blobs Alone

PostgreSQL is authoritative but not the only copy. Every signed [manifest envelope](/design/cryptography/provenance/#asset-manifest) is stored in `blobs/` and carries what index rows need: suite and protocol version, `amk_version`, ciphertext hash and size, creator user/device, `album_id`, `file_id`, `prior_provenance_hash`, action, [`client_version`](/design/cryptography/provenance/#client-build-identification), and the device signature.

- A rebuild walks the envelopes, checks each signature against the cached device directory, and writes rows for the blobs each names. It MUST be idempotent.
- A blob no envelope names is an orphan for [GC](#deletion-and-garbage-collection). An envelope naming a missing blob is a dangling reference — except a missing original on an [`awaiting-original`](/design/transfer/upload/#upload-tiering-staged-uploads) asset.
- An envelope failing structural validation MUST be moved to `quarantine/` with a `.reason.json`, never dropped.
- Rebuild is a recovery path, never the hot path. Proactive consistency checking is the [integrity scrub](/design/filesystem/maintenance/#server-side-integrity-scrub).

## Manifest Envelope Validation

Every write — upload creation, chunk, finalization, lifecycle manifest, federation pull — passes structural envelope validation **before** any state is persisted. The checklist and rejection codes are owned by [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants).

## Content-Addressing and Deduplication

A blob already present is never stored twice. Duplicate and merge semantics at session creation are owned by [Transfer — Upload](/design/transfer/upload/#deduplication-and-merge). Whether a blob is referenced is decided by a query over committed rows, never a stored counter.

## PostgreSQL: What the Server Knows

Only key-free facts:

- `asset_id`, `owner_id`, `album_id`, `upload_user_id`
- blob content hashes and each blob's role
- `amk_version`, declared ciphertext size, `content_type`
- `uploaded` flag and server-visible lifecycle state
- the server's trusted `received_at` (authoritative for time-based policy) beside the client's audit-only `timestamp`
- `client_version` (audit-only, queryable to scope a defective build)
- [provenance records](/design/cryptography/provenance/#provenance-of-library-modifications)

No plaintext capture date, dimensions, EXIF, tags, or filename ever reaches the server; those live in the encrypted [metadata blob](/design/cryptography/encryption/#metadata-encryption). Session creation writes a pending row (`uploaded = false`) that finalization flips ([state machine](/design/transfer/upload/#session-state-machine)).

## Ownership, Partitioning, and Quota

`owner_id` is the billing and namespace entity, mirrored as an MLS group ([Owner Group Key](/design/cryptography/keys/#owner-group-keys-ogks)). Quota is charged to `upload_user_id` ([Quota](/design/quota/)). The blob store is global; every *reference* is owner-scoped, and dedup is owner-scoped.

The owner record carries a non-secret `default_album_id` (plus optional scope overrides) naming the [default album](/design/organization/#the-default-album). It is discovery only: writes are still gated on album write capability ([invariant 6](/design/threat-model/validation/#server-side-validation-invariants)).

## Deletion and Garbage Collection

The server cannot read `is_deleted`; lifecycle state is client-signalled and recorded on the asset row. Soft delete is a state change. A blob is removed only when provably unreferenced, and every failure mode biases toward keeping bytes.

- **Reference count is a query** over committed asset, derivative, metadata and provenance rows (plus live [share-link](/design/share-links/#storing-the-record) records) — never a maintained counter.
- **Mark, then sweep.** Zero references marks a blob (`collectable_since`); it is swept only after a 24 h grace window ([`DEFAULT_GRACE_WINDOW`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/gc/mod.rs)) and only if zero references is re-confirmed at sweep time. A reappearing reference cancels the mark. This reclaims the crash-after-rename orphan without racing a late reference.
- **A Postgres↔filesystem mismatch is never resolved by deletion.** An unreferenced blob is an orphan (GC). A committed row referencing a missing blob is a loud integrity error — logged, surfaced, never auto-deleted.
- Every GC decision is logged with hash, observed count, and mark/sweep times; a dry-run mode reports without removing.

## Storage Verification

The server attests key-free whether a blob is stored (`stat` in `blobs/`), indexed (committed `uploaded = true` row), and retrievable (referenced, not `collectable_since`, not quarantined). Anything marked collectable, quarantined, or missing is non-retrievable, so a client never releases a copy the server is about to lose. The wire contract and verify-before-destroy rule are owned by [Transfer — Durability](/design/transfer/durability/); the route is [`capsule-server::verify`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/verify/mod.rs).

## Validation

- **Layout round-trip (unit).** A finalized blob lives at exactly its sharded address and re-hashes to its name.
- **Shard enumeration (unit).** Scrub, GC and rebuild walks recover the same blob set across a partially populated shard tree.
- **Rebuild idempotency (smoke).** Drop the index, rebuild, compare to the expected set; re-run yields no changes.
- **Quarantine on malformed envelope (unit).** The object moves to `quarantine/` with a `.reason.json` naming the failed check.
- **GC safety (unit).** Swept only after the grace window; a re-reference during it cancels.
- **Dangling reference (unit).** Surfaced, never auto-deleted, never treated as collectable.
- **Verdict composition (unit).** Owner: [Transfer — Durability](/design/transfer/durability/).
