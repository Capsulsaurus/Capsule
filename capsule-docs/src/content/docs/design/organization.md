---
title: Asset Organization
description: Albums (container and view), default-album resolution, asset stacks, and trash retention
---

**Albums** are Capsule's organizational backbone: [container albums](#container-albums) are the cryptographic unit every asset belongs to, while [view albums](#system--smart-albums-views) are derived, key-free presentations. On top of albums, **stacks** group related files (RAW+JPEG pairs, bursts, live photos) so a library stays tidy, and **trash** stages every destructive operation behind a signed retention window so a buggy or hostile actor cannot silently destroy data. Stacks and trash are metadata-only — they never touch the underlying asset bytes.

Implemented across `capsule-core::domain::stack_type` (stack-type enums), `capsule-core::library` (default-album resolution and client-side view evaluation), the metadata sidecar layer for `stack_membership` (see [Metadata](/design/metadata/)), the signed `delete`-manifest envelope for `retention_until`, and the service layer in `capsule-api-service::album`/`stack` for server-side enforcement. The retention contract — the `retention_until` field signed into the `delete` manifest — is the load-bearing piece that prevents a hostile server from accelerating purges.

## Albums

The UI calls two different things "albums," and the design keeps them strictly separate:

- **[Container albums](#container-albums)** — the real cryptographic unit. Every asset belongs to exactly one.
- **[View albums](#system--smart-albums-views)** — derived, key-free presentations computed client-side. They hold no keys and own no assets.

### Container Albums

A container album is Capsule's primary organizational unit and its primary **sharing and access-control boundary**. An album *is* an MLS group: its cryptographic identity (the per-epoch [AMK](/design/cryptography/keys/#album-master-keys-amks)) and membership operations are owned by [Cryptography — Keys](/design/cryptography/keys/) and [MLS](/design/cryptography/mls/), and its server-side storage shape (rows, blob references, `protocol_version` pin) lives in the [Filesystem — Server](/design/filesystem/server/) Postgres schema. This section owns the *interaction surface* over that machinery.

- **Membership and roles.** Each member holds one of the album's three capabilities — read (AMK only), write (AMK + write-tier key), or admin (also the admin-tier key) — delivered over MLS to that member's devices ([Keys — Album Master Keys](/design/cryptography/keys/#album-master-keys-amks)). A role change is an MLS commit and bumps the AMK epoch.
- **Invitation and join.** An admin invites a user by fetching and verifying their [device directory](/design/cryptography/keys/#device-directory) and issuing an MLS `Add` for all their devices; the `Welcome` delivers the AMK range set by the album's `history_policy` ([MLS — History Delivery](/design/cryptography/mls/#history-delivery-for-new-joiners)). Inviting a user on another home server also issues a [federation capability](/design/federation/#federation-capabilities); inviting a non-account recipient uses a [share link](/design/share-links/). Joining is acceptance of the `Welcome`; leaving or removal is an MLS `Remove` + epoch bump.
- **Album-level policy** — `history_policy`, the `protocol_version` pin, and the default `retention_until` — is fixed at creation and changed only through an [album upgrade ceremony](/design/versioning/#album-upgrade-ceremony), never ad hoc.

Dialog copy and on-screen presentation remain a client-UX detail.

### The Default Album

A container album must be explicitly created, but a brand-new account has none — so an import would have nowhere to land. Capsule guarantees a **default album**: a de facto, nameless container that exists for every owner from [first-device enrollment](/design/device-enrollment/#first-device-enrollment) onward and receives any import the user does not file elsewhere.

- **De facto and nameless.** It is an ordinary container album in every cryptographic and lifecycle respect — its own MLS group, random per-epoch AMK, `history_policy`, `protocol_version` pin, retention — but carries no user-assigned name; a client typically surfaces it as the library's primary view.
- **Specially identified.** Its album ID is **derived deterministically from the account master key** (the master key derives the *identifier*, not any key — see [Keys — Key Chain](/design/cryptography/keys/#key-chain)). The ID is therefore unique per user, unguessable before creation, and recomputable on any of the user's devices and after recovery — so a device can locate the default album from the master key alone, without waiting on a synced pointer.
- **Designation is a server-side owner pointer.** Which container is *currently* the default is a non-secret `default_album_id` on the owner record ([Filesystem — Server](/design/filesystem/server/#ownership-partitioning-and-quota)), defaulting to the derived de facto album. The pointer is not security-bearing — a write still requires real album write capability ([server-side invariants](/design/threat-model/validation/#server-side-validation-invariants), invariant 6).
- **One or more defaults, context-driven.** A client may register **scope overrides** — `(scope → album)` mappings that re-point the default for a context (a per-source auto-import mapping; "while viewing album X, new photos default to X"). The resolution rule, `resolve_default_album(context)`, returns the active scope's override if set, else the owner pointer, else the derived de facto album. It **always** resolves to a container — a [view](#system--smart-albums-views) can never be an import destination. The [import planner](/design/import/pipeline/#plan--confirm) consumes this when the user picks no album.
- **Stable.** Re-designating the default just moves the pointer. The current default **cannot be deleted while designated** — the user must repoint first, or the client recreates the derived de facto album — so import always has a home.

### System & Smart Albums (Views)

View albums are organizational surfaces computed entirely client-side over the assets the user can already decrypt (the union of their container-album memberships), materialized by querying the [local index](/design/filesystem/client/#local-index-staleness). A view is **not** an MLS group, holds **no** AMK, **owns no assets**, and is **not** a sharing or access-control boundary — sharing happens only at the container tier. Two kinds:

- **System albums** — built-in and implicit. The canonical one is **All** — every asset the user can see; because that is the union over their containers, every asset appears in it (which is exactly why the [default album](#the-default-album) matters: an import always enters *some* container and so shows up in All). [Trash](#recycling) is another system view, over lifecycle state.
- **Smart / dynamic albums** — user-defined filtered views whose membership is a predicate over sidecar fields and AI-derived attributes ([Metadata](/design/metadata/#sidecar-schema-v1), [AI](/design/ai/)). Membership is **computed**, never stored: editing a smart album, or an asset's attributes, never moves or re-encrypts an asset. A definition (predicate + display name) is user content — stored in a client-side, E2E-encrypted document synced across the user's devices with the same [CRDT semantics](/design/metadata/#collaborative-metadata) as other collaborative metadata, so the server never learns it.

## Asset Stacking

Related files often belong together — RAW+JPEG pairs, bursts, a video and its external audio track. Rather than clutter the library with near-identical entries, Capsule groups them into one stack via best-effort auto-detection.

**Stacking is metadata-only.** A stack edit modifies the `stack_membership` field of each member asset's sidecar and emits a `metadata-update` provenance record per affected asset. It **never** deletes, rewrites, or merges the underlying asset bytes — even a "best photo" choice within a burst is just the `role = primary` pointer in metadata, not a destructive operation. A buggy or malicious stack edit therefore cannot lose original bytes. The full atomicity rule (stage all `.tmp` files, rename together, discard on any rename failure) lives in [Filesystem — Atomic Writes](/design/filesystem/maintenance/#atomic-writes-and-crash-recovery) and [Threat Model — Atomicity Invariants](/design/threat-model/validation/#atomicity-invariants).

### Stack Membership Schema

The `stack_membership` field on each member sidecar carries:

```rust
StackMembership {
  stack_id:           UUIDv7,
  stack_type:         StackType,        // closed enum, below
  role:               StackRole,        // primary | member | proxy
  member_index:       Option<u32>,      // ordering within the stack (burst sequence, video chapter index)
}
```

`stack_type` is a closed enum per `protocol_version` — adding a new stack type bumps the version. Old albums never see the new type.

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

The cross-module case — full lifecycle including stack creation, member edit, soft delete, restore, and final hard purge — is one bounded E2E case in [Module Map](/design/module-map/#e2e-test-surface).
