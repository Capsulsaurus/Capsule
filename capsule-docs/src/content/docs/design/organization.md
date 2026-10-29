---
title: Asset Organization
description: Albums (container and view), default-album resolution, asset stacks, and trash retention
status: draft
---

[Container albums](#container-albums) are the cryptographic unit every asset belongs to; [view albums](#system--smart-albums-views) are derived, key-free presentations. **Stacks**, culling, hiding and trash are metadata-only — they never touch asset bytes. The load-bearing security piece is `retention_until`, signed into the `delete` manifest so a hostile server cannot accelerate purges.

## Albums

- **[Container albums](#container-albums)** — the real cryptographic unit. Every asset belongs to exactly one.
- **[View albums](#system--smart-albums-views)** — computed client-side; hold no keys, own no assets.

### Container Albums

A container album is the **sharing and access-control boundary** and *is* an MLS group. Its keys and membership operations are owned by [Keys](/design/cryptography/keys/) and [MLS](/design/cryptography/mls/); its storage shape by [Filesystem — Server](/design/filesystem/server/). This section owns the interaction surface.

- **Moves are idempotent.** A move is a signed lifecycle action naming `(asset, target album, epoch)`; a replay finds the target state and no-ops; concurrent moves resolve by MLS commit order ([grouping convergence](/design/metadata/#grouping-convergence-requirement)).
- **Roles.** read (AMK), write (AMK + write-tier key) or admin (also admin-tier key), delivered over MLS ([AMKs](/design/cryptography/keys/#album-master-keys-amks)). A role change is an MLS commit and bumps the AMK epoch.
- **Invite and join.** An MLS `Add` for all of the invitee's verified [devices](/design/cryptography/keys/#device-directory); the `Welcome` delivers AMKs per `history_policy` ([History Delivery](/design/cryptography/mls/#history-delivery-for-new-joiners)). Remote users also get a [federation capability](/design/federation/#federation-capabilities); non-account recipients use a [share link](/design/share-links/). Removal is an MLS `Remove` + epoch bump. Pending the MLS implementation.
- **Album policy** (`history_policy`, `protocol_version` pin, default `retention_until`) is fixed at creation and changed only by an [album upgrade ceremony](/design/versioning/#album-upgrade-ceremony).

### The Default Album

Every owner has a nameless, otherwise ordinary **default album** from [first-device enrollment](/design/device-enrollment/#first-device-enrollment) onward, so an import always has a container to land in.

- **Derived id.** Its album id is derived deterministically from the account master key (an identifier, not a key — [Key Chain](/design/cryptography/keys/#key-chain)), so any of the owner's devices can locate it, including after recovery.
- **Designation is a server-side pointer.** The current default is a non-secret `default_album_id` on the owner record ([Filesystem — Server](/design/filesystem/server/#ownership-partitioning-and-quota)), defaulting to the derived album. It is not security-bearing: writes still need album write capability ([invariant 6](/design/threat-model/validation/#server-side-validation-invariants)).
- **Always a container.** `resolve_default_album(context)` never returns a view. The [import planner](/design/import/pipeline/#plan--confirm) uses it when the user picks no album.
- **Undeletable while designated.** The user must repoint first.

#### Scope Grammar (Local Source → Album Mapping)

A **scope** is the canonical identity of an import source:

```rust
Scope {
  platform:    PlatformTag,     // closed enum (shared with device cohorts)
  source_kind: SourceKind,      // closed enum: camera_roll | screenshots | app_collection | folder | watched_dir | removable_volume
  locator:     String,          // canonical, per the table below
}
// scope_id = SHA-256( canonical-CBOR([ "capsule-import-scope/v1", platform, source_kind, locator ]) )
```

`scope_id` is deterministic, so same-platform devices agree. Locators are chosen for stability across reinstall:

| Platform | Source | Canonical locator | Notes |
| --- | --- | --- | --- |
| iOS | camera roll / screenshots / user collection | smart-album subtype name, or the collection's `localIdentifier` | `localIdentifier` is per-device; cross-device only via override rows |
| Android | MediaStore bucket | **relative path** (`DCIM/Camera`) | never `BUCKET_ID` (differs across devices and OS versions) |
| Desktop | folder / watched dir | canonicalized path (symlinks resolved) | |
| Any | removable volume | volume UUID + relative path | stable across mount points |

- **Resolution order** (first match wins): explicit user pick → `scope_id` override row → per-`SourceKind` default row → `default_album_id` pointer → derived default album. The planner records which rule fired.
- **Rows** live in the [library-settings document](/design/metadata/#the-library-settings-document) (`scope_overrides`, `source_kind_defaults`), E2E-encrypted, LWW per key.
- **Unmapped sources ask once**; the answer becomes the scope's override row. Automated imports never invent destinations.

v1 ships only explicit pick → pointer → derived album; the override and default rows are deferred with the library-settings document.

### System & Smart Albums (Views)

Views are computed client-side from the [local index](/design/filesystem/client/#local-index-staleness). A view is **not** an MLS group, holds **no** AMK, owns no assets and is **not** a sharing boundary. The [aggregated federated album](/design/federation/#federated-shared-albums-aggregated-albums) is a view.

- **System albums** — **All** (every visible asset; hence the need for a [default album](#the-default-album)) and [Trash](#recycling).
- **Smart albums** — user-defined predicates over sidecar and AI fields. Membership is **computed, never stored**; editing a definition or asset moves and re-encrypts nothing. Definitions are carried in the [library-settings document](/design/metadata/#the-library-settings-document); this doc owns their shape.

#### Smart-Album Definition Schema

**Deferred post-v1** with the library-settings document; System views ship in v1. Normative as written.

The grammar is closed, versioned and declarative: definitions sync to every device, so they must evaluate identically and carry no code, regex or unbounded input.

```rust
SmartAlbumDefinition {
  smart_album_id:   UUIDv7,
  predicate_schema: u16,              // a later value gates evaluation on older clients
  display_name:     Lww<String>,      // ≤ 256 bytes
  predicate:        Predicate,
  sort:             Option<SortSpec>, // closed key set; default (capture_timestamp, desc)
}

Predicate =                           // depth ≤ 8, ≤ 64 terms total
  | All(Vec<Predicate>)               // AND — empty ⇒ matches every asset
  | Any(Vec<Predicate>)               // OR  — empty ⇒ matches none
  | Not(Box<Predicate>)
  | Term(Term)

Term { field: QueryField, op: Operator, operand: Operand }  // operand shape fixed by (field, op)
```

| Class | Fields | Valid operators | Operand |
| --- | --- | --- | --- |
| temporal | `capture_timestamp`, `import_timestamp` | `before`, `after`, `in_range` | RFC 3339 (half-open `[start, end)` for `in_range`) |
| enum | `content_type`, `media_kind` (image\|video), `gps.datum` | `eq`, `in` | value(s) from the field's closed enum |
| numeric | `rating`, `dimensions.width`, `dimensions.height` | `eq`, `gte`, `lte`, `in_range` | `u32` (`[lo, hi]` for `in_range`) |
| trinary/bool | `cull`, `hidden` | `is` | `pick`\|`neutral`\|`reject`; `true`\|`false` |
| set | `tags_user`, `tags_ai`, `stack_type`, `people_cluster`, `album_id` | `contains`, `contains_any`, `contains_all` | ≤ 64 members, each ≤ 256 bytes |
| presence | `gps`, `camera_id` | `exists` | `bool` |

- **Closed.** Unknown `field`/`op` or a mistyped `operand` is a **structural rejection**, not a tolerated unknown. Extending the grammar needs a later `predicate_schema` (plus a [sidecar-schema](/design/metadata/#schema-versioning-rules) bump for a new sidecar field). A newer `predicate_schema` is preserved verbatim and never evaluated.
- **AI terms.** `tags_ai`/`people_cluster` terms name their `(model_id, model_version)` slot; a stale slot evaluates as excluded ([AI staleness](/design/ai/#ai-output-containment)).
- **Convergence.** The whole definition is one LWW register (a stamped `None` deletes it), so there is no partial-predicate merge. Evaluation is a pure function of `(definition, decryptable assets)` in sorted `asset_id` order.

## Asset Stacking

Stacks group related files (RAW+JPEG, bursts, live photos) via best-effort auto-detection. v1 auto-detects RAW+JPEG (plus XMP sidecar attachment) only; manual stacking works for every type.

**Stacking is metadata-only.** A stack edit writes each member's `stack_membership` LWW register (leave = stamped `None`) and emits a `metadata-update` provenance record per asset. It never deletes, rewrites or merges bytes; "best photo" is just `role = primary`. Multi-asset edits are staged and committed atomically ([Atomic Writes](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery), [Atomicity Invariants](/design/threat-model/validation/#atomicity-invariants)).

### Stack Membership Schema

```rust
StackMembership {
  stack_id:     UUIDv7,
  stack_type:   StackType,    // closed enum per protocol_version
  role:         StackRole,    // primary | member | proxy
  member_index: Option<u32>,  // burst sequence, chapter index
}
```

A new stack type requires a later-dated `protocol_version`; the Rust enum is normative.

### Stack Types

- **Photography & mobile:** RAW + JPEG pair; burst (best photo in front); Live Photo (still + short clip); portrait/depth (image + depth map); smart selection (AI-grouped near-duplicates).
- **Technical & creative:** exposure bracketing (HDR); focus stack; pixel shift; panorama.
- **Video & audio:** proxy/optimized (master + proxy); chaptered video (e.g. `GOPR001.mp4`, `GOPR002.mp4`); dual-system audio (video + external WAV/AIFF).

## Labels

Users attach free-form **labels** at four levels. Every level stores labels as an OR-set with
[`add_id` binding](/design/metadata/#add-id-binding), so concurrent adds and removes from any
number of devices converge deterministically, and every change is a signed `metadata-update`.

| Level | Where the labels live | Readable by |
| --- | --- | --- |
| **Asset** | `tags_user` in the asset's [sidecar](/design/metadata/#sidecar-schema-v1) | the asset's album members |
| **Region** of an image | `regions` in the asset's sidecar, one OR-set per region | the asset's album members |
| **Stack** | the stack's `StackDocV1` | the stack's album members |
| **Album** | the album's `AlbumDocV1` | the album's members |

- **A label** is a Unicode string, NFC-normalized and trimmed, 1–256 bytes, case-preserving. It
  carries no hierarchy and no type; meaning is the user's.
- **User and AI labels never mix.** AI suggestions live only in `tags_ai` and reach a user
  label only by an explicit promote ([Tag Provenance](/design/metadata/#tag-provenance-and-namespacing)).
- **A region** is a circle `{cx, cy, r}` or rectangle `{x, y, w, h}` in coordinates normalized
  to `[0, 1]` over the original's **display orientation** (after EXIF orientation), so a region
  survives every derivative size. A circle's centre is a fraction of width and height, and its
  radius `r` is a fraction of the shorter side, `min(width, height)`, so a circle is round on
  any aspect ratio; a circle or rectangle lies wholly inside the image. Each region has a UUIDv7
  and its own labels OR-set; deleting a region is a stamped `None` in its LWW register.
- **Stack and album records.** `StackDocV1` (keyed by `stack_id`) and `AlbumDocV1` (keyed by
  `album_id`) are signed, versioned documents sealed under the album's current AMK, following
  the same rules as the sidecar: version first, unknown keys preserved under the signature,
  CRDT fields only. `AlbumDocV1` also carries the album's name (LWW), so a restored or newly
  joined device sees it; the name follows the label rule at creation and at every rename.
  On the device the records are plaintext at rest like sidecars
  ([layout](/design/filesystem/client/#desktop-library-layout)). Every member of a stack belongs to the same container album; moving
  one member out removes it from the stack.
- **Privacy.** Region labels can name people, so regions are stripped by default on export and
  from [share links](/design/share-links/), and kept only when the user opts in for that export
  ([Privacy on Export](/design/metadata/#privacy-on-export)); asset, stack and album labels follow the same rule as captions.
- **Search.** Smart-album `contains` predicates over `tags_user` match a label at any level
  attached to the asset: its own, its live regions', its stack's and its album's. A deleted
  region's labels never match, and a stack's labels match only while the asset is a current
  member of that stack.
- **Merge.** Asset, stack and album labels merge as OR-sets. A region's register merges as a
  semilattice: deleting a region is final (its tombstone absorbs any concurrent or later edit;
  a region id is never reused), and two live writes keep the later shape and the union of their
  labels. Album names merge by LWW.
- **Restore.** A backup artifact carries each album's and stack's record sealed under its AMK,
  so a restored device recovers album names and labels rather than showing `Recovered album`.

## Culling

A **trinary flag per asset** — `pick | neutral | reject` — in the sidecar's `cull` LWW register ([schema](/design/metadata/#sidecar-schema-v1)); `neutral` is the wire-absent default. It is independent of the star `rating`.

- **Workflow.** Flagging never touches bytes and is reversible; the only destructive step is moving rejects to [trash](#recycling), soft per retention.
- **Groups.** A stack has no flag of its own; its state is **derived** from members (all-rejected, any-pick, else mixed). Flagging a collapsed stack writes each member atomically like any [stack edit](#asset-stacking).
- **Sync.** Converges like every LWW field ([grouping convergence](/design/metadata/#grouping-convergence-requirement)).

## Hidden Assets

A `hidden` asset (sidecar LWW, wire-absent = visible) is excluded from default views — timeline, search, system views — and appears only in the Hidden view behind the fresh-local-auth gate ([Local Gallery — SR1](/design/local-gallery/)). Hiding is view-layer only — not deletion, not access control; the asset keeps its album and keeps syncing.

## Recycling

Delete defaults to trash (a soft delete, synced as a metadata change). A hard delete happens only when the user empties trash, retention expires, or the user requests immediate deletion. Deletion is a lifecycle action like any other ([Provenance](/design/cryptography/provenance/#provenance-of-library-modifications), [The Closed Action Set](/design/authorization/#the-closed-action-set)).

### Retention Window

`retention_until` is **signed into the `delete` manifest** at delete time, in its server-visible envelope, so the keyless purge worker enforces it against the server's [trusted clock](/design/filesystem/server/#postgresql-what-the-server-knows). Default 30 days, extendable per delete or album policy.

- The server **cannot accelerate** a purge: a hard purge before `retention_until` is rejected; server config is irrelevant.
- The server **cannot delay** a purge beyond a user-issued `trash-restore` or signed shorter-retention re-issue.
- `trash-restore` before `retention_until` recovers the asset and appends a record; the delete manifest stays in the chain.
- A `trash-restore` the server accepts at or after `retention_until`, by the same trusted clock, is refused `409 error.upload.retention_elapsed` and writes nothing, whether or not the purge has run. A purged asset cannot be restored: a restore that reaches the asset after its purge committed is refused the same way, even if the server accepted it just before the window closed. No restore ever meets a purged row, and a refused one leaves the asset in the trash until the purge.

### Deletion State Machine

The client's library owns the whole machine. An app calls its verbs and keeps no retention logic or index write of its own, because a transition that is not a signed record neither syncs nor survives an index rebuild.

| From | Verb | To | Record | Refused when |
| --- | --- | --- | --- | --- |
| live | delete | trashed | signed `delete` carrying `retention_until` | never; a delete of a trashed asset signs nothing and keeps its window |
| trashed | restore | live | signed `trash-restore` | now is at or after `retention_until` |
| trashed | purge | purged | before `retention_until`, a signed shorter-retention `delete` re-issue first | the asset is not trashed: a purge never skips the trash |

- **Judged by the signed window.** Every expiry and restore decision reads the `retention_until` of the asset's latest `delete` record, never how long ago the delete happened. A `delete` with no readable window never expires.
- **Purge.** Removes the original, every derivative and local cache entry, the sidecar, and the index row. The provenance chain stays as the record of the deletion, so a reopen or rebuild never brings the asset back.
- **Index.** The trash columns are a projection of the chain, rebuilt from it.

## Validation

Each rule above is a unit or smoke test; notably, purge is refused before `retention_until` and `trash-restore` at or after it, under a mocked clock, and smart-album evaluation is byte-identical across platforms.
