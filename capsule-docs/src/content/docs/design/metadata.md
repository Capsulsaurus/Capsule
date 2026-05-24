---
title: Metadata
description: How Capsule extracts and utilizes metadata from assets
---

## Design Philosophy

All metadata processing in Capsule is handled by `capsule-core`, which is implemented in Rust and exposed to all languages via FFI. It handles the I/O natively and is generally opaque to minimize FFI surface.

This doc is the **single source of truth** for the CBOR sidecar schema. Per the [single-source-of-truth rule](/design/principles/#single-source-of-truth), other docs reference fields here by name and never re-declare them.

## Metadata Capabilities

We minimize the logic involved in repository and leverage dependencies where useful. This is the rough breakdown (subject to being outdated):

- `capsule-core`: Extracts the filesystem metadata for verification and indexing.

## Sidecar Schema v1

The CBOR sidecar is the client's canonical, plaintext-local-only metadata record (see [Filesystem — Client Filesystem](/design/filesystem/#client-filesystem)). It is **self-describing**: field 0 carries the schema version so any reader can detect a schema it does not implement *before* parsing the rest. Versioning the schema in-band is what prevents a faulty or old client from corrupting state with a partial parse (see [Threat Model — Schema Evolution](/design/threat-model/)).

```rust
SidecarV1 {
  sidecar_schema:        u16,             // FIELD 0 — readable before parsing the rest. Currently 1.
  crypto_suite_id:       u16,             // matches the asset's manifest; see Cryptography
  uuid:                  UUIDv7,
  hash:                  { algo: String, value: bytes },   // canonical plaintext hash
  capture_timestamp:     RFC3339,
  import_timestamp:      RFC3339,
  content_type:          String,          // closed enum per protocol_version
  dimensions:            Option<{ width: u32, height: u32 }>,

  // collaborative metadata (see Collaborative Metadata below)
  tags_user:             OR_set<(tag: String, add_id)>,
  tags_ai:               OR_set<(tag: String, add_id, model_id: String, model_version: String)>,
  caption_lww:           Option<{ value: String, ts: RFC3339, by: device_id }>,
  superseded_captions:   Vec<{ value: String, written_by: device_id, ts: RFC3339 }>,  // bounded ≤ 16
  rating_lww:            Option<{ value: u8, ts: RFC3339, by: device_id }>,

  // identifiers (see Identifiers below; privacy-on-export rules apply)
  camera_id:             Option<{ model: String, serial: String }>,
  device_id:             UUIDv4,
  session_id:            UUIDv7,

  // geolocation (see Geolocation below)
  gps:                   Option<{ lat: f64, lon: f64, source: GpsSource }>,

  // provenance binding
  provenance_chain_hash: [u8; 32],        // hash of the latest ProvenanceRecord for this asset

  // forward-compat
  _unknown:              Map,             // unknown CBOR keys preserved verbatim, never executed

  // signature
  signature:             Hybrid(Ed25519, ML-DSA-65),  // covers every byte above, including _unknown
}
```

### Schema Versioning Rules

- `sidecar_schema` is **CBOR field 0 by deterministic key order** (RFC 8949 §4.2). A reader can determine the schema before allocating a parser for the rest.
- A client whose `max_known_sidecar_schema < this.sidecar_schema` **refuses to write** to that sidecar. Reading is allowed only in read-only mode if explicitly opted-in. This is the [refuse-by-default rule](/design/threat-model/) from the threat model — an old client cannot strip-and-resign a newer sidecar.
- The signature covers every byte including `_unknown`, so stripping unknown fields invalidates the signature and is detectable.
- A schema bump is a coordinated change; per [Versioning — Album Protocol Version Pinning](/design/versioning/#album-protocol-version-pinning), an album's pinned protocol version constrains which sidecar schemas may be written into it.

### Add-id Binding

`add_id` is the tuple `(device_id: UUIDv4, monotonic_counter: u64)`, where `monotonic_counter` is incremented per-device per-(asset, OR-set) pair. Every OR-set add carries an `add_id`; every OR-set remove targets a specific `add_id`. A remove that names an `add_id` the receiver has never observed an add for is **rejected**, not silently no-op — preventing the "remove an element you never added" attack noted in the [Threat Model](/design/threat-model/).

## Identifiers

The three identifying fields defined inside the sidecar schema are subject to the [Privacy on Export](#privacy-on-export) rules below when an asset crosses a trust boundary.

- **Camera identifier (`camera_id`).** Model ID of the device plus a unique identifier for the specific device (e.g. serial number). Useful for grouping shots from the same physical camera across libraries.
- **Device identifier (`device_id`).** UUIDv4 generated on the original importing device. Useful for provenance.
- **Session ID (`session_id`).** Identifies the authenticated session in which the asset was imported. Defined in [Session Management](/design/authentication/#session-id).

## Privacy on Export

The identifiers above and several other metadata fields are **fingerprinting surface** if they leave the user's trust boundary unredacted: a camera serial uniquely links every photo to one physical device, and precise GPS reveals home addresses. When an asset crosses a boundary, Capsule strips these fields by default and only includes them on explicit opt-in.

A boundary crossing is any of:

- A **share link** is generated for a non-member of the album.
- An **external backup** is exported to media the user will hand off (e.g. cloud storage shared with someone else, a physical drive given to a friend).
- A **federated peer** outside the owning user's home server fetches the asset (see [Federation](/design/federation/)).

When the boundary is crossed, the following fields are stripped from the exported metadata blob unless the user has explicitly opted in to retain them:

| Field                                                   | Default on export                         | Opt-in retains |
| ------------------------------------------------------- | ----------------------------------------- | -------------- |
| Camera serial number                                    | Stripped                                  | Full value     |
| Device identifier (UUIDv4)                              | Stripped                                  | Full value     |
| Session ID                                              | Stripped                                  | Full value     |
| GPS coordinates                                         | Truncated to city-level precision (~1 km) | Full precision |
| Personal contact tags (faces matched to a known person) | Stripped                                  | Retained       |

Stripping happens at the moment of export — the encrypted sidecar inside the user's library is untouched, so the user does not lose the data locally. Retention opt-in is per-export, not a sticky account setting, to prevent foot-guns where a user opts in once and forgets.

Capsule's *own* devices syncing the *same user's* library do **not** trigger this redaction — that is intra-trust, not a boundary crossing.

## Collaborative Metadata

User-editable metadata on a shared album — tags, captions, ratings — can be edited concurrently on different devices, including offline. To make these merges deterministic, such fields are modelled as CRDTs:

- **Tags:** an OR-set (observed-remove set) with explicit [`add_id` binding](#add-id-binding), so a tag added on one device and removed on another converge predictably, and a remove that targets an unknown `add_id` is rejected rather than treated as a no-op.
- **Single-value fields** (`caption_lww`, `rating_lww`): last-writer-wins registers keyed by a signed timestamp and the writing `device_id` as the lexicographic tiebreaker.

### Surfacing Concurrent Edits

A plain LWW register loses one side of a tied edit silently — a real problem when two people caption the same photo from different devices within seconds. Capsule keeps the most recent value as authoritative *and* preserves the displaced ones:

- The losing value of every concurrent caption edit lands in `superseded_captions`, capped at 16 entries (oldest evicted). Each entry carries who wrote it and when, so the UI can surface a "this caption replaced another" hint and let the user restore the earlier value.
- Ratings are unambiguous numerically; they do not need a superseded log.

This converts a silent-data-loss damage vector (a buggy client clobbering another device's edit) into an explicit, recoverable surface. See [Threat Model — Forbidden Client Behaviors](/design/threat-model/) for the corresponding rule that clients must never strip `superseded_captions`.

### How Operations Travel

We encrypt the **operations**, not the resulting state. Merges are then commutative and associative, so order of arrival does not matter and a peer replaying a stale operation cannot corrupt current state. The operation log reconciles into the canonical CBOR sidecar, which remains the source of truth (see [Core Principles](/design/principles/) — recovery-first).

Each operation carries the same `prior_provenance_hash` chain link as any [lifecycle action](/design/authorization/#asset-lifecycle), so a metadata-update is provenance-tracked exactly like a create or delete.

Album *membership* is deliberately **not** a CRDT here — it is driven by MLS proposals and commits (see [Group Membership](/design/cryptography/#group-membership)), which already resolve concurrent changes.

This LWW/OR-set approach is intentionally simpler than a full event-graph with state resolution: photo metadata does not need it, and the extra machinery would not be functionally justified.

## Tag Provenance and Namespacing

User tags and AI-suggested tags live in **structurally separate OR-sets** (`tags_user` and `tags_ai` in the [sidecar schema](#sidecar-schema-v1)). The separation is structural, not policy:

- An AI tag can never overwrite a user tag and vice versa — they are different fields, so the question does not arise. A hallucinating model cannot pollute user intent.
- Every `tags_ai` entry carries `model_id` and `model_version` (see [ML Models](/design/ml-models/)). When the canonical model for that slot changes, AI tags from the old model are flagged as stale; cross-model semantic comparison is forbidden (see [Threat Model — Client-Side Validation Invariants](/design/threat-model/)).
- A user can **promote** an AI tag — explicit user action copies the entry to `tags_user` (with a fresh user-scoped `add_id`) and may optionally remove it from `tags_ai`. Promotion is a signed lifecycle operation; never automatic.
- A user can **dismiss** an AI tag — an OR-set remove on `tags_ai` keyed by the original `add_id`.

The same dual-namespace structure applies to any future ML-derived metadata field that overlays a user-editable one (face labels, location guesses, etc.). The owner doc for the model is [ML Models](/design/ml-models/); the storage shape is owned here.

## Geolocation

Most modern camera devices record geolocation data. This is almost universally in **WGS-84 (Earth Coordinates)**. However, mapping data in China (perhaps there are also other countries) use obfuscated coordinates, namely:

- GCJ-02 (Mars Coordinates): The obfuscated coordinate system mandated by the Chinese government for national security. All authorized maps inside mainland China (AMap/Gaode, Tencent Maps, Apple Maps via AMap) use this.  
- BD-09 (Baidu Coordinates): Baidu Maps takes GCJ-02 and applies a second layer of obfuscation. You only need to worry about this if you specifically use the Baidu Maps SDK.

While annoying, we can translate WGS-84 coordinates into the obfuscated coordinates with a deterministic algorithm before plotting on maps. Capsule does this strictly on the client-side with the capability found in `capsule-core`.

### Mapping Providers

These are the recommended mapping providers for all scenarios:

- All Apple devices: Apple Maps (uses AMap data in China so it works globally)
- Web clients in China: AMap (Gaode) JavaScript API
- Web clients outside of China: Google Maps JavaScript API
- All non-Apple devices in China: AMap/Gaode (Tencent Maps is also fine but AMap has better support for geolocation and POI search)
- All non-Apple devices outside China: Google Maps (this is the most robust and developer-friendly provider).
