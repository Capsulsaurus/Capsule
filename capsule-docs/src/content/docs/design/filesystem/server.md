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
├── ledger/{owner_id}/{first_seq}.log         # owner ledger, append-only, signed
├── ledger/{owner_id}/attachments/{hash}      # documents a ledger entry names by hash
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

An asset is a **bundle** of blobs ([Transfer — Upload](/design/transfer/upload/#what-gets-uploaded)): encrypted original, derivatives, metadata blob, and the signed manifest stored as the provenance blob. The store does not distinguish them; roles live in PostgreSQL, with the manifest envelopes and the [owner ledger](#the-owner-ledger) as the key-free durable fallback.

## Recovering the Index from Blobs Alone

PostgreSQL's asset index is a **projection** of the blob store, never the only copy. Losing
the database, or finding it corrupt, costs a rebuild, not a library. Accounts, credentials and
everything else that is not an asset fact still come from the PostgreSQL backup
([what is not rebuilt](#what-a-rebuild-cannot-recover)).

### The Owner Ledger

Every signed [manifest envelope](/design/cryptography/provenance/#asset-manifest) is stored in
`blobs/` and names everything an index row needs except who owns it and in what order the server
accepted it. The **owner ledger** carries those facts:

```text
{blob_root}/ledger/{owner_id}/{first_seq}.log   # append-only segments
{blob_root}/ledger/{owner_id}/attachments/{hash} # documents an entry names by hash
```

- One entry per accepted manifest, per device-directory version and per lifecycle action:
  `sync_seq`, the manifest's content hash, `upload_user_id`, uploader device, `received_at` and
  the custody receipt. Each entry carries the hash of the previous one and is signed by the
  attestation key, so a truncated or edited ledger is detectable.
- An entry is appended and fsynced **after** the blobs it names are committed and **before** the
  index row commits. A crash between the two leaves an entry without a row, which the next
  rebuild or scrub replays; never a row without an entry.
- The `sync_seq` and the custody receipt are minted inside the index transaction, so a
  finalization writes two entries: a **custody** entry before the transaction (content hash,
  size, role, uploader, `received_at`) whose failure fails the finalization, and an **indexed**
  entry after it carrying the `sync_seq` and the receipt. A missing indexed entry is a sequence
  the rebuild cannot prove, handled like a lost tail.
- A write the index **refuses** after its custody entry is durable (a conflicting role, an
  incomplete or stale replace, a vanished asset) appends a **refused** entry naming the upload
  instead. A custody entry is resolved by an indexed or a refused entry; rebuild replays only
  unresolved ones. A write the index could not answer (an outage) gets neither, and is replayed
  like a crash. The refused append is best-effort: replay re-runs the checks that refused it.
- Each entry carries its owner, its position (`seq`, from 1, contiguous) and the hash of its
  predecessor inside the signed core. Edits, removals, a dropped head and a torn tail are
  detected from the chain alone. A tail cut exactly at an entry boundary is detected only
  against a head recorded outside the ledger (the **anchor**, below). A segment is a sequence of
  length-prefixed canonical-CBOR frames; the only bytes ever cut are those after the last whole
  entry when no complete entry starts anywhere after it (a short frame, or zeroes a crash left),
  which were never acknowledged. Damage followed by an entry refuses further appends.
- The **anchor** is each owner's ledger head, `(seq, entry hash)`, recorded in PostgreSQL
  (`owner_ledger_heads`) after every append is durable. It only rises, so it never names an
  entry the ledger did not hold; recording it is best-effort, so a lost record leaves it behind
  the ledger, which weakens the check and never fakes a fault. Rebuild and the scrub verify each
  ledger against it: a ledger that ends below it lost its tail (`BehindAnchor`), and one holding
  a different entry at its position was rewritten (`AnchorMismatch`). The server checks the
  same before its first append per owner after a start, and refuses that owner's writes while
  the ledger disagrees with its anchor, so a loss is never buried under new entries. Rebuild
  and the scrub walk every owner the index anchored as well as every owner with a ledger, so an
  owner whose ledger lost every entry is reported too. The way out is a rebuild under
  `--apply`: it clears a cut by re-anchoring at the head that is left, and a caught rewrite
  only when the operator adds `--accept-rewrites` after reading the report (below). It lives in the index
  rather than beside the ledger because a cut that took `BLOB_ROOT`'s tail would take a head
  file with it, and rather than in the custody receipt because receipts are client-held signed
  evidence with a frozen format. The anchor survives exactly when the index does: a rebuild
  into an empty schema checks the chain alone, and one alongside a restored backup checks
  against the head at the backup.
- The ledger is never rewritten. A retention purge appends a `lifecycle` entry with action
  `purge` before the index drops the asset's references, and a second one with action `purged`
  for the same asset once the index answers that the purge committed; the purged blobs go
  through [GC](#deletion-and-garbage-collection) as before. The `purged` entry is what resolves
  a purge: rebuild pairs it with the latest unconfirmed `purge` of that asset before it, and
  empties a tombstone only for a confirmed purge, at the `purge` entry's position. A purge the
  index refused (the asset was restored, or re-deleted under a later floor, after the worker
  listed it) is never confirmed, so a rebuild keeps every reference the live row kept, whatever
  order the purge and the concurrent op reached the ledger in. The confirmation is
  best-effort: a lost one keeps references to blobs GC may since have swept, which a rebuild
  reports as dangling rather than losing.
- Two processes append to one ledger: the server and an operator's `purge --apply`. An append
  holds an exclusive file lock on the owner's ledger directory from reading the tail to the
  fsync, and reads the tail from disk whenever another process has appended since. A scan holds
  the lock shared.
- Device-directory versions are in the ledger, so a rebuild verifies signatures against the
  directory as it stood when each manifest was accepted. The entry names the signed document by
  hash; the document itself, with the identity key it verifies under, is kept beside the ledger
  under that hash (`ledger/{owner_id}/attachments/`), written before the entry. The database
  keeps only the version in force, so the blob root is the one place every version lives, and
  a rebuild needs nothing else to verify against them.

### Rebuild

`capsule-server rebuild` reconstructs the asset index from `ledger/` plus `blobs/`, into an empty
schema or alongside a restored backup. It is a dry run unless given `--apply`, and exits non-zero
when it finds a fault ([`capsule-server::ledger::rebuild`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/ledger/rebuild.rs)):

1. Verify each owner's ledger chain: signatures under the attestation key's published half when
   the host holds `ATTESTATION_KEY_SEED`, links and positions otherwise, and the report says
   which, against the owner's anchor. The owners are those with a ledger and those with an
   anchor. A break stops that owner's replay at the last good entry and is reported; other
   owners continue. Under `--apply` the anchor is raised to the verified head, or, after a
   reported tail cut that moved the owner's epoch, lowered to it (cleared when nothing is left),
   so the loss is reported once and the server writes to the owner again. A caught rewrite
   keeps its anchor, and the server keeps refusing that owner's writes, since the mismatch is
   the only evidence of the rewrite. The operator recovers it by reading the report and running
   `rebuild --apply --accept-rewrites`, which re-anchors at the last entry before the mismatch
   and accepts the ledger as it stands; the next pass verifies and replays it.
2. Replay entries in ledger order: uploads and drop adoptions (custody entries the index did
   not refuse) and lifecycle ops, including confirmed retention purges. Decode each named manifest and
   write the rows they imply, the way the index's own writes would. An op with no recorded
   position is replayed only if it still chains onto the head the replay has reached. Each
   manifest's `device_sig` is verified against the signer's directory version in force at that
   point: the last one before the manifest's entry when the signer's ledger is the one replayed,
   and otherwise the last one the server's clock recorded no later than it, since two ledgers
   share no order. The versions come from the copies beside the ledger, replayed through the
   store's own checks (a version that does not strictly advance, or that is signed under
   another identity key than the account's first, was refused and is not in force; when a
   version with no usable copy comes before any key is known, the account's key is unknown and
   no later version vouches for anything). A device
   the version in force does not list, or lists as revoked, or that was added after the
   manifest's timestamp, fails; a device a later version revoked does not. A manifest that
   fails, or that falls under a version whose copy is missing or does not match its entry, is
   reported and its asset is not restored; when the index lacks the row, the owner moves to a
   new [sync epoch](#sync-epoch-and-client-re-announce), so the clients holding the manifest
   re-announce it and the server verifies it against the live directory. Replay is idempotent;
   a row the backup already holds is compared, never overwritten.
3. Recompute quota attribution for the restored rows. Blob references are a query over rows,
   so restoring the rows recomputes them.
4. Report blobs no entry names (orphans, left to GC), entries naming a missing blob (dangling,
   except an [`awaiting-original`](/design/transfer/upload/#upload-tiering-staged-uploads)
   asset), purged tombstones whose manifests GC has swept (only the database backup holds
   those rows), and envelopes failing structural validation (moved to `quarantine/` with a
   `.reason.json`, never dropped; a record from a newer client is reported and left in place).

Decoding manifests is confined to **one module**, the server's only signed-CBOR parser, shared
by rebuild and the [integrity scrub](/design/filesystem/maintenance/#server-side-integrity-scrub)
and fuzzed ([`capsule-server::ledger::manifest`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/ledger/manifest.rs)).
A provenance blob is the chain head's provenance record, and the parser checks that the record
and the manifest inside it agree. The write path never decodes manifests.

### Sync Epoch and Client Re-Announce

Rebuilt rows keep their ledger `sync_seq`, so clients' sync cursors stay valid. When a rebuild
cannot prove the sequence complete (a ledger chain break or lost tail), it numbers the rows it
cannot place above every number the owner has used and increments the owner's **sync epoch**.
The cursor carries the epoch it was minted in. A cursor from an older epoch is refused with
`409 error.sync.cursor_stale`; the client drops its cursor and its high-water marks, resyncs
from zero and discards nothing (invariant 5 in
[Versioning](/design/versioning/#system-invariants)).

After an epoch change, each client **re-announces** the signed manifests it holds whose blobs
the server reports missing from the index: manifests only, no bytes. The server verifies each
like an upload's manifest and restores the row if every named blob is present; otherwise the
client re-uploads through the normal [transfer protocol](/design/transfer/upload/). A
re-announced manifest can only restore what its owner's devices signed.

`POST /v1/sync/reannounce` takes a batch of chain-head provenance records and answers each one:
`restored`, `already_indexed`, `missing_blobs` naming what the store lacks, or `refused` with an
`error.sync.reannounce_*` code
([`capsule-server::sync::reannounce`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/sync/reannounce.rs)).
A manifest passes the write authority an upload's envelope passes, except the clock-drift
check: `created_by_user` is the caller, the caller may write to its album and it carries the
album's pin, and its device is in the caller's directory from before its timestamp. The head is
walked back through the store to its `create`: every manifest before it becomes the row's
superseded chain, as the live index retains it, and the newest one naming a metadata blob
supplies the metadata. A manifest missing from that walk is answered as a missing blob. A deletion is not re-announced, and
derivatives are not restored: an asset manifest does not name them, so the next push re-attaches
them. A restore appends the ledger entries a finalization would, before the index is touched,
so a later rebuild keeps the row. The client side is `capsule sync reannounce`, which
`capsule sync` runs itself after a stale cursor when it is given the library.

### What a Rebuild Cannot Recover

Restored from the PostgreSQL backup, never derived from blobs: accounts, credentials and TOTP,
escrow and sealed identities, share links and guest drops, moderation standing, federation
capabilities and peers, revoked tokens, device cohorts, and in-flight album upgrades. This is
why the [self-hosting guide](/guides/self-hosting/#backups) requires backing up PostgreSQL and
`BLOB_ROOT` together; the ledger makes a stale or partial database backup recoverable, not
optional.

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
- **Ledger crash window (unit).** A crash after the ledger append and before the row commit is replayed by rebuild; a tampered or truncated segment is reported and stops only that owner.
- **Ledger anchor (unit).** A ledger cut at an entry boundary is reported as behind its anchor by rebuild and the scrub when the index survived, with and without the attestation key, including a ledger that lost every entry; `--apply` re-anchors after the cut and the next pass is clean; a caught rewrite keeps its anchor unless `--accept-rewrites` is given.
- **Epoch (integration).** A rebuild with a lost ledger tail bumps the epoch; an old cursor is refused, the client resyncs and re-announces, and the index matches the pre-loss one.
- **Quarantine on malformed envelope (unit).** The object moves to `quarantine/` with a `.reason.json` naming the failed check.
- **GC safety (unit).** Swept only after the grace window; a re-reference during it cancels.
- **Dangling reference (unit).** Surfaced, never auto-deleted, never treated as collectable.
- **Verdict composition (unit).** Owner: [Transfer — Durability](/design/transfer/durability/).
