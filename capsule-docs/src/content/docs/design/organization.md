---
title: Asset Organization
description: Details on how assets are organized and grouped in Capsule
---

## Keywords

- [Albums and Collections](#albums-and-collections): Organize your media into albums and collections for easy browsing and sharing.
- [Asset Stacking](#asset-stacking): Group related files (e.g., RAW+JPEG pairs, burst photos, video chapters) into a single "stack" to keep your library organized.

## Albums and Collections

## Asset Stacking

In large media collections, it’s common for related files to belong together. Instead of cluttering your library with dozens of nearly identical files, Capsule "stacks" them into a single unit.

You’ve likely seen this in action before—think of how photo apps group RAW+JPG pairs or how video editors sync external audio with camera footage. Capsule uses a "best-effort" auto-detection system to identify these relationships and keep your workspace clean.

**Stacking is metadata-only.** A stack edit modifies the `stack_membership` field of each member asset's sidecar and emits a `metadata-update` provenance record per affected asset. It **never** deletes, rewrites, or merges the underlying asset bytes — even a "best photo" choice within a burst is a pointer in metadata, not a destructive operation. A buggy or malicious stack edit therefore cannot lose original bytes. The full atomicity rule (stage all `.tmp` files, rename together, discard on any rename failure) lives in [Filesystem — Atomic Writes and Crash Recovery](/design/filesystem/#atomic-writes-and-crash-recovery) and [Threat Model — Atomicity Invariants](/design/threat-model/#atomicity-invariants).

### Photography & Mobile Stacks

* **RAW + JPEG Pairs:** The classic "prosumer" stack. We treat the uncompressed RAW file and the processed JPEG as one asset to keep your grid tidy.
* **Burst Stacks:** A sequence of high-speed stills (e.g., 10–30 fps). The app identifies a "Best Photo" and tucks the rest behind it.
* **Live Photos:** A JPEG or HEIC paired with a 1.5–3 second video clip, managed as a single interactive unit.
* **Portrait/Depth Stacks:** An image paired with its depth map. This allows you to adjust the bokeh (background blur) after the shot is taken.
* **Smart Selection:** AI-driven grouping of visually similar images taken within seconds of each other to reduce "clutter."

### Technical & Creative Stacks

* **Exposure Bracketing (HDR):** Multiple shots of the same scene at different exposure levels (e.g., -2, 0, +2 EV) to be merged into a single High Dynamic Range image.
* **Focus Stacks:** A series of shots with shifting focus points. Often used in macro photography to create "infinite" depth of field.
* **Pixel Shift Stacks:** Found in high-end mirrorless cameras. The sensor moves slightly to capture multiple shots, which are stacked for ultra-high resolution and perfect color.
* **Panorama (Stitched):** A sequence of horizontal or vertical shots intended to be merged into a single wide-field image.

### Video & Audio Stacks

* **Proxy/Optimized Stacks:** Pairs a heavy "Master" file (like 8K RAW) with a lightweight "Proxy" (like 1080p ProRes) for smoother editing performance.
* **Chaptered Video:** Action cameras (like GoPro) often split long recordings into 4GB chunks. We stack files like `GOPR001.mp4` and `GOPR002.mp4` so they appear as one continuous video.
* **Dual-System Audio:** Groups video files with high-quality external audio (WAV/AIFF) using timecode or waveform matching.

## Recycling

When you delete an asset, it defaults to trash (i.e. soft delete). On sync, new items in trash are essentially a metadata update rather than removal. A true "delete" operation is only performed when the user explicitly empties the trash, the asset has been in the trash for its full retention period, or the user requests immediate deletion.

For consistency, deletion of assets is functionally similar to addition and modification of assets. See [Provenance of Library Modifications](/design/cryptography/#provenance-of-library-modifications) and [Authorization — The Closed Action Set](/design/authorization/#the-closed-action-set).

### Retention Window

The trash retention window is **signed into the `delete` manifest at delete time** as the `retention_until` field — not server-configured at purge time. The default is 30 days; the user can extend it per delete or per album-policy. Because the retention is part of the signed manifest:

- The server **cannot accelerate** a purge by changing a server-side config — the cryptographic floor on retention is the signed manifest's `retention_until`. A hard purge before that timestamp is rejected (the server's purge worker reads `retention_until` from the manifest, not from a local policy).
- The server **cannot delay** a purge beyond an order issued by a `trash-restore` or a signed shorter-retention re-issue — the user remains in control.
- A `trash-restore` action issued before `retention_until` recovers the asset, appends a new provenance record, and rewinds the local lifecycle state. The original delete manifest is **not removed** from the provenance chain — it remains as a record of "this was deleted on date X and restored on date Y."

This addresses the damage scenario where a hostile server unilaterally accelerates a purge to delete an asset the user expected to be recoverable, as well as the scenario where a buggy server retains data past the user's chosen window.
