---
title: Metadata
description: The CBOR sidecar schema v1, the CRDT semantics for collaborative metadata, identifiers, and geolocation
---

The CBOR sidecar is the canonical, plaintext-local-only metadata record for every asset (see [Filesystem — Client](/design/filesystem/client/)). It is **self-describing**: field 0 carries the schema version so any reader can detect a schema it does not implement *before* parsing the rest. Versioning the schema in-band is what prevents a faulty or old client from corrupting state with a partial parse.

This doc is the **single source of truth** for the CBOR sidecar schema. The schema below — every field, type, and ordering rule — is the contract every implementation must conform to byte-for-byte (else cross-peer signatures break). Per the [SSoT rule](/design/principles/#single-source-of-truth), other docs reference fields here by name and never re-declare them.

All metadata processing lives in `capsule-core::metadata` (extraction, filtering, querying) and `capsule-core::sidecar` (encoding, signing, schema versioning). Implementation is in Rust and exposed to all native clients via FFI from `capsule-core` — the I/O is handled natively to minimize FFI surface.

## Sidecar Schema v1

```rust
SidecarV1 {
  sidecar_schema:        u16,             // FIELD 0 — readable before parsing the rest. Currently 1.
  crypto_suite_id:       u16,             // matches the asset's manifest; see Cryptography
  uuid:                  UUIDv7,
  hash:                  bytes,           // canonical plaintext digest; algorithm + length fixed by crypto_suite_id (see Primitives)
  capture_timestamp:     RFC3339,
  import_timestamp:      RFC3339,
  content_type:          String,          // closed enum per protocol_version
  dimensions:            Option<{ width: u32, height: u32 }>,

  // display placeholder — image-derived, lives inside this encrypted sidecar (see Thumbnails — LQIP)
  lqip:                  Option<{ chromahash: bytes, format_version: u16, dominant_color: [u8; 3] }>,

  // collaborative metadata (see Collaborative Metadata below)
  tags_user:             OR_set<(tag: String, add_id)>,
  tags_ai:               OR_set<(tag: String, add_id, model_id: String, model_version: String)>,
  caption_lww:           Option<{ value: String, ts: RFC3339, by: device_id }>,
  superseded_captions:   Vec<{ value: String, written_by: device_id, ts: RFC3339 }>,  // bounded ≤ 16
  rating_lww:            Option<{ value: u8, ts: RFC3339, by: device_id }>,

  // organization — stack grouping; StackMembership shape owned by Asset Organization
  stack_membership:      Option<StackMembership>,

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

### Canonical CBOR Encoding

The sidecar — and the [encrypted metadata blob](/design/cryptography/encryption/#metadata-encryption) whose plaintext is this same CBOR document — must serialize **byte-identically across every implementation and language**: the bytes are what the [signed manifest](/design/cryptography/provenance/#asset-manifest) and content hash commit to, so one divergent byte makes an honest sidecar look forged to another platform or [federated](/design/federation/) peer. The canonical rules are RFC 8949 §4.2 deterministic encoding, normative here:

- **Definite-length encoding only** — no indefinite-length maps, arrays, text strings, or byte strings.
- **Shortest-form integers** — the smallest of the 1/2/4/8-byte encodings that represents the value.
- **Map keys sorted by the bytewise lexicographic order of their *encoded* form, with no duplicate keys.** This ordering governs *every* map, including `_unknown` — unknown keys are re-sorted into the same canonical order on write, so a round-trip through any conformant client is byte-stable and the signature (which covers `_unknown`) still verifies.
- **Floats** in the shortest IEEE-754 form (16/32/64-bit) that round-trips the value exactly; the canonical quiet NaN for NaN. Capsule avoids floats in signed structures where an integer or string suffices.
- **Field 0** (`sidecar_schema`) sorts first under the rule above, so a reader reads the schema version before parsing the rest.

Every implementation — the Rust `capsule-core::sidecar` encoder and any FFI consumer — MUST emit identical bytes for the same document, enforced as a **blocking cross-language conformance gate** against shared **known-answer vectors** committed in `capsule-core::sidecar` (the same fixtures [Encryption](/design/cryptography/encryption/#metadata-blob-wire-format) tests against): a consumer that drifts cannot ship, because its signatures would not verify across peers.

### Local and Server Metadata Equivalence

The plaintext of the server's [encrypted metadata blob](/design/cryptography/encryption/#metadata-encryption) *is* this signed `SidecarV1` — the same canonical CBOR document the client stores at `media/{uuid}.cbor`. Two facts bind the local copy to what the server exposes, so the two can never silently diverge:

- The asset's [signed manifest](/design/cryptography/provenance/#asset-manifest) commits to `metadata_blob_hash`, the content address of the current encrypted metadata blob, on every `create`, `replace`, and `metadata-update`. Both manifest signatures cover it, so the metadata bytes the server holds and exposes are signature-bound to the asset.
- The sidecar carries its own hybrid signature over every byte (including `_unknown`). A client that decrypts the metadata blob recomputes this canonical CBOR and **MUST** find it byte-identical to the locally-stored signed sidecar, and the blob's content hash **MUST** equal the manifest's `metadata_blob_hash`.

A client therefore never persists a sidecar that does not round-trip to the committed `metadata_blob_hash`, and a server can expose only the exact metadata bytes the originating client encrypted. The matching client-side check is a [client-side validation invariant](/design/threat-model/validation/#client-side-validation-invariants); the no-key server enforces the blob-hash match structurally as [invariant 25](/design/threat-model/validation/#server-side-validation-invariants).

### Add-id Binding

`add_id` is the tuple `(device_id: UUIDv4, monotonic_counter: u64)`, where `monotonic_counter` is incremented per-device per-(asset, OR-set) pair. Every OR-set add carries an `add_id`; every OR-set remove targets a specific `add_id`. A remove that names an `add_id` the receiver has never observed an add for is **rejected**, not silently no-op — preventing the "remove an element you never added" attack noted in the [Threat Model](/design/threat-model/scenarios/).

**Counter durability across restarts.** A `monotonic_counter` must never repeat for a given `(device_id, asset, OR-set)`: a reused `add_id` would alias two distinct adds, so removing one would silently delete the other and break OR-set convergence. The counter is persisted in the local [index](/design/filesystem/client/#desktop-library-layout), and on client restart or reinstall it is **reseeded to one past the maximum `add_id.counter` this device has ever issued**, recovered from the signed sidecars themselves (a device's own past `add_id`s are durably recorded in the sidecars it wrote). An add lost to a crash *before* its sidecar was persisted was never observed by any peer, so its counter may be safely reused — correctness depends only on never reusing a counter that ever reached a written sidecar. A counter is reset to zero only when the device can prove it has issued nothing — i.e. no sidecar bears its `device_id`. This makes the counter monotonic over the lifetime of a `device_id`, not merely within one process.

## Identifiers

The three identifying fields defined inside the sidecar schema are subject to the [Privacy on Export](#privacy-on-export) rules below when an asset crosses a trust boundary.

- **Camera identifier (`camera_id`).** Model ID of the device plus a unique identifier for the specific device (e.g. serial number). Useful for grouping shots from the same physical camera across libraries.
- **Device identifier (`device_id`).** UUIDv4 generated on the original importing device. Useful for provenance.
- **Session ID (`session_id`).** Identifies the authenticated session in which the asset was imported. Defined in [Session Management](/design/authentication/#session-id).

## Privacy on Export

The identifiers above and several other metadata fields are **fingerprinting surface** if they leave the user's trust boundary unredacted: a camera serial uniquely links every photo to one physical device, and precise GPS reveals home addresses. When an asset crosses a boundary, Capsule strips these fields by default and only includes them on explicit opt-in.

A boundary crossing is any of:

- A **[share link](/design/share-links/)** is generated for a non-member of the album.
- An **external backup** is exported to media the user will hand off (e.g. cloud storage shared with someone else, a physical drive given to a friend).
- A **federated peer** outside the owning user's home server fetches the asset (see [Federation](/design/federation/)).

When the boundary is crossed, the following fields are stripped from the exported metadata blob unless the user has explicitly opted in to retain them:

| Field                                                   | Default on export                         | Opt-in retains |
| ------------------------------------------------------- | ----------------------------------------- | -------------- |
| Camera serial number                                    | Stripped                                  | Full value     |
| Device identifier (UUIDv4)                              | Stripped                                  | Full value     |
| Session ID                                              | Stripped                                  | Full value     |
| GPS coordinates                                         | Rounded to 2 decimal places (≈1 km) | Full precision |
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

This converts a silent-data-loss damage vector (a buggy client clobbering another device's edit) into an explicit, recoverable surface. See [Threat Model — Forbidden Client Behaviors](/design/threat-model/schema-rules/#forbidden-client-behaviors) for the corresponding rule that clients must never strip `superseded_captions`.

### How Operations Travel

We encrypt the **operations**, not the resulting state. Merges are then commutative and associative, so order of arrival does not matter and a peer replaying a stale operation cannot corrupt current state. The operation log reconciles into the canonical CBOR sidecar, which remains the source of truth (see [Core Principles](/design/principles/) — recovery-first).

Each operation carries the same `prior_provenance_hash` chain link as any [lifecycle action](/design/authorization/#the-closed-action-set), so a metadata-update is provenance-tracked exactly like a create or delete.

Album *membership* is deliberately **not** a CRDT here — it is driven by MLS proposals and commits (see [Cryptography — MLS](/design/cryptography/mls/)), which already resolve concurrent changes.

The same encrypted-operation path also carries the per-owner **library-settings document** — [smart-album](/design/organization/#system--smart-albums-views) definitions (predicate + display name) and similar client-authored organizational state — synced and merged across devices like any other collaborative metadata, and never legible to the server. (The [default-album](/design/organization/#the-default-album) *designation* is separate: a non-secret server-side owner pointer, not part of this encrypted document.)

This LWW/OR-set approach is intentionally simpler than a full event-graph with state resolution: photo metadata does not need it, and the extra machinery would not be functionally justified.

## Tag Provenance and Namespacing

User tags and AI-suggested tags live in **structurally separate OR-sets** (`tags_user` and `tags_ai` in the [sidecar schema](#sidecar-schema-v1)). The separation is structural, not policy:

- An AI tag can never overwrite a user tag and vice versa — they are different fields, so the question does not arise. A hallucinating model cannot pollute user intent.
- Every `tags_ai` entry carries `model_id` and `model_version` (see [AI — Embedding Provenance](/design/ai/#embedding-provenance)). When the canonical model for that slot changes, AI tags from the old model are flagged as stale; cross-model semantic comparison is forbidden (see [Threat Model — Client-Side Validation Invariants](/design/threat-model/validation/#client-side-validation-invariants)).
- A user can **promote** an AI tag — explicit user action copies the entry to `tags_user` (with a fresh user-scoped `add_id`) and may optionally remove it from `tags_ai`. Promotion is a signed lifecycle operation; never automatic.
- A user can **dismiss** an AI tag — an OR-set remove on `tags_ai` keyed by the original `add_id`.

The same dual-namespace structure applies to any future ML-derived metadata field that overlays a user-editable one (face labels, location guesses, etc.). The owner doc for the model is [AI/ML Integrations](/design/ai/); the storage shape is owned here.

## Geolocation

GPS is stored canonically in **WGS-84** (`gps.lat` / `gps.lon`), the near-universal camera format. Some jurisdictions mandate obfuscated coordinates for display — notably China's **GCJ-02**, and Baidu's **BD-09** (a second obfuscation layer over GCJ-02). Capsule always stores WGS-84 and converts to the required system **deterministically and client-side** (in `capsule-core`) at plot time; the stored coordinate is never the obfuscated one. Per-platform map-provider selection is a client/deployment concern, not part of this schema.

## Validation

The sidecar schema is the contract; validation focuses on serde determinism + CRDT correctness.

- **Canonical CBOR conformance (unit + cross-language).** Encode a fixture sidecar (including a populated `_unknown` map); assert byte-identical output across runs, platforms, and every FFI consumer, matching the shared known-answer vectors for the [canonical ruleset](#canonical-cbor-encoding) — key sort including `_unknown`, shortest-form integers, definite-length only. Re-decode; assert structural equality. This is a **blocking conformance gate**, not advisory.
- **Add-id counter durability (unit).** Issue adds advancing the counter; drop the in-memory counter to simulate a restart/reinstall; reseed from the device's existing sidecars; assert the next `add_id.counter` is strictly greater than every counter the device previously issued — never a reuse.
- **Schema versioning enforcement (unit).** Construct a sidecar with `sidecar_schema = N+1`; load on a reader whose `max_known = N`; assert write-refusal. Construct with `sidecar_schema = N`; assert acceptance.
- **OR-set merge convergence (unit).** Generate add/remove operations from N devices in random order; merge in every permutation; assert byte-identical final state across permutations.
- **Add-id rejection (unit).** Issue a remove with an `add_id` never observed locally; assert rejection (not silent no-op).
- **LWW with superseded capture (unit).** Two devices write captions within milliseconds; merge; assert the winner is the lexicographic-tiebreak chosen, and the loser appears in `superseded_captions`.
- **Privacy-on-export stripping (unit).** Each row of the privacy table is a fixture test: assert the field is stripped by default, retained when opt-in is set, and that the local sidecar is unchanged either way.
- **Local–server metadata equivalence (unit).** Seal a sidecar into a metadata blob; assert that decrypting it is byte-identical to the signed sidecar and that the blob's content hash equals the manifest's `metadata_blob_hash`. Mutate the local sidecar by one byte; assert the round-trip check rejects it rather than persisting a divergent copy.
- **Concurrent-edit reconciliation (smoke).** Two test clients edit the same album offline; merge over MLS; assert convergence with no manual conflict resolution needed.

Cross-module case: metadata edited on device A → synced via server → applied on device B with correct CRDT merge. Bounded E2E surface in [Module Map](/design/module-map/#e2e-test-surface).

## Related

- [Asset Organization](/design/organization/) — albums and stacks that consume the `stack_membership` field.
- [AI/ML Integrations](/design/ai/) — owner of the models behind `tags_ai` and the reserved AI-facet fields.
- [Thumbnails and Previews](/design/thumbnails/) — owner of the LQIP scheme carried in the `lqip` field.
