---
title: Server Filesystem
description: The server's blob store layout, Postgres index, and required services
status: draft
---

The server holds ciphertext blobs and a key-free index mapping assets to blobs. It holds no key, so it never decodes, extracts metadata or generates thumbnails. The layout below is a contract: rebuilding the index from blob bytes depends on it.

## Required Services

Two required systems, one code path:

- **Blob store**: the encrypted bytes. Adapters: filesystem and in-memory ([`capsule-server::blob`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/blob/mod.rs)).
- **PostgreSQL 18**: the authoritative durable index of ownership, album and blob references, lifecycle state, pending-asset rows, the device-cohort map and the quota ledger, and the expiring state: upload-session records, [](/design/authentication/) sessions and ceremonies, rate counters and federation pull leases. Tested against `postgres:18` on glibc, because asset-id order relies on `COLLATE "C"` behaving as deployed. [](/design/transfer/upload/#session-lifetime-and-discard) owns session lifetime.

Rules:

- The server MUST refuse to start without `DATABASE_URL` unless started with `--memory`, which binds every port to its in-memory adapter and logs a `WARN` on every start. `--memory` is a development profile, never a deployment mode.
- Each state port has exactly two adapters: PostgreSQL and in-memory. [`capsule-server::store`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/store/mod.rs) states the map. No other database or side store backs a port.
- Each expiring port keeps its own typed table, and each row carries its own `expires_at`. A read MUST treat a row at or past its `expires_at` (by the injected clock) as absent, so deleting expired rows is housekeeping, never correctness; [`capsule-server::store::sweep`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/store/sweep.rs) deletes them in bounded batches. A generic TTL or compare-and-set abstraction MUST NOT be introduced.
- The hot upload path (committed set, status) never touches the durable asset row.

### Availability

Both required systems are on the path of every write and every authenticated read, and the server has **no degraded mode**. Sessions, revocation, replay windows and every security budget live in PostgreSQL and fail closed, so a database the server cannot reach is an outage of sign-in, uploads, sharing and federation alike, exactly as an unreachable index already was. A degraded mode that admitted requests without those checks would trade an outage for a replay window or a revocation that does nothing, which the system invariants forbid. Availability therefore comes from PostgreSQL's own high availability (a streaming standby with failover), operated outside Capsule, and from the blob root's storage. While either is unreachable the server logs it at `ERROR` and answers `503` with `Retry-After`, never a guess.

### Server Clock

The server's clock is the **trusted clock**: it stamps `received_at` on receipts and ledger entries, decides retention windows, session and link expiry, and hand-off deadlines. Clients never send it wall-clock deadlines, and it never sends them one: every deadline crosses the wire as a duration. The host MUST keep it synchronized ([](/guides/self-hosting/#clock)). The server checks what it can:

- **At boot** it reads the latest instant it has recorded (the newest receipt or ledger entry). A clock earlier than that by more than five minutes is a stale snapshot or a broken time source, and `serve` refuses to start, naming both instants, because every expiry and retention decision it made would be judged against a wrong "now".
- **The operator can acknowledge a recorded future instant.** A clock that once jumped forward and stamped a receipt or ledger entry before it was corrected would otherwise keep `serve` from starting until real time caught up. Setting `CLOCK_ACKNOWLEDGE_RECORDED` to the exact latest instant the refusal named lets `serve` start once: it logs the acknowledgement at `WARN` with both instants, records it in PostgreSQL with the operator-supplied value and the clock's reading, and from then on checks only instants recorded after it. A value that is not the instant the server reads is refused, so a setting left in place cannot hide a later fault. The acknowledged receipts and entries keep their recorded times; rules timed from them, such as a retention window, err toward keeping data.
- **At runtime** a backward step larger than one second between successive reads of the wall clock is logged at `ERROR` with the step size. Receipts and ledger entries keep the clock's value; the server never invents a time to keep them monotonic.
- Durations the server measures itself (stall bounds, lease and lock timeouts) use the monotonic clock, which no step moves.

### Schema migrations

- `serve` MUST NOT migrate. On boot it refuses a schema behind the binary, naming `capsule-server-migration up`. A schema ahead of it is tolerated for rolling deploys, so a destructive migration is split in two.
- Every applied migration records a SHA-256 checksum of its source in `capsule_migration_checksums`, and boot refuses a mismatch with `PostgresError::SchemaDrift`. **An applied migration is never edited**: a schema change is always a new ordinal ([`capsule-server::postgres`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/postgres/mod.rs)).

## Blob Store Layout

```text
{blob_root}/
├── incoming/{upload_id}.bin                 # staged upload, positionally written, pre-verification
├── blobs/{hash[0:2]}/{hash[2:4]}/{hash}.bin # finalized, content-addressed, immutable
├── ledger/{owner_id}/{first_seq}.log         # owner ledger, append-only, signed
├── ledger/{owner_id}/attachments/{hash}      # documents a ledger entry names by hash
├── quarantine/                              # rejected and lost objects + .reason.json, for humans
└── .server/{version,config,lock}            # operator metadata (plaintext, not user data); lock: the owning process
```

- The whole tree MUST be on one filesystem that meets [](#blob-root-filesystem-contract), so finalization renames are atomic.
- `incoming/` holds one positionally written file per session, with no per-chunk staging and no assembly ([](/design/transfer/upload/#staging)).
- `blobs/` is sharded on the hash's own hex prefix so the three enumerations (integrity scrub, GC and index rebuild; see [](/design/filesystem/maintenance/#server-side-integrity-scrub), [](#deletion-and-garbage-collection), [](#recovering-the-index-from-blobs-alone)) stay affordable at scale. Lookup is a `stat` at a known address either way.
- `incoming/` and `quarantine/` are not sharded: neither is content-addressed, and an operator reads quarantine.

Commit discipline ([`capsule-server::blob::fs`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/blob/fs.rs)):

- Non-upload temp files live inside the target shard (`.{hash}.{uuid}.tmp`). Uploads need none: the `incoming/` file is renamed straight to its address.
- Order: fsync staged bytes, fsync each newly created shard directory in its parent, rename, fsync the destination directory, and only then write the index row. A lost directory entry under a committed row would be a dangling reference; the reverse is an orphan GC reclaims.
- **A commit onto an address already present replaces the file.** The freshly verified stage is renamed over the existing file, never discarded in its favour: the existing file has not been re-read since it was written, and may be the rotted or half-synced copy a failed flush left. The content is the same by its address, so a reader holding the old file open is unaffected.
- **A failed flush is never retried** ([](/design/transfer/#failure-model)). An `fsync` error on a stage, a blob, a shard directory or a ledger segment fails the operation it belongs to and marks the blob root degraded until restart.
- The shard derivation is tied to the 64-hex-char digest. Changing the digest is a layout change (`.server/version` bump and data move), guarded at compile time.

### Quarantine Retention

`quarantine/` holds objects pulled out of the store with the reason: envelopes that failed structural validation, and blobs that failed a re-hash or could not be read ([](/design/transfer/#repairing-a-lost-or-corrupt-blob)). It is operator evidence, not user data, and it is bounded:

- A quarantined blob whose address has since been **repaired** (verified bytes are back at the address) is deleted automatically 30 days after the repair, since an intact copy exists.
- Everything else stays until an operator deletes it (`capsule-server quarantine prune`, which lists before it removes, and removes nothing without `--apply`). Unrepaired bytes may be the last trace of a user's data, and the server never decides they are worthless.
- The scrub reports the quarantine's object count, byte total and oldest entry on every run ([](/design/filesystem/maintenance/#server-side-integrity-scrub)), so growth is visible before it matters.

## Blob Root Ownership

**Exactly one `serve` process owns a blob root**, and it proves it: at boot, before it lists sessions or recovers a claim, it takes an exclusive lock on `.server/lock` and holds it for its lifetime. A second `serve` on the same root, on this host or another sharing the filesystem, finds the lock held and refuses to start, naming the root. Maintenance commands that may run beside `serve` (`gc`, `purge`, `scrub`) do not take it; `rebuild --apply` takes it, so it cannot run while `serve` does.

The upload path depends on this. In-flight range claims, the finalize claim and the two per-chunk rate budgets are process state ([](/design/transfer/upload/#finalization-and-integrity)), and boot recovery treats every claim it finds as abandoned. With two processes on one root, a booting one would take over its sibling's live finalizations. The lock turns that from a documented assumption into a refusal. [ADR-0025](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0025-a-blob-root-has-exactly-one-serving-process.md) records the roads not taken.

Scaling is therefore vertical first, then by read replicas of PostgreSQL for the sync feed. Several upload-serving processes need, at least: a root per process with requests routed to the owner by upload id, the two chunk budgets counted in PostgreSQL or pinned to the owner, and a fenced claim lease. None is in v1.

## Blob Root Filesystem Contract

The blob root's filesystem MUST provide, for every process that touches the tree:

1. **Atomic rename within the tree**, replacing an existing target, so a blob appears at its address whole or not at all.
2. **Durable `fsync` on files and directories**: a flush that returns success survives power loss ([](/design/transfer/#failure-model)).
3. **Advisory file locks shared by every process and host that writes the tree**: an exclusive `flock`, or the byte-range lock a network filesystem emulates it with, held by one process excludes every other ([](#concurrent-appenders), [](#blob-root-ownership)).
4. **Positional reads and writes** at arbitrary offsets of an existing file.

At boot `serve` checks what it can: that the root's filesystem type is not one known to break the contract (FUSE, SMB/CIFS, overlay upper layers), that a probe file renames atomically onto an existing name, and that a second descriptor cannot take the exclusive lock the first holds. A failed check refuses to start. Firmware that acknowledges flushes it has not made, and a hypervisor that ignores them, cannot be detected from inside, which is why the operator's power-cut drill exists. Which concrete filesystems and mounts satisfy the contract is the operator's checklist ([](/guides/self-hosting/#blob-directory)).

### Why POSIX, Not Object Storage

The blob root is a POSIX filesystem because the commit protocol is built from two primitives an object store does not offer together:

- **Rename-commit.** A stage is written positionally on the 4 KiB grid, verified, and published by one atomic rename. An object store has no rename. Its multipart upload requires parts of at least 5 MiB except the last, so the protocol's adaptive chunks and out-of-order ranges cannot map onto parts, and the stage would still need a local filesystem.
- **`flock`.** The owner ledger's appenders and the root's single owner exclude each other with a file lock. An object store has neither append nor a lock, so the ledger would need conditional writes per segment or a move into PostgreSQL.

Object storage is out of v1. A future adapter would need: local staging with a verified upload by conditional write (`If-None-Match: *`) as the commit; a ledger with a single writer enforced by conditional writes; GC deletes that cannot race a re-commit (object versioning or a lock held in PostgreSQL); listing costs budgeted for the scrub and rebuild walks; and the per-address mutual exclusion GC already requires. The blob port already isolates the store ([`capsule-server::blob`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/blob/mod.rs)), so this is an adapter, not a protocol change.

### Reads Stream; No mmap

Blob reads stream through a bounded buffer by positional reads, in windows no larger than one fetch window. A blob is never memory-mapped: a file truncated or replaced under a mapping (a quarantine, a repair, an operator's mistake, a network filesystem's reconnect) raises `SIGBUS` on the next access and kills the whole process, where a read only returns an error to one request.

## Uniform, Opaque Blobs

An asset is a **bundle** of blobs ([](/design/transfer/upload/#what-gets-uploaded)): encrypted original, derivatives, metadata blob, and the signed manifest stored as the provenance blob. The store does not distinguish them. Roles live in PostgreSQL, with the manifest envelopes and the owner ledger ([](#the-owner-ledger)) as the key-free durable fallback.

A lifecycle op moves the provenance role to its new record. The record it moved past stays referenced in the asset's **superseded chain** until a purge closes the retention window, or until compaction releases it.

**Compaction of superseded provenance.** Every edit adds a superseded record of a few kilobytes (its hybrid device signature alone is about 3.4 KB), so a long-edited asset's chain grows without bound. The server never compacts a chain on its own, because only a device can sign the record that makes earlier ones unnecessary, and the server never produces signed data. A superseded record becomes collectable only when:

- a later record of the same chain, signed by an authorized device, is a **checkpoint** at or after it: the signed checkpoint record that [](/design/cryptography/provenance/#chained-append-only-structure) designates as the chain's v2 extension, against which a device that first sees the asset verifies instead of walking back to the create; and
- no `delete` at or before that checkpoint still has an open retention window, since the record is the server's rebuttal in a proof-of-loss dispute until the window closes ([](/design/transfer/durability/#proof-of-loss)).

A client SHOULD checkpoint a chain once it holds more than 64 superseded records. Until checkpoints exist, the superseded chain is bounded only by the asset's edit count, which is a known gap. The blob read route serves each superseded record as a provenance blob of the asset that holds it ([](/reference/api/sync/)): byte for byte as uploaded, under the read authority of the live record, with the same refusals (a deleted or held asset's records are `410`). A device that first sees an asset past its `create` fetches the record each `prior_provenance_hash` names back to the create, verifies the chain in order, and holds the asset from its start. The sidecar comes from the asset's live metadata blob, since a superseded metadata blob is collected.

## Recovering the Index from Blobs Alone

PostgreSQL's asset index is a **projection** of the blob store, never the only copy. Losing the database, or finding it corrupt, costs a rebuild, not a library. Accounts, credentials and everything else that is not an asset fact still come from the PostgreSQL backup ([](#what-a-rebuild-cannot-recover)).

### The Owner Ledger

Every signed manifest envelope ([](/design/cryptography/provenance/#asset-manifest)) is stored in `blobs/` and names everything an index row needs except who owns it and the order the server accepted it in. The **owner ledger** (`ledger/` in [](#blob-store-layout)) carries those facts.

**Entries.**

- One entry per accepted manifest, per device-directory version and per lifecycle action: `sync_seq`, the manifest's content hash, `upload_user_id`, uploader device, `received_at` and the custody receipt. The ledger is where a receipt's bytes are kept; PostgreSQL only indexes them ([](/design/transfer/durability/#custody-receipts)).
- Each entry carries its owner, its position (`seq`, from 1, contiguous) and its predecessor's hash. Edits, removals, a dropped head and a torn tail are detected from the chain alone. A tail cut exactly at an entry boundary is detected only against the anchor ([](#anchor)).
- **Signed checkpoints.** The attestation key signs a **checkpoint** entry naming the head it covers, not each entry: the hash chain already binds every entry to its successors, so one signature over a head authenticates the whole prefix. The appender writes a checkpoint at most one second after an unsigned entry was appended, after every 64 entries, and before a clean shutdown. An entry after the last checkpoint is verified by its links and the anchor only, the same check a host without the key makes, and rebuild and the scrub report how many such entries each owner has. A per-entry hybrid signature would cost about 3.4 KB on every entry, more than the rest of the entry; a checkpoint spends it once per burst.
- A segment is a sequence of length-prefixed canonical-CBOR frames. The only bytes ever cut are those after the last whole entry when no complete entry starts anywhere after them (a short frame, or zeroes a crash left); they were never acknowledged. Damage followed by an entry refuses further appends.

**Write order.**

- An entry is appended and fsynced **after** the blobs it names are committed and **before** the index row commits. A crash between the two leaves an entry without a row, which the next rebuild or scrub replays; never a row without an entry.
- The `sync_seq` and the custody receipt are minted inside the index transaction, so a finalization writes two entries: a **custody** entry before the transaction (content hash, size, role, uploader, `received_at`), whose failure fails the finalization, and an **indexed** entry after it carrying the `sync_seq` and the receipt. The index keeps the receipt's bytes until that indexed entry is durable and then drops them, and boot and the scrub append an indexed entry for any receipt whose bytes the index still holds, so a crash between the two loses no receipt. A missing indexed entry is a sequence the rebuild cannot prove, handled like a lost tail.
- **An `fsync` error seals the segment.** The appender writes nothing more to it, fails the operation, and refuses the owner's writes until a restart re-reads the tail and checks it against the anchor ([](/design/transfer/#failure-model)).
- A write the index **refuses** after its custody entry is durable (a conflicting role, an incomplete or stale replace, a vanished asset) appends a **refused** entry naming the upload instead. An indexed or a refused entry resolves a custody entry, and rebuild replays only unresolved ones. A write the index could not answer (an outage) gets neither and is replayed like a crash. The refused append is best-effort: replay re-runs the checks that refused it.

#### Anchor

The **anchor** is each owner's ledger head, `(seq, entry hash)`, recorded in PostgreSQL (`owner_ledger_heads`) after every append is durable.

- It only rises, so it never names an entry the ledger did not hold. Recording it is best-effort: a lost record leaves it behind the ledger, which weakens the check and never fakes a fault.
- Rebuild and the scrub verify each ledger against it. A ledger that ends below it lost its tail (`BehindAnchor`); one holding a different entry at its position was rewritten (`AnchorMismatch`). Both walk every owner the index anchored as well as every owner with a ledger, so an owner whose ledger lost every entry is reported too.
- The server checks the same before its first append per owner after a start, and refuses that owner's writes while the ledger disagrees with its anchor, so a loss is never buried under new entries.
- The way out is `rebuild --apply`. It clears a cut by re-anchoring at the head that is left, and clears a caught rewrite only when the operator adds `--accept-rewrites` after reading the report ([](#rebuild)).
- It lives in the index, not beside the ledger, because a cut that took `BLOB_ROOT`'s tail would take a head file with it. It is not in the custody receipt because a receipt is client-held signed evidence about one blob, and a ledger head is a fact about the server's storage that a client neither needs nor can check. The anchor survives exactly when the index does: a rebuild into an empty schema checks the chain alone, and one alongside a restored backup checks against the head at the backup.

#### Purges

The ledger is never rewritten. A retention purge appends a `lifecycle` entry with action `purge` before the index drops the asset's references, then, once the index answers that the purge committed, a second one with action `purged` naming the `purge` entry's ledger position. The purged blobs go through garbage collection ([](#deletion-and-garbage-collection)) as before.

- The `purged` entry resolves a purge. Rebuild confirms exactly the `purge` entry it names, so two concurrent purge runs cannot confirm each other's refused purge, and empties a tombstone only for a confirmed purge, at the `purge` entry's position.
- A purge the index refused (the asset was restored, or re-deleted under a later floor, after the worker listed it) is never confirmed. A rebuild therefore keeps every reference the live row kept, whatever order the purge and the concurrent op reached the ledger in.
- The only restore that can race a purge is one the server accepted before `retention_until`; one at or after the floor is refused ([](/design/organization/#retention-window)). Whichever reaches the row lock first wins, and a restore arriving after a committed purge is refused too, so no restore follows a committed purge.
- The confirmation is best-effort. A lost one keeps references to blobs GC may since have swept, which a rebuild reports as dangling rather than losing.

#### Concurrent appenders

Two processes append to one ledger: the server and an operator's `purge --apply`.

- An append holds an exclusive file lock on `.lock`, an empty file in the owner's ledger directory opened for writing, from reading the tail to the fsync. It re-reads the tail from disk whenever another process has appended since.
- A scan holds the lock shared through a read-only descriptor and never creates the file. Where no append has made it yet the scan reads unlocked, so a scrub or rebuild leaves no file in `ledger/`, whatever user it runs as. The lock file is not a segment, so rebuild and the scrub never read it.
- It is a file rather than the directory because a directory opens read-only, and where `flock` is emulated with byte-range locks (Linux on NFS) an exclusive lock on a read-only descriptor is refused.

#### Directory versions

Device-directory versions are in the ledger, so a rebuild verifies signatures against the directory as it stood when each manifest was accepted. The entry names the signed document by hash. The document itself, with the identity key it verifies under, is written beside the ledger under that hash (`ledger/{owner_id}/attachments/`) before the entry. The database keeps only the version in force, so the blob root is the one place every version lives, and a rebuild needs nothing else to verify against them.

### Rebuild

`capsule-server rebuild` reconstructs the asset index from `ledger/` plus `blobs/`, into an empty schema or alongside a restored backup. It is a dry run unless given `--apply`, and exits non-zero when it finds a fault ([`capsule-server::ledger::rebuild`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/ledger/rebuild.rs)).

1. **Verify each owner's ledger chain** against the owner's anchor, for every owner with a ledger or an anchor. It verifies checkpoint signatures under the attestation key's published half when the host holds `ATTESTATION_KEY_SEED`, links and positions otherwise, and the report says which, and how many entries follow the last checkpoint. A break stops that owner's replay at the last good entry and is reported; other owners continue.
   - Under `--apply` the anchor is raised to the verified head or, after a reported tail cut that moved the owner's epoch, lowered to it (cleared when nothing is left). The loss is reported once and the server writes to the owner again.
   - A caught rewrite keeps its anchor and the server keeps refusing that owner's writes, since the mismatch is the only evidence of the rewrite. The operator reads the report and runs `rebuild --apply --accept-rewrites`, which re-anchors at the last entry before the mismatch and accepts the ledger as it stands; the next pass verifies and replays it.
2. **Replay entries in ledger order**: uploads and drop adoptions (custody entries the index did not refuse) and lifecycle ops, including confirmed retention purges. Decode each named manifest and write the rows it implies, the way the index's own writes would. Replay is idempotent; a row the backup already holds is compared, never overwritten.
   - An op with no recorded position is replayed only if it still chains onto the head the replay has reached and, for a `trash-restore`, only if no confirmed purge precedes it, since the index refuses a restore of a purged row ([](/design/organization/#retention-window)).
   - Each manifest's `device_sig` is verified against the signer's directory version in force at that point: the last one before the manifest's entry when the signer's ledger is the one replayed, otherwise the last one the server's clock recorded no later than it, since two ledgers share no order.
   - Versions come from the copies beside the ledger, replayed through the store's own checks. A version that does not strictly advance, or that is signed under an identity key other than the account's first, was refused and is not in force. When a version with no usable copy comes before any key is known, the account's key is unknown and no later version vouches for anything.
   - A device the version in force does not list, or lists as revoked, fails; a device a later version revoked does not. No manifest `timestamp` is compared (invariant 7 of [](/design/threat-model/validation/#server-side-validation-invariants)).
   - A manifest that fails, or falls under a version whose copy is missing or does not match its entry, is reported and its asset is not restored. When the index lacks the row, the owner moves to a new sync epoch ([](#sync-epoch-and-client-re-announce)), so the clients holding the manifest re-announce it and the server verifies it against the live directory.
3. **Recompute quota attribution** for the restored rows. Blob references are a query over rows, so restoring the rows recomputes them.
4. **Report** blobs no entry names (orphans, left to GC); entries naming a missing blob (dangling, except an `awaiting-original` asset; see [](/design/transfer/upload/#upload-tiering-staged-uploads)); purged tombstones whose manifests GC has swept (only the database backup holds those rows); and envelopes failing structural validation (moved to `quarantine/` with a `.reason.json`, never dropped; a record from a newer client is reported and left in place).

Decoding manifests is confined to **one module**, the server's only signed-CBOR parser, shared by rebuild and the integrity scrub ([](/design/filesystem/maintenance/#server-side-integrity-scrub)) and fuzzed ([`capsule-server::ledger::manifest`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/ledger/manifest.rs)). A provenance blob is the chain head's provenance record, and the parser checks that the record and the manifest inside it agree. The write path never decodes manifests.

### Sync Epoch and Client Re-Announce

Rebuilt rows keep their ledger `sync_seq`, so clients' sync cursors stay valid. When a rebuild cannot prove the sequence complete (a ledger chain break or lost tail), it numbers the rows it cannot place above every number the owner has used and increments the owner's **sync epoch**. A cursor carries the epoch it was minted in. A cursor from an older epoch is refused with `error.sync.cursor_stale`; the client drops its cursor and high-water marks, resyncs from zero and discards nothing (invariant 5 of [](/design/versioning/#system-invariants)).

After an epoch change, each client **re-announces** the signed manifests it holds whose blobs the server reports missing from the index: manifests only, no bytes. The server verifies each like an upload's manifest and restores the row if every named blob is present; otherwise the client re-uploads through the normal transfer protocol ([](/design/transfer/upload/)). A re-announced manifest can only restore what its owner's devices signed.

The re-announce route ([](/reference/api/sync/), [`capsule-server::sync::reannounce`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/sync/reannounce.rs)) takes a batch of chain-head provenance records and answers each `restored`, `already_indexed`, `missing_blobs` (naming what the store lacks) or `refused` with an `error.sync.reannounce_*` code.

- A manifest passes the write authority an upload's envelope passes, except the clock-drift check: `created_by_user` is the caller, the caller may write to its album and it carries the album's pin, and its device is listed and not revoked in the caller's directory version in force when the server receives the re-announce (invariant 7 of [](/design/threat-model/validation/#server-side-validation-invariants)). No timestamp is compared.
- The head is walked back through the store to its `create`. Every manifest before it becomes the row's superseded chain, as the live index retains it, and the newest one naming a metadata blob supplies the metadata. A manifest missing from that walk is answered as a missing blob.
- A deletion is not re-announced. Derivatives are not restored: an asset manifest does not name them, so the next push re-attaches them.
- A restore appends the ledger entries a finalization would, before the index is touched, so a later rebuild keeps the row.

The client side is `capsule sync reannounce`, which `capsule sync` runs itself after a stale cursor when given the library.

### What a Rebuild Cannot Recover

Restored from the PostgreSQL backup, never derived from blobs: accounts, credentials and TOTP, escrow and sealed identities, share links and guest drops, moderation standing, federation capabilities and peers, revoked tokens, device cohorts, and in-flight album upgrades. This is why the self-hosting guide ([](/guides/self-hosting/#backups)) requires backing up PostgreSQL and `BLOB_ROOT` together. The ledger makes a stale or partial database backup recoverable, not optional.

## Manifest Envelope Validation

Every write (upload creation, chunk, finalization, lifecycle manifest, federation pull) passes structural envelope validation **before** any state is persisted. [](/design/threat-model/validation/#server-side-validation-invariants) owns the checklist and rejection codes.

## Content-Addressing and Deduplication

A blob already present is never stored twice. [](/design/transfer/upload/#deduplication-and-merge) owns duplicate and merge semantics at session creation. Whether a blob is referenced is decided by a query over committed rows, never a stored counter.

## PostgreSQL: What the Server Knows

Only key-free facts:

- `asset_id`, `owner_id`, `album_id`, `upload_user_id`
- blob content hashes and each blob's role. Every content address and hash column is `bytea` holding the 32 raw bytes, with a check that it is exactly 32 long; hex exists only at the API boundary. Hex text doubles every copy, and an address is copied into heap rows and into every index that names it
- `amk_version`, declared ciphertext size, `content_type`
- `uploaded` flag and server-visible lifecycle state
- the server's trusted `received_at` (authoritative for time-based policy) beside the client's audit-only `timestamp`
- `client_version` (audit-only, queryable to scope a defective build)
- provenance records ([](/design/cryptography/provenance/#provenance-of-library-modifications))

No plaintext capture date, dimensions, EXIF, tags or filename ever reaches the server; those live in the encrypted metadata blob ([](/design/cryptography/encryption/#metadata-encryption)). Session creation writes a pending row (`uploaded = false`) that finalization flips ([](/design/transfer/upload/#session-state-machine)).

## Ownership, Partitioning, and Quota

`owner_id` is the billing and namespace entity, mirrored as an MLS group ([](/design/cryptography/keys/#owner-group-keys-ogks)). Quota is charged to `upload_user_id` ([](/design/quota/)). The blob store is global; every *reference* is owner-scoped, and dedup is owner-scoped.

The owner record carries a non-secret `default_album_id` (plus optional scope overrides) naming the default album ([](/design/organization/#the-default-album)). It is discovery only: writes are still gated on album write capability (invariant 6 of [](/design/threat-model/validation/#server-side-validation-invariants)).

## Deletion and Garbage Collection

The server cannot read `is_deleted`: lifecycle state is client-signalled and recorded on the asset row, and soft delete is a state change. A blob is removed only when provably unreferenced, and every failure mode biases toward keeping bytes.

- **Reference count is a query**, never a maintained counter. A blob is referenced by any of:
  - a committed asset, derivative, metadata or provenance row, and a superseded record not yet released ([](#uniform-opaque-blobs));
  - a live share-link record ([](/design/share-links/#storing-the-record));
  - a pulled federated album's claim ([](/design/federation/));
  - a guest drop in an inbox, until it is adopted, rejected or expires ([](/design/web-upload/#drop-and-adoption-lifecycle)). A drop has no asset row, and without this a drop the owner has not reviewed within the grace window would be swept from under its inbox row;
  - an upload session that is not terminal and declared that hash, a drop session included. A session declares its hash at creation, before any byte moves, so a finalization in flight is always visible to the collector.
- **Mark, then sweep.** Zero references marks a blob (`collectable_since`). It is swept only after a 24 h grace window ([`DEFAULT_GRACE_WINDOW`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/gc/mod.rs)) and only if zero references is re-confirmed at sweep time. A reappearing reference cancels the mark. This reclaims the crash-after-rename orphan without racing a late reference.
- **Sweep and commit exclude each other per address.** The sweep's re-confirmation through its unlink, and a finalization's commit through its index transaction, hold the same per-address lock. Otherwise a re-upload of bytes whose last holder was purged could commit onto the existing file (or rename over it) just as a sweep that had already re-confirmed zero references unlinks it, leaving a fresh index row and custody receipt naming nothing. With the lock, either the sweep finishes first and the commit places the stage at an empty address, or the commit finishes first and the sweep sees the reference.
- **A Postgres↔filesystem mismatch is never resolved by deletion.** An unreferenced blob is an orphan for GC. A committed row referencing a missing blob is a loud integrity error: logged, surfaced, recorded as lost and repaired from any client that holds the bytes ([](/design/transfer/#repairing-a-lost-or-corrupt-blob)), never auto-deleted.
- Every GC decision is logged with hash, observed count and mark/sweep times; a dry-run mode reports without removing.

## Storage Verification

The server attests, key-free, whether a blob is stored (`stat` in `blobs/`, and not recorded as lost), indexed (committed `uploaded = true` row) and retrievable (referenced, not `collectable_since`, not quarantined). A read error or a failed re-hash on a committed blob records it lost ([](/design/transfer/#repairing-a-lost-or-corrupt-blob)). Anything collectable, quarantined or missing is non-retrievable, so a client never releases a copy the server is about to lose. [](/design/transfer/durability/) owns the wire contract and the verify-before-destroy rule; the route is [`capsule-server::verify`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/verify/mod.rs).

## Growth and Retention

Every table and directory that grows with traffic MUST have a stated bound, and that bound lives in the doc that owns the data. Expiring state needs no entry: each such table carries `expires_at` and is swept ([](#required-services)). The append-only stores, at the target scale of 100k+ assets per account and 100k+ accounts:

| Store | Grows with | Bound and retention | Hot query, and its index |
| --- | --- | --- | --- |
| Blobs (`blobs/`) | Finalized blobs | Referenced blobs; the rest are collected ([](#deletion-and-garbage-collection)) | Lookup by address (a `stat`) |
| Owner ledger (`ledger/`) | One custody and one indexed entry per finalization, one per lifecycle op | Never rewritten: grows with the owner's lifetime writes, partitioned by owner. An account's ledger goes with the account. | Tail append under the owner's lock |
| `custody_receipts` (index) | One row per blob finalization | Rows of an asset's receipts go with its purge; the bytes stay in the ledger ([](/design/transfer/durability/#custody-receipts)) | An asset's receipts in order, `(owner, asset, position)` |
| `applied_manifests` | One row per applied lifecycle write | 30 days. The row only answers a replay with the sequence number it first minted; a replay older than that is answered from the chain, as an already-applied record at the asset's current sequence | Lookup by manifest hash (primary key) |
| `asset_superseded` | One row per edit | Until purge or a checkpoint releases it ([](#uniform-opaque-blobs)) | An asset's chain in order, `(asset, position)` |
| `moderation_events` | Operator actions only, never user traffic | Kept for the account's lifetime, as client-verifiable evidence ([](/design/moderation/#the-moderation-record)) | An account's record in order, `(user, seq)` |
| `activity_events` | Account events | Bounded already: 90 days and at most 1,000 per account ([](/design/activity-log/#retention)) | An account's events in order |
| `quarantine/` | Rejected and lost objects | [](#quarantine-retention) | Operator listing |

Partitioning or sharding, when a single instance stops sufficing, is by `owner_id`, which every hot table already carries.

## Per-Blob Overhead

What one finalized blob costs the server beyond its own bytes. Sizes assume ext4 with 4 KiB blocks and the hybrid attestation signature (64-byte Ed25519 plus 3,309-byte ML-DSA-65).

| Item | Per blob | Notes |
| --- | --- | --- |
| Self-description | 56 B header plus 16 B per 65,520-byte chunk ([](/design/cryptography/encryption/#blob-header)) | About 0.03% of a large blob |
| Filesystem | ~2 KiB average tail slack, a 256 B inode and a ~76 B directory entry | Dominates only for small blobs (thumbnails, metadata, manifests) |
| PostgreSQL rows and indexes | ~0.5 KB | The blob row with its two indexes and its quota attribution. Raw `bytea` addresses save about 33 B on every copy of an address relative to hex text, around 200 B per blob across the rows and indexes that carry it |
| Custody receipt | ~3.9 KB, once, in the ledger | Plus an index row of ~0.1 KB. Keeping a second full copy in PostgreSQL would add another 3.9 KB |
| Ledger entries | ~0.4 KB of entry cores, plus an amortized checkpoint | Signing each entry instead would add ~6.8 KB per finalization (two hybrid signatures) |

Leaving filesystem slack aside, the total is about 5 KB of metadata per blob, against about 15.5 KB with per-entry ledger signatures, hex addresses and a receipt stored twice. An asset is typically seven blobs (original, metadata, provenance, two derivatives and their two manifests), so about 35 KB per asset instead of about 110 KB: roughly 3.5 GB of metadata for a 100k-asset library instead of 11 GB. The receipt's signature is now the dominant term, and it is what the custody guarantee costs.

## Known Gaps

- **No root lock.** Nothing in `capsule-server/src` takes `.server/lock`, so blob-root ownership by one `serve` process ([](#blob-root-ownership)) is an assumption, not a refusal.
- **The per-address lock covers only federation pulls.** The collector's sweep runs inside `sweep_unless_held` (`capsule-server/src/federation/pull/pulled/postgres.rs:1068`), which excludes a pulled-metadata claim; an upload finalization's commit and index transaction take no lock on the address ([](#deletion-and-garbage-collection)).
- **One receipt chain per server.** Receipts are chained server-wide under `LOCK TABLE custody_receipts` (`capsule-server/src/attestation/postgres.rs:127`), not per owner ([](/design/transfer/durability/#known-gaps)).
- **No clock check.** `serve` neither compares its clock with the latest recorded instant at boot nor logs a backward step at runtime, and `CLOCK_ACKNOWLEDGE_RECORDED` does not exist ([](#server-clock)).

## Validation

- **Layout round-trip (unit).** A finalized blob lives at exactly its sharded address and re-hashes to its name.
- **Shard enumeration (unit).** Scrub, GC and rebuild walks recover the same blob set across a partially populated shard tree.
- **Rebuild idempotency (smoke).** Drop the index, rebuild, compare to the expected set; a re-run yields no changes.
- **Ledger crash window (unit).** A crash after the ledger append and before the row commit is replayed by rebuild; a tampered or truncated segment is reported and stops only that owner.
- **Ledger anchor (unit).** Rebuild and the scrub report a ledger cut at an entry boundary as behind its anchor when the index survived, with and without the attestation key, including a ledger that lost every entry; `--apply` re-anchors after the cut and the next pass is clean; a caught rewrite keeps its anchor unless `--accept-rewrites` is given.
- **Epoch (integration).** A rebuild with a lost ledger tail bumps the epoch; an old cursor is refused, the client resyncs and re-announces, and the index matches the pre-loss one.
- **Quarantine on malformed envelope (unit).** The object moves to `quarantine/` with a `.reason.json` naming the failed check.
- **GC safety (unit).** Swept only after the grace window; a re-reference during it cancels; a drop in an inbox and a live session declaring the hash are references.
- **GC versus commit (unit).** A sweep and a finalization of the same marked address, interleaved at every step, never leave an index row naming a missing blob.
- **Commit onto a present address (unit).** A corrupted file at the address is replaced by the verified stage.
- **Blob-root ownership (smoke).** A second `serve` on the root refuses to start while the first holds it, and starts once the first exits.
- **Filesystem contract (unit).** Each boot probe fails closed against a mock filesystem that breaks it.
- **Failed flush (unit).** An injected `fsync` error fails the session or finalization, is never retried, and marks the root degraded.
- **Clock regression (unit).** A clock earlier than the newest recorded instant by more than the tolerance refuses boot.
- **Ledger checkpoints (unit).** Rebuild verifies a checkpointed prefix by signature and reports the unsigned tail; a receipt whose ledger entry was lost in a crash is appended at boot.
- **Dangling reference (unit).** Surfaced, never auto-deleted, never treated as collectable.
- **Verdict composition (unit).** Owner: [](/design/transfer/durability/).
