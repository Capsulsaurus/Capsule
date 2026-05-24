---
title: Filesystem
description: How Capsule structures files on disk, on the server and on clients
---

Capsule's end-to-end encryption splits the filesystem into two fundamentally
different roles. The **server** stores only opaque, content-addressed
ciphertext — it never holds a decryption key and cannot interpret a single byte
it stores (see [Cryptography](/design/cryptography/)). **Clients** hold the keys, so a
client filesystem is a working library of plaintext media, sidecar metadata, and
rebuildable caches. The two layouts share a small set of principles but
otherwise have little in common.

This document covers on-disk structure only. The import pipeline, the upload
protocol, and synchronization are covered in
[Import and Synchronization](/design/import-synchronization/); metadata extraction in
[Metadata](/design/metadata/); derivative generation in
[Thumbnails and Previews](/design/thumbnails/); grouping and trash semantics in
[Asset Organization](/design/organization/); backup and recovery in
[Backup and Recovery](/design/backup-recovery/).

## Shared Principles

These follow directly from [Core Principles](/design/principles/):

- **Recovery-first.** No database is required to interpret canonical data. On
  the client, sidecar files are the source of truth and the index is a
  rebuildable cache. On the server, PostgreSQL is the authoritative index, but
  it holds only key-free facts.
- **Atomic writes.** Every write that must not tear uses temp-file + atomic
  rename on the same filesystem. Direct overwrites risk corruption on power loss.
- **Ephemeral derived data.** Only originals and their canonical metadata are
  irreplaceable. Thumbnails, transcodes, parsed-metadata caches, and the query
  index can all be regenerated and are treated as such.
- **4 KiB alignment.** Data is processed and written block-aligned to 4 KiB,
  which matches memory and disks and enables the reflink assembly path below.
- **Content-addressing.** Stored blobs are named by their ciphertext content hash —
  the same hash everywhere a content address is needed (see
  [Cryptography Primitives Inventory](/design/cryptography/#primitives-inventory)).

## Server vs Client at a Glance

| Concern      | Server                                     | Client                                        |
| ------------ | ------------------------------------------ | --------------------------------------------- |
| Holds keys   | No                                         | Yes                                           |
| Stored form  | Opaque ciphertext blobs                    | Plaintext media + CBOR sidecars               |
| Naming       | Content-addressed by ciphertext hash       | UUIDv7 stems, date-bucketed                   |
| Index        | PostgreSQL (key-free facts only)           | SQLite (rebuildable, full plaintext metadata) |
| Derived data | Stored as client-generated encrypted blobs | Generated locally, cached, rebuildable        |
| Originals    | Always retained while referenced           | Present only if synced locally                |

## Server Filesystem

### Stores by Deployment Profile

The server's durable state is always split across **two required systems** plus an **optional third** for high-concurrency deployments:

- **Blob store** (filesystem) — the encrypted bytes of every asset. *Required.*
- **PostgreSQL** — the authoritative index: ownership, album references, blob
  references, lifecycle state, and (in the default profile) upload-session state.
  *Required.*
- **Valkey** — volatile upload-session state (offsets, status) with a 24-hour
  TTL. *Optional.* Recommended only for deployments where upload-session hot-path
  contention on PostgreSQL becomes measurable.

This gives two concrete deployment profiles:

| Profile                     | Session state lives in                                                                                             | When to choose it                                                                      |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------- |
| **Default (Postgres-only)** | `upload_sessions` table with `expires_at` TTL column and a periodic sweep                                          | Self-hosted, small-to-medium servers, single-node deployments. Reduces ops surface.    |
| **High-concurrency**        | Valkey (keyed `upload:session:{id}`) with native 24-hour TTL; PostgreSQL still holds the durable pending-asset row | Large multi-tenant deployments where session-table contention is a measured bottleneck |

Switching profiles is operationally invisible to clients — the upload protocol does not change, only where the server stores volatile session counters. The [upload protocol](/design/import-synchronization/) is written to be store-agnostic.

The server performs no decoding, no metadata extraction, and no thumbnail
generation — it cannot, since it never holds a key.

### Blob Store Layout

```text
{blob_root}/
├── incoming/
│   ├── {upload_id}_{n}.part        # in-flight chunk
│   └── {upload_id}.bin             # assembled blob, pre-verification
├── blobs/
│   └── {hash[0:2]}/{hash[2:4]}/
│       └── {hash}                  # finalized blob, content-addressed
└── .server/
    ├── version                     # server filesystem schema version
    └── config                      # server-wide configuration
```

- **`{blob_root}`**: absolute path configured at server startup. The entire tree
  must be on a single filesystem so that finalization renames are atomic.
- **`incoming/`**: live uploads. Chunks land as `{upload_id}_{n}.part`; on
  finalization they are concatenated into `{upload_id}.bin`. The 4 KiB chunk
  alignment is what allows each chunk to be reflinked into place on
  copy-on-write filesystems, turning assembly into a near-instant metadata
  operation. See the upload protocol in
  [Import and Synchronization](/design/import-synchronization/).
- **`blobs/`**: the finalized store. A blob's filename is its [ciphertext content hash](/design/cryptography/#primitives-inventory); the two-level hex-prefix shard keeps directory sizes bounded for
  multi-million-blob stores. A finalized blob is immutable.
- **`.server/`**: the server operator's own configuration and schema version.
  This is plaintext server metadata, not user data — it is the one thing under
  `{blob_root}` that is not an encrypted blob.

### Uniform, Opaque Blobs

A single asset produces a **bundle** of blobs (see
[Import and Synchronization](/design/import-synchronization/) — "What Gets Uploaded"):
the encrypted original, encrypted derivatives (thumbnails, previews, LQIP), the
encrypted CBOR metadata blob, and the encrypted provenance blob (see
[Cryptography — Provenance of Library Modifications](/design/cryptography/#provenance-of-library-modifications)).
The blob store does not distinguish them — every blob is just content-addressed
ciphertext. The mapping from an asset to its constituent blobs, and the role of
each blob, lives entirely in PostgreSQL.

### Recovering the Index from Blobs Alone

The PostgreSQL index is authoritative but **not the only copy** of what the
server knows. Every blob carries enough server-visible structural metadata —
the [unencrypted portion](/design/cryptography/#provenance-and-signed-manifest)
of the asset manifest — to rebuild the index row that referenced it. This is
the server-side counterpart of the recovery-first principle that lets a client
rebuild its index from CBOR sidecars.

The server-visible portion of a blob includes:

- `crypto_suite_id`, `protocol_version`, `amk_version` — what bundle of
  primitives encrypted this asset and which album epoch
- the ciphertext hash (`hash.value`) and declared size — content address and
  storage attribution
- `created_by_user`, `created_by_device`, `album_id`, `file_id`,
  `prior_provenance_hash`, `action` — owner, provenance chain link, and
  lifecycle action
- the device's hybrid signature — provenance attribution; verifiable against
  the public device directory even without any key Capsule's server holds

A rebuild walks `blobs/`, reads the manifest envelope of each blob, verifies
the device signature against the cached device directory, and writes an index
row. The rebuild is idempotent: re-running it against an existing index
produces no changes. The full envelope check list a server runs at recovery is
the same list it runs at write time — see
[Threat Model — Server-Side Validation Invariants](/design/threat-model/#server-side-validation-invariants).

A blob whose manifest envelope fails structural validation during rebuild is
**quarantined**, not silently dropped — moved to `{blob_root}/quarantine/`
with a sibling `.reason.json` recording the rejection code. This guarantees
that an unrecoverable byte sequence is preserved for forensic inspection
rather than vanishing on rebuild.

Operationally the rebuild is invoked when a PostgreSQL restore is incomplete
or a logical-corruption event is detected; it is **never** the hot path. The
hot path runs through the authoritative PG index. The recovery path's job is
to make the index reconstructible if PG is lost, not to substitute for it.

### Manifest Envelope Validation (Server-Side)

Every write — `POST /upload`, `PATCH /upload/{id}`, finalization, any
lifecycle manifest, any federation pull — passes through structural
validation of the manifest envelope **before** any state is persisted. The
server holds no decryption key, so it cannot verify the cryptographic
signatures; but it does enforce that every envelope field is present,
structurally well-formed, within bounds, and consistent with the album the
manifest claims to address.

The complete refuse-by-default checklist is owned by
[Threat Model — Server-Side Validation Invariants](/design/threat-model/#server-side-validation-invariants).
A rejection at any check returns the rejection code listed there and writes
no state. This is what defeats the version-mismatched-client damage class
without requiring the server to hold a key.

### Content-Addressing and Deduplication

Naming blobs by their [ciphertext content hash](/design/cryptography/#primitives-inventory) makes deduplication free: a blob already present
is never stored twice. At upload-session creation the server checks for a blob
with the same content hash already owned by the uploader — an exact
local-and-remote duplicate is rejected up front, and an asset that exists
remotely under a *different* ciphertext resolves to a **merge** that links the
existing blob rather than storing a second copy (see
[Import and Synchronization](/design/import-synchronization/) — "Deduplication and
Merge"). Reference counting in PostgreSQL determines when a blob is genuinely
unreferenced.

### PostgreSQL: What the Server Knows

The server index records only what can be known without a key:

- `asset_id`, `owner_id`, `album_id`, `upload_user_id`
- references to the asset's blobs (their [content hashes](/design/cryptography/#primitives-inventory)) and each blob's role
- `amk_version` — which album-key epoch encrypted the asset (see
  [Cryptography](/design/cryptography/))
- declared ciphertext size and `content_type`
- the `uploaded` flag and server-visible lifecycle state
- creation/modification timestamps and provenance records (see
  [Cryptography](/design/cryptography/) — "Provenance of Library Modifications")

No plaintext capture date, dimensions, EXIF, tags, or filename ever reaches the
server. Those live inside the encrypted metadata blob (see [Metadata Encryption](/design/cryptography/#metadata-encryption)) and are readable only by authorized clients.

Session creation writes a *pending* asset row (`uploaded = false`) that reserves
the asset ID the bundle's blobs reference; finalization flips it. See the
session lifecycle in [Import and Synchronization](/design/import-synchronization/).

### Ownership, Partitioning, and Quota

`owner_id` is the billing and namespace entity; the `owner_id` → user-set
mapping lives in PostgreSQL and is mirrored as an MLS group (the Owner Group
Key — see [Cryptography](/design/cryptography/)). Storage quota is accounted to
`upload_user_id`, which is distinct from `owner_id`. The blob store itself is
not partitioned by owner — content-addressing is global — but every blob
*reference* is owner-scoped in PostgreSQL, and deduplication checks are scoped
to the owner.

### Deletion and Garbage Collection

The server cannot read an asset's `is_deleted` flag — it is inside the encrypted
metadata blob. Lifecycle transitions are therefore signalled by the client and
recorded as server-visible state on the asset row; soft delete is a state
change, not a file operation. Permanent deletion drops the asset's blob
references, and a blob whose reference count reaches zero becomes eligible for a
garbage-collection sweep. Consistent with the data-integrity principle, blob
removal is conservative — a blob is deleted only after its references are
provably gone.

## Client Filesystem

Clients hold keys, so a client stores plaintext. Desktop clients keep a
self-contained library directory; mobile clients use platform-sandboxed storage.

What a client keeps locally depends on its sync setting — *metadata only*,
*metadata + thumbnails*, or *metadata + thumbnails + original* (see
[Import and Synchronization](/design/import-synchronization/) — "Synchronization
Scope"). A library therefore routinely contains assets whose original is
server-only, and the layout must represent an asset whether or not its original
bytes are present locally.

### Desktop Library Layout

```text
{library_root}/
├── media/
│   └── {YYYY}/{YYYY-MM}/
│       ├── {uuid}.{ext}            # original media (plaintext; absent if not synced locally)
│       ├── {uuid}.cbor             # canonical metadata sidecar (plaintext, signed)
│       └── {uuid}.provenance.cbor  # append-only signed provenance chain
├── cache/
│   ├── thumbnails/{size}/{uuid[0:2]}/{uuid[2:4]}/{uuid}.{fmt}
│   ├── meta/{uuid[0:2]}/{uuid[2:4]}/{uuid}.meta.cbor    # verbose parsed metadata
│   └── transcodes/{uuid[0:2]}/{uuid[2:4]}/{uuid}.{ext}
├── index/
│   └── library.sqlite              # rebuildable query + vector index
└── .library/
    ├── version                     # library schema version
    ├── config                      # user preferences and library state
    ├── lock                        # process lock file (ephemeral)
    ├── trash/
    │   └── {uuid}.{ext}            # soft-deleted media
    └── quarantine/
        ├── {uuid}.{ext}            # irreplaceable bytes that failed validation
        └── {uuid}.reason.json      # parse error / signature failure / schema mismatch
```

- **`media/`**: originals, their sidecars, and their provenance chains. Filenames are
  `{UUIDv7}.{extension}` (always lowercase), `{UUIDv7}.cbor`, and
  `{UUIDv7}.provenance.cbor` respectively. The CBOR sidecar is the client's
  canonical, self-describing metadata record (see
  [Metadata — Sidecar Schema v1](/design/metadata/#sidecar-schema-v1)) — the
  plaintext counterpart of the encrypted metadata blob the server stores. The
  `.provenance.cbor` file is an append-only signed log per asset (see
  [Cryptography — Provenance of Library Modifications](/design/cryptography/#provenance-of-library-modifications));
  the client never deletes it, so a hard-deleted asset leaves a
  tombstone-with-history. Per the recovery-first principle, the entire library
  is reconstructible from these three files alone. Files are date-bucketed by
  capture timestamp because the client, unlike the server, can read capture
  dates.
- **`cache/`**: purely derived and rebuildable — thumbnails and previews (formats declared in [Thumbnails and Previews](/design/thumbnails/#thumbnail-and-preview-formats)), verbose
  parsed-metadata caches, and transcodes. Sharded by UUID prefix to bound
  directory sizes. Deletable at any time; never a source of truth.
- **`index/library.sqlite`**: a rebuildable query cache over the sidecars, and
  the local vector index backing AI features (`sqlite-vec` — see
  [AI/ML Integrations](/design/ai/)). On a schema change it may be dropped and rebuilt
  rather than migrated, since it is always reconstructible.
- **`.library/`**: library-scoped state — schema version, user configuration, a
  process lock file that prevents two app instances from opening the same
  library, the trash (soft-delete retention area), and `quarantine/` (where
  irreplaceable bytes that failed structural or signature validation are
  preserved verbatim alongside a `.reason.json` recording the rejection). The
  quarantine area is the union surface listed in
  [Threat Model — Quarantine Surfaces](/design/threat-model/#quarantine-surfaces).

The full sidecar and SQLite schemas are owned by [Metadata](/design/metadata/) and not
duplicated here.

### Mobile Clients

Android and iOS use platform-sandboxed storage rather than a user-visible
library directory. The logical model is the same — originals (when synced),
canonical metadata, rebuildable caches, and a local SQLite index — but placement
follows each platform's sandbox rules. Capsule deliberately does not store
rebuildable derivatives in OS-managed cache locations: the OS may evict them
indiscriminately, and a thumbnail that is expensive to regenerate is not
genuinely disposable (see [Import and Synchronization](/design/import-synchronization/)
— "Space Recovery").

### Local Index Staleness

SQLite may lag the filesystem after external edits or interrupted operations.
The client verifies file existence before acting on an index row and triggers a
full rebuild from sidecars when it detects structural inconsistency. Because the
index is always rebuildable, this recovery is safe.

### Space Recovery

Majority of data except non-backed up files are considered ephemeral but are not
considered disposable nor to be stored in cache storage. It is much easier for
the Capsule app to determine which versions of the same data can be retained and
which can be deleted. Storing thumbnails as cache may result in them being
deleted by the OS indiscriminately, when it is in fact useful. We provide tools
to analyze the biggest storage consumers and allow users to selectively delete
data.

## Library Self-Maintenance

The data-integrity principle treats client storage as *potentially lost* (see
[Core Principles](/design/principles/)): unlike the server, a client library
sits on consumer hardware, syncs only partially, and is edited by a long-lived
process that can be killed mid-write. A client therefore never assumes its
library is consistent — it periodically *proves* it is, repairs what it can
repair safely, and surfaces what it cannot. Three routines do this:
**scrubbing** removes the debris of interrupted operations, **self-validation**
confirms the library is structurally and bitwise intact, and **deduplication**
collapses byte-identical assets. All three are conservative — consistent with
"we can NEVER delete data unexpectedly," irreplaceable data is never removed
without explicit user confirmation.

### Scrubbing

A startup **scrub** sweeps the debris of interrupted writes. Atomic writes
(below) stage to `.tmp` files; a crash between the write and the rename strands
them. The scrub walks `media/` and removes `.tmp` files older than a few minutes
— the age floor avoids racing a write that is legitimately in flight elsewhere
in the process. It runs at most once every seven days, gated by a
`last_scrubbed_at` timestamp in the library config, since stale temp files are
harmless clutter rather than an urgent fault. Every removal is logged. The
server performs the equivalent sweep of stale `.part`/`.bin` files (see
[Atomic Writes and Crash Recovery](#atomic-writes-and-crash-recovery)).

### Self-Validation

Validation answers a stronger question than scrubbing: *is the library still a
faithful, interpretable copy of its assets?* It runs in two tiers, separated by
cost.

**Structural validation** is a cheap directory walk, run at startup. It checks
the invariants of the [layout](#desktop-library-layout):

- Every `{uuid}.{ext}` original has a matching `{uuid}.cbor` sidecar and
  `{uuid}.provenance.cbor` chain. Every sidecar parses as valid CBOR with its
  required fields present, has a `sidecar_schema` ≤ the client's max known
  (per the [tightened Postel's Law](/design/principles/)), and bears a valid
  signature from a device in the user's directory.
- A sidecar's `uuid` field matches its filename, and its date bucket matches its
  capture timestamp.
- Every `cache/` entry (thumbnail, transcode, parsed-metadata cache) and every
  `.library/trash/` file refers to an asset the library still knows.
- The provenance chain for each asset is walkable from `create` to head, with
  each record's `prior_provenance_hash` matching the preceding record's content
  hash. A break in the chain is a quarantine surface, not a silent skip.
- Index rows reference files that exist — this subsumes
  [Local Index Staleness](#local-index-staleness) above.

**Content validation** is expensive — it recomputes the [content hash](/design/cryptography/#primitives-inventory) of each locally
present original and compares it against the sidecar's `hash` field (the
algorithm-tagged form declared in [Metadata — Sidecar Schema v1](/design/metadata/#sidecar-schema-v1);
the algorithm itself follows whatever `crypto_suite_id` the sidecar carries).
The original is the only irreplaceable thing on a client, so
silent bit rot is the worst failure a client can suffer and nothing else detects
it. Because hashing every original is heavy I/O, content validation is not run
at startup: it is scheduled opportunistically (device idle, on power, unmetered)
and throttled, can be triggered on demand, and re-verifies each original on a
slow rolling cadence rather than all at once.

### Repair

Repair follows directly from the data-integrity principle — *ephemeral data is
rebuilt silently; irreplaceable data is never destroyed to resolve an
inconsistency.*

| Finding                          | Action                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| -------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Stale `.tmp` / partial file      | Deleted by the scrub.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| Orphaned `cache/` entry          | Deleted — derived and rebuildable.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| Index inconsistency              | Index dropped and rebuilt from sidecars — always safe.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| Orphaned sidecar (no original)   | Expected when the [sync scope](/design/import-synchronization/#synchronization-scope) is metadata-only — not a fault. Flagged only if the scope says the original should be present locally, in which case the original is re-fetched from the server.                                                                                                                                                                                                                                                                                                      |
| Orphaned original (no sidecar)   | The file is irreplaceable, so it is never deleted. It is moved to `.library/quarantine/` and surfaced to the user; the client attempts to re-derive a minimal sidecar from the file itself and the server index.                                                                                                                                                                                                                                                                                                                                            |
| Malformed CBOR sidecar           | The bytes are preserved — moved verbatim to `.library/quarantine/{uuid}.cbor` with a sibling `.reason.json` recording the parse error, and surfaced to the user. **Never silent-skipped:** a sidecar whose CBOR does not parse, whose required fields are missing, or whose `sidecar_schema` is above the client's max known is treated as a quarantine surface (see [Threat Model — Quarantine Surfaces](/design/threat-model/#quarantine-surfaces)). The client attempts to re-fetch a current sidecar from the server before treating the asset as lost. |
| Sidecar signature invalid        | Same as malformed: quarantined, never auto-overwritten. The client re-fetches; a persistent failure surfaces the asset as "provenance broken" rather than silently dropping it.                                                                                                                                                                                                                                                                                                                                                                             |
| Corrupt original (hash mismatch) | If the asset also exists on the server, the ciphertext blob is re-fetched and its derivatives re-generated. If the corrupt copy is the only copy — this device was its uploader and it was never synced — it cannot be auto-healed and is surfaced loudly.                                                                                                                                                                                                                                                                                                  |

Every finding and every repair is logged, so the state of the library is
reconstructible after the fact.

### Deduplication

Capsule deduplicates at three distinct layers, and they must not be confused:

- **Server-side ciphertext dedup** — content-addressed blobs are never stored
  twice (see [Content-Addressing and Deduplication](#content-addressing-and-deduplication)).
- **Import-time dedup** — import refuses an asset already uploaded from this
  library and resolves a remote-only match to a merge (see
  [Import and Synchronization](/design/import-synchronization/#deduplication-and-merge)).
- **Intra-library dedup** — described here: two assets *within one client
  library* whose originals are byte-identical.

Import-time dedup catches most duplicates as they arrive, but it cannot catch
all of them. Byte-identical assets still accumulate — the same file imported
from two different sources, a folder import that overlaps an earlier one, an
asset re-imported after its sidecar was lost, or a backup restored over a
library that still holds the originals.

The dedup key is the plaintext **`hash.value`** recorded in every sidecar (the
algorithm-tagged form from [Metadata — Sidecar Schema v1](/design/metadata/#sidecar-schema-v1)) —
the same value the index lets the client look up directly. Two assets that share
it are exact duplicates. This is deliberately distinct from the server's
*ciphertext* hash: two devices may encrypt the same plaintext under different
album keys, so only the plaintext hash identifies duplicates across a library.

Deduplication is **not** stacking. A RAW+JPEG pair, a burst, and a Live Photo
are *different bytes* deliberately kept together — they are
[stacked](/design/organization/#asset-stacking), never deduplicated.
Visually-similar but non-identical photos are a separate AI grouping feature
(Smart Selection) that never deletes. Dedup only ever acts on originals that are
bit-for-bit identical.

Resolution is conservative and never silent. The client presents each duplicate
set and lets the user choose the survivor. On merge, the survivor inherits the
union of album memberships and tags (merged through the OR-set CRDT — see
[Metadata](/design/metadata/#collaborative-metadata)), the highest rating, and
the earliest import and capture timestamps; the losing copy is soft-deleted into
the trash, so the action is reversible and is recorded as a signed,
provenance-tracked modification like any other deletion (see
[Provenance of Library Modifications](/design/cryptography/#provenance-of-library-modifications)).
Whole-library deduplication is a user-initiated maintenance action or a surfaced
suggestion — never an automatic background deletion — consistent with the rule
that data is never removed unexpectedly.

## Atomic Writes and Crash Recovery

Every write that must not tear uses temp-file + atomic rename, staged on the
same filesystem as its destination. The atomicity rule is enforced at three
granularities — the single file, the per-asset bundle, and the multi-asset
edit — each of which is owned by a section of
[Threat Model — Atomicity Invariants](/design/threat-model/#atomicity-invariants).

- **Client — single-file writes.** Sidecar and provenance appends stage to
  `{uuid}.cbor.tmp` and `{uuid}.provenance.cbor.tmp` in the destination
  directory, then rename into place. A direct overwrite is never used.
- **Client — per-asset bundle.** An asset import or update is a *bundle*:
  original (when present locally), sidecar, and a new provenance record.
  All `.tmp` files stage first; only after every staged file is on disk do
  the renames execute, and only in a fixed order (original → sidecar →
  provenance). A failure at any rename discards every remaining `.tmp` and
  rolls back the renames already done by deleting the just-renamed targets,
  so the on-disk state never reflects a partial bundle. The
  `.provenance.cbor` is the last to be renamed, so the existence of a new
  provenance record implies the rest of the bundle is committed.
- **Client — stack edit.** A stack edit touches multiple sidecars and writes
  a single provenance record per affected asset. All `.tmp` files (one per
  sidecar plus one per provenance file) stage first and rename together; any
  rename failure discards the entire batch. There is no partial stack.
- **Server — chunk assembly.** Chunks stage as `{upload_id}_{n}.part`; the
  assembled blob is `{upload_id}.bin`. The blob is renamed into its
  content-addressed location under `blobs/` only after the ciphertext hash
  is recomputed and matches the declared value (see
  [Import and Synchronization — Finalization and Integrity](/design/import-synchronization/#finalization-and-integrity)).
- **Server — finalization transaction.** The manifest envelope insert, the
  blob rename, the metadata blob insert, the provenance blob insert, and
  the asset row update commit in a single PostgreSQL transaction. The
  server never exposes an asset whose bundle is partially persisted; a
  crash between any pair leaves the session in `WaitingForProcessing` and
  the next finalization attempt either completes the bundle or fails it
  cleanly.

On startup, each side scrubs incomplete work: stale `.part`, `.tmp`, and `.bin`
files left by an interrupted upload or import are identified and removed, and
the cleanup is logged. A blob or media file is never published, on either side,
until its integrity has been verified.

## Encrypted Backups

A backup is an export artifact — encrypted, self-describing, and kept outside
both `{library_root}` and `{blob_root}` — so it is not part of the live library
or the server blob store, and may be stored on external or cloud storage. Its
format, the master-key escrow, and the recovery flow are covered in
[Backup and Recovery](/design/backup-recovery/).
