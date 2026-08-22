---
title: Server Filesystem
description: The server's blob store layout, Postgres index, and required services
status: draft
---

The server's job is to hold ciphertext blobs and a key-free index that maps assets to blobs. It performs no decoding, no metadata extraction, and no thumbnail generation — it cannot, since it never holds a decryption key. The blob layout below **is** the contract: a server-side rebuild (re-deriving the Postgres index from blob bytes) depends on the file naming and the manifest envelope being exactly as specified here.

Implementation is planned in `capsule-api::{blob,index}` behind a narrow Capsule-owned blob-store port with arbitrary backend support. Volatile session state will live in Valkey (see [Required Services](#required-services)); it is not a versioned API surface.

## Required Services

The server's state is split across **three required systems** — one code path, no optional profiles:

- **Blob store** (filesystem) — the encrypted bytes of every asset.
- **PostgreSQL** — the authoritative durable index: ownership, album references, blob references, lifecycle state, and the pending-asset rows uploads create.
- **Valkey** — volatile session state: upload-session records (offsets, status) keyed `upload:session:{id}`, with the store's native 24-hour TTL as the lifetime **cap** (the ≥1-hour survival floor and pressure-discard semantics are owned by [Upload Protocol — Session Lifetime and Discard](/design/import/upload-protocol/#session-lifetime-and-discard)); [auth session records](/design/authentication/) ride the same store.

The durable/volatile split is design, not a tuning knob: the hot upload path — offset increments and status transitions — never touches the durable Postgres asset row, which is written exactly twice per upload (pending row at session creation, `uploaded` flip at finalization). A Postgres-resident session table would be a second implementation of the same contract; Capsule ships exactly one.

## Blob Store Layout

```text
{blob_root}/
├── incoming/
│   └── {upload_id}.bin             # in-progress append-only upload, pre-verification
├── blobs/
│   └── {hash}.bin                  # finalized blob, content-addressed
└── .server/
    ├── version                     # server filesystem schema version
    └── config                      # server-wide configuration
```

- **`{blob_root}`**: absolute path configured at server startup. The entire tree must be on a single filesystem so that finalization renames are atomic.
- **`incoming/`**: live uploads. Each session owns a single append-only file `{upload_id}.bin`; accepted chunks are appended in order, and the 4 KiB chunk alignment keeps every write block-aligned. There is no per-chunk staging and no assembly step. See [Import — Upload Protocol: Append-Only Storage](/design/import/upload-protocol/#append-only-storage).
- **`blobs/`**: the finalized store. A blob's filename is its [ciphertext content hash](/design/cryptography/primitives/) with a `.bin` suffix, in a single flat directory — amended 2026-07-12 to the landed layout: no hot path enumerates the directory, and modern filesystems handle multi-million-entry directories. If directory scaling ever bites, the pre-agreed migration is a two-level hex-prefix shard (`{hash[0:2]}/{hash[2:4]}/{hash}`) behind a `.server/version` bump. **The flat namespace must be re-ratified explicitly in the `capsule-api::blob` port design, not inherited**: it is recorded here as a measured fact of the shipped store, and no design record on either side states *why* the shard was dropped — a single directory of content-hashes has directory-entry and `readdir`/rebuild-scan scaling consequences the shard was chosen to avoid, so the new server must re-decide it on the merits (with the rebuild scan, not just the serve path, as the sizing case). A finalized blob is immutable.
- **`.server/`**: the server operator's own configuration and schema version. This is plaintext server metadata, not user data — it is the one thing under `{blob_root}` that is not an encrypted blob.

## Uniform, Opaque Blobs

A single asset produces a **bundle** of blobs (see [Import — Upload Protocol: What Gets Uploaded](/design/import/upload-protocol/#what-gets-uploaded)): the encrypted original, encrypted derivatives (thumbnails, previews), and the encrypted CBOR metadata blob (which carries the LQIP) — every one of them fully opaque, content-addressed ciphertext the store does not distinguish. Beside them, each write persists its signed **manifest envelope object** (see [Provenance — Physical placement](/design/cryptography/provenance/#asset-manifest)): a small, deliberately server-visible signed CBOR object, stored content-addressed like any blob, whose append-only sequence is the asset's provenance chain. The hot-path mapping from an asset to its blobs and their roles lives in PostgreSQL, with the envelope objects as its durable, key-free fallback.

## Recovering the Index from Blobs Alone

The PostgreSQL index is authoritative but **not the only copy** of what the server knows. Every write persists its signed [manifest envelope object](/design/cryptography/provenance/#asset-manifest) in the blob store beside the opaque ciphertext blobs it names, and those envelopes carry everything needed to rebuild the index rows. This is the server-side counterpart of the recovery-first principle that lets a client rebuild its index from CBOR sidecars.

The server-visible envelope includes:

- `crypto_suite_id`, `protocol_version`, `amk_version` — what bundle of primitives encrypted this asset and which album epoch
- the ciphertext hash and declared size — content address and storage attribution
- `created_by_user`, `created_by_device`, `album_id`, `file_id`, `prior_provenance_hash`, `action` — owner, provenance chain link, and lifecycle action
- `client_version` — the [exact client build](/design/cryptography/provenance/#client-build-identification) that produced the write, down to the commit hash — what scopes a defective-build incident to the assets it touched
- the device's hybrid signature — provenance attribution; verifiable against the public device directory even without any key the server holds

A rebuild walks the **envelope objects** under `blobs/`, verifies each device signature against the cached device directory, and writes index rows for the blobs each envelope names (original and derivatives by `ciphertext_hash` + role, the metadata blob by `metadata_blob_hash`). A ciphertext blob referenced by no envelope surfaces as an orphan for [GC](#deletion-and-garbage-collection); an envelope naming a missing blob surfaces as a dangling reference — **except** a missing *original* on an asset whose feed state is [`awaiting-original`](/design/import/download-sync/#upload-tiering-staged-uploads), which is expected staged-upload state, not corruption. The rebuild is idempotent: re-running it against an existing index produces no changes. The full envelope check list a server runs at recovery is the same list it runs at write time — see [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants).

An envelope object that fails structural validation during rebuild is **quarantined**, not silently dropped — moved to `{blob_root}/quarantine/` with a sibling `.reason.json` recording the rejection code. This guarantees that an unrecoverable byte sequence is preserved for forensic inspection rather than vanishing on rebuild.

Operationally the rebuild is invoked when a PostgreSQL restore is incomplete or a logical-corruption event is detected; it is **never** the hot path. The hot path runs through the authoritative PG index. The recovery path's job is to make the index reconstructible if PG is lost, not to substitute for it. Recovery is also not *verification*: the proactive, read-only check that a frozen Postgres + blob-store pair is mutually consistent — the path that detects the drift before anyone needs a rebuild — is the [server-side integrity scrub](/design/filesystem/maintenance/#server-side-integrity-scrub).

## Manifest Envelope Validation

Every write — `POST /upload`, `PATCH /upload/{id}`, finalization, any lifecycle manifest, any federation pull — passes through structural validation of the manifest envelope **before** any state is persisted. The server holds no decryption key, so it cannot verify the cryptographic signatures; but it does enforce that every envelope field is present, structurally well-formed, within bounds, and consistent with the album the manifest claims to address.

The complete refuse-by-default checklist is owned by [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants). A rejection at any check returns the rejection code listed there and writes no state. This is what defeats the version-mismatched-client damage class without requiring the server to hold a key.

## Content-Addressing and Deduplication

Naming blobs by their [ciphertext content hash](/design/cryptography/primitives/) makes deduplication free: a blob already present is never stored twice. At upload-session creation the server checks for a blob with the same content hash already owned by the uploader — an exact local-and-remote duplicate is rejected up front, and an asset that exists remotely under a *different* ciphertext resolves to a **merge** that links the existing blob rather than storing a second copy (see [Import — Upload Protocol: Deduplication and Merge](/design/import/upload-protocol/#deduplication-and-merge)). Reference counting in PostgreSQL determines when a blob is genuinely unreferenced.

## PostgreSQL: What the Server Knows

The server index records only what can be known without a key:

- `asset_id`, `owner_id`, `album_id`, `upload_user_id`
- references to the asset's blobs (their [content hashes](/design/cryptography/primitives/)) and each blob's role
- `amk_version` — which album-key epoch encrypted the asset
- declared ciphertext size and `content_type`
- the `uploaded` flag and server-visible lifecycle state
- the server's own trusted `received_at` per write — the authoritative clock for time-based policy (retention, rate limits) — alongside the client's self-asserted, audit-only `timestamp`
- `client_version` — the [exact producing client build](/design/cryptography/provenance/#client-build-identification), audit-only, kept queryable so one defective build's writes can be enumerated
- provenance records (see [Cryptography — Provenance](/design/cryptography/provenance/#provenance-of-library-modifications))

No plaintext capture date, dimensions, EXIF, tags, or filename ever reaches the server. Those live inside the encrypted metadata blob (see [Metadata Encryption](/design/cryptography/encryption/#metadata-encryption)) and are readable only by authorized clients.

Session creation writes a *pending* asset row (`uploaded = false`) that reserves the asset ID the bundle's blobs reference; finalization flips it. See the [session state machine](/design/import/upload-protocol/#session-state-machine).

## Ownership, Partitioning, and Quota

`owner_id` is the billing and namespace entity; the `owner_id` → user-set mapping lives in PostgreSQL and is mirrored as an MLS group (the [Owner Group Key](/design/cryptography/keys/#owner-group-keys-ogks)). Storage quota is accounted to `upload_user_id`, which is distinct from `owner_id` — the full quota model is owned by [Quota](/design/quota/). The blob store itself is not partitioned by owner — content-addressing is global — but every blob *reference* is owner-scoped in PostgreSQL, and deduplication checks are scoped to the owner.

The owner record also carries a non-secret **`default_album_id`** pointer (and an optional `(scope → album_id)` override map) naming the owner's [default album](/design/organization/#the-default-album) — the import destination when the user picks none. It is a plain UUID the server stores and serves but never acts on for authorization: a write is still gated on real album write capability ([invariant 6](/design/threat-model/validation/#server-side-validation-invariants)), so the pointer is discovery convenience, not a security control. Album *contents* stay E2E-encrypted; the server learns only which album UUID is currently the default.

## Deletion and Garbage Collection

The server cannot read an asset's `is_deleted` flag — it lives inside the encrypted metadata blob. Lifecycle transitions are signalled by the client and recorded as server-visible state on the asset row; soft delete is a state change, not a file operation. Permanent deletion drops the asset's blob references. A blob is removed **only** when it is provably unreferenced, and the mechanism is deliberately built so that a bug biases toward *keeping* bytes, never deleting live ones.

- **Reference counting is the single source of truth.** A blob's reference count is a query over committed asset / derivative / metadata / provenance rows — never a separately-maintained counter that could drift out of sync. A blob is GC-eligible only when that query returns zero.
- **Two-phase mark-and-sweep with a grace window.** Reaching zero references *marks* a blob (records `collectable_since`); it is swept only after a configurable grace window (default 24–72 h) **and** only after the zero-reference count is re-confirmed inside the deleting transaction (`SELECT … FOR UPDATE` over the reference set). A reference reappearing during the window — an in-flight finalization retry, a concurrent merge — cancels the mark. This reclaims the finalization-crash orphan (a blob renamed into `blobs/` whose Postgres commit never landed; see [Maintenance — Atomic Writes](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery)) without ever racing a legitimate late reference.
- **A Postgres↔filesystem mismatch is never resolved by deletion.** The two directions are asymmetric because only one risks data loss. A blob in `blobs/` with no referencing row is an orphan, reclaimed by the zero-reference sweep above. A committed row referencing a blob **missing** from `blobs/` is a *loud* integrity error — surfaced, logged, and quarantined for an operator — **never** auto-deleted: erasing the dangling row would destroy the only record that the asset should exist, exactly the data-loss class the [data-integrity principle](/design/principles/) forbids.
- **Auditable, reversible by default.** Every GC decision is logged with the blob hash, the observed reference count, and the mark/sweep timestamps (per the [traceability principle](/design/principles/)); a dry-run mode reports what *would* be collected without removing anything, so a suspect sweep can be inspected before it runs.

## Storage Verification

Clients need to confirm an asset is *safely stored* before they discard their only local copy — not just that a hash matches, but that the server physically holds the bytes, has them indexed, and would serve them. The server answers this without any key, composing the three facts it already tracks: the blob is present in `blobs/` (a `stat`), it is referenced by a committed `uploaded = true` row, and it is retrievable — reference count > 0 and **not** `collectable_since` (mid-[GC](#deletion-and-garbage-collection)), quarantined, or a [dangling-reference integrity error](#deletion-and-garbage-collection). A blob that is marked collectable, quarantined, or missing from `blobs/` is reported non-retrievable so a client never releases a local copy the server is about to or has already lost. The wire contract, the per-blob verdict shape, and the client-side **verify-before-destroy** rule that consumes it are owned by [Import — Storage Verification](/design/import/storage-verification/); the route will live in `capsule-api::blob`.

## Validation

- **Layout round-trip (unit).** Upload, finalize, rename, and assert the blob lives at exactly `blobs/{hash}.bin` on disk. Recompute the hash from disk; assert match.
- **Index rebuild idempotency (smoke).** Take a real testcontainer Postgres + a populated `blobs/` tree, drop the index tables, run the rebuild routine, assert every row matches a hand-derived expected set. Re-run; assert zero changes.
- **Quarantine on malformed envelope (unit).** Inject a blob with a corrupted manifest envelope into `blobs/`; run rebuild; assert the blob moves to `quarantine/` with a `.reason.json` that names the structural check that failed.
- **Reference-count GC safety (unit).** Decrement a blob's last reference; assert eligibility for GC; assert GC only proceeds after a configurable grace period; concurrent re-reference during the grace period cancels GC.
- **Dangling-reference safety (unit).** Point a committed row at a blob hash absent from `blobs/`; run the integrity check; assert the row is surfaced/quarantined and **never** auto-deleted, and that the missing blob is not treated as collectable.
- **Storage-verification verdict (unit).** Compose the no-key verdict for a finalized asset (stored + indexed + retrievable → `durable`); then mark a referenced blob `collectable_since` and assert it reports non-retrievable, and remove a blob from `blobs/` and assert non-stored. Owner: [Import — Storage Verification](/design/import/storage-verification/).

Cross-module cases (upload → finalize → rebuild from blobs) are bounded E2E surface listed in [Module Map](/design/module-map/#e2e-test-surface).
