---
title: Asset Organization
description: Albums (container and view), default-album resolution, asset stacks, and trash retention
status: draft
---

**Albums** are Capsule's organizational backbone: [container albums](#container-albums) are the cryptographic unit every asset belongs to, while [view albums](#system--smart-albums-views) are derived, key-free presentations. On top of albums, **stacks** group related files (RAW+JPEG pairs, bursts, live photos) so a library stays tidy, and **trash** stages every destructive operation behind a signed retention window so a buggy or hostile actor cannot silently destroy data. Stacks and trash are metadata-only — they never touch the underlying asset bytes.

The client contracts span `capsule-core::domain::stack_type` (stack-type enums), `capsule-core::library` (default-album resolution and client-side view evaluation), the metadata sidecar layer for `stack_membership` (see [Metadata](/design/metadata/)), and the signed `delete`-manifest envelope for `retention_until`. Server-side enforcement is planned in `capsule-server::album`. The retention contract — the `retention_until` field signed into the `delete` manifest — is the load-bearing piece that prevents a hostile server from accelerating purges.

## Albums

The UI calls two different things "albums," and the design keeps them strictly separate:

- **[Container albums](#container-albums)** — the real cryptographic unit. Every asset belongs to exactly one.
- **[View albums](#system--smart-albums-views)** — derived, key-free presentations computed client-side. They hold no keys and own no assets.

### Container Albums

A container album is Capsule's primary organizational unit and its primary **sharing and access-control boundary**. An album *is* an MLS group: its cryptographic identity (the per-epoch [AMK](/design/cryptography/keys/#album-master-keys-amks)) and membership operations are owned by [Cryptography — Keys](/design/cryptography/keys/) and [MLS](/design/cryptography/mls/), and its server-side storage shape (rows, blob references, `protocol_version` pin) lives in the [Filesystem — Server](/design/filesystem/server/) Postgres schema. This section owns the *interaction surface* over that machinery.

- **Moves are idempotent.** Every asset lives in exactly one container; moving it is a signed lifecycle action naming `(asset, target album, epoch)`. Replaying a move finds the target state already in place and no-ops; concurrent moves resolve through the MLS commit order — see the [grouping-convergence requirement](/design/metadata/#grouping-convergence-requirement).
- **Membership and roles.** Each member holds one of the album's three capabilities — read (AMK only), write (AMK + write-tier key), or admin (also the admin-tier key) — delivered over MLS to that member's devices ([Keys — Album Master Keys](/design/cryptography/keys/#album-master-keys-amks)). A role change is an MLS commit and bumps the AMK epoch.
- **Invitation and join.** An admin invites a user by fetching and verifying their [device directory](/design/cryptography/keys/#device-directory) and issuing an MLS `Add` for all their devices; the `Welcome` delivers the AMK range set by the album's `history_policy` ([MLS — History Delivery](/design/cryptography/mls/#history-delivery-for-new-joiners)). Inviting a user on another home server also issues a [federation capability](/design/federation/#federation-capabilities); inviting a non-account recipient uses a [share link](/design/share-links/). Joining is acceptance of the `Welcome`; leaving or removal is an MLS `Remove` + epoch bump. (The MLS membership operations this surface invokes are pending the MLS implementation — see the [MLS status note](/design/cryptography/mls/).)
- **Album-level policy** — `history_policy`, the `protocol_version` pin, and the default `retention_until` — is fixed at creation and changed only through an [album upgrade ceremony](/design/versioning/#album-upgrade-ceremony), never ad hoc.

Dialog copy and on-screen presentation remain a client-UX detail.

### The Default Album

A container album must be explicitly created, but a brand-new account has none — so an import would have nowhere to land. Capsule guarantees a **default album**: a de facto, nameless container that exists for every owner from [first-device enrollment](/design/device-enrollment/#first-device-enrollment) onward and receives any import the user does not file elsewhere.

- **De facto and nameless.** It is an ordinary container album in every cryptographic and lifecycle respect — its own MLS group, random per-epoch AMK, `history_policy`, `protocol_version` pin, retention — but carries no user-assigned name; a client typically surfaces it as the library's primary view.
- **Specially identified.** Its album ID is **derived deterministically from the account master key** (the master key derives the *identifier*, not any key — see [Keys — Key Chain](/design/cryptography/keys/#key-chain)). The ID is therefore unique per user, unguessable before creation, and recomputable on any of the user's devices and after recovery — so a device can locate the default album from the master key alone, without waiting on a synced pointer.
- **Designation is a server-side owner pointer.** Which container is *currently* the default is a non-secret `default_album_id` on the owner record ([Filesystem — Server](/design/filesystem/server/#ownership-partitioning-and-quota)), defaulting to the derived de facto album. The pointer is not security-bearing — a write still requires real album write capability ([server-side invariants](/design/threat-model/validation/#server-side-validation-invariants), invariant 6).
- **One or more defaults, context-driven.** A client may register **scope overrides** — `(scope → album)` mappings that re-point the default for a context (a per-source auto-import mapping; "while viewing album X, new photos default to X"). The resolution rule, `resolve_default_album(context)`, returns the active scope's override if set, else the owner pointer, else the derived de facto album. It **always** resolves to a container — a [view](#system--smart-albums-views) can never be an import destination. The [import planner](/design/import/pipeline/#plan--confirm) consumes this when the user picks no album. The scope grammar is formalized below.
- **Stable.** Re-designating the default just moves the pointer. The current default **cannot be deleted while designated** — the user must repoint first, or the client recreates the derived de facto album — so import always has a home.

#### Scope Grammar (Local Source → Album Mapping)

How a local folder, camera roll, or watched directory on each platform maps to a remote album is a formal contract, not per-client improvisation. A **scope** is the canonical identity of an import source:

```rust
Scope {
  platform:    PlatformTag,     // closed enum (shared with device cohorts)
  source_kind: SourceKind,      // closed enum: camera_roll | screenshots | app_collection | folder | watched_dir | removable_volume
  locator:     String,          // canonical, per the platform table below
}
// scope_id = SHA-256( canonical-CBOR([ "capsule-import-scope/v1", platform, source_kind, locator ]) )
```

`scope_id` is deterministic (domain-separated canonical CBOR — the same discipline as every derived identifier), so two devices of the same platform looking at the same source compute the same scope, and the mapping table needs no coordination protocol. Per-platform canonical locators, chosen for stability across reinstall:

| Platform | Source | Canonical locator | Notes |
| --- | --- | --- | --- |
| iOS | camera roll / screenshots / user collection | the smart-album subtype name, or the user collection's title-independent `localIdentifier` | `localIdentifier` survives reinstall on the same device; cross-device it maps only via the override table |
| Android | MediaStore bucket | **relative path** (`DCIM/Camera`, `Pictures/Screenshots`) | never `BUCKET_ID` — it is a hash of the display name that differs across devices and OS versions |
| Desktop | folder / watched dir | library-relative or absolute canonicalized path (symlinks resolved) | |
| Any | removable volume | volume UUID + relative path | the volume UUID makes a re-mounted card the same scope regardless of mount point |

- **The mapping table lives in the [library-settings document](/design/metadata/#the-library-settings-document)** (its `scope_overrides` and `source_kind_defaults` sections) — per-owner, E2E-encrypted, synced across devices with the same CRDT semantics as other collaborative metadata (each row an LWW register keyed by `scope_id`, resp. `SourceKind`). The server never learns what a scope means.
- **Resolution order** (first match wins): explicit user pick at import time → `scope_id` override row → per-source-kind default row (e.g. "all screenshots → Screenshots") → the owner's `default_album_id` pointer → the derived de facto album. Deterministic by construction; the planner records which rule fired so a surprising destination is explainable after the fact.
- **Unmapped sources ask once.** The first import from a new scope surfaces a "where should photos from *X* go?" choice, whose answer is written as the scope's override row — automated imports never silently invent destinations.

**Status note.** The settings-document-backed rows (`scope_overrides`, `source_kind_defaults`) are deferred post-v1 with the [library-settings document](/design/metadata/#the-library-settings-document); v1 ships the base resolution — explicit user pick → the owner's `default_album_id` pointer → the derived de facto album, with the fired rule recorded (slice `S-B12`).

### System & Smart Albums (Views)

View albums are organizational surfaces computed entirely client-side over the assets the user can already decrypt (the union of their container-album memberships), materialized by querying the [local index](/design/filesystem/client/#local-index-staleness). The [aggregated federated album](/design/federation/#federated-shared-albums-aggregated-albums) is a view in exactly this sense — its predicate is an album-group id spanning constituents on different home servers. A view is **not** an MLS group, holds **no** AMK, **owns no assets**, and is **not** a sharing or access-control boundary — sharing happens only at the container tier. Two kinds:

- **System albums** — built-in and implicit. The canonical one is **All** — every asset the user can see; because that is the union over their containers, every asset appears in it (which is exactly why the [default album](#the-default-album) matters: an import always enters *some* container and so shows up in All). [Trash](#recycling) is another system view, over lifecycle state.
- **Smart / dynamic albums** — user-defined filtered views whose membership is a predicate over sidecar fields and AI-derived attributes ([Metadata](/design/metadata/#sidecar-schema-v1), [AI](/design/ai/)). Membership is **computed**, never stored: editing a smart album, or an asset's attributes, never moves or re-encrypts an asset. A definition (predicate + display name) is user content — stored in the per-owner, E2E-encrypted **library-settings document** whose envelope, keying, and versioning are owned by [Metadata — The Library-Settings Document](/design/metadata/#the-library-settings-document), synced across the user's devices with the same [CRDT semantics](/design/metadata/#collaborative-metadata) as other collaborative metadata, so the server never learns it. This doc owns the definition's *shape* — the [Smart-Album Definition Schema](#smart-album-definition-schema) below — while that document owns how it is carried and merged.

#### Smart-Album Definition Schema

**Status: designed, implementation deferred post-v1** with the [library-settings document](/design/metadata/#the-library-settings-document) that carries it (decision 2026-07-12). System views (All, Trash) ship in v1; user-defined smart albums wait on that document. The grammar below is normative as written.

A smart album's definition is stored as one entry in the library-settings document's `smart_albums` map ([envelope owned by Metadata](/design/metadata/#the-library-settings-document)); this section owns its *shape* — a **closed, versioned, declarative predicate grammar** over the queryable [sidecar](/design/metadata/#sidecar-schema-v1) and [AI-namespaced](/design/ai/#ai-output-containment) fields. It is declarative on purpose: a definition is stored data that syncs to every one of the owner's devices, so it must evaluate identically everywhere and can carry no code, no regex, and no unbounded input.

```rust
SmartAlbumDefinition {
  smart_album_id:   UUIDv7,
  predicate_schema: u16,              // closed-grammar version; a later value gates evaluation on older clients
  display_name:     Lww<String>,      // ≤ 256 bytes; converges like caption_lww
  predicate:        Predicate,        // closed tree, below
  sort:             Option<SortSpec>, // closed key set; default (capture_timestamp, desc)
}

Predicate =                           // bounded: depth ≤ 8, ≤ 64 terms total
  | All(Vec<Predicate>)               // AND — empty ⇒ matches every asset
  | Any(Vec<Predicate>)               // OR  — empty ⇒ matches none
  | Not(Box<Predicate>)
  | Term(Term)

Term {
  field:   QueryField,                // closed enum, below
  op:      Operator,                  // closed enum; must be valid for the field's type class
  operand: Operand,                   // typed literal; shape fixed by (field, op)
}
```

**Queryable fields (`QueryField`) — closed set, one type class each:**

| Class | Fields | Valid operators | Operand |
| --- | --- | --- | --- |
| temporal | `capture_timestamp`, `import_timestamp` | `before`, `after`, `in_range` | RFC 3339 (a half-open `[start, end)` pair for `in_range`) |
| enum | `content_type`, `media_kind` (image\|video, derived from `content_type`), `gps.datum` | `eq`, `in` | a value / set drawn from the field's own closed enum |
| numeric | `rating`, `dimensions.width`, `dimensions.height` | `eq`, `gte`, `lte`, `in_range` | `u32` (a `[lo, hi]` pair for `in_range`) |
| trinary/bool | `cull`, `hidden` | `is` | the field's literal (`pick`\|`neutral`\|`reject`; `true`\|`false`) |
| set | `tags_user`, `tags_ai`, `stack_type`, `people_cluster`, `album_id` | `contains`, `contains_any`, `contains_all` | a string / id set, ≤ 64 members, each ≤ 256 bytes |
| presence | `gps`, `camera_id` | `exists` | `bool` |

Adding a field, operator, or operand type is a **new, later-dated `predicate_schema`** (and, where it queries a new sidecar field, a coordinated [sidecar-schema](/design/metadata/#schema-versioning-rules) bump); the value sets are otherwise closed, and a term naming an unknown `field`/`op`, or an `operand` mistyped for its `(field, op)`, is a **structural rejection** at the definition validator — never a "future to ignore." A `tags_ai` or `people_cluster` term names the `(model_id, model_version)` slot it queries and is subject to the [AI staleness rule](/design/ai/#ai-output-containment): a term over a slot whose canonical model changed evaluates as stale-excluded until regenerated, never compared across model versions.

**LWW-safety and convergence.** The whole `SmartAlbumDefinition` is stored as the value of one LWW register keyed by `smart_album_id` (a stamped `None` deletes it), so authoring or editing a smart album is a single stamped write that converges under the [grouping-convergence requirement](/design/metadata/#grouping-convergence-requirement) — there is never a partial-predicate merge. **Membership is computed, never stored:** evaluation is a pure function of `(definition, the assets the viewer can decrypt)` processed in sorted `asset_id` order, so recomputation is idempotent and identical across devices — the same determinism the [aggregated album](/design/federation/#membership-and-rendering) and [AI grouping](/design/ai/#ai-output-containment) rely on. Editing a definition, or an asset's attributes, moves and re-encrypts nothing.

## Asset Stacking

Related files often belong together — RAW+JPEG pairs, bursts, a video and its external audio track. Rather than clutter the library with near-identical entries, Capsule groups them into one stack via best-effort auto-detection.

**Status note.** v1 auto-detection covers RAW+JPEG pairing (plus XMP sidecar attachment) only; detectors for the remaining [stack types](#stack-types) are post-v1. The schema and LWW semantics below are normative for every type now, and manual stacking of any type is unaffected.

**Stacking is metadata-only.** A stack edit modifies the `stack_membership` field of each member asset's sidecar — an LWW register over `Option<StackMembership>` (leaving a stack is a stamped `None`), so concurrent stack edits from different devices converge order-independently under the [grouping-convergence requirement](/design/metadata/#grouping-convergence-requirement) — and emits a `metadata-update` provenance record per affected asset. It **never** deletes, rewrites, or merges the underlying asset bytes — even a "best photo" choice within a burst is just the `role = primary` pointer in metadata, not a destructive operation. A buggy or malicious stack edit therefore cannot lose original bytes. The full atomicity rule (stage all `.tmp` files, rename together, discard on any rename failure) lives in [Filesystem — Atomic Writes](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery) and [Threat Model — Atomicity Invariants](/design/threat-model/validation/#atomicity-invariants).

### Stack Membership Schema

The `stack_membership` register's value on each member sidecar carries:

```rust
StackMembership {
  stack_id:           UUIDv7,
  stack_type:         StackType,        // closed enum, below
  role:               StackRole,        // primary | member | proxy
  member_index:       Option<u32>,      // ordering within the stack (burst sequence, video chapter index)
}
```

`stack_type` is a closed enum per `protocol_version` — adding a new stack type requires a new (later-dated) version. Old albums never see the new type. The authoritative value set is the closed Rust enum `capsule-core::domain::stack_type`, one variant per type below; the taxonomy prose is descriptive, the enum is normative.

### Stack Types

**Photography & Mobile Stacks**

- **RAW + JPEG Pairs:** The classic "prosumer" stack. The uncompressed RAW and the processed JPEG are treated as one asset to keep the grid tidy.
- **Burst Stacks:** A sequence of high-speed stills (e.g., 10–30 fps). The app identifies a "Best Photo" and tucks the rest behind it.
- **Live Photos:** A JPEG or HEIC paired with a 1.5–3 second video clip, managed as a single interactive unit.
- **Portrait/Depth Stacks:** An image paired with its depth map. Enables adjusting bokeh after the shot is taken.
- **Smart Selection:** AI-driven grouping of visually similar images taken within seconds of each other.

**Technical & Creative Stacks**

- **Exposure Bracketing (HDR):** Multiple shots of the same scene at different exposure levels (e.g., -2, 0, +2 EV) to be merged into a single HDR image.
- **Focus Stacks:** A series of shots with shifting focus points. Often used in macro photography to create "infinite" depth of field.
- **Pixel Shift Stacks:** Found in high-end mirrorless cameras. The sensor moves slightly to capture multiple shots, stacked for ultra-high resolution and perfect color.
- **Panorama (Stitched):** A sequence of horizontal or vertical shots intended to be merged into a single wide-field image.

**Video & Audio Stacks**

- **Proxy/Optimized Stacks:** Pairs a heavy "Master" file (like 8K RAW) with a lightweight "Proxy" (like 1080p ProRes) for smoother editing performance.
- **Chaptered Video:** Action cameras (like GoPro) often split long recordings into 4GB chunks. Files like `GOPR001.mp4` and `GOPR002.mp4` are stacked so they appear as one continuous video.
- **Dual-System Audio:** Groups video files with high-quality external audio (WAV/AIFF) using timecode or waveform matching.

## Culling

Culling is the review pass photographers make after a shoot: keep, undecided, toss. Capsule models it as a **trinary flag per asset** — `pick | neutral | reject` — stored in the sidecar's `cull` LWW register ([schema](/design/metadata/#sidecar-schema-v1)); `neutral` is the never-flagged default and is wire-absent. The flag is orthogonal to the numeric star `rating` (a reject can carry three stars; tools that conflate them force lossy workflows).

- **Workflow.** Flag during review (single keystroke/swipe per asset), filter the view by flag, then act: batch-move rejects to [trash](#recycling) (the only destructive step, and it is soft-per-retention like any delete), promote picks into albums or shares. Flagging itself never touches bytes and is fully reversible.
- **Groups.** A stack or burst has no stored flag of its own — a group's cull state is **derived** from its members (all-rejected, any-pick, else mixed), so there is no second source of truth to diverge. Flagging a collapsed stack applies the flag to each member (one `metadata-update` per member, atomically staged like any [stack edit](#asset-stacking)).
- **Sync.** Like every LWW field, concurrent flags converge under the [grouping-convergence requirement](/design/metadata/#grouping-convergence-requirement).

The dedicated culling UX (keyboard-driven review mode, reject-sweep) is client work tracked as its own slice in the repo-root `SLICES.md`; the schema and semantics above are frozen now so sidecars written today survive it unchanged.

## Hidden Assets

Every asset carries a `hidden` flag (sidecar LWW register, wire-absent default = visible). A hidden asset is **excluded from default views** — timeline, search results, and system views — and appears only in the dedicated Hidden view, which sits behind the same fresh-local-auth gate as Recently Deleted ([Local Gallery — SR1](/design/local-gallery/)). Hiding is view-layer only: the asset stays in its container album, keeps syncing, and remains reachable from contexts that reference it directly (its stack, a share it was already part of).

Hiding is for "don't surface this" (a document photo, an awkward duplicate that must stay); it is not deletion and not access control. Sidecar-style companion files (the JPEG half of a RAW+JPEG pair, a Live Photo's video) are already suppressed from default views by their stack `role` — collapsed stacks show only the `primary` — so `hidden` is not needed for them; it exists for the cases stacking cannot express.

## Recycling

When you delete an asset, it defaults to trash (i.e. soft delete). On sync, new items in trash are essentially a metadata update rather than removal. A true "delete" operation is only performed when the user explicitly empties the trash, the asset has been in the trash for its full retention period, or the user requests immediate deletion.

For consistency, deletion of assets is functionally similar to addition and modification of assets. See [Cryptography — Provenance](/design/cryptography/provenance/#provenance-of-library-modifications) and [Authorization — The Closed Action Set](/design/authorization/#the-closed-action-set).

### Retention Window

The trash retention window is **signed into the `delete` manifest at delete time** as the `retention_until` field — not server-configured at purge time. It lives in the manifest's **server-visible envelope** (like `action` and `prior_provenance_hash`), so the keyless purge worker reads and enforces it without any decryption key, comparing it against the server's own [trusted clock](/design/filesystem/server/#postgresql-what-the-server-knows). The default is 30 days; the user can extend it per delete or per album policy. Because retention is part of the signed manifest:

- The server **cannot accelerate** a purge by changing a server-side config — the cryptographic floor on retention is the signed manifest's `retention_until`. A hard purge before that timestamp is rejected (the server's purge worker reads `retention_until` from the manifest, not from a local policy).
- The server **cannot delay** a purge beyond an order issued by a `trash-restore` or a signed shorter-retention re-issue — the user remains in control.
- A `trash-restore` action issued before `retention_until` recovers the asset, appends a new provenance record, and rewinds the local lifecycle state. The original delete manifest is **not removed** from the provenance chain — it remains as a record of "this was deleted on date X and restored on date Y."

This addresses the damage scenario where a hostile server unilaterally accelerates a purge to delete an asset the user expected to be recoverable, as well as the scenario where a buggy server retains data past the user's chosen window.

## Validation

- **Stack edit metadata-only (unit).** Build a stack edit; assert no asset bytes are touched on disk; only sidecars and provenance records are modified.
- **Stack edit atomicity (unit).** Inject a rename failure mid-bundle; assert all staged `.tmp` files are discarded and on-disk state reflects no partial stack.
- **Closed stack-type enum rejection (unit).** Set `stack_type = "future-stack-type"`; assert structural rejection at the sidecar validator.
- **Retention-window honor (smoke).** Issue a `delete` with `retention_until = now + 30d`. Mock the server clock to `now + 15d`; assert purge worker refuses. Move to `now + 31d`; assert purge proceeds.
- **Trash-restore round-trip (smoke).** Delete → restore → assert asset reappears in live set, provenance chain has delete + restore records, original delete record is preserved.
- **Hostile-server purge defense (smoke).** Mock a server that attempts purge before `retention_until`; assert the purge worker (running the no-key envelope check) refuses.
- **Closed predicate-grammar rejection (unit).** Build [`SmartAlbumDefinition`](#smart-album-definition-schema) terms with an unknown `field`, an `op` invalid for the field's type class, and an `operand` mistyped for its `(field, op)`; assert each is a structural rejection at the definition validator, not a tolerated unknown.
- **Predicate bounds (unit).** A predicate exceeding depth 8 or 64 terms, or a set operand over 64 members / 256 bytes per member, is rejected before evaluation.
- **Predicate evaluation determinism (unit).** Evaluate one definition over a fixed asset set (assets in sorted `asset_id` order) across runs and platforms; assert byte-identical membership — the grouping-convergence guarantee.
- **Forward `predicate_schema` preserve-not-drop (unit).** A definition with `predicate_schema = N+1` loaded on a `max_known = N` client is preserved verbatim through a sync round-trip and is never evaluated or stripped.
- **AI-term staleness (unit).** A `tags_ai`/`people_cluster` term whose queried `(model_id, model_version)` slot changed evaluates as stale-excluded, mirroring the `tags_ai` staleness rule.

The cross-module case — full lifecycle including stack creation, member edit, soft delete, restore, and final hard purge — is one bounded E2E case in [Module Map](/design/module-map/#e2e-test-surface).
