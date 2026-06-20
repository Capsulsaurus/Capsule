---
title: Import Pipeline
description: How Capsule scans, plans, and executes a local import on a single device
---

The import pipeline is the workflow a client runs to bring assets from an external source (a camera, a filesystem directory) into Capsule's management. It is implemented in `capsule-core::import` and runs entirely client-side — no server is contacted until the [upload protocol](/design/import/upload-protocol/) is invoked at the tail of the pipeline.

Every import is **deterministic** and **idempotent**. Imports can be partially completed; each is identified by an *import ID* and resumable. The planner is pure (given the same inputs it produces the same plan), which makes the bulk of the pipeline unit-testable without any I/O.

## Pipeline Stages

```text
Initiate ──▶ Scan & Extract ──▶ Plan & Confirm ──▶ Execute ──▶ (Upload)
```

### Initiate

Imports begin in one of two ways:

- **Manual.** The user selects files or directories through the UI. The selection can point to a flat structure or a standardized directory structure (e.g. DCIM).
- **Automated.** Platforms (primarily mobile) detect new media in watched directories and trigger imports automatically.

### Scan & Extract

Files are walked, parsed, and their metadata extracted — see [Metadata](/design/metadata/) for the canonical schema. Format support is strictly gated: a file whose format is not in the supported set is rejected here rather than later, so the failure surfaces before the user is asked to confirm.

The server independently enforces a closed-enum `content_type` allow-list at session creation (see [Threat Model — Server-Side Validation Invariants](/design/threat-model/validation/#server-side-validation-invariants)), so even a malicious or buggy client declaring an unsupported format is rejected before any bytes are uploaded. Bytes received over the wire on the receiving side are decoded only inside the [sandboxed decoder](/design/clients/#sandboxed-decoder), so a format-mismatch attack cannot reach the host process.

### Plan & Confirm

The planner is **pure**: given the scanned files and their extracted metadata, it produces an `ImportPlan { added: [..], skipped: [..], conflicts: [..], total_size }` deterministically. The plan is shown to the user (summary of what will be imported, total size, any issues), and the user confirms or adjusts.

**Free-space probe.** Alongside `total_size`, the client probes the library volume's available space (`capsule-core::library::available_bytes()`) and sets `streaming_recommended: bool` on the plan when `total_size` is near or over free space (a configurable headroom margin). The probe is I/O, so it runs *outside* the pure planner and is attached at confirmation — the planner stays deterministic — exactly as the destination-pointer snapshot is a planner input rather than a discovered-later value. `streaming_recommended` is what surfaces [import-upload streaming mode](#import-upload-streaming-mode) to the user before execution begins.

- If an asset is already uploaded *locally* in the library, import refuses it — no merge needed.
- If an asset already exists *remotely* under a different ciphertext (e.g. re-encrypted under a newer album key), import still admits it; the [upload protocol](/design/import/upload-protocol/#deduplication-and-merge) then resolves it as a merge (the existing blob is linked rather than re-uploaded).

**Destination resolution.** Each added asset is assigned a destination [container album](/design/organization/#container-albums). If the user picks one explicitly the planner uses it; otherwise it calls `resolve_default_album(context)` — the active scope's override, else the owner [default-album](/design/organization/#the-default-album) pointer, else the derived de facto album. To keep the planner pure, the active context and a snapshot of the pointer/overrides are planner *inputs*, so the resolved `album_id` is deterministic and recorded in the `ImportPlan` rather than discovered later at upload time.

The planner's purity is what lets it be unit-tested exhaustively without filesystem fixtures: every edge case (overlapping selections, mixed formats, sidecar pairing, partial state from a prior interrupted import) becomes a table of `(scan_input → expected_plan)` pairs.

### Execute

For each file in the plan, in [upload prioritization](#upload-prioritization) order:

1. **Move into the detected space.** The planner determined which library directory each asset belongs in; execution moves files into place.
2. **Compute cryptographic metadata.** Encrypt under the resolved destination album's AMK (see [Asset Encryption](/design/cryptography/encryption/#authenticated-asset-encryption)), produce the [signed manifest](/design/cryptography/provenance/#asset-manifest).
3. **Generate thumbnails and previews.** Per [Thumbnails](/design/thumbnails/).
4. **Hand off to the upload protocol.** Each blob (original + derivatives + metadata) becomes its own upload session — see [Upload Protocol](/design/import/upload-protocol/).

Step 1–3 can be parallelized across files. The executor is cancellation-aware: a partially-executed plan can be aborted cleanly and resumed (re-running the import re-derives the plan and skips already-completed work via the deterministic planner).

## Import-Upload Streaming Mode

The default pipeline imports every file into the local library *before* upload, so the device temporarily holds the whole import on disk. That is impossible on a storage-constrained device — a laptop with a near-full SSD importing a library larger than its free space. **Streaming mode** removes the requirement that the full import ever land locally at once.

It is **auto-detected**: when the [free-space probe](#plan--confirm) reports `total_size` is near or over available space, the plan is marked `streaming_recommended` and the user confirms a streaming import. Instead of executing all files and then uploading, the executor runs a bounded **sliding window**:

1. Import the next file (or a small window of files) into the library — encrypt, sign the [manifest](/design/cryptography/provenance/#asset-manifest), generate derivatives — exactly as the normal [Execute](#execute) step.
2. Upload its bundle via the [upload protocol](/design/import/upload-protocol/).
3. Confirm the asset is durably stored with [`POST /storage/verify`](/design/import/storage-verification/#verify-before-destroy).
4. **Release** the local original (and delete the [Move-mode](#execute) source, if any) only after the `durable` verdict; the asset becomes an ordinary server-only, re-fetchable representation (see [Space Recovery](/design/filesystem/client/#space-recovery)).
5. Advance the window to the next file.

Peak local disk is therefore bounded to the in-flight window, not the whole import, so an import far larger than free space can complete. The verify-before-release step *is* the [verify-before-destroy](/design/import/storage-verification/#verify-before-destroy) rule applied per asset — the device never drops the only copy of bytes the server has not confirmed it holds.

**Halt on lost connectivity.** Streaming mode depends on the server: releasing local copies is safe only because the server is confirmed to hold them. If the connection to the server drops, the pipeline **pauses** — it stops admitting new source files into the library (continuing would refill the very disk the mode exists to spare) and waits, rather than importing ahead into space it does not have. In-flight uploads resume via the protocol's [`HEAD` resumption](/design/import/upload-protocol/#session-lifecycle) once connectivity returns; if the source media itself is removed mid-import, the deterministic planner re-derives the remaining work on resume. Bulk streaming import is a *large reconciliation* and obeys the same metered/Wi-Fi [connection rules](/design/import/download-sync/#synchronization-criteria) as auto-sync.

**Quota.** Streaming creates one upload session per asset, so server [quota](/design/quota/#enforcement-points) is enforced exactly as for any upload — at session creation, no new enforcement point. If quota is exhausted mid-stream the next session creation is refused; the pipeline pauses and surfaces the remediable state rather than failing the whole import, and no local original is released for an asset that did not upload.

## Upload Prioritization

When many files are processed simultaneously, the order they are *started* is decided by these heuristics:

- **Last Modified Times.** Newer or recently modified files are likely more relevant to the user. Filesystem mtime is the cross-platform signal, with fallbacks where a platform reports it unreliably.
- **Directory Depth.** Files closer to the root of the specified paths are processed first.
- **File Size.** A useful secondary heuristic, but ordering of in-flight uploads is left to the OS / TCP stack and the [adaptive chunk sizing](/design/import/upload-protocol/#adaptive-chunk-sizing) the upload protocol exposes; we do not micro-manage it here.

File **type/extension** is deliberately *not* a prioritization criterion — prioritizing purely by type may produce anomalies. Instead we have exceptions for sidecar files (e.g. `.xmp` associated with an image, `.wav` associated with a video) that travel with their parent asset.

The pipeline decides which assets to *start*; the [upload protocol](/design/import/upload-protocol/#adaptive-chunk-sizing) decides how they stream.

## Contracts the Pipeline Exposes

What the rest of the system depends on this module for:

- `ImportPlan` — the deterministic output of the planner; rendered to the UI for confirmation. Schema fields: `added` (each entry carrying its resolved destination `album_id`), `skipped`, `conflicts`, `total_size`, `import_id` (UUIDv7), and `streaming_recommended` (set at confirmation from the [free-space probe](#plan--confirm), not by the pure planner).
- `available_bytes() → u64` — the library volume's free space (a thin `statvfs` / `GetDiskFreeSpaceEx` wrapper in `capsule-core::library`); the input that decides `streaming_recommended`.
- `execute(plan, cancel_token) → ImportExecutionReport` — the executor entry-point. Honors the cancel token at every file boundary. Returns per-file status. In [streaming mode](#import-upload-streaming-mode) it drives the per-asset import→upload→verify→release window instead of executing-then-uploading in bulk.
- A stable progress event stream so the UI can report per-asset state (queued / encrypting / uploading / done / failed).

## Validation

- **Planner determinism (unit).** Table-driven tests over `(scan_input, library_state) → expected_plan`. Every conflict-resolution and dedup-detection path is its own row. Default-album resolution is part of the input snapshot, so a given `(context, pointer/overrides)` yields a deterministic destination `album_id`.
- **Scanner format-rejection (unit).** Every unsupported extension and every malformed-header case produces a structured rejection, never a panic.
- **Executor cancellation (smoke).** Run a real executor against a temp library, cancel mid-flight, assert no partial bundle is left on disk and a re-run produces the same plan minus already-completed files.
- **Resume after interruption (smoke).** Plan → execute partially → kill the process → re-run. The deterministic planner re-derives the same plan; already-completed assets are skipped.
- **Streaming auto-detect (unit).** With `available_bytes()` mocked below and above `total_size + headroom`, assert `streaming_recommended` is set in the constrained case and clear otherwise.
- **Streaming release gating (smoke).** Run a streaming import with `/storage/verify` mocked: assert each local original (and Move-mode source) is released *only* after its `durable` verdict, and that a non-`durable` verdict leaves the local copy in place.
- **Streaming halt-on-disconnect (smoke).** Drop the server connection mid-stream; assert the pipeline stops admitting new source files into the library (no unbounded local growth) and resumes uploading via `HEAD` on reconnect without re-importing completed assets.

The cross-module case — pipeline → upload protocol → server finalization → assets visible in `/sync` — is bounded E2E surface listed in [Module Map](/design/module-map/#e2e-test-surface).
