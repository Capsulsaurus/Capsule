---
title: Metadata
description: The CBOR sidecar schema v1, the CRDT semantics for collaborative metadata, identifiers, and geolocation
status: draft
---

The CBOR sidecar is the canonical, plaintext-local-only metadata record for every asset (see [Filesystem — Client](/design/filesystem/client/)). It is **self-describing**: field 0 carries the schema version so any reader can detect a schema it does not implement *before* parsing the rest. Versioning the schema in-band is what prevents a faulty or old client from corrupting state with a partial parse.

This doc is the **single source of truth** for the CBOR sidecar schema. The schema below — every field, type, and ordering rule — is the contract every implementation must conform to byte-for-byte (else cross-peer signatures break). Per the [SSoT rule](/design/principles/#single-source-of-truth), other docs reference fields here by name and never re-declare them.

Rawshift owns media metadata extraction and normalization. `capsule-core::metadata` maps those outputs into Capsule fields and owns filtering and querying, while `capsule-core::sidecar` owns encoding, signing, and schema versioning. Capsule calls Chromahash **0.7.1** directly; that boundary is deliberately separate from Rawshift. Shared Rust contracts are exposed to native clients through `capsule-core` FFI while platform I/O stays native.

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
  caption_lww:           Option<{ value: String, ts: RFC3339, by: device_id }>,  // value bounded ≤ 4096 bytes
  superseded_captions:   Vec<{ value: String, written_by: device_id, ts: RFC3339 }>,  // bounded ≤ 16
  rating_lww:            Option<{ value: u8, ts: RFC3339, by: device_id }>,

  // organization — stack grouping; StackMembership shape owned by Asset Organization.
  // An LWW register over Option<StackMembership> (leave = a stamped None), wire-absent
  // when never written, so stack edits converge like caption/rating.
  stack_membership:      Lww<Option<StackMembership>>,

  // organization — culling + visibility (semantics owned by Asset Organization).
  // LWW registers, wire-absent when never written (never-flagged / visible).
  cull:                  Lww<CullFlag>,               // pick | neutral | reject
  hidden:                Lww<bool>,

  // identifiers (see Identifiers below; privacy-on-export rules apply)
  camera_id:             Option<{ model: String, serial: String }>,
  device_id:             UUIDv4,
  session_id:            UUIDv7,

  // geolocation (see Geolocation below)
  gps:                   Option<{ lat: f64, lon: f64, source: GpsSource,
                                  datum: GpsDatum /* wire-absent ⇒ wgs84 */ }>,

  // provenance binding — the PRIOR chain head; see Provenance Binding and Sealing Order below
  provenance_chain_hash: Option<[u8; 32]>, // hash of the provenance record PRECEDING the write that seals this
                                           //   sidecar — always equal to that write's manifest
                                           //   prior_provenance_hash; absent only on the initial create

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

### Closed Enum Value Sets

Three sidecar fields are closed enums whose authoritative value sets live here (the blanket closed-enum rule is [Threat Model — Schema Rules](/design/threat-model/schema-rules/); the code mirror is a closed Rust enum in `capsule-core::domain`, and adding a value requires a new, later-dated `protocol_version`):

- **`content_type`** — MIME syntax, exactly **one canonical value per format** (never an alias like `image/jpg`). The v1 set:
  - images: `image/jpeg`, `image/png`, `image/webp`, `image/gif`, `image/tiff`, `image/heic`, `image/avif`, `image/jxl`, `image/x-adobe-dng`
  - video: `video/mp4`, `video/quicktime`, `video/x-matroska`, `video/webm`
- **`gps.source` (`GpsSource`)** — `exif` (written by the capturing device), `manual` (set by the user), `inferred` (client-derived, e.g. from a paired device's location or an ML suggestion). An `inferred` value is written to the canonical `gps` field only on **explicit user confirmation** — the same promotion rule as `tags_ai` → `tags_user`, so an automated guess can never silently overwrite capture truth.
- **`gps.datum` (`GpsDatum`)** — `wgs84 | gcj02`. The coordinate is stored **verbatim in the datum the source supplied**, never converted at rest: GCJ-02 → WGS-84 has no exact inverse, so converting on input would destroy the user's ground truth (the raw-input-is-truth principle). **BD-09 is never a storable datum** — BD-09 input is folded to GCJ-02 at the input edge and stored as `datum = gcj02`. The fold uses the **error-bounded iterative inverse** of the forward transform (refined BD-09 → GCJ-02, sub-meter bound, implemented in-house in `capsule-core::domain::gps_datum` — no crate is adopted): only the forward GCJ-02 → BD-09 direction is closed-form, and a sub-meter bound is far below consumer-GPS noise, so the bounded inverse is accepted rather than refusing BD-09 input (decision 2026-07-12, amending the earlier "closed-form and exact" claim). The field is **wire-absent when `wgs84`**, so every existing sidecar and known-answer vector stays byte-identical; it is an additive optional key within sidecar schema v1 (older v1 readers preserve-and-ignore it per the [request-side Postel rule](/design/threat-model/schema-rules/), no `sidecar_schema` bump — if the implementing slice finds the nested `gps` decoder strict rather than tolerant, its documented fallback is a schema bump). The value set is closed: a third datum requires a new `protocol_version`. `gps` is a single atomic value under CRDT merge — `datum` travels with `lat`/`lon` in one write, so no merge rule changes.

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

### Provenance Binding and Sealing Order

`provenance_chain_hash` binds the sidecar to a specific point in the asset's [provenance chain](/design/cryptography/provenance/#provenance-of-library-modifications). It references the **prior** chain head — the record *preceding* the write that seals this sidecar — never "the latest record": the latest record *is* the write being produced, and its manifest commits to `metadata_blob_hash`, so a sidecar referencing it would have to contain a hash of a structure that contains a hash of the sidecar itself. Referencing the prior head keeps the binding well-founded, and makes the sidecar and manifest mutually checkable: **`sidecar.provenance_chain_hash` MUST equal the sealing manifest's `prior_provenance_hash`** (both absent exactly on `create`), a divergence being quarantined like any round-trip failure.

The sealing order every writer follows:

1. **Fix the prior head** `H` — the current chain head for this asset (`None` on `create`).
2. **Author and sign the sidecar** with `provenance_chain_hash = H`.
3. **Seal** the sidecar into the [metadata blob](/design/cryptography/encryption/#metadata-blob-wire-format); compute its content hash.
4. **Build and sign the manifest** with `prior_provenance_hash = H` and `metadata_blob_hash` from step 3; append it as the new chain head.

### Add-id Binding

`add_id` is the tuple `(device_id: UUIDv4, monotonic_counter: u64)`, where `monotonic_counter` is incremented per-device per-(asset, OR-set) pair. Every OR-set add carries an `add_id`; every OR-set remove targets a specific `add_id`. A remove that names an `add_id` the receiver has never observed an add for is **rejected**, not silently no-op — preventing the "remove an element you never added" attack noted in the [Threat Model](/design/threat-model/scenarios/).

**Counter durability across restarts.** A `monotonic_counter` must never repeat for a given `(device_id, asset, OR-set)`: a reused `add_id` would alias two distinct adds, so removing one would silently delete the other and break OR-set convergence. The counter is persisted in the local [index](/design/filesystem/client/#desktop-library-layout), and on client restart or reinstall it is **reseeded to one past the maximum `add_id.counter` this device has ever issued**, recovered from the signed sidecars themselves (a device's own past `add_id`s are durably recorded in the sidecars it wrote). An add lost to a crash *before* its sidecar was persisted was never observed by any peer, so its counter may be safely reused — correctness depends only on never reusing a counter that ever reached a written sidecar. A counter is reset to zero only when the device can prove it has issued nothing — i.e. no sidecar bears its `device_id`. This makes the counter monotonic over the lifetime of a `device_id`, not merely within one process.

When the device's own sidecars are not held locally (a metadata-only sync scope, or local loss repaired from the server), the reseed source is the same sidecars fetched back from the server — the durable record is the signed sidecar wherever it is held, so the rule is unchanged. And in practice a *reinstalled* device re-enrolls with a **new** `device_id` (device keys are hardware-bound and non-exportable, so the old identity cannot be resumed), which is why the reset-to-zero case is safe: it applies only to genuinely fresh device identities.

## Identifiers

**One canonical asset identity.** The sidecar's `uuid`, the manifest's `file_id`, the provenance chain's and server index's `asset_id`, and the metadata-key-salt's `blob_id` are **the same UUIDv7**, minted once at import and never re-minted for the asset's lifetime. The per-schema names survive for local readability only; they never diverge, and every equality between them may be assumed (and is asserted) by validators. UUID versions across the system: the asset id, `session_id`, `stack_id`, and `import_id` are UUIDv7 (time-ordered); `device_id` is UUIDv4 (unordered — a device id must not leak creation ordering).

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

**Status note.** The strip table is implemented in `capsule_core::metadata::export_policy` and applied **client-side, at the moment a share link is issued** — which is the only place it can be applied, since the server holds no key to the metadata a share serves. The server's complementary guarantee is containment: a share link serves only the content addresses its own record enumerates, so a stripped share cannot be walked sideways into the unstripped blob (slices `S-C4`, `S-C50`). Together these are the one export surface v1 ships, mandatory and with no opt-out. The external-backup handoff crossing waits on a client file-export command, which is post-v1; federated peers receive ciphertext, so their strip applies when a share boundary is crossed, not on the pull itself.

## Collaborative Metadata

User-editable metadata on a shared album — tags, captions, ratings — can be edited concurrently on different devices, including offline. To make these merges deterministic, such fields are modelled as CRDTs:

- **Tags:** an OR-set (observed-remove set) with explicit [`add_id` binding](#add-id-binding), so a tag added on one device and removed on another converge predictably, and a remove that targets an unknown `add_id` is rejected rather than treated as a no-op.
- **Single-value fields** (`caption_lww`, `rating_lww`, `stack_membership`, `cull`, `hidden`): last-writer-wins registers keyed by a signed timestamp and the writing `device_id` as the lexicographic tiebreaker. `stack_membership`'s value domain includes "no membership" (a stamped `None`), so joining, moving between, and leaving stacks are all the same LWW write and converge identically.

### Surfacing Concurrent Edits

A plain LWW register loses one side of a tied edit silently — a real problem when two people caption the same photo from different devices within seconds. Capsule keeps the most recent value as authoritative *and* preserves the displaced ones:

- The losing value of every concurrent caption edit lands in `superseded_captions`, capped at 16 entries (oldest evicted). Each entry carries who wrote it and when, so the UI can surface a "this caption replaced another" hint and let the user restore the earlier value.
- Ratings are unambiguous numerically; they do not need a superseded log.

This converts a silent-data-loss damage vector (a buggy client clobbering another device's edit) into an explicit, recoverable surface. See [Threat Model — Forbidden Client Behaviors](/design/threat-model/schema-rules/#forbidden-client-behaviors) for the corresponding rule that clients must never strip `superseded_captions`.

### How Operations Travel

We encrypt the **operations**, not the resulting state. Merges are then commutative and associative, so order of arrival does not matter and a peer replaying a stale operation cannot corrupt current state. The operation log reconciles into the canonical CBOR sidecar, which remains the source of truth (see [Core Principles](/design/principles/) — recovery-first). Operations name **known fields only** — an operation targeting a field the receiver does not know is from a newer schema and is version-gated like any forward sidecar — so reconciliation rewrites only the CRDT fields it understands and re-emits `_unknown` verbatim from the stored sidecar bytes; the byte-fidelity of unknown fields survives the op path exactly as it survives a whole-sidecar rewrite.

Each operation carries the same `prior_provenance_hash` chain link as any [lifecycle action](/design/authorization/#the-closed-action-set), so a metadata-update is provenance-tracked exactly like a create or delete.

The same encrypted-operation path also carries each album's [album-group assertion](/design/federation/#the-album-group-assertion) (schema owned by Federation) and the per-owner **library-settings document** — [smart-album](/design/organization/#system--smart-albums-views) definitions (predicate + display name) and similar client-authored organizational state — synced and merged across devices like any other collaborative metadata, and never legible to the server. Its concrete schema is [The Library-Settings Document](#the-library-settings-document) below. (The [default-album](/design/organization/#the-default-album) *designation* is separate: a non-secret server-side owner pointer, not part of this encrypted document.)

### The Library-Settings Document

**Status: designed, implementation deferred post-v1** (with the [OGK](/design/cryptography/keys/#owner-group-keys-ogks) cluster it is keyed under — decision 2026-07-12). The schema below is normative as written; the Validation bullets are the future implementation slice's acceptance contract.

The per-owner state the [operation path](#how-operations-travel) carries above is one concrete, versioned CBOR document — the **library-settings document**. This doc owns its *envelope, versioning, keying, and merge discipline*; the schema of each section it carries is owned by the domain that owns that surface, linked below and never restated here.

**Logical shape.** The reconciled document is:

```rust
LibrarySettingsV1 {
  settings_schema:          u16,  // FIELD 0 — readable before parsing the rest. Currently 1.
  smart_albums:             Map<UUIDv7,     Lww<Option<SmartAlbumDefinition>>>,  // schema owned by Organization
  scope_overrides:          Map<scope_id,   Lww<Option<album_id>>>,             // rows + grammar owned by Organization — Scope Grammar
  source_kind_defaults:     Map<SourceKind, Lww<Option<album_id>>>,             // per-source-kind default rows (Organization — Scope Grammar)
  aggregated_album_covers:  Map<group_id,   Lww<Option<asset_id>>>,             // per-viewer cover; semantics owned by Federation
  _unknown:                 Map,   // unknown CBOR keys preserved verbatim, never executed
}
```

Every section is a **keyed map of LWW registers over `Option<value>`**: a value sets the entry, a stamped `None` is a tombstone (deleting a smart album, clearing a scope mapping, resetting a cover to its fallback), so every mutation is one LWW write and converges exactly like [`stack_membership`](#collaborative-metadata). Unlike a sidecar the document carries **no** `signature` field of its own — it is not content-addressed by a manifest; each mutation is instead a signed operation on the operation path (below), and the reconciled document is the local source of truth those operations rebuild.

- **`smart_albums`** — each value is a [`SmartAlbumDefinition`](/design/organization/#smart-album-definition-schema) (predicate + display name); the closed predicate grammar and its `predicate_schema` versioning are owned by [Organization](/design/organization/#smart-album-definition-schema).
- **`scope_overrides` / `source_kind_defaults`** — the `scope_id → album_id` and per-`SourceKind` default rows whose grammar and resolution order are owned by [Organization — Scope Grammar](/design/organization/#scope-grammar-local-source--album-mapping). This document is only their transport and storage.
- **`aggregated_album_covers`** — a per-viewer `group_id → asset_id` choice for an [aggregated album](/design/federation/#federated-shared-albums-aggregated-albums); `None`/absent falls back to the newest constituent asset. Deliberately per-viewer, never shared state — the rendering semantics are owned by [Federation](/design/federation/#membership-and-rendering).

**Keying (per-owner, not per-album).** The document is readable by all of the owner's devices and owner-set members and by **no** album co-member, so it is keyed under the [Owner Group Key](/design/cryptography/keys/#owner-group-keys-ogks) — *not* an AMK. Its logical identity `settings_doc_id` is derived from the [account master key](/design/cryptography/keys/#key-chain) under a dedicated `info` label — an *identifier*, not a key, the same discipline as the [default album](/design/organization/#the-default-album)'s id — so any enrolled device recomputes it from the master key alone, even after recovery. Operations are sealed with the standalone-AEAD [metadata-blob wire format](/design/cryptography/encryption/#metadata-blob-wire-format), with two substitutions this doc pins: the key is `HKDF-SHA512(ikm = OGK_v{n}, salt = settings_doc_id || nonce, info = "library-settings/v1", length = 32)`, and a server-visible `ogk_version` (big-endian `u32`) precedes the nonce to select the OGK epoch — the OGK analogue of a manifest's `amk_version`. The fresh per-operation `nonce` folded into the salt re-rolls **both** key and nonce on every write (a reused `nonce` is refused), exactly as for a [metadata rewrite](/design/cryptography/encryption/#metadata-blob-wire-format). A member removed from the owner set derives no future OGK epoch and is dropped from future reads/writes by [OGK revocation](/design/cryptography/keys/#owner-group-keys-ogks); a rewrite always uses the current epoch and is never re-encrypted on an epoch bump alone.

**Encoding & versioning.** The plaintext is [canonical CBOR](#canonical-cbor-encoding) — the same byte-exact ruleset as the sidecar, so `settings_schema` sorts first as CBOR field 0 and `_unknown` keys re-sort into canonical order and survive round-trips. Versioning mirrors the [sidecar rules](#schema-versioning-rules):

- A client whose `max_known_settings_schema < settings_schema` **refuses to write** the document (refuse-by-default); reading is allowed only in explicitly opted-in read-only mode. An old client cannot strip-and-rewrite a newer document.
- Section value schemas version **independently and forward-compatibly**: a `SmartAlbumDefinition` carries its own `predicate_schema` (owned by Organization). A definition whose `predicate_schema` exceeds the reader's maximum is **preserved verbatim and never evaluated** — surfaced as "created by a newer app version," never stripped — so it survives sync round-trips intact (the [never-strip rule](/design/threat-model/schema-rules/#forbidden-client-behaviors)). Unknown top-level keys land in `_unknown` under the same rule.

**How it travels & merges.** The document rides the [operation path](#how-operations-travel): each mutation is a device-signed, OGK-sealed operation naming `(section, key, stamped value)` and carrying the `prior_provenance_hash` chain link, so a settings edit is provenance-tracked like a `metadata-update`. Merge is **field-wise** — each section map merges its entries independently as LWW registers keyed by `(ts, device_id)`, with no cross-field invariants, so any arrival order converges (the [grouping-convergence requirement](#grouping-convergence-requirement)). The server stores only ciphertext plus the server-visible `ogk_version` and never learns any section's meaning.

### Grouping Convergence (Requirement)

**Every grouping operation — manual or automatic/AI — is idempotent and order-independent.** Applying the same operation twice, or applying a set of operations in any arrival order, yields the same state. This is a requirement satisfied *by construction*, not by convention; each grouping structure names its mechanism:

| Structure | Mechanism | Why it converges |
| --- | --- | --- |
| Tags (`tags_user` / `tags_ai`) | OR-set | Add/remove keyed by `add_id`; merges commutative, associative, idempotent |
| Caption / rating / `stack_membership` / `cull` / `hidden` | LWW register | Total order on `(ts, device_id)`; replay of any op is a no-op |
| Smart albums, people clusters, [aggregated federated albums](/design/federation/) | Computed views | Nothing stored — membership is a deterministic function of inputs; recomputation is idempotent by definition ([views](/design/organization/#system--smart-albums-views), [AI determinism](/design/ai/#ai-output-containment)) |
| Container-album membership | Single home + ordered lifecycle ops | Exactly one container per asset; a move is a signed lifecycle action whose replay finds the target state already in place and no-ops ([Organization](/design/organization/#container-albums)); concurrency is resolved by MLS commit order, below |

Album *membership* is deliberately **not** a CRDT here — it is driven by MLS proposals and commits (see [Cryptography — MLS](/design/cryptography/mls/)), which already resolve concurrent changes into one total order.

This LWW/OR-set approach is intentionally simpler than a full event-graph with state resolution: photo metadata does not need it, and the extra machinery would not be functionally justified.

## Tag Provenance and Namespacing

User tags and AI-suggested tags live in **structurally separate OR-sets** (`tags_user` and `tags_ai` in the [sidecar schema](#sidecar-schema-v1)). The separation is structural, not policy:

- An AI tag can never overwrite a user tag and vice versa — they are different fields, so the question does not arise. A hallucinating model cannot pollute user intent.
- Every `tags_ai` entry carries `model_id` and `model_version` (see [AI — Embedding Provenance](/design/ai/#embedding-provenance)). When the canonical model for that slot changes, AI tags from the old model are flagged as stale; cross-model semantic comparison is forbidden (see [Threat Model — Client-Side Validation Invariants](/design/threat-model/validation/#client-side-validation-invariants)).
- A user can **promote** an AI tag — explicit user action copies the entry to `tags_user` (with a fresh user-scoped `add_id`) and may optionally remove it from `tags_ai`. Promotion is a signed lifecycle operation; never automatic.
- A user can **dismiss** an AI tag — an OR-set remove on `tags_ai` keyed by the original `add_id`.

The same dual-namespace structure applies to any future ML-derived metadata field that overlays a user-editable one (face labels, location guesses, etc.). The owner doc for the model is [AI/ML Integrations](/design/ai/); the storage shape is owned here.

## Geolocation

GPS is stored in **the coordinate datum the source supplied**, tagged by [`gps.datum`](#closed-enum-value-sets) — WGS-84 (the near-universal camera format, and the wire-absent default) or GCJ-02 (China's legally mandated obfuscated datum, which user-entered coordinates from Chinese maps arrive in). The stored value is never converted at rest; conversion between datums for display or search happens **deterministically and client-side** (in `capsule-core`), with the lossy GCJ-02 → WGS-84 inverse marked approximate wherever it surfaces; those display-side conversions are unscheduled and are not part of `S-A7`/`S-A8`. Baidu's **BD-09** (a second obfuscation layer over GCJ-02) exists only at the input edge: it is folded to GCJ-02 on entry — via the [error-bounded refined inverse](#closed-enum-value-sets), sub-meter — and never stored. Per-platform map-provider selection is a client/deployment concern, not part of this schema. Implementation is slice `S-A7`; the fold flip from the earlier refusal contract is `S-A8`.

## Validation

The sidecar schema is the contract; validation focuses on serde determinism + CRDT correctness.

- **Canonical CBOR conformance (unit + cross-language).** Encode a fixture sidecar (including a populated `_unknown` map); assert byte-identical output across runs, platforms, and every FFI consumer, matching the shared known-answer vectors for the [canonical ruleset](#canonical-cbor-encoding) — key sort including `_unknown`, shortest-form integers, definite-length only. Re-decode; assert structural equality. This is a **blocking conformance gate**, not advisory.
- **Add-id counter durability (unit).** Issue adds advancing the counter; drop the in-memory counter to simulate a restart/reinstall; reseed from the device's existing sidecars; assert the next `add_id.counter` is strictly greater than every counter the device previously issued — never a reuse.
- **Schema versioning enforcement (unit).** Construct a sidecar with `sidecar_schema = N+1`; load on a reader whose `max_known = N`; assert write-refusal. Construct with `sidecar_schema = N`; assert acceptance.
- **OR-set merge convergence (unit).** Generate add/remove operations from N devices in random order; merge in every permutation; assert byte-identical final state across permutations.
- **Add-id rejection (unit).** Issue a remove with an `add_id` never observed locally; assert rejection (not silent no-op).
- **LWW with superseded capture (unit).** Two devices write captions within milliseconds; merge; assert the winner is the lexicographic-tiebreak chosen, and the loser appears in `superseded_captions`.
- **Privacy-on-export stripping (unit).** Each row of the privacy table is a fixture test: assert the field is stripped by default, retained when opt-in is set, and that the local sidecar is unchanged either way.
- **Datum verbatim storage (unit).** A GCJ-02 input round-trips unconverted with `datum = gcj02`; a BD-09 input asserts the fold to GCJ-02 within the documented sub-meter bound (and deterministically — same input, same output); a WGS-84 write asserts `datum` is wire-absent and the encoded sidecar is byte-identical to the pre-`datum` vector.
- **Local–server metadata equivalence (unit).** Seal a sidecar into a metadata blob; assert that decrypting it is byte-identical to the signed sidecar and that the blob's content hash equals the manifest's `metadata_blob_hash`. Mutate the local sidecar by one byte; assert the round-trip check rejects it rather than persisting a divergent copy.
- **Concurrent-edit reconciliation (smoke).** Two test clients edit the same album offline; merge over MLS; assert convergence with no manual conflict resolution needed.
- **Library-settings field-0 + refuse-by-default (unit).** Construct a [library-settings document](#the-library-settings-document) with `settings_schema = N+1`; load on a `max_known = N` client; assert write-refusal; assert `settings_schema` decodes as CBOR field 0 before the rest of the map is parsed.
- **Library-settings OGK keying (unit).** Seal the document under the owner's OGK; assert every owner-set device decrypts it and an album co-member *not* in the owner set cannot; assert a member removed from the owner set cannot derive the post-removal `ogk_version`.
- **`settings_doc_id` derivation determinism (unit).** Recompute `settings_doc_id` from the master key on a second device and after recovery; assert byte-identical, and that the derivation yields an identifier, never a usable key.
- **Library-settings wire-format + rewrite re-roll (unit).** Assert the sealed document matches the standalone-AEAD wire format against the shared [canonical-CBOR vectors](#canonical-cbor-encoding); re-seal under the constant `settings_doc_id` and assert both derived key and `nonce` change and a reused `nonce` is refused.
- **Library-settings field-wise merge convergence (unit).** Generate concurrent edits and delete-tombstones to `smart_albums`, `scope_overrides`, and `aggregated_album_covers` from N devices; merge in every permutation; assert byte-identical reconciled state.

Cross-module case: metadata edited on device A → synced via server → applied on device B with correct CRDT merge. Bounded E2E surface in [Module Map](/design/module-map/#e2e-test-surface).

## Related

- [Asset Organization](/design/organization/) — albums and stacks that consume the `stack_membership` field.
- [AI/ML Integrations](/design/ai/) — owner of the models behind `tags_ai` and the reserved AI-facet fields.
- [Thumbnails and Previews](/design/thumbnails/) — owner of the LQIP scheme carried in the `lqip` field.
