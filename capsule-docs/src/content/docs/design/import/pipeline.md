---
title: Import Pipeline
description: How Capsule scans, plans, and executes a local import on a single device
status: draft
---

The import pipeline runs entirely client-side in `capsule-core::import`; no server is contacted until bytes are handed to the [upload protocol](/design/transfer/upload/). Every import is **deterministic**, **idempotent** and resumable by *import id*. The planner is pure, so most of the pipeline is testable without I/O.

```text
Initiate ──▶ Scan & Extract ──▶ Plan & Confirm ──▶ Execute ──▶ (Upload)
```

## Pipeline Stages

### Initiate

Manually (the user picks files or directories, flat or DCIM-style) or automatically (a platform watches directories for new media).

### Scan & Extract

Files are walked and their [metadata](/design/metadata/) extracted. An unsupported format is rejected here, before confirmation. The server independently enforces a closed `content_type` set at session creation, and received bytes are decoded only in the [sandboxed decoder](/design/clients/#sandboxed-decoder).

### Plan & Confirm

The pure planner turns scanned files into an `ImportPlan` (added, skipped, conflicts, total size, import id), shown to the user for confirmation.

- An asset already in the local library is refused.
- An asset that exists remotely under different ciphertext is admitted and resolves as a [merge](/design/transfer/upload/#deduplication-and-merge).
- **Destination.** Each added asset gets a [container album](/design/organization/#container-albums): the user's pick, else `resolve_default_album` (scope override, then the [default-album](/design/organization/#the-default-album) pointer). The context and pointer snapshot are planner inputs, so the resolved `album_id` is recorded in the plan; a later re-point does not retarget a confirmed plan.
- **Free-space probe.** Outside the pure planner, the client compares `total_size` with `capsule-core::library::available_bytes` and sets `streaming_recommended` when the import is near or over free space.

### Execute

For each planned file, in [prioritization](#upload-prioritization) order:

1. move it into its library location;
2. encrypt under the destination album's key and sign the [manifest](/design/cryptography/provenance/#asset-manifest) — the signed path in `capsule-core::lifecycle::Workspace`;
3. generate [derivatives](/design/thumbnails/);
4. hand each blob of the bundle to its own upload session.

Steps 1–3 parallelize across files. Execution is cancellable at file boundaries; re-running re-derives the plan and skips completed work. A library holding pre-signed-path unsigned sidecars is reported by the index rebuild and brought forward with `Workspace::migrate_unsigned_sidecars`.

## Import-Upload Streaming Mode

For a storage-constrained device, streaming mode (`capsule-core::import::streaming`) bounds peak disk to one in-flight asset instead of the whole import. When the plan is `streaming_recommended` and the user confirms, the executor runs a sliding window:

1. import the next file onto the signed path, deferring source release;
2. upload its bundle;
3. pass the [release gate](/design/transfer/durability/#verify-before-destroy);
4. only then release the local original and delete any Move-mode source;
5. advance.

- The plan MUST have headroom for its **largest single asset**; otherwise confirmation fails with a "free *Y* to import *X*" error.
- On lost connectivity or exhausted [quota](/design/quota/#enforcement-points) the window **pauses** — it admits no new source files — and in-flight uploads resume from server offsets. No original is released for an asset that did not upload.
- Streaming is a large reconciliation under the [synchronization criteria](/design/sync/#synchronization-criteria), and is mutually exclusive with a [staged upload policy](/design/transfer/upload/#upload-tiering-staged-uploads).

## Third-Party Importers

An importer is a **source adapter** (`capsule-core::import::importers`) on the filesystem scanner's contract: it yields entries into [Scan & Extract](#scan--extract) and adds no stage and no server surface. It folds out-of-band exporter metadata into extraction under one precedence rule: **embedded EXIF wins, except for what the exporter is authoritative for** (album membership, favourites, user-typed descriptions). Because the fold happens at extraction, an archive yields the same plan on every run.

Google Takeout is implemented; iCloud Photos and Immich are planned post-v1. [Camera import](/design/import/camera-import/) uses the same seam. Fixed Takeout mappings:

| Exporter field | Written as | Why |
| --- | --- | --- |
| Favourite | `rating = 5` | Capsule has no favourite; writing a `cull` pick would fabricate a [review decision](/design/organization/#culling). |
| Album membership | `tags_user` entries | An import may not invent container albums ([default album](/design/organization/#the-default-album)); tags are searchable and rebuildable as smart albums. |
| Exporter GPS fix | `GpsSource::Manual` | `Exif` would be false inside a signed record. |
| Over-long description | truncated on a char boundary, with a warning | Truncation loses less than dropping. |

At the write site, capture time is the file's EXIF instant, else the adapter's **folded** value, else the import clock — never the raw exporter value, which would beat a floating EXIF time the precedence rule says should win.

## Upload Prioritization

The pipeline decides which assets to **start**: recently modified files first, then shallower directory depth, with file size secondary. File type is not a criterion; sidecars (`.xmp`, paired audio) travel with their parent. How started uploads stream is the [upload protocol's](/design/transfer/upload/#adaptive-chunk-sizing) concern.

## Contracts the Pipeline Exposes

- `ImportPlan` — the planner's deterministic output, each added entry carrying its `album_id`; `streaming_recommended` is attached at confirmation.
- `available_bytes()` — library-volume free space.
- `execute(plan, cancel_token)` — honours cancellation at every file boundary and returns per-file status; in streaming mode it drives the import → upload → verify → release window.
- A progress event stream (queued / processing / encrypting / uploading / done / failed), `capsule-core::import::progress`.
