---
title: Library Maintenance and Atomic Writes
description: How Capsule keeps client and server storage consistent, repairs what it can, and writes atomically
status: draft
---

A client library sits on consumer hardware and is edited by a process that can be killed mid-write, so a client never assumes its library is consistent: it periodically proves it, repairs what is safe and surfaces the rest. The server runs an equivalent integrity scrub ([](#server-side-integrity-scrub)). All routines are conservative: irreplaceable data MUST NOT be removed without explicit user confirmation.

This page is the canonical statement of the atomic-write rules ([](#atomic-writes-and-crash-recovery)); other docs link rather than restate them.

## Scrubbing

A startup scrub removes `.tmp` debris under `media/` older than an age floor, so it never races an in-flight write, at most once per interval. Both constants live in [`capsule-library::library::scrub`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-library/src/library/scrub.rs). Every removal is logged. The server sweeps stale `incoming/*.bin` likewise.

## Self-Validation

### Structural Validation (Cheap, at Startup)

Checks the client layout ([](/design/filesystem/client/#desktop-library-layout)):

- Every original has a sidecar and provenance chain. Every sidecar parses, has its required fields, a `sidecar_schema` ≤ the client's max known ([](/design/principles/#postels-law-asymmetric)), and a valid signature from a directory device.
- A sidecar's `uuid` matches its filename. Date-bucket drift is expected after a capture-date correction and repaired as a rename bundle; it is a fault only when unexplained.
- Every `cache/` entry refers to a known asset.
- Each provenance chain walks from `create` to head. A break is a quarantine surface, never skipped.
- Index rows reference existing files ([](/design/filesystem/client/#local-index-staleness)).

### Content Validation (Expensive, Scheduled)

Re-hashes each local original against the sidecar's plaintext `hash`, with the algorithm set by the sidecar's `crypto_suite_id` (which governs plaintext and ciphertext digests in lockstep). Silent bit rot of an original is the worst client failure, and nothing else detects it. It MUST NOT run at startup; it runs idle, on power, unmetered, throttled and on a slow rolling cadence, or on demand.

## Server-Side Integrity Scrub

An operator-invoked, schedulable command that verifies a Postgres + blob-store pair on a code path separate from the write path, so one bug cannot hide another. It is **read-only**: it classifies and reports, never repairs. Repair stays with GC ([](/design/filesystem/server/#deletion-and-garbage-collection)), index rebuild ([](/design/filesystem/server/#recovering-the-index-from-blobs-alone), which also rebuilds drifted pulled quota totals) or the operator. [`capsule-server::scrub`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/scrub/mod.rs) defines the finding classes.

1. **Row → blob.** Every committed reference resolves under `blobs/`. A miss is a dangling reference, except a missing original on an `awaiting-original` asset ([](/design/transfer/upload/#upload-tiering-staged-uploads)).
2. **Blob → row.** Every blob is referenced by a committed row or a live share link ([](/design/share-links/#storing-the-record)); otherwise it is an orphan, reported for GC.
3. **Byte integrity (deep mode).** Blobs re-hash to their names, rolling and throttled, with the same semantics as `deep` in [](/design/transfer/durability/).
4. **Chain ⇄ index.** Each asset's chain walks back to a `create` through retained manifests, stopping at the first break, and the head must name a held provenance blob. Signatures are not verified; that is the client's `verify_asset` ([](/design/cryptography/keys/#write-authorization)).
5. **Mirrored facts.** Facts Postgres mirrors from the signed manifest match the manifest's own copies.
6. **Ledger ⇄ index.** Every owner ledger chain ([](/design/filesystem/server/#the-owner-ledger)) verifies, and every entry has its row. It verifies signatures when the host holds `ATTESTATION_KEY_SEED`, links and positions otherwise, and the report says which. It reports a ledgered upload the index has no row for. Each ledger is verified against its owner's anchor ([](/design/filesystem/server/#anchor)), so a tail cut exactly at an entry boundary is reported whenever the index survived. The scrub reads the anchor and never moves it.
7. **Debris and quarantine inventory.** Unattached `incoming/` files and everything in `quarantine/` are listed.
8. **Quota ledger.** Every account's stored pulled quota total ([](/design/quota/#accounting-model)) and holdings equal what its subscriptions, pulled blobs and attributions derive; an account that differs is a drift finding. A ledger that derives the total on every read cannot drift and reports none. The derivation is the authority by definition, so `rebuild --apply` rebuilds every drifted account under its lock and a dry-run rebuild names them; the scrub only reports.

Checks 4 and 5 decode manifests through the server's single manifest parser, shared with rebuild ([](/design/filesystem/server/#rebuild)); the write path never decodes them. The server retains an asset's whole manifest chain until purge, which makes check 4 and the proof-of-loss rebuttal ([](/design/transfer/durability/#proof-of-loss)) possible.

Findings are **classified, never adjudicated**: the report names the check and both sides' evidence without assuming which side is wrong. Findings are logged structured; a non-zero count is the exit signal.

## Repair

*Ephemeral data is rebuilt silently; irreplaceable data is never destroyed to resolve an inconsistency.*

| Finding | Action |
| --- | --- |
| Stale `.tmp` | Deleted by the scrub. |
| Orphaned `cache/` entry | Deleted. |
| Bundle without an index row | Re-indexed on open from its sidecar and chain, through the same per-asset path as a rebuild ([](#atomic-writes-and-crash-recovery)). |
| Index inconsistency | Rebuilt from sidecars and chains: row, user tags and stacks. Safe but not lossless (index-only state such as importer-formed stack placement is lost), so rebuild is never a schema-upgrade path. |
| Sidecar without original | Expected under metadata-only sync scope ([](/design/sync/#synchronization-scope)); otherwise re-fetch the original. |
| Original without sidecar | Never deleted: quarantined, surfaced, minimal sidecar re-derived. |
| Sidecar without provenance chain | Not restored on open; listed as unanchored and left untouched. A write renames its chain into place last, so an interrupted one leaves this rather than a torn chain. |
| Malformed sidecar | Bytes quarantined verbatim with reason, never silently skipped; current sidecar re-fetched first. |
| Invalid sidecar signature | Quarantined, never overwritten; persistent failure shown as "provenance broken". |
| Corrupt original | Re-fetched and derivatives regenerated if the server holds it; surfaced loudly if this is the only copy. |

Every finding and repair is logged.

## Deduplication

Three distinct layers:

- **Server ciphertext dedup**: [](/design/filesystem/server/#content-addressing-and-deduplication).
- **Import-time dedup**: [](/design/transfer/upload/#deduplication-and-merge).
- **Intra-library dedup** (here): byte-identical originals within one library, from overlapping imports or a restore over a live library.

The key is the sidecar's plaintext `hash`, not the ciphertext hash, which differs per album key. Dedup is not stacking ([](/design/organization/#asset-stacking)) and never acts on merely similar images. Resolution is user-chosen, never automatic. The survivor takes the union of albums and tags (OR-set semantics, [](/design/metadata/#collaborative-metadata)), the highest rating and the earliest timestamps. The loser is soft-deleted with a signed provenance record, so the action is reversible.

## Atomic Writes and Crash Recovery

Every write that must not tear stages a temp file on the destination filesystem, then renames. Cross-doc invariants: [](/design/threat-model/validation/#atomicity-invariants).

- **Client single file.** Stage `{name}.tmp` in the destination directory, then rename. Never overwrite in place.
- **Client per-asset bundle.** Stage all files, then rename in fixed order: original, sidecar, provenance. A rewrite first stages the displaced version as `.prev` so rollback can restore it. Any failure rolls back completed renames, so a new provenance record implies a committed bundle.
- **Client stack edit.** All sidecars and per-asset provenance records stage and rename as one batch; there is no partial stack.
- **Client index write.** An asset's index rows (row, user tags, original's cache entry) are written after its bundle is durable, in one transaction, so a reader never sees a row with a partial tag set. A bulk import commits these rows once per bounded batch, each asset a savepoint within it, and the batch commits before any source is released. A crash between bundle and commit leaves a bundle with no row; opening the workspace compares the indexed ids with the bundles on disk and re-indexes the missing ones from their sidecars, at the cost of one id scan when none is missing.
- **Server finalization.** Order: claim (CAS), re-hash the staged file, envelope checks, `BlobStore::commit` (atomic rename + directory fsync), owner-ledger append, then index record and custody receipt in one transaction ([](/design/transfer/upload/#finalization-and-integrity)).
  - A crash before commit leaves `incoming/` debris. A crash after commit but before the index transaction leaves an orphan GC reclaims (or a ledger entry rebuild replays), and the retry is idempotent because the content address is one object.
  - An indexed blob without a receipt, or a receipt without custody, MUST NOT occur.
  - The session stays `WaitingForProcessing` until finalization completes or fails cleanly.

On startup each side scrubs incomplete work. On the server the session's committed set, not the staged file, is authoritative ([](/design/transfer/upload/#session-lifetime-and-discard)). Neither side publishes anything before its integrity is verified.

## Encrypted Backups

A backup is an export artifact outside both `{library_root}` and `{blob_root}`; see [](/design/backup-recovery/).

## Validation

- **Scrub age floor (unit).** Young `.tmp` kept; aged one removed.
- **Structural validation (unit).** One negative case per check, each yielding a structured finding.
- **Content validation throttling (smoke).** Respects power and connectivity gates; does not stall the app.
- **Repair safety (unit).** Each table row asserts its exact action.
- **Intra-library dedup (unit).** Correct survivor merge; reversible soft-delete with provenance.
- **Atomic-write crash simulation (smoke).** Interrupt between every staged step; no partial bundle on restart.
- **Server scrub corruption matrix (smoke).** Each seeded class reported exactly, non-zero exit, store and index byte-identical afterward.
- **Server scrub clean store (unit).** Zero findings, identical on re-run; `awaiting-original` gap yields no finding.
