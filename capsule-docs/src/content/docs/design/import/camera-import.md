---
title: Import — Camera Import
description: Tethered and wireless in-camera import over PTP/IP
status: draft
---

**Status: planned, post-v1** (slice `S-B9`, gated on the in-house `ptpip-rs` library — see the repo-root `SLICES.md`). This doc is the owner contract for tethered/wireless camera ingestion so the seam is fixed now; no code exists.

Professional cameras expose their storage over **PTP/IP (ISO 15740 over TCP/IP)**. Capsule speaks it through `ptpip-rs`, an in-house Rust implementation (a library gate like `rawshift` and `spargen`) — portable across every client platform including mobile, which rules out binding libgphoto2 (C, LGPL, desktop-oriented). Vendor extensions (Sony first, per issue #365) live behind the crate; Capsule consumes only the adapter surface below.

## The Camera Is a Source Adapter

A connected camera is an import **source adapter** on the same contract as the filesystem scanner and the [third-party importers](/design/import/pipeline/#third-party-importers): it enumerates media into [Scan & Extract](/design/import/pipeline/#scan--extract), and everything downstream — plan, confirm, execute, upload — is the unmodified [import pipeline](/design/import/pipeline/). Camera import adds no pipeline stage and no server surface.

- **Lives in:** `capsule-core::import::camera` (planned, post-v1) over the gated `ptpip-rs` crate.
- **Public surface:** the shared source-adapter trait; a discovery/pairing surface for clients (list cameras, connect, session state).
- **Defers to:** [Import — Pipeline](/design/import/pipeline/) (planner/executor), [Metadata](/design/metadata/) (extraction schema), [Import — Upload Protocol](/design/import/upload-protocol/) (upload), and the pipeline's dedup rules (below).

## Contract

- **Deterministic enumeration.** PTP object handles are enumerated in a fixed order — by storage id, then object handle id, with capture date as the tiebreaker — so the pure planner's determinism contract holds: the same camera state yields the same `ImportPlan`.
- **Dedupe on re-import is by content, not by source.** The planner's existing content-hash dedup is what prevents a re-connected card or camera from re-importing prior assets; the adapter does not track "seen" state of its own. Pulling the same card out of the camera and importing it over the filesystem scanner dedups identically.
- **Integrity end-to-end.** PTP/IP carries no checksums of its own; TCP integrity is treated as transport-grade only. Every transferred object is hashed on receipt, and that hash *is* the content hash the [manifest](/design/cryptography/provenance/#asset-manifest) commits to — a corrupted transfer surfaces as a hash mismatch at import, before anything is signed.
- **Resumable per object.** A dropped connection resumes at the object boundary (partial-object ranged transfer where the camera supports `GetPartialObject`); completed objects are never re-pulled (dedup covers the crash window).
- **Read-only by default.** The adapter never deletes or mutates camera storage. A delete-after-verified-import option (Sony's extension surface) is a client feature that may come later; it must gate on the same [verify-before-destroy](/design/import/storage-verification/#verify-before-destroy) rule as every destructive path.
- **Discovery and pairing.** mDNS (`_ptp._tcp`) where the camera advertises, manual IP entry otherwise; the PTP/IP pairing handshake (GUID + friendly name) is remembered per camera. Pairing state is client-local config, not library metadata.

## Validation

- **Unit** — the adapter against a mock PTP/IP responder: deterministic enumeration order, resume-at-object-boundary, hash-mismatch rejection, read-only invariant (no write/delete op ever issued in a standard import).
- **Smoke** — a bench lane against real hardware (Sony first), exercised where hardware CI allows, mirroring the hardware-signer smoke posture.
- **No new E2E case:** once live, camera import rides bounded E2E case 2 (full import + upload + finalize) with the camera adapter as the source — the [Module Map](/design/module-map/#e2e-test-surface) bound is unchanged.
