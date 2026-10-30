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

### Album Order and Recency

Every native app and the CLI list container albums in one order, computed once in `capsule-library`, which the CLI links directly and the native apps reach through the FFI; no app sorts albums on its own. Albums shared by another owner rank in it like the user's own. Smart and aggregated albums rank in it by their members. The system albums (All, Trash) and the Hidden view are listed apart from that order. The web client holds no keys in v1, so it cannot read capture times; it keeps its key-free order from the sync feed until in-browser decrypt lands. The default is **most recent asset first**: an album ranks by the latest [effective](/design/metadata/#as-imported-and-user-values) capture time among its assets that are neither hidden nor trashed. An album with no such asset ranks by its own creation time. Ties are broken by ascending `album_id`.

- **Derived, never stored.** Recency is a query over the [local index](/design/filesystem/client/#local-index-staleness), so imports, moves, deletes, restores and capture-date edits change it with nothing extra to keep in step.
- **Not on the server.** Capture times are inside the encrypted metadata, so the server cannot compute album recency and is never asked to.
- **No viewing history.** Albums record no viewing or opening time, synced or on the server, because that would put viewing habits into metadata that leaves the device. Any state a later "recently viewed" view needs is per-device and local only.

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

### Culling UI

Culling is a high-volume, repetitive pass, so every native client's culling surface is built for speed without a pointer and for use without sight:

- **Keyboard first.** With a hardware keyboard attached, the whole pass works without a pointer: one key each for pick, neutral and reject, next and previous asset, expand and collapse a stack, and the reject sweep. A verdict advances to the next asset. Bindings are discoverable in the platform's usual way (the menu bar on macOS, the hold-⌘ shortcut overlay on iPadOS, an in-app shortcut list on iPhone, the keyboard shortcuts helper on Android).
- **Accessible.** Each verdict is also an accessibility action on the focused asset (VoiceOver and TalkBack custom actions), with no gesture as its only path. The asset's current flag and its position in the pass are announced. Flag state is never shown by colour alone.
- **Audited before it ships.** A culling surface passes each platform's full automated accessibility audit with no exclusions: `performAccessibilityAudit(for: .all)` on Apple platforms, and the Compose UI accessibility checks on Android.

## Hidden Assets

A `hidden` asset (sidecar LWW, wire-absent = visible) is excluded from default views — timeline, search, system views — and appears only in the Hidden view behind the fresh-local-auth gate ([Local Gallery — SR1](/design/local-gallery/)). Hiding is view-layer only — not deletion, not access control; the asset keeps its album and keeps syncing.

## Recycling

Delete defaults to trash (a soft delete, synced as a signed `delete` lifecycle record). A hard delete happens only when the user empties trash, retention expires, or the user requests immediate deletion. Deletion is a lifecycle action like any other ([Provenance](/design/cryptography/provenance/#provenance-of-library-modifications), [The Closed Action Set](/design/authorization/#the-closed-action-set)).

### Deletion State Machine

Deletion is owned entirely by `capsule-library`'s `Workspace` and reaches the apps through the FFI as verbs: delete, restore, purge and list trash. An app holds no deletion or retention logic of its own, and never writes the index row directly. Live and Trashed are derived from the asset's signed chain, never stored on their own, so rebuilding the index reproduces them. Purged is recorded as a tombstone only on the evidence [Sync](/design/sync/#discovering-what-changed) names (the retention window seen closed, or a delete entry marked `purged`) or by this device's own purge. Missing bytes are never read as a purge, because a device whose scope is metadata-only holds no bytes for a Trashed asset either.

```text
Live ──delete──▶ Trashed ──restore (before retention_until)──▶ Live
                    │
                    └──purge (emptied, expired, or immediate)──▶ Purged
```

| From | Verb | To | Record | Refused when |
| --- | --- | --- | --- | --- |
| live | delete | trashed | signed `delete` carrying `retention_until` | never; a delete of a trashed asset signs nothing and keeps its window |
| trashed | restore | live | signed `trash-restore` | now is at or after `retention_until` |
| trashed | purge | purged | before `retention_until`, a signed shorter-retention `delete` re-issue first | the asset is not trashed: a purge never skips the trash |

- **Delete** appends a signed `delete` record carrying `retention_until`, then updates the index from it. A delete that writes only the index is not a delete: it never syncs, and a rebuild brings the asset back.
- **Expiry** is judged by the signed `retention_until`, on the device as on the server, never by when the local row changed. Every expiry and restore decision reads the `retention_until` of the asset's latest `delete` record, and a `delete` with no readable window never expires. The device judges expiry exactly as [Peering — Sync](/design/peering/#sync) does. It judges only once a sync has drained the feed to its head, so a restore made on another device is applied first, and it judges by the server's clock as that drain saw it (the latest `changed_at`), never by its own: a device clock running ahead would purge early, and its tombstone would then refuse a restore the server accepted in time. A late purge loses nothing; an early one can. It never purges on expiry an asset whose `delete` the server has not indexed (a held delete, below), since the server has no window running for it; only the user's own purge removes that asset. A device that cannot drain the feed does not purge on expiry; one that never signed in has no drained feed, so its trash empties only when the user purges.
- **Purge** before `retention_until` (emptying the trash, or immediate deletion) first appends the signed shorter-retention re-issue the [retention window](#retention-window) requires, so the chain and the server agree that the purge is due. Purge then removes the original, the sidecar, every derivative and cache entry, and the index row, including its stack memberships. The provenance file stays, as the tombstone-with-history ([Filesystem — Client](/design/filesystem/client/)). That kept chain is the peering tombstone itself, below: no other file records a purge, whether this device purged by its own decision or on the evidence [Sync](/design/sync/#discovering-what-changed) names, so an index rebuild reproduces Purged from the provenance file alone.
- **Tombstone.** A kept chain headed by a `delete` with no sidecar and no original beside it is the library's peering tombstone: the workspace reads it back on open, holds the asset at that head, and refuses any backup or peer copy of it. Its head is also the re-issue an early purge must deliver, so a failed delivery is resent from it. A chain with a lost sidecar and any other head is damage, not a purge.
- **An upload in flight is redacted.** Deleting an asset whose upload has not finished aborts this device's open upload sessions for it, drops its pending [tiers](/design/transfer/upload/#upload-tiering-staged-uploads), and removes it from this device's upload queue, on every server it was queued for. A restore returns it to Live, and so to the queue. Every device redacts the same way: a device that learns of the delete over [peering](/design/peering/#sync) holds the asset Trashed, so it uploads no create of it either. No device uploads an asset in the trash only so that its delete can follow.
  - **Held records.** The server never sees a lifecycle record for an asset it has not indexed ([Peering — Delegation](/design/peering/#delegation)). A record goes to the server only once the server's feed lists the asset, whichever device uploaded its create, and then as every record after the head the server holds, in chain order, so the `delete` supersedes the create and each later record chains onto the one before. Until then it is held, and travels to the user's other devices over peering. Any device holding the chain sends it once the create is indexed. If the create never reaches the server, no record after it does either.
  - **Where held records live.** In the asset's own provenance chain, which is the only thing any delivery sends from. The [delegation](/design/peering/#delegation) ledger keeps a copy of a delegated asset's stretch only to know which assets wait on a hand-off's custody; once it releases them, they are delivered like any other. Every delivery compares the chain with the head a fresh feed drain names and sends only what the server lacks, so losing the upload queue, the delegation ledger or the index loses nothing, and a stretch one command delivered is not sent again by another. A purge keeps the chain as the tombstone, so a held `delete`, and the shorter-retention re-issue an early purge appends, outlive the purge and are sent from the tombstone if the create is indexed later. The server then purges by that re-issued window.
  - **A restore while the delete is held** is sent after the delete it undoes, in the same delivery. A restore that undoes a held delete whose `retention_until` has passed is never sent, and neither is the delete or anything after them. Judged by the later of the device's clock and the latest server `changed_at` the drain saw, this is the case where the server would apply the delete, refuse the restore and purge an asset the user restored in time. The server keeps the asset Live at the head it holds, and the library keeps every record. The chain is hash-linked, so a later delete of the asset never reaches the server either: it stays Live there, and only the library's trash holds it.
  - Redaction never weakens what keeps data safe. The local original stays pinned exactly as an unredacted staged upload pins it, until T2 is custody-confirmed or the asset's purge. A restore of an asset whose delete is still held, made before `retention_until` by the device's clock, is never lost because the held records reached the server late.

### Retention Window

`retention_until` is **signed into the `delete` manifest** at delete time, in its server-visible envelope, so the keyless purge worker enforces it against the server's [trusted clock](/design/filesystem/server/#postgresql-what-the-server-knows). Default 30 days, extendable per delete or album policy.

- The server **cannot accelerate** a purge: a hard purge before `retention_until` is rejected; server config is irrelevant.
- The server **cannot delay** a purge beyond a user-issued `trash-restore` or signed shorter-retention re-issue.
- `trash-restore` before `retention_until` recovers the asset and appends a record; the delete manifest stays in the chain.
- A `trash-restore` the server accepts at or after `retention_until`, by the same trusted clock, is refused `409 error.upload.retention_elapsed` and writes nothing, whether or not the purge has run. A purged asset cannot be restored: a restore that reaches the asset after its purge committed is refused the same way, even if the server accepted it just before the window closed. No restore ever meets a purged row, and a refused one leaves the asset in the trash until the purge.

## Validation

Each rule above is a unit or smoke test; notably, purge is refused before `retention_until` and `trash-restore` at or after it, under a mocked clock, and smart-album evaluation is byte-identical across platforms.
