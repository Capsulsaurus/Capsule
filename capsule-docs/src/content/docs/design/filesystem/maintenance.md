---
title: Library Maintenance and Atomic Writes
description: How Capsule keeps client and server storage consistent, repairs what it can, and writes atomically
status: draft
---

A client library sits on consumer hardware and is edited by a process that can be killed mid-write, so a client never assumes its library is consistent: it periodically proves it, repairs what is safe, and surfaces the rest. The server runs an equivalent [integrity scrub](#server-side-integrity-scrub). All routines are conservative: irreplaceable data MUST NOT be removed without explicit user confirmation.

This doc is the canonical statement of the [atomic-write](#atomic-writes-and-crash-recovery) rules; other docs reference rather than restate them.

## Scrubbing

A startup scrub removes `.tmp` debris under `media/` older than an age floor (so it never races an in-flight write), at most once per interval; both constants live in [`capsule-library::library::scrub`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-library/src/library/scrub.rs). Every removal is logged. The server sweeps stale `incoming/*.bin` likewise.

## Self-Validation

### Structural Validation (Cheap, at Startup)

Checks the [client layout](/design/filesystem/client/#desktop-library-layout):

- Every original has a sidecar and provenance chain; every sidecar parses, has its required fields, a `sidecar_schema` ≤ the client's max known ([Postel's law](/design/principles/#postels-law-asymmetric)), and a valid signature from a directory device.
- A sidecar's `uuid` matches its filename. Date-bucket drift is expected after a capture-date correction and repaired as a rename bundle; it is a fault only when unexplained.
- Every `cache/` and `trash/` entry refers to a known asset.
- Each provenance chain walks from `create` to head; a break is a quarantine surface, never skipped.
- Index rows reference existing files ([index staleness](/design/filesystem/client/#local-index-staleness)).

### Content Validation (Expensive, Scheduled)

Re-hashes each local original against the sidecar's plaintext `hash` (algorithm per the sidecar's `crypto_suite_id`, which governs plaintext and ciphertext digests in lockstep). Silent bit rot of an original is the worst client failure and nothing else detects it. It MUST NOT run at startup: it runs idle, on power, unmetered, throttled, on a slow rolling cadence, or on demand.

## Server-Side Integrity Scrub

An operator-invoked (schedulable) command that verifies a Postgres + blob-store pair, on a code path separate from the write path so one bug cannot hide another. It is **read-only**: it classifies and reports, never repairs. Repair stays with [GC](/design/filesystem/server/#deletion-and-garbage-collection), [index rebuild](/design/filesystem/server/#recovering-the-index-from-blobs-alone), or the operator. Finding classes are defined in [`capsule-server::scrub`](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-server/src/scrub/mod.rs).

1. **Row → blob.** Every committed reference resolves under `blobs/`; a miss is a dangling reference — except a missing original on an [`awaiting-original`](/design/transfer/upload/#upload-tiering-staged-uploads) asset.
2. **Blob → row.** Every blob is referenced by a committed row or a live [share link](/design/share-links/#storing-the-record); otherwise an orphan, reported for GC.
3. **Byte integrity (deep mode).** Blobs re-hash to their names; rolling and throttled. Same semantics as [durability `deep`](/design/transfer/durability/).
4. **Chain ⇄ index.** Each asset's chain walks back to a `create` through retained manifests; the walk stops at the first break. The head must name a held provenance blob. Signatures are not verified — that is the client's [`verify_asset`](/design/cryptography/keys/#write-authorization).
5. **Mirrored facts.** Facts Postgres mirrors from the signed manifest match the manifest's own copies.
6. **Ledger ⇄ index.** Every [owner ledger](/design/filesystem/server/#the-owner-ledger) chain verifies, and every entry has its row.
7. **Debris and quarantine inventory.** Unattached `incoming/` files and everything in `quarantine/` are listed.

Checks 4 and 5 decode manifests through the server's single manifest parser, shared with [rebuild](/design/filesystem/server/#rebuild); the write path never decodes them. Check 6 verifies signatures when the host holds `ATTESTATION_KEY_SEED` and links and positions otherwise, and the report says which; it reports a ledgered upload the index has no row for. Each ledger is verified against the head the index recorded for its owner (the [anchor](/design/filesystem/server/#the-owner-ledger)), so a tail cut exactly at an entry boundary is reported whenever the index survived; the scrub reads the anchor and never moves it.

The server retains an asset's whole manifest chain until purge, which is what makes check 4 and the [proof-of-loss rebuttal](/design/transfer/durability/#proof-of-loss) possible.

Findings are **classified, never adjudicated**: the report names the check and both sides' evidence without assuming which side is wrong. Findings are logged structured; a non-zero count is the exit signal.

## Repair

*Ephemeral data is rebuilt silently; irreplaceable data is never destroyed to resolve an inconsistency.*

| Finding | Action |
| --- | --- |
| Stale `.tmp` | Deleted by the scrub. |
| Orphaned `cache/` entry | Deleted. |
| Index inconsistency | Rebuilt from sidecars and chains. Safe but not lossless (index-only state such as importer-formed stack placement is lost), so rebuild is never a schema-upgrade path. |
| Sidecar without original | Expected under metadata-only [sync scope](/design/sync/#synchronization-scope); otherwise re-fetch the original. |
| Original without sidecar | Never deleted: quarantined, surfaced, minimal sidecar re-derived. |
| Sidecar without provenance chain | Not restored on open; listed as unanchored and left untouched. A write renames its chain into place last, so an interrupted one leaves this rather than a torn chain. |
| Malformed sidecar | Bytes quarantined verbatim with reason; never silently skipped; current sidecar re-fetched first. |
| Invalid sidecar signature | Quarantined, never overwritten; persistent failure shown as "provenance broken". |
| Corrupt original | Re-fetched and derivatives regenerated if the server holds it; if this is the only copy, surfaced loudly. |

Every finding and repair is logged.

## Deduplication

Three layers, not to be confused:

- **Server ciphertext dedup** — [content addressing](/design/filesystem/server/#content-addressing-and-deduplication).
- **Import-time dedup** — [Transfer — Upload](/design/transfer/upload/#deduplication-and-merge).
- **Intra-library dedup** — here: byte-identical originals within one library (overlapping imports, restore over a live library).

The key is the sidecar's plaintext `hash`, not the ciphertext hash (which differs per album key). Dedup is not [stacking](/design/organization/#asset-stacking) and never acts on merely similar images. Resolution is user-chosen, never automatic: the survivor takes the union of albums and tags ([OR-set](/design/metadata/#collaborative-metadata)), the highest rating, and the earliest timestamps; the loser is soft-deleted with a signed provenance record, so the action is reversible.

## Atomic Writes and Crash Recovery

Every write that must not tear stages a temp file on the destination filesystem, then renames. Cross-doc invariants: [Threat Model — Atomicity Invariants](/design/threat-model/validation/#atomicity-invariants).

- **Client single file.** Stage `{name}.tmp` in the destination directory, rename. Never overwrite in place.
- **Client per-asset bundle.** Stage all files, then rename in fixed order original → sidecar → provenance. A rewrite first stages the displaced version as `.prev` so rollback can restore it. Any failure rolls back completed renames. A new provenance record therefore implies a committed bundle.
- **Client stack edit.** All sidecars and per-asset provenance records stage and rename as one batch; no partial stack.
- **Server finalization.** Order: claim (CAS) → re-hash staged file → envelope checks → `BlobStore::commit` (atomic rename + directory fsync) → owner-ledger append → index record and custody receipt in one transaction ([Transfer — Upload](/design/transfer/upload/#finalization-and-integrity)). A crash before commit leaves `incoming/` debris; after commit but before the index transaction, an orphan GC reclaims (or a ledger entry rebuild replays), and the retry is idempotent because the content address is one object. An indexed blob without a receipt, or a receipt without custody, MUST NOT occur. The session stays `WaitingForProcessing` until finalization completes or fails cleanly.

On startup each side scrubs incomplete work. An `incoming/*.bin` shorter than its session's recorded offset fails the session — the file is authoritative. Nothing is published, on either side, before its integrity is verified.

## Encrypted Backups

A backup is an export artifact outside both `{library_root}` and `{blob_root}`; see [Backup and Recovery](/design/backup-recovery/).

## Validation

- **Scrub age floor (unit).** Young `.tmp` kept; aged one removed.
- **Structural validation (unit).** One negative case per check, each yielding a structured finding.
- **Content validation throttling (smoke).** Respects power and connectivity gates; does not stall the app.
- **Repair safety (unit).** Each table row asserts its exact action.
- **Intra-library dedup (unit).** Correct survivor merge; reversible soft-delete with provenance.
- **Atomic-write crash simulation (smoke).** Interrupt between every staged step; no partial bundle on restart.
- **Server scrub corruption matrix (smoke).** Each seeded class reported exactly, non-zero exit, store and index byte-identical afterward.
- **Server scrub clean store (unit).** Zero findings, identical on re-run; `awaiting-original` gap yields no finding.
