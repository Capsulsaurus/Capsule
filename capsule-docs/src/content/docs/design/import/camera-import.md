---
title: Import — Camera Import
description: Tethered and wireless in-camera import over PTP/IP
status: draft
---

**Planned, post-v1; no code exists** (`capsule-core::import::camera`). Cameras expose storage over **PTP/IP** (ISO 15740), spoken through the in-house `ptpip-rs` crate — portable to mobile, unlike LGPL libgphoto2. Vendor extensions stay behind that crate.

## The Camera Is a Source Adapter

A camera is an import source adapter on the same contract as the filesystem scanner and the [third-party importers](/design/import/pipeline/#third-party-importers): it feeds [Scan & Extract](/design/import/pipeline/#scan--extract), and planning, execution and [upload](/design/transfer/upload/) are unchanged. It adds no pipeline stage and no server surface.

## Contract

- **Deterministic enumeration** by storage id, then object handle, then capture date, so the planner stays deterministic.
- **Dedup by content, not source.** The planner's content-hash dedup prevents re-import; the adapter keeps no "seen" state.
- **Integrity.** Every object is hashed on receipt, and that hash is the one the [manifest](/design/cryptography/provenance/#asset-manifest) commits to; corruption fails at import, before signing.
- **Resumable per object**, with partial-object transfer where the camera supports it.
- **Read-only.** The adapter never mutates camera storage. Any future delete-after-import MUST pass [Verify Before Destroy](/design/transfer/durability/#verify-before-destroy).
- **Discovery** via mDNS (`_ptp._tcp`) or manual IP; pairing state is client-local config.
