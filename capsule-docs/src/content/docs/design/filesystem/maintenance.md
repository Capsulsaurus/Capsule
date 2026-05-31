---
title: Library Maintenance and Atomic Writes
description: How Capsule keeps client storage consistent, repairs what it can, and writes atomically
---

The data-integrity principle treats client storage as *potentially lost* (see [Core Principles](/design/principles/)): unlike the server, a client library sits on consumer hardware, syncs only partially, and is edited by a long-lived process that can be killed mid-write. A client therefore never assumes its library is consistent — it periodically *proves* it is, repairs what it can repair safely, and surfaces what it cannot.

The maintenance routines live in `capsule-core::library`: [`scrub`](#scrubbing), [self-validation](#self-validation), [repair](#repair), and [`dedup`](#deduplication). The server runs an equivalent scrub of stale `.part`/`.bin` files under `incoming/`. All routines are **conservative** — consistent with "we can NEVER delete data unexpectedly," irreplaceable data is never removed without explicit user confirmation.

This doc also owns the granularity rules for [atomic writes](#atomic-writes-and-crash-recovery), which other docs reference but should not restate.

## Scrubbing

A startup **scrub** sweeps the debris of interrupted writes. Atomic writes (below) stage to `.tmp` files; a crash between the write and the rename strands them. The scrub walks `media/` and removes `.tmp` files older than **10 minutes** (configurable) — the age floor avoids racing a write that is legitimately in flight elsewhere in the process. It runs at most once every seven days, gated by a `last_scrubbed_at` timestamp in the library config, since stale temp files are harmless clutter rather than an urgent fault. Every removal is logged. The server performs the equivalent sweep of stale `.part`/`.bin` files (see [Atomic Writes and Crash Recovery](#atomic-writes-and-crash-recovery)).

## Self-Validation

Validation answers a stronger question than scrubbing: *is the library still a faithful, interpretable copy of its assets?* It runs in two tiers, separated by cost.

### Structural Validation (Cheap, at Startup)

A directory walk that checks the invariants of the [client layout](/design/filesystem/client/#desktop-library-layout):

- Every `{uuid}.{ext}` original has a matching `{uuid}.cbor` sidecar and `{uuid}.provenance.cbor` chain. Every sidecar parses as valid CBOR with its required fields present, has a `sidecar_schema` ≤ the client's max known (per the [tightened Postel's Law](/design/principles/#postels-law-asymmetric)), and bears a valid signature from a device in the user's directory.
- A sidecar's `uuid` field matches its filename, and its date bucket matches its capture timestamp.
- Every `cache/` entry (thumbnail, transcode, parsed-metadata cache) and every `.library/trash/` file refers to an asset the library still knows.
- The provenance chain for each asset is walkable from `create` to head, with each record's `prior_provenance_hash` matching the preceding record's content hash. A break — a missing record or a non-matching `prior_provenance_hash` — is a quarantine surface, not a silent skip.
- Index rows reference files that exist — this subsumes the [local index staleness](/design/filesystem/client/#local-index-staleness) check.

### Content Validation (Expensive, Scheduled)

Recomputes the [content hash](/design/cryptography/primitives/) of each locally present original and compares it against the sidecar's `hash` field (the algorithm-tagged form declared in [Metadata — Sidecar Schema v1](/design/metadata/#sidecar-schema-v1); the algorithm itself follows whatever `crypto_suite_id` the sidecar carries). The original is the only irreplaceable thing on a client, so silent bit rot is the worst failure a client can suffer and nothing else detects it.

Because hashing every original is heavy I/O, content validation is **not** run at startup: it is scheduled opportunistically (device idle, on power, unmetered) and throttled, can be triggered on demand, and re-verifies each original on a slow rolling cadence rather than all at once.

## Repair

Repair follows directly from the data-integrity principle — *ephemeral data is rebuilt silently; irreplaceable data is never destroyed to resolve an inconsistency.*

| Finding                          | Action                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| -------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Stale `.tmp` / partial file      | Deleted by the scrub.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| Orphaned `cache/` entry          | Deleted — derived and rebuildable.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| Index inconsistency              | Index dropped and rebuilt from sidecars — always safe.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| Orphaned sidecar (no original)   | Expected when the [sync scope](/design/import/download-sync/#synchronization-scope) is metadata-only — not a fault. Flagged only if the scope says the original should be present locally, in which case the original is re-fetched from the server.                                                                                                                                                                                                                                                                                                                  |
| Orphaned original (no sidecar)   | The file is irreplaceable, so it is never deleted. It is moved to `.library/quarantine/` and surfaced to the user; the client attempts to re-derive a minimal sidecar from the file itself and the server index.                                                                                                                                                                                                                                                                                                                                                      |
| Malformed CBOR sidecar           | The bytes are preserved — moved verbatim to `.library/quarantine/{uuid}.cbor` with a sibling `.reason.json` recording the parse error, and surfaced to the user. **Never silent-skipped:** a sidecar whose CBOR does not parse, whose required fields are missing, or whose `sidecar_schema` is above the client's max known is treated as a quarantine surface (see [Threat Model — Quarantine Surfaces](/design/threat-model/scenarios/#quarantine-surfaces)). The client attempts to re-fetch a current sidecar from the server before treating the asset as lost. |
| Sidecar signature invalid        | Same as malformed: quarantined, never auto-overwritten. The client re-fetches; a persistent failure surfaces the asset as "provenance broken" rather than silently dropping it.                                                                                                                                                                                                                                                                                                                                                                                       |
| Corrupt original (hash mismatch) | If the asset also exists on the server, the ciphertext blob is re-fetched and its derivatives re-generated. If the corrupt copy is the only copy — this device was its uploader and it was never synced — it cannot be auto-healed and is surfaced loudly.                                                                                                                                                                                                                                                                                                            |

Every finding and every repair is logged, so the state of the library is reconstructible after the fact.

## Deduplication

Capsule deduplicates at three distinct layers, and they must not be confused:

- **Server-side ciphertext dedup** — content-addressed blobs are never stored twice (see [Server — Content-Addressing and Deduplication](/design/filesystem/server/#content-addressing-and-deduplication)).
- **Import-time dedup** — import refuses an asset already uploaded from this library and resolves a remote-only match to a merge (see [Upload Protocol — Deduplication and Merge](/design/import/upload-protocol/#deduplication-and-merge)).
- **Intra-library dedup** — described here: two assets *within one client library* whose originals are byte-identical.

Import-time dedup catches most duplicates as they arrive, but it cannot catch all of them. Byte-identical assets still accumulate — the same file imported from two different sources, a folder import that overlaps an earlier one, an asset re-imported after its sidecar was lost, or a backup restored over a library that still holds the originals.

The dedup key is the plaintext **`hash`** digest recorded in every sidecar (see [Metadata — Sidecar Schema v1](/design/metadata/#sidecar-schema-v1)) — the same value the index lets the client look up directly. Two assets that share it are exact duplicates. This is deliberately distinct from the server's *ciphertext* hash: two devices may encrypt the same plaintext under different album keys, so only the plaintext hash identifies duplicates across a library.

Deduplication is **not** stacking. A RAW+JPEG pair, a burst, and a Live Photo are *different bytes* deliberately kept together — they are [stacked](/design/organization/#asset-stacking), never deduplicated. Visually-similar but non-identical photos are a separate AI grouping feature (Smart Selection) that never deletes. Dedup only ever acts on originals that are bit-for-bit identical.

Resolution is conservative and never silent. The client presents each duplicate set and lets the user choose the survivor. On merge, the survivor inherits the union of album memberships and tags (merged through the OR-set CRDT — see [Metadata — Collaborative Metadata](/design/metadata/#collaborative-metadata)), the highest rating, and the earliest import and capture timestamps; the losing copy is soft-deleted into the trash, so the action is reversible and is recorded as a signed, provenance-tracked modification like any other deletion (see [Provenance](/design/cryptography/provenance/#provenance-of-library-modifications)). Whole-library deduplication is a user-initiated maintenance action or a surfaced suggestion — never an automatic background deletion — consistent with the rule that data is never removed unexpectedly.

## Atomic Writes and Crash Recovery

Every write that must not tear uses temp-file + atomic rename, staged on the same filesystem as its destination. The atomicity rule is enforced at three granularities — the single file, the per-asset bundle, and the multi-asset edit. These are also the canonical statement of the rule; [Threat Model — Atomicity Invariants](/design/threat-model/validation/#atomicity-invariants) cross-references them and is where the cross-doc invariant lives.

- **Client — single-file writes.** Sidecar and provenance appends stage to `{uuid}.cbor.tmp` and `{uuid}.provenance.cbor.tmp` in the destination directory, then rename into place. A direct overwrite is never used.
- **Client — per-asset bundle.** An asset import or update is a *bundle*: original (when present locally), sidecar, and a new provenance record. All `.tmp` files stage first; only after every staged file is on disk do the renames execute, and only in a fixed order (original → sidecar → provenance). A failure at any rename discards every remaining `.tmp` and rolls back the renames already done by deleting the just-renamed targets, so the on-disk state never reflects a partial bundle. The `.provenance.cbor` is the last to be renamed, so the existence of a new provenance record implies the rest of the bundle is committed.
- **Client — stack edit.** A stack edit touches multiple sidecars and writes a single provenance record per affected asset. All `.tmp` files (one per sidecar plus one per provenance file) stage first and rename together; any rename failure discards the entire batch. There is no partial stack.
- **Server — chunk assembly.** Chunks stage as `{upload_id}_{n}.part`; the assembled blob is `{upload_id}.bin`. The blob is renamed into its content-addressed location under `blobs/` only after the ciphertext hash is recomputed and matches the declared value (see [Upload Protocol — Finalization and Integrity](/design/import/upload-protocol/#finalization-and-integrity)).
- **Server — finalization transaction.** The blob rename into its content-addressed `blobs/` location is a filesystem operation and so necessarily happens *before* the Postgres commit; the manifest-envelope insert, metadata-blob insert, provenance-blob insert, and asset-row `uploaded` flip then commit in a **single PostgreSQL transaction**. That ordering is what makes every crash point safe: a crash *before* the rename leaves only `incoming/` debris (scrubbed below); a crash *after* the rename but *before* the commit leaves a finalized blob in `blobs/` that **no committed row references** — an orphan the [reference-count GC](/design/filesystem/server/#deletion-and-garbage-collection) reclaims, while the idempotent retry re-finalizes against the already-present blob (re-placing a content-addressed hash is a no-op). The "single transaction" guarantee is over the **index rows**; blob *placement* is idempotent and GC-safe precisely because it is content-addressed. The server never exposes an asset whose index bundle is partially persisted — the session stays in `WaitingForProcessing` until a finalization attempt commits the whole bundle or fails it cleanly.

On startup, each side scrubs incomplete work: stale `.part`, `.tmp`, and `.bin` files left by an interrupted upload or import are identified and removed, and the cleanup is logged. A blob or media file is never published, on either side, until its integrity has been verified.

## Encrypted Backups

A backup is an export artifact — encrypted, self-describing, and kept outside both `{library_root}` and `{blob_root}` — so it is not part of the live library or the server blob store, and may be stored on external or cloud storage. Its format, the master-key escrow, and the recovery flow are covered in [Backup and Recovery](/design/backup-recovery/).

## Validation

- **Scrub age-floor (unit).** Create a `.tmp` file aged < N minutes; assert scrub leaves it. Age it past the floor; assert removal.
- **Structural validation (unit).** Each invariant in the [Structural Validation](#structural-validation-cheap-at-startup) list gets a negative test case (missing sidecar, missing provenance, schema regression, signature failure, date-bucket drift, orphaned cache/trash entry, broken provenance chain). Each produces a structured finding.
- **Content validation throttling (smoke).** Inject many originals; trigger content validation; assert it does not stall the app and respects power/connectivity gates.
- **Repair safety (unit).** Each row of the repair table is a unit test: trigger the finding, run repair, assert the *exact* action (delete vs quarantine vs re-fetch) was taken.
- **Intra-library dedup correctness (unit).** Two assets with identical plaintext hash; assert dedup proposes the right survivor (union albums, max rating, earliest timestamps), records a soft-delete provenance for the loser, and is reversible.
- **Atomic-write crash simulation (smoke).** Programmatically interrupt a bundle write between each pair of staged steps; assert no on-disk state reflects a partial bundle on next startup.

Cross-module case (server crash mid-finalization → recovery on restart) is bounded E2E surface in [Module Map](/design/module-map/#e2e-test-surface).
