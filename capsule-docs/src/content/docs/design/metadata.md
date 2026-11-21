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
  media_subtype:         Option<MediaSubtype>,     // as imported; wire-absent when none (Closed Enum Value Sets)
  original_filename:     Option<String>,           // as imported; base name only; wire-absent when unknown (Original File Name)

  // display placeholder (see Thumbnails — LQIP)
  lqip:                  Option<{ chromahash: bytes, format_version: u16, dominant_color: [u8; 3] }>,

  // collaborative metadata (see Collaborative Metadata)
  tags_user:             OR_set<(tag: String, add_id)>,
  tags_ai:               OR_set<(tag: String, add_id, model_id: String, model_version: String)>,
  caption_lww:           Option<{ value: String, ts: RFC3339, by: device_id }>,  // value ≤ 4096 bytes; ts and by are audit-only
  superseded_captions:   Vec<{ value: String, written_by: device_id, ts: RFC3339 }>,  // ≤ 16
  rating_lww:            Option<{ value: u8, ts: RFC3339, by: device_id }>,

  // organization (semantics owned by Asset Organization); LWW, wire-absent when never written
  stack_membership:      Lww<Option<StackMembership>>,  // leave = a stamped None
  regions:               Map<UUIDv7, Lww<Option<Region>>>,  // labelled image regions; delete = a stamped None
  cull:                  Lww<CullFlag>,                 // pick | neutral | reject
  hidden:                Lww<bool>,

  // identifiers (Privacy on Export applies)
  camera_id:             Option<{ model: String, serial: String }>,  // EXIF Model and BodySerialNumber; serial "" when absent
  device_id:             UUIDv4,
  session_id:            UUIDv7,

  // geolocation (Privacy on Export applies to both)
  gps:                   Option<Gps>,     // as imported; never overwritten by a user edit
  gps_user:              Lww<Option<Gps>>,  // the user's edit (source manual); a stamped None = no location
  // Gps = { lat: f64, lon: f64, source: GpsSource, datum: GpsDatum /* wire-absent ⇒ wgs84 */ }

  // how it was taken; as imported, never user-edited (Capture Settings)
  capture_settings:      Option<CaptureSettings>,  // wire-absent when the original records none
  video:                 Option<VideoInfo>,        // video only; as imported, never user-edited (Video Info)

  // the PRIOR provenance chain head; absent only on create
  provenance_chain_hash: Option<[u8; 32]>,

  _unknown:              Map,             // unknown CBOR keys preserved verbatim, never executed
  signature:             Hybrid(Ed25519, ML-DSA-65),  // covers every byte above, including _unknown
}

CaptureSettings {                     // every field optional and wire-absent when unknown
  camera_make:       Option<String>,   // EXIF Make
  lens_make:         Option<String>,   // EXIF LensMake
  lens_model:        Option<String>,   // EXIF LensModel
  focal_length:      Option<[u32; 2]>, // EXIF FocalLength: millimetres, the rational as stored
  focal_length_35mm: Option<u32>,      // EXIF FocalLengthIn35mmFilm
  f_number:          Option<[u32; 2]>, // EXIF FNumber: the rational as stored
  exposure_time:     Option<[u32; 2]>, // EXIF ExposureTime: seconds, the rational as stored
  iso:               Option<u32>,      // EXIF PhotographicSensitivity
  exposure_bias:     Option<[i32; 2]>, // EXIF ExposureBiasValue: EV, the signed rational as stored
  flash_fired:       Option<bool>,     // EXIF Flash, bit 0
}

VideoInfo {                           // every field optional and wire-absent when unknown
  codec:       Option<String>,         // the container's codec identifier: ISO BMFF sample entry (`hvc1`), Matroska CodecID (`V_VP9`)
  frame_rate:  Option<[u32; 2]>,       // frames per second, the rational as the container records it (30000/1001)
  duration_ms: Option<u64>,            // the presentation duration, in milliseconds, rounded down
}

Region {
  shape:  Circle { cx: f32, cy: f32, r: f32 } | Rect { x: f32, y: f32, w: f32, h: f32 },  // [0, 1], display orientation
  labels: OR_set<(label: String, add_id)>,
}
```

`tags_user` holds the asset's user **labels** and `regions` its labelled image regions. [](/design/organization/#labels) specifies both, with the stack and album label records. `regions` was added without a `sidecar_schema` bump: older readers keep it verbatim as an unknown key. `media_subtype`, `original_filename`, `capture_settings` and `video` are additive top-level keys under the same rule. They are top-level rather than nested in an existing value because an older reader drops an unknown key nested inside a known one and would re-sign without it.

### Original File Name

`original_filename` is the name the original had when it entered the library. Export's `original` naming scheme ([](/design/export/#file-and-folder-names)) and the info page ([](/design/clients/)) use it.

- **Where it comes from.** On desktop, the imported file's name. On Apple platforms, the original filename Photos records for the asset's resource. On Android, the MediaStore display name. From an exporter archive, the name the exporter's record gives.
- **A base name, never a path.** A directory path leaks account names and folder structure, and no feature needs it.
- **Bounded.** The name is NFC-normalized, with control characters and both `/` and `\` removed. It is truncated on a character boundary to 255 bytes, keeping the extension. An empty result is absent.
- **As imported, never edited.** The user names an asset with its caption. A rename changes nothing here.
- **Stripped at a trust boundary by default**, because a name such as `anna-birthday-lisbon.jpg` can identify a person or a place ([](#privacy-on-export)).

### Capture Settings

`capture_settings` records how the photo was taken: the lens and the exposure. `camera_id` records which camera body took it. Import fills both from the original's EXIF. Today it fills neither: `camera_id` is written as `None` (`capsule-library/src/lifecycle/import.rs:1057`), and the extractor reads only Make and Model (`capsule-core/src/exif/extract.rs`).

- **In the sidecar, because the original may not be local.** A client's info page shows the camera, lens, focal length, aperture, shutter speed and ISO for an asset whose original is not on the device: a metadata-only scope, or a thumbnail-only cache. Derivatives carry no EXIF, so re-reading the original is not an option. The sidecar is the only record that reaches every device without the original ([](#local-and-server-metadata-equivalence)), so these values must live in it to exist at all.
- **As imported, never edited.** These are facts about the capture, with no user register. Re-deriving them from the original (the same repair as for capture time, [](#as-imported-and-user-values)) is a `metadata-update` that rewrites only `capture_settings` and `camera_id`.
- **Rationals verbatim.** EXIF stores these values as rationals, and they are kept as the `[numerator, denominator]` pair the file holds. Rounding to a float would put non-canonical values into a signed structure ([](#canonical-cbor-encoding)), and the display form (`f/1.8`, `1/250 s`) is a client concern. A zero denominator makes the field absent.
- **Bounded strings.** Each string is NFC-normalized, has its NUL padding and control characters removed, is trimmed, and is truncated on a character boundary to 128 bytes. An empty result is absent.
- **No lens serial.** EXIF `LensSerialNumber` is not stored. It identifies the owner and no feature needs it.
- **Searchable.** Camera and lens are search axes ([](/design/search/#indexed-axes)), and `camera`, `lens`, `capture_settings.iso` and `capture_settings.focal_length_35mm` are smart-album terms ([](/design/organization/#smart-album-definition-schema)).

### Video Info

`video` records what a video's container says about its main video track. Import fills it for every `video/*` `content_type` and never for a still.

- **In the sidecar, for the same reason as capture settings.** The info page shows codec, frame rate and duration for a video whose original is not on the device ([](/design/clients/#asset-info-page)), and only the sidecar reaches every device without the original.
- **As imported, never edited.** These are facts about the file. Re-deriving them from the original is a `metadata-update` that rewrites only `video`.
- **Rational frame rate.** The rate is kept as the `[numerator, denominator]` pair the container records, so 29.97 fps stays `30000/1001` and the signed bytes stay canonical ([](#canonical-cbor-encoding)). A zero in either term makes the field absent. A variable-rate track records its nominal rate, else none.
- **Bounded codec.** `codec` is the identifier verbatim, with control characters removed and truncated to 32 bytes. An empty result is absent. The display name (`HEVC`, `VP9`) is a client concern.

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

- **`content_type`**: exactly one canonical MIME value per format, never an alias such as `image/jpg`. The still values are exactly those `StillFormat::mime` returns (`capsule-core/src/media/detect.rs`).
  - images: `image/jpeg`, `image/png`, `image/webp`, `image/gif`, `image/tiff`, `image/heic`, `image/avif`, `image/jxl`, `image/x-portable-anymap`
  - RAW images, the set search and smart albums call `raw`: `image/x-adobe-dng`, `image/x-sony-arw`, `image/x-canon-cr2`, `image/x-canon-cr3`, `image/x-canon-crw`, `image/x-nikon-nef`, `image/x-fuji-raf`
  - video: `video/mp4`, `video/quicktime`, `video/x-matroska`, `video/webm`
- **`media_subtype`**: `screenshot` | `screen_recording`. Import sets it from the platform's own flag where there is one (Photos' screenshot and screen-recording subtypes), else from a source scope whose `SourceKind` is `screenshots` ([](/design/organization/#scope-grammar-local-source--album-mapping)), else from the file's own screenshot marker. It is never inferred from the pixels. Absent means an ordinary capture. It drives the Screenshots collection and the latest-screenshots shortcut ([](/design/platform-integrations/)).
- **`gps.source`**: `exif` | `manual` | `derived`. A `derived` value is computed from other evidence, such as a GPS track or a neighbouring asset's fix. It is written only by an explicit user action, never automatically, the same rule as promoting `tags_ai` to `tags_user`, and never over an `exif` or `manual` fix.
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

`provenance_chain_hash` references the **prior** chain head, never the write that seals this sidecar: that manifest commits to the sidecar's hash, so referencing it would be circular. **`sidecar.provenance_chain_hash` MUST equal the sealing manifest's `prior_provenance_hash`**, both absent on `create`. Every reader compares the two before it applies a sidecar, and a divergence is quarantined, never applied; a writer setting the field is not enough on its own ([](/design/threat-model/risks/)).

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

When an asset crosses a trust boundary (a share link for a non-member, a plaintext export or an external backup handed off, or a federated peer outside the owner's home server; see [](/design/share-links/), [](/design/export/) and [](/design/federation/)), these fields are stripped unless the user opts in **per export**:

| Field | Default on export | Opt-in retains |
| --- | --- | --- |
| Camera serial number (the `serial` of `camera_id`; the model stays) | Emptied | Full value |
| Device identifier (`device_id`, every register stamp's device, every OR-set `add_id` device) | Nil UUID; OR-set elements re-issued under it, tombstones dropped | Full value |
| Session ID | Nil UUID | Full value |
| Edit history (the `superseded` log of every LWW register: prior captions, ratings, capture times, flags, locations, region versions) | Emptied; only each register's current value leaves | Retained |
| GPS coordinates (`gps` and every `gps_user` value, superseded ones included when the history is retained) | Rounded to 2 decimal places (≈1 km) | Full precision |
| Personal contact tags (faces matched to a known person) | Stripped with `regions`, the only field that names a face | Retained |
| Image regions and their labels (`regions`) | Stripped | Retained |
| Original file name (`original_filename`) | Stripped | Retained |

- `capture_settings`, `video`, `media_subtype` and the camera model identify no one and leave unchanged.
- Keys the exporting reader does not know (`_unknown`) are always dropped, under every opt-in: a key a newer schema added may itself be a boundary-crossing field.
- A copy with anything stripped is unsigned, because the library signature covered what was removed.
- Stripping applies to the exported copy only; the library sidecar is untouched. The opt-in is chosen for one export and never stored. Syncing the same user's own devices is not a boundary crossing.
- Stripping is the **issuing client's** obligation, since the server holds no key. The server's half is containment: a share serves only the content addresses its record enumerates.

A share link carries each covered asset's stripped sidecar inside its sealed record ([](/design/share-links/#the-share-metadata-record)), so the blob the server stores is already stripped. Plaintext export applies the same strip, through its presets ([](/design/export/#trust-boundary)).

## Collaborative Metadata

User-editable fields may be edited concurrently and offline, so they are CRDTs:

- **Tags**: OR-sets with `add_id` binding ([](#add-id-binding)).
- **Single-value fields** (`caption_lww`, `rating_lww`, `capture_timestamp_user`, `gps_user`, `stack_membership`, `cull`, `hidden`): LWW registers in which the **later write on the asset's provenance chain wins**. The chain is the only order ([](/design/authorization/#authorizing-a-lifecycle-operation)). A writer extends it only at its head, and a write made offline against an old head is re-signed on the new head before it is accepted. A register's `ts` and `by` record who wrote it and when they said they did, and are audit-only: no merge compares them, so a device whose clock reads 2099 wins nothing it did not append last ([](/design/threat-model/schema-rules/)). `stack_membership` admits a stamped `None`, so join, move and leave are each one LWW write.

### Surfacing Concurrent Edits

The losing value of a concurrent caption edit (one written against a head that another caption write had already superseded) is appended to `superseded_captions` (≤ 16, oldest evicted) with author and time, so the UI can offer restore. Ratings keep no log. Clients must never strip `superseded_captions` ([](/design/threat-model/schema-rules/#forbidden-client-behaviors)).

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
  people:                   PeopleSection,                                       // People — Corrections
  memories:                 Map<memory_id,  Lww<Option<MemoryState>>>,          // Memories — Synced State
  _unknown:                 Map,
}
```

This page owns the envelope, keying, versioning and merge. [](/design/organization/#smart-album-definition-schema), [](/design/organization/#scope-grammar-local-source--album-mapping), [](/design/federation/#membership-and-rendering), [](/design/people/#corrections) and [](/design/memories/#synced-state) own the section value schemas. An absent or `None` cover falls back to the newest constituent asset.

- **Shape.** Every section is a keyed map of LWW registers over `Option<value>`; a stamped `None` is a tombstone. The document has no `signature` field: each mutation is a signed operation.
- **Keying.** Readable by the owner's devices and owner-set members and by **no** album co-member, so it is keyed under the OGK, not an AMK. `settings_doc_id` is an identifier, not a key, derived from the account master key ([](/design/cryptography/keys/#key-chain)) under a dedicated label. Operations use the metadata-blob wire format ([](/design/cryptography/encryption/#metadata-blob-wire-format)) with key `HKDF-SHA512(ikm = OGK_v{n}, salt = settings_doc_id || nonce, info = "library-settings/v1", length = 32)` and a server-visible big-endian `u32` `ogk_version` before the nonce. A fresh nonce per write re-rolls key and nonce; a reused nonce is refused. Rewrites use the current epoch, and removed owner-set members derive no future epoch.
- **Versioning.** [](#canonical-cbor-encoding), with refuse-to-write when `max_known_settings_schema < settings_schema`, as for sidecars. A `SmartAlbumDefinition` with a newer `predicate_schema` is preserved verbatim and never evaluated or stripped.
- **Merge.** Each mutation is a device-signed, OGK-sealed `(section, key, stamped value)` operation with the `prior_provenance_hash` link. Merge is field-wise LWW by position on the document's own operation chain, under the same rule as an asset's registers ([](#collaborative-metadata)), with no cross-field invariants. The `ts` inside an operation is audit-only. The server sees only ciphertext and `ogk_version`.

### Grouping Convergence (Requirement)

**Every grouping operation, manual or AI, is idempotent and order-independent** by construction:

| Structure | Mechanism | Why it converges |
| --- | --- | --- |
| Tags (`tags_user` / `tags_ai`) | OR-set | Add/remove keyed by `add_id`; commutative, associative, idempotent |
| Regions (`regions`) | Map of LWW registers, each holding an OR-set of labels | Merged as a semilattice: a delete is final, live writes keep the later shape and the union of labels |
| Caption / rating / `capture_timestamp_user` / `gps_user` / `stack_membership` / `cull` / `hidden` | LWW register | Total order by provenance-chain position; replay is a no-op |
| Smart albums, unnamed people clusters, aggregated federated albums ([](/design/federation/)) | Computed views | Nothing stored; membership is a deterministic function of inputs ([](/design/organization/#system--smart-albums-views), [](/design/ai/#ai-output-containment)) |
| People corrections (names, merges, assignments) | LWW maps in the library-settings document | People are a pure function of faces plus corrections, so devices that hold the same corrections show the same people ([](/design/people/)) |
| Container-album membership | Single home + ordered lifecycle ops | One container per asset; a replayed move no-ops ([](/design/organization/#container-albums)); concurrency ordered by MLS commits |

Album membership is not a CRDT: MLS commits ([](/design/cryptography/mls/)) already give a total order.

## Tag Provenance and Namespacing

User and AI tags are **structurally separate** OR-sets (`tags_user`, `tags_ai`), so an AI tag can never overwrite user intent.

- Every `tags_ai` entry carries `model_id` and `model_version` ([](/design/ai/#embedding-provenance)). When the canonical model changes, old tags are stale; cross-model comparison is forbidden ([](/design/threat-model/validation/#client-side-validation-invariants)).
- **Promote**: an explicit, signed user action, never automatic, copies the entry to `tags_user` with a fresh `add_id`.
- **Dismiss**: an OR-set remove on `tags_ai` by the original `add_id`.

Any future ML field overlaying a user-editable one (face labels, location guesses) uses the same dual namespace.

## Geolocation

GPS is stored in the datum the source supplied (`gps.datum`, [](#closed-enum-value-sets)): WGS-84 by default, or GCJ-02 for coordinates from Chinese maps. BD-09 exists only at the input edge. Datum conversion for search, clustering and display is deterministic and client-side, with GCJ-02 → WGS-84 marked approximate ([](/design/places/#datum-and-basemaps)). Place names are derived from the location and never stored in the sidecar ([](/design/places/#place-names)). A user's location edit lands in `gps_user` beside the imported fix ([](#as-imported-and-user-values)), stored verbatim in the datum the user entered it in.

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
- A far-future `ts` on a register write loses to a later write on the chain.
- A sidecar with no `media_subtype`, `original_filename`, `capture_settings` or `video` encodes byte-identically to the pre-field vector; an older reader keeps each verbatim and re-signs nothing.
- `video` keeps a fixture's `30000/1001` frame rate exactly, is absent on a still, and drops a zero-term rate and an empty codec.
- `capture_settings` round-trips each EXIF fixture's rationals exactly; a zero denominator, an over-long or NUL-padded string, and a lens serial are each handled as specified.
- A one-byte local mutation fails the local–server equivalence check.
- A sidecar round-trips between schema v1 and v2 readers.

## Related

- [](/design/organization/): consumes `stack_membership`, `cull`, `hidden`.
- [](/design/ai/): models behind `tags_ai`.
- [](/design/thumbnails/): the LQIP scheme in `lqip`.
