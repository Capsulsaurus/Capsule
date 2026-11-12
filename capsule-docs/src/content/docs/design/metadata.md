---
title: Metadata
description: The CBOR sidecar schema v1, the CRDT semantics for collaborative metadata, identifiers, and geolocation
status: draft
---

The CBOR sidecar is the canonical metadata record for every asset, plaintext and local only ([](/design/filesystem/client/)). This page is its **single source of truth**: every implementation must encode it byte-for-byte as specified, or cross-peer signatures break. Other docs name fields and never re-declare them.

Rawshift extracts media metadata and Capsule maps it into these fields. Chromahash is called directly, never through Rawshift.

## Sidecar Schema v1

```rust
SidecarV1 {
  sidecar_schema:        u16,             // FIELD 0 — readable before parsing the rest. Currently 1.
  crypto_suite_id:       u16,             // matches the asset's manifest; see Cryptography
  uuid:                  UUIDv7,
  hash:                  bytes,           // canonical plaintext digest; algorithm + length fixed by crypto_suite_id
  capture_timestamp:     RFC3339,         // as imported; never overwritten by a user edit
  capture_timestamp_user: Lww<RFC3339>,   // the user's correction; wire-absent when never written
  import_timestamp:      RFC3339,
  content_type:          String,          // closed enum, below
  dimensions:            Option<{ width: u32, height: u32 }>,

  // display placeholder (see Thumbnails — LQIP)
  lqip:                  Option<{ chromahash: bytes, format_version: u16, dominant_color: [u8; 3] }>,

  // collaborative metadata (see Collaborative Metadata)
  tags_user:             OR_set<(tag: String, add_id)>,
  tags_ai:               OR_set<(tag: String, add_id, model_id: String, model_version: String)>,
  caption_lww:           Option<{ value: String, ts: RFC3339, by: device_id }>,  // value ≤ 4096 bytes
  superseded_captions:   Vec<{ value: String, written_by: device_id, ts: RFC3339 }>,  // ≤ 16
  rating_lww:            Option<{ value: u8, ts: RFC3339, by: device_id }>,

  // organization (semantics owned by Asset Organization); LWW, wire-absent when never written
  stack_membership:      Lww<Option<StackMembership>>,  // leave = a stamped None
  regions:               Map<UUIDv7, Lww<Option<Region>>>,  // labelled image regions; delete = a stamped None
  cull:                  Lww<CullFlag>,                 // pick | neutral | reject
  hidden:                Lww<bool>,

  // identifiers (Privacy on Export applies)
  camera_id:             Option<{ model: String, serial: String }>,
  device_id:             UUIDv4,
  session_id:            UUIDv7,

  // geolocation (Privacy on Export applies to both)
  gps:                   Option<Gps>,     // as imported; never overwritten by a user edit
  gps_user:              Lww<Option<Gps>>,  // the user's edit (source manual); a stamped None = no location
  // Gps = { lat: f64, lon: f64, source: GpsSource, datum: GpsDatum /* wire-absent ⇒ wgs84 */ }

  // the PRIOR provenance chain head; absent only on create
  provenance_chain_hash: Option<[u8; 32]>,

  _unknown:              Map,             // unknown CBOR keys preserved verbatim, never executed
  signature:             Hybrid(Ed25519, ML-DSA-65),  // covers every byte above, including _unknown
}

Region {
  shape:  Circle { cx: f32, cy: f32, r: f32 } | Rect { x: f32, y: f32, w: f32, h: f32 },  // [0, 1], display orientation
  labels: OR_set<(label: String, add_id)>,
}
```

`tags_user` holds the asset's user **labels** and `regions` its labelled image regions. [](/design/organization/#labels) specifies both, with the stack and album label records. `regions` was added without a `sidecar_schema` bump: older readers keep it verbatim as an unknown key.

### As-Imported and User Values

Capture time and location each have two fields: the value the import recorded and the user's correction.

- `capture_timestamp` and `gps` hold what the original said when it entered the library: its own EXIF, else the exporter record folded with it. A user edit never overwrites them.
- `capture_timestamp_user` and `gps_user` are LWW registers holding the user's correction. Both are wire-absent until first written, so an uncorrected sidecar encodes byte-identically to one written before they existed, and older readers keep them verbatim as unknown keys. A `gps_user` value always has `source = manual`. A stamped `None` means "no location": it hides the imported fix without erasing it.
- The **effective** value is the user's register if ever written, else the as-imported field. The timeline, search and every client view use it.
- The `media/{YYYY}/{YYYY-MM}` shard is fixed at import from `capture_timestamp`; a correction does not move the bundle.
- Re-deriving the as-imported value from the original (the capture-time repair for libraries imported before EXIF dates were read) rewrites `capture_timestamp` only and never touches the user's register.

### Schema Versioning Rules

- `sidecar_schema` is CBOR field 0 under deterministic key order, so a reader knows the schema before parsing the rest.
- A client whose `max_known_sidecar_schema < sidecar_schema` **refuses to write** the sidecar and reads it only in opted-in read-only mode (refuse-by-default, [](/design/threat-model/)), so an old client cannot strip-and-resign a newer sidecar.
- The signature covers `_unknown`, so stripping unknown fields is detectable.
- An album's pinned protocol version constrains which sidecar schemas may be written into it ([](/design/versioning/#album-protocol-version-pinning)).

### Closed Enum Value Sets

These value sets are closed ([](/design/threat-model/schema-rules/)); adding a value requires a new, later-dated `protocol_version`.

- **`content_type`**: exactly one canonical MIME value per format, never an alias such as `image/jpg`.
  - images: `image/jpeg`, `image/png`, `image/webp`, `image/gif`, `image/tiff`, `image/heic`, `image/avif`, `image/jxl`, `image/x-adobe-dng`
  - video: `video/mp4`, `video/quicktime`, `video/x-matroska`, `video/webm`
- **`gps.source`**: `exif` | `manual` | `inferred`. An `inferred` value is written to `gps` only on explicit user confirmation, the same promotion rule as `tags_ai` → `tags_user`.
- **`gps.datum`**: `wgs84` | `gcj02`, stored **verbatim** as supplied and never converted at rest, since GCJ-02 → WGS-84 has no exact inverse. BD-09 is never stored: it is folded to GCJ-02 at the input edge with an error-bounded (sub-meter) iterative inverse. `datum` is an additive key within schema v1, wire-absent when `wgs84`, so pre-`datum` sidecars stay byte-identical. `gps` merges as one atomic value.

### Canonical CBOR Encoding

The sidecar and the encrypted metadata blob ([](/design/cryptography/encryption/#metadata-encryption)), whose plaintext is this document, MUST serialize byte-identically in every implementation because the signed manifest ([](/design/cryptography/provenance/#asset-manifest)) commits to the bytes. Encoding is RFC 8949 §4.2 deterministic:

- Definite-length encoding only.
- Shortest-form integers.
- Bignums (tags 2 and 3) in preferred form (RFC 8949 §3.4.3): a magnitude that fits in 64 bits is the plain integer, and the byte string carries no leading zeros. Canonicalization refuses a negative bignum below -2^127.
- Map keys sorted by bytewise order of their encoded form, no duplicates. This includes `_unknown`, re-sorted on write so round-trips are byte-stable.
- Floats in the shortest IEEE-754 form that round-trips exactly; canonical quiet NaN. Prefer integers or strings in signed structures.

Conformance is a **blocking cross-language gate** against shared known-answer vectors, the same fixtures the metadata blob wire format uses ([](/design/cryptography/encryption/#metadata-blob-wire-format)).

### Local and Server Metadata Equivalence

The plaintext of the server's encrypted metadata blob *is* this signed `SidecarV1`, the same bytes the client stores at `media/{uuid}.cbor`:

- The signed manifest ([](/design/cryptography/provenance/#asset-manifest)) commits to `metadata_blob_hash` on every `create`, `replace` and `metadata-update`.
- A client that decrypts the blob MUST find it byte-identical to its locally stored signed sidecar, and the blob's content hash MUST equal `metadata_blob_hash`.

A client never persists a sidecar that does not round-trip to the committed hash ([](/design/threat-model/validation/#client-side-validation-invariants)); the server enforces the blob-hash match as invariant 25 of [](/design/threat-model/validation/#server-side-validation-invariants).

### Provenance Binding and Sealing Order

`provenance_chain_hash` references the **prior** chain head, never the write that seals this sidecar: that manifest commits to the sidecar's hash, so referencing it would be circular. **`sidecar.provenance_chain_hash` MUST equal the sealing manifest's `prior_provenance_hash`**, both absent on `create`; divergence is quarantined.

Sealing order:

1. Fix the prior head `H` (`None` on `create`).
2. Author and sign the sidecar with `provenance_chain_hash = H`.
3. Seal it into the metadata blob ([](/design/cryptography/encryption/#metadata-blob-wire-format)) and compute its content hash.
4. Build and sign the manifest with `prior_provenance_hash = H` and that `metadata_blob_hash`; append it as the new head.

### Add-id Binding

`add_id = (device_id: UUIDv4, monotonic_counter: u64)`, the counter incremented per device per (asset, OR-set). Every OR-set remove targets a specific `add_id`. A remove naming an `add_id` the receiver never observed is **rejected**, not a no-op ([](/design/threat-model/scenarios/)).

**Counter durability.** A counter MUST never repeat for a `(device_id, asset, OR-set)` once it reached a written sidecar, or two adds alias. It is persisted in the local index ([](/design/filesystem/client/#desktop-library-layout)) and on restart reseeded to one past the maximum counter in the device's own signed sidecars, fetched from the server if not held locally. It resets to zero only when no sidecar bears the `device_id`; in practice a reinstall enrolls a new `device_id`.

## Identifiers

The sidecar `uuid`, manifest `file_id`, provenance/server `asset_id` and metadata-key-salt `blob_id` are **the same UUIDv7**, minted once at import and never re-minted; validators assert equality. `session_id`, `stack_id` and `import_id` are UUIDv7. `device_id` is UUIDv4 so it leaks no creation order.

- **`camera_id`**: device model plus a per-device identifier such as the serial.
- **`device_id`**: generated on the importing device.
- **`session_id`**: the authenticated import session ([](/design/authentication/#session-id)).

## Privacy on Export

When an asset crosses a trust boundary (a share link for a non-member, an external backup handed off, or a federated peer outside the owner's home server; see [](/design/share-links/) and [](/design/federation/)), these fields are stripped unless the user opts in **per export**:

| Field | Default on export | Opt-in retains |
| --- | --- | --- |
| Camera serial number (the `serial` of `camera_id`; the model stays) | Emptied | Full value |
| Device identifier (`device_id`, every register stamp's device, every OR-set `add_id` device) | Nil UUID; OR-set elements re-issued under it, tombstones dropped | Full value |
| Session ID | Nil UUID | Full value |
| Edit history (the `superseded` log of every LWW register: prior captions, ratings, capture times, flags, locations, region versions) | Emptied; only each register's current value leaves | Retained |
| GPS coordinates (`gps` and every `gps_user` value, superseded ones included when the history is retained) | Rounded to 2 decimal places (≈1 km) | Full precision |
| Personal contact tags (faces matched to a known person) | Stripped with `regions`, the only field that names a face | Retained |
| Image regions and their labels (`regions`) | Stripped | Retained |

- Keys the exporting reader does not know (`_unknown`) are always dropped, under every opt-in: a key a newer schema added may itself be a boundary-crossing field.
- A copy with anything stripped is unsigned, because the library signature covered what was removed.
- Stripping applies to the exported copy only; the library sidecar is untouched. The opt-in is chosen for one export and never stored. Syncing the same user's own devices is not a boundary crossing.
- Stripping is the **issuing client's** obligation, since the server holds no key. The server's half is containment: a share serves only the content addresses its record enumerates.

A share link carries each covered asset's stripped sidecar inside its sealed record ([](/design/share-links/#the-share-metadata-record)), so the blob the server stores is already stripped. The external-backup crossing waits on a post-v1 client export command, which applies the same strip.

## Collaborative Metadata

User-editable fields may be edited concurrently and offline, so they are CRDTs:

- **Tags**: OR-sets with `add_id` binding ([](#add-id-binding)).
- **Single-value fields** (`caption_lww`, `rating_lww`, `capture_timestamp_user`, `gps_user`, `stack_membership`, `cull`, `hidden`): LWW registers ordered by signed timestamp, with `device_id` as lexicographic tiebreaker. `stack_membership` admits a stamped `None`, so join, move and leave are each one LWW write.

### Surfacing Concurrent Edits

The losing value of a concurrent caption edit is appended to `superseded_captions` (≤ 16, oldest evicted) with author and time, so the UI can offer restore. Ratings keep no log. Clients must never strip `superseded_captions` ([](/design/threat-model/schema-rules/#forbidden-client-behaviors)).

### How Operations Travel

Capsule encrypts **operations**, not state. Merges are commutative and associative, so arrival order and stale replays do not matter. Operations reconcile into the sidecar, which stays the source of truth. They name **known fields only** (unknown-field ops are version-gated), and reconciliation re-emits `_unknown` verbatim. Each operation carries the `prior_provenance_hash` chain link like any lifecycle action ([](/design/authorization/#the-closed-action-set)).

The same path carries each album's album-group assertion ([](/design/federation/#the-album-group-assertion)) and the per-owner library-settings document ([](#the-library-settings-document)). The default-album designation ([](/design/organization/#the-default-album)) is separate: a non-secret server-side pointer.

### The Library-Settings Document

**Deferred post-v1** with the OGK cluster it is keyed under ([](/design/cryptography/keys/#owner-group-keys-ogks)). The schema is normative as written.

```rust
LibrarySettingsV1 {
  settings_schema:          u16,  // FIELD 0. Currently 1.
  smart_albums:             Map<UUIDv7,     Lww<Option<SmartAlbumDefinition>>>,  // Organization
  scope_overrides:          Map<scope_id,   Lww<Option<album_id>>>,             // Organization — Scope Grammar
  source_kind_defaults:     Map<SourceKind, Lww<Option<album_id>>>,             // Organization — Scope Grammar
  aggregated_album_covers:  Map<group_id,   Lww<Option<asset_id>>>,             // per-viewer; Federation
  _unknown:                 Map,
}
```

This page owns the envelope, keying, versioning and merge. [](/design/organization/#smart-album-definition-schema), [](/design/organization/#scope-grammar-local-source--album-mapping) and [](/design/federation/#membership-and-rendering) own the section value schemas. An absent or `None` cover falls back to the newest constituent asset.

- **Shape.** Every section is a keyed map of LWW registers over `Option<value>`; a stamped `None` is a tombstone. The document has no `signature` field: each mutation is a signed operation.
- **Keying.** Readable by the owner's devices and owner-set members and by **no** album co-member, so it is keyed under the OGK, not an AMK. `settings_doc_id` is an identifier, not a key, derived from the account master key ([](/design/cryptography/keys/#key-chain)) under a dedicated label. Operations use the metadata-blob wire format ([](/design/cryptography/encryption/#metadata-blob-wire-format)) with key `HKDF-SHA512(ikm = OGK_v{n}, salt = settings_doc_id || nonce, info = "library-settings/v1", length = 32)` and a server-visible big-endian `u32` `ogk_version` before the nonce. A fresh nonce per write re-rolls key and nonce; a reused nonce is refused. Rewrites use the current epoch, and removed owner-set members derive no future epoch.
- **Versioning.** [](#canonical-cbor-encoding), with refuse-to-write when `max_known_settings_schema < settings_schema`, as for sidecars. A `SmartAlbumDefinition` with a newer `predicate_schema` is preserved verbatim and never evaluated or stripped.
- **Merge.** Each mutation is a device-signed, OGK-sealed `(section, key, stamped value)` operation with the `prior_provenance_hash` link. Merge is field-wise LWW on `(ts, device_id)` with no cross-field invariants. The server sees only ciphertext and `ogk_version`.

### Grouping Convergence (Requirement)

**Every grouping operation, manual or AI, is idempotent and order-independent** by construction:

| Structure | Mechanism | Why it converges |
| --- | --- | --- |
| Tags (`tags_user` / `tags_ai`) | OR-set | Add/remove keyed by `add_id`; commutative, associative, idempotent |
| Regions (`regions`) | Map of LWW registers, each holding an OR-set of labels | Merged as a semilattice: a delete is final, live writes keep the later shape and the union of labels |
| Caption / rating / `capture_timestamp_user` / `gps_user` / `stack_membership` / `cull` / `hidden` | LWW register | Total order on `(ts, device_id)`; replay is a no-op |
| Smart albums, people clusters, aggregated federated albums ([](/design/federation/)) | Computed views | Nothing stored; membership is a deterministic function of inputs ([](/design/organization/#system--smart-albums-views), [](/design/ai/#ai-output-containment)) |
| Container-album membership | Single home + ordered lifecycle ops | One container per asset; a replayed move no-ops ([](/design/organization/#container-albums)); concurrency ordered by MLS commits |

Album membership is not a CRDT: MLS commits ([](/design/cryptography/mls/)) already give a total order.

## Tag Provenance and Namespacing

User and AI tags are **structurally separate** OR-sets (`tags_user`, `tags_ai`), so an AI tag can never overwrite user intent.

- Every `tags_ai` entry carries `model_id` and `model_version` ([](/design/ai/#embedding-provenance)). When the canonical model changes, old tags are stale; cross-model comparison is forbidden ([](/design/threat-model/validation/#client-side-validation-invariants)).
- **Promote**: an explicit, signed user action, never automatic, copies the entry to `tags_user` with a fresh `add_id`.
- **Dismiss**: an OR-set remove on `tags_ai` by the original `add_id`.

Any future ML field overlaying a user-editable one (face labels, location guesses) uses the same dual namespace.

## Geolocation

GPS is stored in the datum the source supplied (`gps.datum`, [](#closed-enum-value-sets)): WGS-84 by default, or GCJ-02 for coordinates from Chinese maps. BD-09 exists only at the input edge. Datum conversion for display or search is deterministic and client-side, with GCJ-02 → WGS-84 marked approximate; display-side conversion is unscheduled. Map-provider choice is a client concern. A user's location edit lands in `gps_user` beside the imported fix ([](#as-imported-and-user-values)), stored verbatim in the datum the user entered it in.

## Failure Modes

| Failure | Detected / contained by | Recovery |
| --- | --- | --- |
| CBOR non-determinism | [](#canonical-cbor-encoding) | Byte-identical re-encode |
| Old client strips unknown sidecar fields | Signature covers all bytes; writes above the client's max `sidecar_schema` refused | Quarantine |

## Validation

Each MUST above is a unit test, and the canonical-CBOR known-answer vectors are a **blocking cross-language gate**. Notable cases:

- Add-id reseed never reuses a counter.
- OR-set and library-settings merges converge byte-identically in every permutation.
- A remove of an unobserved `add_id` is rejected.
- A near-simultaneous caption loser lands in `superseded_captions`.
- A capture-time or location edit keeps the as-imported value through a re-import, a reopen and an index rebuild.
- Each privacy-table row strips by default and leaves the local sidecar unchanged.
- A WGS-84 sidecar encodes byte-identically to the pre-`datum` vector.
- A one-byte local mutation fails the local–server equivalence check.
- A sidecar round-trips between schema v1 and v2 readers.

## Related

- [](/design/organization/): consumes `stack_membership`, `cull`, `hidden`.
- [](/design/ai/): models behind `tags_ai`.
- [](/design/thumbnails/): the LQIP scheme in `lqip`.
